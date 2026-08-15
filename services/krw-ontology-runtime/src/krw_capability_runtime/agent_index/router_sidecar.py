"""Serving Index V5 router sidecar for immutable v3 ontology releases.

The sidecar is a deterministic, derived search artifact.  It does not change
the ontology schema or the company/global-spine databases that remain the
canonical evidence stores.
"""

from __future__ import annotations

import hashlib
import html
import json
import math
import os
import re
import shutil
import sqlite3
import time
import unicodedata
from collections import Counter, OrderedDict, defaultdict
from collections.abc import Iterable, Mapping, Sequence
from dataclasses import dataclass, field
from functools import lru_cache
from itertools import combinations
from pathlib import Path
from typing import Any
from urllib.parse import quote

from krw_capability_runtime.agent_index.cache_seal import (
    assert_trusted_immutable_copy,
    immutable_sqlite_cache_seal_path,
    read_immutable_sqlite_cache_seal,
    read_immutable_sqlite_cache_sha256,
    remove_immutable_sqlite_cache_seal,
    write_immutable_sqlite_cache_seal,
)
from krw_capability_runtime.agent_index.router_coherence import (
    ROUTER_COHERENCE_RELATIVE_PATH,
    RouterCoherence,
)
from krw_capability_runtime.agent_index.spine_schema import (
    read_spine_verification_sha256,
    record_spine_verification_sha256,
)

ROUTER_SIDECAR_SCHEMA_VERSION = "krw-ontology-router-sidecar/v6"
ROUTER_SIDECAR_BUILDER_VERSION = "router-sidecar-builder/v6"
ROUTER_SIDECAR_RELATIVE_PATH = Path("indexes") / "router_sidecar.sqlite"
ROUTER_RANKING_PROFILE_FILENAME = "router_ranking_v1.json"
DEFAULT_ROUTER_SQLITE_THREADS = 8
DEFAULT_ROUTER_SQLITE_CACHE_KIB = 524_288
DEFAULT_ROUTER_SOURCE_MMAP_BYTES = 1_073_741_824

ROUTER_SIDECAR_TABLES = (
    "metadata",
    "ranking_profile",
    "ticker_profile",
    "alias_lookup",
    "routing_unit",
    "routing_fts",
    "routing_term_posting",
    "micro_routing_unit",
    "micro_routing_fts",
    "term_stats",
    "facet_posting",
    "short_token_posting",
    "graph_prior",
)

ROUTER_SIDECAR_REQUIRED_METADATA_KEYS = (
    "schema_version",
    "builder_version",
    "source_global_spine_sha256",
    "ranking_profile_sha256",
    "coarse_profile_sha256",
    "schema_sql_sha256",
    "content_sha256",
    "build_fingerprint_sha256",
    "counts",
)


def _connect_immutable_readonly(path: Path) -> sqlite3.Connection:
    uri = f"file:{quote(path.resolve().as_posix(), safe='/')}?mode=ro&immutable=1"
    return sqlite3.connect(uri, uri=True)


_WORD_RE = re.compile(r"[0-9A-Za-z]+|[가-힣]+")
_HANGUL_RE = re.compile(r"^[가-힣]+$")
_SEC_REGISTRANT_NAME_RE = re.compile(
    r"(?m)^([^\n|]{2,200})\s*\n\s*\n"
    r"\(Exact name of Registrant as specified in its charter\)",
    re.IGNORECASE,
)
_SEC_INLINE_XBRL_REGISTRANT_RE = re.compile(
    r"<ix:nonNumeric\b"
    r"(?=[^>]*\bname\s*=\s*['\"]dei:EntityRegistrantName['\"])"
    r"[^>]*>(.*?)</ix:nonNumeric\s*>",
    re.IGNORECASE | re.DOTALL,
)
_HTML_TAG_RE = re.compile(r"<[^>]+>")

_MICRO_ROUTING_FTS_SQL = """
CREATE VIRTUAL TABLE IF NOT EXISTS micro_routing_fts USING fts5(
    micro_unit_id UNINDEXED,
    routing_unit_id,
    ticker UNINDEXED,
    topic_text,
    factor_text,
    metric_text,
    entity_text,
    mechanism_text,
    tokenize = 'unicode61 remove_diacritics 2'
);
"""

_SCHEMA_SQL = f"""
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS metadata (
    key TEXT PRIMARY KEY,
    value_json TEXT NOT NULL
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS ranking_profile (
    profile_id TEXT PRIMARY KEY,
    profile_format TEXT NOT NULL,
    profile_version INTEGER NOT NULL,
    config_json TEXT NOT NULL,
    config_sha256 TEXT NOT NULL,
    active INTEGER NOT NULL CHECK(active IN (0, 1))
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS ticker_profile (
    ticker TEXT PRIMARY KEY,
    company_name TEXT NOT NULL,
    aliases_json TEXT NOT NULL,
    routing_unit_count INTEGER NOT NULL,
    topic_family_count INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS routing_unit (
    routing_unit_id TEXT PRIMARY KEY,
    ticker TEXT NOT NULL,
    company_name TEXT NOT NULL,
    topic_family_key TEXT NOT NULL,
    topic_family_label TEXT NOT NULL,
    company_text TEXT NOT NULL,
    topic_text TEXT NOT NULL,
    factor_text TEXT NOT NULL,
    metric_text TEXT NOT NULL,
    entity_text TEXT NOT NULL,
    mechanism_text TEXT NOT NULL,
    source_object_ids_json TEXT NOT NULL,
    evidence_grades_json TEXT NOT NULL,
    materiality_max REAL,
    topic_count INTEGER NOT NULL,
    factor_count INTEGER NOT NULL,
    metric_count INTEGER NOT NULL,
    entity_count INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS alias_lookup (
    alias_norm TEXT NOT NULL,
    alias_kind TEXT NOT NULL,
    canonical_key TEXT NOT NULL,
    ticker TEXT NOT NULL,
    routing_unit_id TEXT NOT NULL,
    display_label TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    priority INTEGER NOT NULL,
    PRIMARY KEY (
        alias_norm, alias_kind, canonical_key, ticker,
        routing_unit_id, source_kind
    )
) WITHOUT ROWID;

CREATE VIRTUAL TABLE IF NOT EXISTS routing_fts USING fts5(
    routing_unit_id UNINDEXED,
    ticker UNINDEXED,
    company_text,
    topic_text,
    factor_text,
    metric_text,
    entity_text,
    mechanism_text,
    tokenize = 'unicode61 remove_diacritics 2'
);

CREATE TABLE IF NOT EXISTS routing_term_posting (
    term_norm TEXT NOT NULL,
    fts_rowid INTEGER NOT NULL,
    PRIMARY KEY (term_norm, fts_rowid)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS term_stats (
    term_norm TEXT PRIMARY KEY,
    fts_unit_df INTEGER NOT NULL CHECK(fts_unit_df >= 0),
    fts_ticker_df INTEGER NOT NULL CHECK(fts_ticker_df >= 0),
    alias_unit_df INTEGER NOT NULL CHECK(alias_unit_df >= 0),
    alias_ticker_df INTEGER NOT NULL CHECK(alias_ticker_df >= 0)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS micro_routing_unit (
    micro_unit_id TEXT PRIMARY KEY,
    routing_unit_id TEXT NOT NULL REFERENCES routing_unit(routing_unit_id),
    ticker TEXT NOT NULL,
    topic_family_key TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    source_cluster_key TEXT NOT NULL,
    topic_text TEXT NOT NULL,
    factor_text TEXT NOT NULL,
    metric_text TEXT NOT NULL,
    entity_text TEXT NOT NULL,
    mechanism_text TEXT NOT NULL,
    source_object_ids_json TEXT NOT NULL,
    evidence_grade TEXT NOT NULL,
    materiality REAL
) WITHOUT ROWID;

{_MICRO_ROUTING_FTS_SQL}

CREATE TABLE IF NOT EXISTS facet_posting (
    facet_kind TEXT NOT NULL,
    facet_key TEXT NOT NULL,
    facet_label TEXT NOT NULL,
    ticker TEXT NOT NULL,
    routing_unit_id TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    source_count INTEGER NOT NULL,
    sample_object_id TEXT NOT NULL,
    sample_document_id TEXT NOT NULL,
    sample_period TEXT NOT NULL,
    sample_document_type TEXT NOT NULL,
    evidence_grade TEXT NOT NULL,
    materiality_max REAL,
    PRIMARY KEY (
        facet_kind, facet_key, ticker, routing_unit_id
    )
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS short_token_posting (
    term_norm TEXT NOT NULL,
    routing_unit_id TEXT NOT NULL,
    ticker TEXT NOT NULL,
    field_name TEXT NOT NULL,
    occurrence_count INTEGER NOT NULL,
    PRIMARY KEY (term_norm, routing_unit_id, field_name)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS graph_prior (
    from_ticker TEXT NOT NULL,
    to_ticker TEXT NOT NULL,
    shared_key_type TEXT NOT NULL,
    shared_key TEXT NOT NULL,
    link_type TEXT NOT NULL,
    weight REAL NOT NULL,
    confidence REAL,
    evidence_grade TEXT NOT NULL,
    materiality REAL,
    explanation TEXT NOT NULL,
    PRIMARY KEY (
        from_ticker, to_ticker, shared_key_type, shared_key, link_type
    )
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_routing_unit_ticker
    ON routing_unit(ticker, topic_family_key);
CREATE INDEX IF NOT EXISTS idx_micro_routing_unit_parent
    ON micro_routing_unit(routing_unit_id, ticker);
CREATE INDEX IF NOT EXISTS idx_micro_routing_unit_source
    ON micro_routing_unit(source_kind, source_cluster_key, ticker);
CREATE INDEX IF NOT EXISTS idx_short_token_term
    ON short_token_posting(term_norm, ticker);
CREATE INDEX IF NOT EXISTS idx_graph_prior_to
    ON graph_prior(to_ticker, shared_key_type, shared_key);
"""

_CONTENT_HASH_TABLES = (
    "ranking_profile",
    "ticker_profile",
    "alias_lookup",
    "routing_unit",
    "routing_term_posting",
    "term_stats",
    "micro_routing_unit",
    "facet_posting",
    "short_token_posting",
    "graph_prior",
)

_CONTENT_HASH_ORDER_BY = {
    "ranking_profile": "profile_id",
    "ticker_profile": "ticker",
    "alias_lookup": ("alias_norm, alias_kind, canonical_key, ticker, routing_unit_id, source_kind"),
    "routing_unit": "routing_unit_id",
    "routing_term_posting": "term_norm, fts_rowid",
    "term_stats": "term_norm",
    "micro_routing_unit": "micro_unit_id",
    "facet_posting": ("facet_kind, facet_key, ticker, routing_unit_id"),
    "short_token_posting": "term_norm, routing_unit_id, field_name",
    "graph_prior": ("from_ticker, to_ticker, shared_key_type, shared_key, link_type"),
}


@dataclass(frozen=True)
class RouterSidecarBuildResult:
    path: Path
    counts: Mapping[str, int]
    metadata: Mapping[str, Any]
    verification: Mapping[str, Any]
    elapsed_ms: int


def _configure_router_sidecar_build_connections(
    source: sqlite3.Connection,
    target: sqlite3.Connection,
) -> dict[str, int | str]:
    """Apply bounded bulk-build settings and report what SQLite accepted."""
    source.execute("PRAGMA query_only=ON")
    requested_mmap = _environment_integer(
        "KRW_ROUTER_SOURCE_MMAP_BYTES",
        DEFAULT_ROUTER_SOURCE_MMAP_BYTES,
        minimum=0,
    )
    source.execute(f"PRAGMA mmap_size={requested_mmap}")

    journal_mode = str(target.execute("PRAGMA journal_mode=OFF").fetchone()[0])
    target.execute("PRAGMA synchronous=OFF")
    # Multi-million-row FTS sorts must spill to disk instead of consuming an
    # unbounded amount of resident memory.
    target.execute("PRAGMA temp_store=FILE")
    cache_kib = _environment_integer(
        "KRW_ROUTER_SQLITE_CACHE_KIB",
        DEFAULT_ROUTER_SQLITE_CACHE_KIB,
        minimum=1,
    )
    target.execute(f"PRAGMA cache_size=-{cache_kib}")
    compiled_threads = _sqlite_compile_max_worker_threads(target)
    requested_threads = _environment_integer(
        "KRW_ROUTER_SQLITE_THREADS",
        DEFAULT_ROUTER_SQLITE_THREADS,
        minimum=0,
    )
    thread_limit = min(
        requested_threads,
        compiled_threads,
        max(0, int(os.cpu_count() or 1) - 1),
    )
    target.execute(f"PRAGMA threads={thread_limit}")
    return {
        "sqlite_version": sqlite3.sqlite_version,
        "journal_mode": journal_mode,
        "synchronous": int(target.execute("PRAGMA synchronous").fetchone()[0]),
        "temp_store": int(target.execute("PRAGMA temp_store").fetchone()[0]),
        "cache_size_kib": abs(int(target.execute("PRAGMA cache_size").fetchone()[0])),
        "compiled_max_worker_threads": compiled_threads,
        "requested_threads": requested_threads,
        "sqlite_threads": int(target.execute("PRAGMA threads").fetchone()[0]),
        "source_mmap_bytes": int(source.execute("PRAGMA mmap_size").fetchone()[0]),
    }


def _sqlite_compile_max_worker_threads(conn: sqlite3.Connection) -> int:
    for row in conn.execute("PRAGMA compile_options"):
        option = str(row[0] or "")
        if option.startswith("MAX_WORKER_THREADS="):
            try:
                return max(0, int(option.split("=", 1)[1]))
            except ValueError:
                return 0
    return 0


def _environment_integer(name: str, default: int, *, minimum: int) -> int:
    raw = os.getenv(name)
    if raw is None:
        return max(minimum, int(default))
    try:
        return max(minimum, int(raw))
    except ValueError:
        return max(minimum, int(default))


@dataclass
class _RoutingUnitAccumulator:
    ticker: str
    company_name: str
    family_key: str
    family_labels: set[str] = field(default_factory=set)
    topics: set[str] = field(default_factory=set)
    factors: set[str] = field(default_factory=set)
    metrics: set[str] = field(default_factory=set)
    entities: set[str] = field(default_factory=set)
    mechanisms: set[str] = field(default_factory=set)
    source_object_ids: set[str] = field(default_factory=set)
    evidence_grades: set[str] = field(default_factory=set)
    materiality_max: float | None = None
    topic_count: int = 0
    factor_count: int = 0
    metric_count: int = 0
    entity_count: int = 0


@dataclass
class _MicroUnitAccumulator:
    ticker: str
    family_key: str
    source_kind: str
    source_cluster_key: str
    topics: set[str] = field(default_factory=set)
    factors: set[str] = field(default_factory=set)
    metrics: set[str] = field(default_factory=set)
    entities: set[str] = field(default_factory=set)
    mechanisms: set[str] = field(default_factory=set)
    source_object_ids: set[str] = field(default_factory=set)
    evidence_grades: set[str] = field(default_factory=set)
    materiality: float | None = None


@dataclass(frozen=True)
class _QueryTermStat:
    term: str
    fts_unit_df: int
    fts_ticker_df: int
    alias_unit_df: int
    alias_ticker_df: int
    information: float
    unit_df_ratio: float
    ticker_df_ratio: float

    def as_dict(self, *, facet_eligible: bool) -> dict[str, Any]:
        return {
            "term": self.term,
            "fts_unit_df": self.fts_unit_df,
            "fts_ticker_df": self.fts_ticker_df,
            "alias_unit_df": self.alias_unit_df,
            "alias_ticker_df": self.alias_ticker_df,
            "information": self.information,
            "unit_df_ratio": self.unit_df_ratio,
            "ticker_df_ratio": self.ticker_df_ratio,
            "facet_eligible": facet_eligible,
        }


def build_router_sidecar(
    global_spine_path: Path | str,
    output_path: Path | str | None = None,
    *,
    release_id: str | None = None,
    ranking_profile_path: Path | str | None = None,
) -> RouterSidecarBuildResult:
    """Build the immutable Serving Index V5 sidecar from a v3 global spine."""
    started_at = time.perf_counter()
    source_path = Path(global_spine_path).expanduser().resolve()
    if not source_path.is_file():
        raise FileNotFoundError(f"Global spine not found: {source_path}")
    target_path = (
        Path(output_path).expanduser().resolve()
        if output_path is not None
        else source_path.parent / ROUTER_SIDECAR_RELATIVE_PATH.name
    )
    profile = load_router_ranking_profile(ranking_profile_path)
    profile_sha256 = _json_sha256(profile)
    coarse_profile_sha256 = _coarse_profile_sha256(profile)
    source_sha256 = immutable_file_sha256(source_path)
    schema_sha256 = hashlib.sha256(_SCHEMA_SQL.encode("utf-8")).hexdigest()
    profile_id = str(profile["profile_id"])
    build_config = profile.get("build") if isinstance(profile.get("build"), Mapping) else {}
    overview_family_key = (
        normalize_router_text(build_config.get("overview_family_key") or "__company__") or "company"
    )

    target_path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = target_path.parent / f".{target_path.name}.{os.getpid()}.{time.time_ns()}.tmp"
    _cleanup_sqlite_files(tmp_path)
    try:
        with (
            _connect_immutable_readonly(source_path) as source,
            sqlite3.connect(tmp_path) as target,
        ):
            source.row_factory = sqlite3.Row
            target.row_factory = sqlite3.Row
            build_settings = _configure_router_sidecar_build_connections(
                source,
                target,
            )
            target.executescript(_SCHEMA_SQL)

            source_metadata = _read_metadata(source)
            company_names = _collect_company_names(
                source,
                release_root=source_path.parent.parent,
            )
            units = _collect_routing_units(
                source,
                facet_target=target,
                company_names=company_names,
                overview_family_key=overview_family_key,
                max_values_per_field=int(build_config["max_values_per_field"]),
                max_source_object_ids=int(build_config["max_source_object_ids"]),
                max_value_chars=int(build_config["max_value_chars"]),
            )
            unit_ids = {key: _routing_unit_id(key[0], key[1]) for key in sorted(units)}
            _write_ranking_profile(target, profile, profile_sha256)
            _write_routing_units(
                target,
                units=units,
                unit_ids=unit_ids,
                profile=profile,
            )
            # The source-coherent micro tier is experimental and materially
            # increases build size and latency. A production-safe coarse build
            # leaves the tables empty when the active profile disables it;
            # build_router_micro_derivative remains the explicit experiment path.
            if bool(profile["micro_rerank"]["enabled"]):
                _write_micro_routing_units(
                    source,
                    target,
                    profile=profile,
                    overview_family_key=overview_family_key,
                )
            _write_ticker_profiles_and_company_aliases(
                target,
                company_names=company_names,
                units=units,
                unit_ids=unit_ids,
                profile=profile,
            )
            _write_short_token_postings(target)
            _write_graph_priors(source, target)

            counts = _sidecar_counts(target)
            content_sha256 = _content_sha256(target)
            resolved_release_id = release_id or source_metadata.get("release_id")
            build_fingerprint = _json_sha256(
                {
                    "builder_version": ROUTER_SIDECAR_BUILDER_VERSION,
                    "content_sha256": content_sha256,
                    "ranking_profile_sha256": profile_sha256,
                    "release_id": resolved_release_id,
                    "schema_sql_sha256": schema_sha256,
                    "schema_version": ROUTER_SIDECAR_SCHEMA_VERSION,
                    "source_global_spine_sha256": source_sha256,
                }
            )
            metadata = {
                "schema_version": ROUTER_SIDECAR_SCHEMA_VERSION,
                "builder_version": ROUTER_SIDECAR_BUILDER_VERSION,
                "release_id": resolved_release_id,
                "source_global_spine_sha256": source_sha256,
                "source_global_spine_schema_version": source_metadata.get("schema_version"),
                "source_manifest_hash": source_metadata.get("source_manifest_hash"),
                "source_created_at": source_metadata.get("created_at"),
                "ranking_profile_id": profile_id,
                "ranking_profile_sha256": profile_sha256,
                "coarse_profile_sha256": coarse_profile_sha256,
                "schema_sql_sha256": schema_sha256,
                "content_sha256": content_sha256,
                "build_fingerprint_sha256": build_fingerprint,
                "build_settings": build_settings,
                "counts": counts,
            }
            _write_metadata(target, metadata)
            target.commit()
            target.execute("INSERT INTO routing_fts(routing_fts) VALUES('optimize')")
            target.execute("INSERT INTO micro_routing_fts(micro_routing_fts) VALUES('optimize')")
            target.commit()
            target.execute("VACUUM")
        verification = verify_router_sidecar(
            tmp_path,
            expected_global_spine_sha256=source_sha256,
            expected_release_id=resolved_release_id,
            deep=True,
        )
        if not verification["ok"]:
            raise RuntimeError(
                "router sidecar failed verification: " + ", ".join(verification["errors"])
            )
        remove_immutable_sqlite_cache_seal(target_path)
        os.replace(tmp_path, target_path)
        write_immutable_sqlite_cache_seal(
            target_path,
            kind="router_sidecar",
            cache_key=str(
                (verification.get("metadata") or {}).get("build_fingerprint_sha256") or ""
            ),
            verification=verification,
            metadata=verification.get("metadata") or {},
            counts=verification.get("counts") or {},
            source_path=source_path,
        )
    finally:
        _cleanup_sqlite_files(tmp_path)

    verification = {
        **verification,
        "path": str(target_path),
    }
    return RouterSidecarBuildResult(
        path=target_path,
        counts=verification.get("counts") or {},
        metadata=verification.get("metadata") or {},
        verification=verification,
        elapsed_ms=int((time.perf_counter() - started_at) * 1000),
    )


def build_router_micro_derivative(
    global_spine_path: Path | str,
    base_sidecar_path: Path | str,
    output_path: Path | str,
    *,
    ranking_profile_path: Path | str | None = None,
) -> RouterSidecarBuildResult:
    """Add the v4 source-coherent micro tier to a verified coarse sidecar copy.

    This exists for bounded index experiments and rolling rebuilds.  It never
    mutates the base artifact, and it accepts a v2 coarse artifact only when
    its semantic content hash and immutable source-spine binding both verify.
    A normal release build should continue to use :func:`build_router_sidecar`.
    """
    started_at = time.perf_counter()
    source_path = Path(global_spine_path).expanduser().resolve()
    base_path = Path(base_sidecar_path).expanduser().resolve()
    target_path = Path(output_path).expanduser().resolve()
    if not source_path.is_file():
        raise FileNotFoundError(f"Global spine not found: {source_path}")
    if not base_path.is_file():
        raise FileNotFoundError(f"Base router sidecar not found: {base_path}")
    if target_path == base_path:
        raise ValueError("micro derivative output must differ from the base sidecar")

    source_sha256 = immutable_file_sha256(source_path)
    with sqlite3.connect(base_path) as base:
        base.row_factory = sqlite3.Row
        integrity = base.execute("PRAGMA integrity_check").fetchall()
        if [tuple(row) for row in integrity] != [("ok",)]:
            raise ValueError("base router sidecar failed SQLite integrity check")
        base_tables = {
            str(row[0])
            for row in base.execute(
                "SELECT name FROM sqlite_master WHERE type IN ('table', 'view')"
            )
        }
        required_coarse_tables = {
            "metadata",
            "ranking_profile",
            "ticker_profile",
            "routing_unit",
            "routing_fts",
            "term_stats",
            "alias_lookup",
            "facet_posting",
            "short_token_posting",
            "graph_prior",
        }
        missing = sorted(required_coarse_tables - base_tables)
        if missing:
            raise ValueError("base router sidecar missing coarse tables: " + ", ".join(missing))
        base_metadata = _read_metadata(base)
        if base_metadata.get("source_global_spine_sha256") != source_sha256:
            raise ValueError("base router sidecar source-spine binding mismatch")
        if base_metadata.get("content_sha256") != _content_sha256(base):
            raise ValueError("base router sidecar semantic content hash mismatch")
        profile_errors = _profile_verification_errors(base, base_metadata)
        if profile_errors:
            raise ValueError(
                "base router sidecar ranking profile invalid: " + ", ".join(profile_errors)
            )
        base_profile_row = base.execute(
            """
            SELECT config_json FROM ranking_profile
            WHERE active = 1 ORDER BY profile_id LIMIT 1
            """
        ).fetchone()
        if base_profile_row is None:
            raise ValueError("base router sidecar active ranking profile is missing")
        base_profile = json.loads(str(base_profile_row["config_json"]))
        base_coarse_profile_sha256 = _coarse_profile_sha256(base_profile)

    profile = load_router_ranking_profile(ranking_profile_path)
    profile_sha256 = _json_sha256(profile)
    coarse_profile_sha256 = _coarse_profile_sha256(profile)
    if coarse_profile_sha256 != base_coarse_profile_sha256:
        raise ValueError(
            "base router sidecar coarse ranking/build profile is incompatible; "
            "use a full sidecar rebuild"
        )
    schema_sha256 = hashlib.sha256(_SCHEMA_SQL.encode("utf-8")).hexdigest()
    build_config = profile["build"]
    overview_family_key = (
        normalize_router_text(build_config.get("overview_family_key") or "__company__") or "company"
    )
    target_path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = target_path.parent / f".{target_path.name}.{os.getpid()}.{time.time_ns()}.tmp"
    _cleanup_sqlite_files(tmp_path)
    try:
        shutil.copyfile(base_path, tmp_path)
        with (
            _connect_immutable_readonly(source_path) as source,
            sqlite3.connect(tmp_path) as target,
        ):
            source.row_factory = sqlite3.Row
            target.row_factory = sqlite3.Row
            source.execute("PRAGMA query_only = ON")
            source.execute("PRAGMA temp_store = FILE")
            target.execute("PRAGMA journal_mode = OFF")
            target.execute("PRAGMA synchronous = OFF")
            target.execute("PRAGMA temp_store = FILE")
            target.executescript(_SCHEMA_SQL)
            target.execute("DELETE FROM micro_routing_unit")
            _recreate_micro_routing_fts(target)
            target.execute("DELETE FROM ranking_profile")
            _write_ranking_profile(target, profile, profile_sha256)
            _write_micro_routing_units(
                source,
                target,
                profile=profile,
                overview_family_key=overview_family_key,
            )
            counts = _sidecar_counts(target)
            content_sha256 = _content_sha256(target)
            release_id = base_metadata.get("release_id")
            build_fingerprint = _json_sha256(
                {
                    "builder_version": ROUTER_SIDECAR_BUILDER_VERSION,
                    "content_sha256": content_sha256,
                    "ranking_profile_sha256": profile_sha256,
                    "release_id": release_id,
                    "schema_sql_sha256": schema_sha256,
                    "schema_version": ROUTER_SIDECAR_SCHEMA_VERSION,
                    "source_global_spine_sha256": source_sha256,
                }
            )
            metadata = {
                **base_metadata,
                "schema_version": ROUTER_SIDECAR_SCHEMA_VERSION,
                "builder_version": ROUTER_SIDECAR_BUILDER_VERSION,
                "ranking_profile_id": profile["profile_id"],
                "ranking_profile_sha256": profile_sha256,
                "coarse_profile_sha256": coarse_profile_sha256,
                "schema_sql_sha256": schema_sha256,
                "content_sha256": content_sha256,
                "build_fingerprint_sha256": build_fingerprint,
                "counts": counts,
                "derivative_base_content_sha256": base_metadata.get("content_sha256"),
                "derivative_base_coarse_profile_sha256": (base_coarse_profile_sha256),
            }
            _write_metadata(target, metadata)
            target.commit()
            target.execute("INSERT INTO micro_routing_fts(micro_routing_fts) VALUES('optimize')")
            target.commit()
            target.execute("VACUUM")
        remove_immutable_sqlite_cache_seal(target_path)
        os.replace(tmp_path, target_path)
    finally:
        _cleanup_sqlite_files(tmp_path)

    verification = verify_router_sidecar(
        target_path,
        expected_global_spine_sha256=source_sha256,
        expected_release_id=base_metadata.get("release_id"),
        deep=True,
    )
    if not verification["ok"]:
        raise RuntimeError(
            "router micro derivative failed verification: " + ", ".join(verification["errors"])
        )
    write_immutable_sqlite_cache_seal(
        target_path,
        kind="router_sidecar",
        cache_key=str((verification.get("metadata") or {}).get("build_fingerprint_sha256") or ""),
        verification=verification,
        metadata=verification.get("metadata") or {},
        counts=verification.get("counts") or {},
    )
    return RouterSidecarBuildResult(
        path=target_path,
        counts=verification.get("counts") or {},
        metadata=verification.get("metadata") or {},
        verification=verification,
        elapsed_ms=int((time.perf_counter() - started_at) * 1000),
    )


def load_router_ranking_profile(path: Path | str | None = None) -> dict[str, Any]:
    """Load and validate the versioned ranking profile used by build and query."""
    profile_path = (
        Path(path).expanduser().resolve()
        if path is not None
        else Path(__file__).with_name(ROUTER_RANKING_PROFILE_FILENAME)
    )
    try:
        profile = json.loads(profile_path.read_text(encoding="utf-8"))
    except FileNotFoundError as exc:
        raise FileNotFoundError(f"Router ranking profile not found: {profile_path}") from exc
    except json.JSONDecodeError as exc:
        raise ValueError(f"Invalid router ranking profile JSON: {profile_path}") from exc
    if not isinstance(profile, dict):
        raise ValueError("router ranking profile must be a JSON object")
    required = {
        "format",
        "profile_id",
        "version",
        "rrf_k",
        "candidate_limit",
        "default_limit",
        "max_query_terms",
        "channel_weights",
        "fusion",
        "fts_field_weights",
        "bigram",
        "aggregation",
        "micro_rerank",
        "alias",
        "lexical",
        "graph",
        "build",
    }
    missing = sorted(required - set(profile))
    if missing:
        raise ValueError(f"router ranking profile missing keys: {', '.join(missing)}")
    if profile.get("format") != "krw-ontology-router-ranking-profile/v1":
        raise ValueError("router ranking profile format is unsupported")
    if not str(profile.get("profile_id") or "").strip():
        raise ValueError("router ranking profile_id is required")
    _validate_router_ranking_profile(profile)
    return profile


def _validate_router_ranking_profile(profile: Mapping[str, Any]) -> None:
    _require_positive_int(profile, "version")
    _require_positive_number(profile, "rrf_k")
    candidate_limit = _require_positive_int(profile, "candidate_limit")
    default_limit = _require_positive_int(profile, "default_limit")
    _require_positive_int(profile, "max_query_terms")
    if default_limit > candidate_limit:
        raise ValueError("router ranking profile default_limit exceeds candidate_limit")

    channels = _require_mapping(profile, "channel_weights")
    _require_weight_map(
        channels,
        keys=("exact_alias", "facet", "fielded_fts", "short_token"),
        label="channel_weights",
    )
    fusion = _require_mapping(profile, "fusion")
    _require_positive_number(fusion, "primary_rrf_scale")
    fts_fields = (
        "company_text",
        "topic_text",
        "factor_text",
        "metric_text",
        "entity_text",
        "mechanism_text",
    )
    _require_weight_map(
        _require_mapping(profile, "fts_field_weights"),
        keys=fts_fields,
        label="fts_field_weights",
    )
    bigram = _require_mapping(profile, "bigram")
    minimum_primary = bigram.get("minimum_primary_results")
    if (
        not isinstance(minimum_primary, int)
        or isinstance(minimum_primary, bool)
        or minimum_primary < 0
    ):
        raise ValueError("router ranking profile bigram.minimum_primary_results must be >= 0")
    _require_weight_map(
        _require_mapping(bigram, "field_weights"),
        keys=fts_fields,
        label="bigram.field_weights",
    )
    _require_bool(bigram, "hangul_only_when_hangul_present")
    _require_ratio(bigram, "minimum_ngram_coverage_ratio")

    lexical = _require_mapping(profile, "lexical")
    _require_positive_int(lexical, "candidate_multiplier")
    _require_ratio(lexical, "min_should_match_ratio")
    _require_ratio(lexical, "relaxed_penalty")
    _require_weight_map(
        lexical,
        keys=("coverage_weight", "information_coverage_weight", "bm25_weight"),
        label="lexical",
    )

    aggregation = _require_mapping(profile, "aggregation")
    units_per_ticker = _require_positive_int(aggregation, "units_per_ticker")
    score_decay = aggregation.get("score_decay")
    if (
        not isinstance(score_decay, Sequence)
        or isinstance(score_decay, (str, bytes, bytearray))
        or len(score_decay) != units_per_ticker
    ):
        raise ValueError(
            "router ranking profile aggregation.score_decay must match units_per_ticker"
        )
    normalized_decay: list[float] = []
    for value in score_decay:
        if (
            not isinstance(value, (int, float))
            or isinstance(value, bool)
            or not math.isfinite(float(value))
            or float(value) <= 0
        ):
            raise ValueError(
                "router ranking profile aggregation.score_decay values must be positive finite numbers"
            )
        normalized_decay.append(float(value))
    if normalized_decay != sorted(normalized_decay, reverse=True):
        raise ValueError("router ranking profile aggregation.score_decay must be non-increasing")

    micro = _require_mapping(profile, "micro_rerank")
    _require_bool(micro, "enabled")
    minimum_query_terms = _require_positive_int(micro, "minimum_query_terms")
    minimum_coherent_terms = _require_positive_int(micro, "minimum_coherent_terms")
    if minimum_coherent_terms > minimum_query_terms:
        raise ValueError(
            "router ranking profile micro_rerank.minimum_coherent_terms exceeds minimum_query_terms"
        )
    _require_positive_int(micro, "candidate_ticker_multiplier")
    _require_positive_int(micro, "micro_unit_candidate_limit")
    _require_positive_int(micro, "max_pair_queries")
    _require_positive_int(micro, "pair_query_limit")
    _require_positive_int(micro, "anchor_gate_min_tickers")
    _require_positive_int(micro, "minimum_anchor_terms")
    _require_ratio(micro, "anchor_max_ticker_df_ratio")
    _require_nonnegative_number(micro, "anchor_min_information")
    _require_ratio(micro, "min_should_match_ratio")
    _require_bool(micro, "coherence_gate")
    _require_weight_map(
        micro,
        keys=(
            "coverage_weight",
            "information_coverage_weight",
            "bm25_weight",
            "coarse_weight",
            "micro_weight",
        ),
        label="micro_rerank",
    )
    micro_units_per_ticker = _require_positive_int(micro, "units_per_ticker")
    micro_decay = micro.get("score_decay")
    if (
        not isinstance(micro_decay, Sequence)
        or isinstance(micro_decay, (str, bytes, bytearray))
        or len(micro_decay) != micro_units_per_ticker
    ):
        raise ValueError(
            "router ranking profile micro_rerank.score_decay must match units_per_ticker"
        )
    normalized_micro_decay = []
    for value in micro_decay:
        if (
            not isinstance(value, (int, float))
            or isinstance(value, bool)
            or not math.isfinite(float(value))
            or float(value) <= 0
        ):
            raise ValueError(
                "router ranking profile micro_rerank.score_decay values must be "
                "positive finite numbers"
            )
        normalized_micro_decay.append(float(value))
    if normalized_micro_decay != sorted(normalized_micro_decay, reverse=True):
        raise ValueError("router ranking profile micro_rerank.score_decay must be non-increasing")
    if micro.get("fallback") != "preserve_coarse_order":
        raise ValueError(
            "router ranking profile micro_rerank.fallback must be preserve_coarse_order"
        )

    alias = _require_mapping(profile, "alias")
    _require_positive_int(alias, "max_ngram_tokens")
    _require_positive_int(alias, "minimum_unique_token_length")
    _require_positive_int(alias, "per_query_term_cap")
    _require_bool(alias, "allow_company_alias_without_explicit_scope")
    _require_ratio(alias, "facet_max_ticker_df_ratio")
    _require_ratio(alias, "facet_max_unit_df_ratio")
    _require_nonnegative_number(alias, "facet_min_information")

    graph = _require_mapping(profile, "graph")
    graph_enabled = _require_bool(graph, "enabled")
    _require_ratio(graph, "minimum_seed_score")
    if graph_enabled:
        raise ValueError(
            "router ranking profile graph.enabled must remain false until "
            "high-confidence seed expansion is implemented"
        )

    build = _require_mapping(profile, "build")
    _require_positive_int(build, "max_source_object_ids")
    _require_positive_int(build, "max_value_chars")
    _require_positive_int(build, "max_values_per_field")
    source_kinds = build.get("micro_source_kinds")
    allowed_source_kinds = {"topic", "factor", "metric", "counterparty"}
    if (
        not isinstance(source_kinds, Sequence)
        or isinstance(source_kinds, (str, bytes, bytearray))
        or not source_kinds
        or len(source_kinds) != len(set(source_kinds))
        or any(kind not in allowed_source_kinds for kind in source_kinds)
    ):
        raise ValueError(
            "router ranking profile build.micro_source_kinds must be a non-empty "
            "unique subset of topic/factor/metric/counterparty"
        )
    if not str(build.get("overview_family_key") or "").strip():
        raise ValueError("router ranking profile build.overview_family_key is required")


def _require_mapping(payload: Mapping[str, Any], key: str) -> Mapping[str, Any]:
    value = payload.get(key)
    if not isinstance(value, Mapping):
        raise ValueError(f"router ranking profile {key} must be an object")
    return value


def _require_positive_int(payload: Mapping[str, Any], key: str) -> int:
    value = payload.get(key)
    if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
        raise ValueError(f"router ranking profile {key} must be a positive integer")
    return value


def _require_positive_number(payload: Mapping[str, Any], key: str) -> float:
    value = payload.get(key)
    if (
        not isinstance(value, (int, float))
        or isinstance(value, bool)
        or not math.isfinite(float(value))
        or float(value) <= 0
    ):
        raise ValueError(f"router ranking profile {key} must be a positive finite number")
    return float(value)


def _require_nonnegative_number(payload: Mapping[str, Any], key: str) -> float:
    value = payload.get(key)
    if (
        not isinstance(value, (int, float))
        or isinstance(value, bool)
        or not math.isfinite(float(value))
        or float(value) < 0
    ):
        raise ValueError(f"router ranking profile {key} must be a non-negative finite number")
    return float(value)


def _require_ratio(payload: Mapping[str, Any], key: str) -> float:
    value = _require_positive_number(payload, key)
    if value > 1:
        raise ValueError(f"router ranking profile {key} must be <= 1")
    return value


def _require_bool(payload: Mapping[str, Any], key: str) -> bool:
    value = payload.get(key)
    if not isinstance(value, bool):
        raise ValueError(f"router ranking profile {key} must be a boolean")
    return value


def _require_weight_map(
    payload: Mapping[str, Any],
    *,
    keys: Sequence[str],
    label: str,
) -> None:
    missing = sorted(set(keys) - set(payload))
    if missing:
        raise ValueError(f"router ranking profile {label} missing keys: {', '.join(missing)}")
    numeric_values: list[float] = []
    for key in keys:
        value = payload.get(key)
        if (
            not isinstance(value, (int, float))
            or isinstance(value, bool)
            or not math.isfinite(float(value))
            or float(value) < 0
        ):
            raise ValueError(
                f"router ranking profile {label}.{key} must be a non-negative finite number"
            )
        numeric_values.append(float(value))
    if not any(value > 0 for value in numeric_values):
        raise ValueError(f"router ranking profile {label} must contain a positive weight")


def create_router_sidecar_schema(conn: sqlite3.Connection) -> None:
    """Create the Serving Index V5 schema in an open SQLite connection."""
    conn.executescript(_SCHEMA_SQL)


def _recreate_micro_routing_fts(conn: sqlite3.Connection) -> None:
    conn.execute("DROP TABLE IF EXISTS micro_routing_fts")
    conn.executescript(_MICRO_ROUTING_FTS_SQL)


def read_router_sidecar_metadata(conn: sqlite3.Connection) -> dict[str, Any]:
    """Read JSON metadata from an open sidecar connection."""
    return _read_metadata(conn)


def verify_router_sidecar(
    path: Path | str,
    *,
    global_spine_path: Path | str | None = None,
    expected_global_spine_sha256: str | None = None,
    expected_release_id: str | None = None,
    expected_ranking_profile_sha256: str | None = None,
    expected_build_fingerprint_sha256: str | None = None,
    deep: bool = True,
) -> dict[str, Any]:
    """Verify schema, profile/content hashes, counts, and optional source binding."""
    resolved = Path(path).expanduser().resolve()
    errors: list[str] = []
    warnings: list[str] = []
    metadata: dict[str, Any] = {}
    counts: dict[str, int] = {}
    tables: list[str] = []
    if not resolved.is_file():
        return {
            "ok": False,
            "errors": ["router_sidecar_missing"],
            "warnings": warnings,
            "path": str(resolved),
            "metadata": metadata,
            "counts": counts,
            "tables": tables,
            "verification_mode": "router-sidecar-deep" if deep else "router-sidecar-light",
        }
    seal, seal_status = read_immutable_sqlite_cache_seal(
        resolved,
        kind="router_sidecar",
    )
    if deep and seal_status == "valid":
        raw_metadata = seal.get("metadata")
        raw_counts = seal.get("counts")
        metadata = dict(raw_metadata) if isinstance(raw_metadata, Mapping) else {}
        counts = {
            str(key): int(value)
            for key, value in dict(raw_counts or {}).items()
            if isinstance(value, int) and not isinstance(value, bool)
        }
        tables = list(ROUTER_SIDECAR_TABLES)
        if metadata.get("schema_version") != ROUTER_SIDECAR_SCHEMA_VERSION:
            errors.append("router_sidecar_schema_version_mismatch")
        if metadata.get("builder_version") != ROUTER_SIDECAR_BUILDER_VERSION:
            errors.append("router_sidecar_builder_version_mismatch")
        if counts.get("routing_unit", 0) != counts.get("routing_fts", 0):
            errors.append("router_sidecar_fts_unit_count_mismatch")
        if counts.get("micro_routing_unit", 0) != counts.get("micro_routing_fts", 0):
            errors.append("router_sidecar_micro_fts_unit_count_mismatch")
        expected_fingerprint = _json_sha256(
            {
                "builder_version": metadata.get("builder_version"),
                "content_sha256": metadata.get("content_sha256"),
                "ranking_profile_sha256": metadata.get("ranking_profile_sha256"),
                "release_id": metadata.get("release_id"),
                "schema_sql_sha256": metadata.get("schema_sql_sha256"),
                "schema_version": metadata.get("schema_version"),
                "source_global_spine_sha256": metadata.get("source_global_spine_sha256"),
            }
        )
        if metadata.get("build_fingerprint_sha256") != expected_fingerprint:
            errors.append("router_sidecar_build_fingerprint_mismatch")
        if (
            expected_global_spine_sha256 is not None
            and metadata.get("source_global_spine_sha256") != expected_global_spine_sha256
        ):
            errors.append("router_sidecar_source_global_spine_hash_mismatch")
        elif global_spine_path is not None:
            source = Path(global_spine_path).expanduser().resolve()
            if not source.is_file():
                errors.append("router_sidecar_source_global_spine_missing")
            elif metadata.get("source_global_spine_sha256") != immutable_file_sha256(source):
                errors.append("router_sidecar_source_global_spine_hash_mismatch")
        if expected_release_id is not None and metadata.get("release_id") != expected_release_id:
            errors.append("router_sidecar_release_id_mismatch")
        if (
            expected_ranking_profile_sha256 is not None
            and metadata.get("ranking_profile_sha256") != expected_ranking_profile_sha256
        ):
            errors.append("router_sidecar_manifest_ranking_profile_hash_mismatch")
        if (
            expected_build_fingerprint_sha256 is not None
            and metadata.get("build_fingerprint_sha256") != expected_build_fingerprint_sha256
        ):
            errors.append("router_sidecar_manifest_build_fingerprint_mismatch")
        return {
            "ok": not errors,
            "errors": errors,
            "warnings": warnings,
            "path": str(resolved),
            "metadata": metadata,
            "counts": counts,
            "tables": tables,
            "verification_mode": "router-sidecar-deep-sealed",
            "deep": True,
            "integrity_check": "ok",
            "integrity_source": "immutable_cache_seal",
            "seal_status": seal_status,
            "seal_trusted": True,
        }
    integrity_check: str | None = None
    try:
        with _connect_immutable_readonly(resolved) as conn:
            conn.row_factory = sqlite3.Row
            if deep:
                integrity = conn.execute("PRAGMA integrity_check").fetchall()
                integrity_check = (
                    "ok" if [tuple(row) for row in integrity] == [("ok",)] else repr(integrity)
                )
                if [tuple(row) for row in integrity] != [("ok",)]:
                    errors.append(f"sqlite_integrity_check_failed:{integrity!r}")
            tables = sorted(
                str(row[0])
                for row in conn.execute(
                    "SELECT name FROM sqlite_master WHERE type IN ('table', 'view')"
                ).fetchall()
            )
            metadata = _read_metadata(conn)
            metadata_counts = metadata.get("counts")
            counts = (
                _sidecar_counts(conn)
                if deep
                else dict(metadata_counts)
                if isinstance(metadata_counts, Mapping)
                else {}
            )
            table_set = set(tables)
            for table in ROUTER_SIDECAR_TABLES:
                if table not in table_set:
                    errors.append(f"router_sidecar_table_missing:{table}")
            for key in ROUTER_SIDECAR_REQUIRED_METADATA_KEYS:
                if key not in metadata:
                    errors.append(f"router_sidecar_metadata_missing:{key}")
            if metadata.get("schema_version") != ROUTER_SIDECAR_SCHEMA_VERSION:
                errors.append("router_sidecar_schema_version_mismatch")
            if metadata.get("builder_version") != ROUTER_SIDECAR_BUILDER_VERSION:
                errors.append("router_sidecar_builder_version_mismatch")
            expected_schema_hash = hashlib.sha256(_SCHEMA_SQL.encode("utf-8")).hexdigest()
            if metadata.get("schema_sql_sha256") != expected_schema_hash:
                errors.append("router_sidecar_schema_hash_mismatch")
            errors.extend(_profile_verification_errors(conn, metadata))
            expected_counts = metadata_counts
            if not isinstance(expected_counts, Mapping):
                errors.append("router_sidecar_counts_missing")
            elif deep and dict(expected_counts) != counts:
                errors.append("router_sidecar_counts_mismatch")
            if deep and counts.get("routing_unit", 0) != counts.get("routing_fts", 0):
                errors.append("router_sidecar_fts_unit_count_mismatch")
            if deep and counts.get("micro_routing_unit", 0) != counts.get("micro_routing_fts", 0):
                errors.append("router_sidecar_micro_fts_unit_count_mismatch")
            if deep and not any("table_missing" in error for error in errors):
                invalid_term_stats = int(
                    conn.execute(
                        """
                        SELECT COUNT(*)
                        FROM term_stats
                        WHERE fts_unit_df < 0
                           OR fts_ticker_df < 0
                           OR fts_ticker_df > fts_unit_df
                           OR alias_unit_df < 0
                           OR alias_ticker_df < 0
                        """
                    ).fetchone()[0]
                )
                if invalid_term_stats:
                    errors.append(f"router_sidecar_term_stats_invalid:{invalid_term_stats}")
                conn.execute("DROP TABLE IF EXISTS temp.routing_fts_vocab_verify")
                conn.execute(
                    "CREATE VIRTUAL TABLE temp.routing_fts_vocab_verify "
                    "USING fts5vocab(main, routing_fts, 'row')"
                )
                fts_df_mismatches = int(
                    conn.execute(
                        """
                        SELECT COUNT(*)
                        FROM temp.routing_fts_vocab_verify AS vocab
                        LEFT JOIN term_stats AS stats
                          ON stats.term_norm = vocab.term
                        WHERE stats.term_norm IS NULL
                           OR stats.fts_unit_df != vocab.doc
                        """
                    ).fetchone()[0]
                )
                conn.execute("DROP TABLE temp.routing_fts_vocab_verify")
                if fts_df_mismatches:
                    errors.append(f"router_sidecar_term_stats_fts_df_mismatch:{fts_df_mismatches}")
                posting_df_mismatches = int(
                    conn.execute(
                        """
                        WITH posting_df AS (
                            SELECT term_norm, COUNT(*) AS unit_count
                            FROM routing_term_posting
                            GROUP BY term_norm
                        )
                        SELECT COUNT(*)
                        FROM term_stats AS stats
                        LEFT JOIN posting_df USING(term_norm)
                        WHERE stats.fts_unit_df > 0
                          AND COALESCE(posting_df.unit_count, 0) != stats.fts_unit_df
                        """
                    ).fetchone()[0]
                )
                if posting_df_mismatches:
                    errors.append(
                        f"router_sidecar_term_posting_df_mismatch:{posting_df_mismatches}"
                    )
                orphan_postings = int(
                    conn.execute(
                        """
                        SELECT COUNT(*)
                        FROM routing_term_posting AS posting
                        LEFT JOIN routing_fts AS fts
                          ON fts.rowid = posting.fts_rowid
                        WHERE fts.rowid IS NULL
                        """
                    ).fetchone()[0]
                )
                if orphan_postings:
                    errors.append(f"router_sidecar_term_posting_orphans:{orphan_postings}")
                alias_df_mismatches = int(
                    conn.execute(
                        """
                        WITH actual AS (
                            SELECT alias_norm AS term_norm,
                                   COUNT(DISTINCT CASE
                                       WHEN routing_unit_id != '' THEN routing_unit_id
                                       ELSE NULL
                                   END) AS unit_count,
                                   COUNT(DISTINCT ticker) AS ticker_count
                            FROM alias_lookup
                            GROUP BY alias_norm
                        )
                        SELECT COUNT(*)
                        FROM actual
                        LEFT JOIN term_stats AS stats USING(term_norm)
                        WHERE stats.term_norm IS NULL
                           OR stats.alias_unit_df != actual.unit_count
                           OR stats.alias_ticker_df != actual.ticker_count
                        """
                    ).fetchone()[0]
                )
                if alias_df_mismatches:
                    errors.append(
                        f"router_sidecar_term_stats_alias_df_mismatch:{alias_df_mismatches}"
                    )
                if metadata.get("content_sha256") != _content_sha256(conn):
                    errors.append("router_sidecar_content_hash_mismatch")
                fk_errors = conn.execute("PRAGMA foreign_key_check").fetchall()
                if fk_errors:
                    errors.append(f"router_sidecar_foreign_key_errors:{len(fk_errors)}")
                micro_projection = conn.execute(
                    """
                    SELECT
                        COUNT(DISTINCT f.micro_unit_id) AS distinct_ids,
                        SUM(
                            CASE
                                WHEN m.micro_unit_id IS NULL
                                  OR f.routing_unit_id != m.routing_unit_id
                                  OR f.ticker != m.ticker
                                  OR f.topic_text != m.topic_text
                                  OR f.factor_text != m.factor_text
                                  OR f.metric_text != m.metric_text
                                  OR f.entity_text != m.entity_text
                                  OR f.mechanism_text != m.mechanism_text
                                THEN 1 ELSE 0
                            END
                        ) AS mismatch_count
                    FROM micro_routing_fts AS f
                    LEFT JOIN micro_routing_unit AS m
                      ON m.micro_unit_id = f.micro_unit_id
                    """
                ).fetchone()
                distinct_micro_ids = int(micro_projection["distinct_ids"] or 0)
                if distinct_micro_ids != counts.get("micro_routing_unit", 0):
                    errors.append("router_sidecar_micro_fts_identity_mismatch")
                micro_projection_mismatches = int(micro_projection["mismatch_count"] or 0)
                if micro_projection_mismatches:
                    errors.append(
                        "router_sidecar_micro_fts_projection_mismatch:"
                        f"{micro_projection_mismatches}"
                    )
            expected_fingerprint = _json_sha256(
                {
                    "builder_version": metadata.get("builder_version"),
                    "content_sha256": metadata.get("content_sha256"),
                    "ranking_profile_sha256": metadata.get("ranking_profile_sha256"),
                    "release_id": metadata.get("release_id"),
                    "schema_sql_sha256": metadata.get("schema_sql_sha256"),
                    "schema_version": metadata.get("schema_version"),
                    "source_global_spine_sha256": metadata.get("source_global_spine_sha256"),
                }
            )
            if metadata.get("build_fingerprint_sha256") != expected_fingerprint:
                errors.append("router_sidecar_build_fingerprint_mismatch")
    except sqlite3.Error as exc:
        errors.append(f"router_sidecar_sqlite_error:{exc}")

    if expected_global_spine_sha256 is not None:
        if metadata.get("source_global_spine_sha256") != expected_global_spine_sha256:
            errors.append("router_sidecar_source_global_spine_hash_mismatch")
    elif global_spine_path is not None:
        source = Path(global_spine_path).expanduser().resolve()
        if not source.is_file():
            errors.append("router_sidecar_source_global_spine_missing")
        elif metadata.get("source_global_spine_sha256") != immutable_file_sha256(source):
            errors.append("router_sidecar_source_global_spine_hash_mismatch")
    if expected_release_id is not None and metadata.get("release_id") != expected_release_id:
        errors.append("router_sidecar_release_id_mismatch")
    if (
        expected_ranking_profile_sha256 is not None
        and metadata.get("ranking_profile_sha256") != expected_ranking_profile_sha256
    ):
        errors.append("router_sidecar_manifest_ranking_profile_hash_mismatch")
    if (
        expected_build_fingerprint_sha256 is not None
        and metadata.get("build_fingerprint_sha256") != expected_build_fingerprint_sha256
    ):
        errors.append("router_sidecar_manifest_build_fingerprint_mismatch")
    return {
        "ok": not errors,
        "errors": errors,
        "warnings": warnings,
        "path": str(resolved),
        "metadata": metadata,
        "counts": counts,
        "tables": tables,
        "verification_mode": "router-sidecar-deep" if deep else "router-sidecar-light",
        "deep": deep,
        "integrity_check": integrity_check,
        "integrity_source": "sqlite_integrity_check" if deep else "none",
        "seal_status": seal_status,
        "seal_trusted": False,
    }


def rebind_router_sidecar_release(
    path: Path | str,
    *,
    release_id: str,
    expected_global_spine_sha256: str | None = None,
    expected_previous_release_id: str | None = None,
    trusted_source_path: Path | str | None = None,
) -> dict[str, Any]:
    """Rebind a copied candidate sidecar to a new immutable release ID.

    This updates metadata and its build fingerprint only.  The caller must use
    it on a candidate copy, never an active release artifact.
    """
    resolved = Path(path).expanduser().resolve()
    trusted_source = (
        Path(trusted_source_path).expanduser().resolve()
        if trusted_source_path is not None
        else None
    )
    normalized_release_id = str(release_id).strip()
    if not normalized_release_id:
        raise ValueError("router sidecar release_id is required")
    inherited_source_verification: dict[str, Any] | None = None
    if trusted_source is not None:
        source_seal, source_seal_status = read_immutable_sqlite_cache_seal(
            trusted_source,
            kind="router_sidecar",
        )
        if source_seal_status != "valid":
            raise ValueError(
                "cannot inherit router sidecar verification from an untrusted source: "
                f"{source_seal_status}"
            )
        inherited_source_verification = verify_router_sidecar(
            trusted_source,
            expected_global_spine_sha256=expected_global_spine_sha256,
            expected_release_id=expected_previous_release_id,
            deep=True,
        )
        if not inherited_source_verification.get("ok"):
            raise ValueError(
                "cannot inherit invalid router sidecar verification: "
                + ", ".join(inherited_source_verification.get("errors") or [])
            )
        assert_trusted_immutable_copy(
            trusted_source,
            resolved,
            cached_source_sha256=read_immutable_sqlite_cache_sha256(trusted_source),
            role="router sidecar",
        )

    verification = verify_router_sidecar(
        resolved,
        expected_global_spine_sha256=expected_global_spine_sha256,
        deep=False,
    )
    if not verification.get("ok"):
        raise ValueError(
            "cannot rebind invalid router sidecar: " + ", ".join(verification.get("errors") or [])
        )
    metadata = dict(verification.get("metadata") or {})
    if (
        expected_previous_release_id is not None
        and metadata.get("release_id") != expected_previous_release_id
    ):
        raise ValueError("router sidecar previous release_id mismatch")
    metadata["release_id"] = normalized_release_id
    metadata["build_fingerprint_sha256"] = _json_sha256(
        {
            "builder_version": metadata.get("builder_version"),
            "content_sha256": metadata.get("content_sha256"),
            "ranking_profile_sha256": metadata.get("ranking_profile_sha256"),
            "release_id": normalized_release_id,
            "schema_sql_sha256": metadata.get("schema_sql_sha256"),
            "schema_version": metadata.get("schema_version"),
            "source_global_spine_sha256": metadata.get("source_global_spine_sha256"),
        }
    )
    with sqlite3.connect(resolved) as conn:
        _write_metadata(
            conn,
            {
                "release_id": metadata["release_id"],
                "build_fingerprint_sha256": metadata["build_fingerprint_sha256"],
            },
        )
        conn.commit()
    remove_immutable_sqlite_cache_seal(resolved)
    rebound = verify_router_sidecar(
        resolved,
        expected_global_spine_sha256=expected_global_spine_sha256,
        expected_release_id=normalized_release_id,
        deep=False,
    )
    if inherited_source_verification is not None:
        inherited = {
            **rebound,
            "integrity_check": "ok",
            "integrity_source": "inherited_immutable_cache_seal",
            "verification_mode": "router-sidecar-deep-sealed-inherited-rebind",
        }
        write_immutable_sqlite_cache_seal(
            resolved,
            kind="router_sidecar",
            cache_key=str(metadata["build_fingerprint_sha256"]),
            verification=inherited,
            metadata=rebound.get("metadata") or {},
            counts=rebound.get("counts") or {},
            source_path=trusted_source,
            details={"inheritance": "controlled-release-id-rebind"},
        )
        rebound = verify_router_sidecar(
            resolved,
            expected_global_spine_sha256=expected_global_spine_sha256,
            expected_release_id=normalized_release_id,
            deep=True,
        )
    return rebound


def rebind_router_sidecar_source(
    path: Path | str,
    *,
    global_spine_path: Path | str,
    release_id: str,
    expected_previous_global_spine_sha256: str | None = None,
    expected_previous_release_id: str | None = None,
) -> dict[str, Any]:
    """Rebind a verified semantic-cache copy to a byte-distinct global spine.

    The caller must independently prove semantic equivalence, normally with a
    content-addressed cache key over all ordered spine fragments and builder
    versions. This function only updates immutable source/release metadata on
    the copied candidate and then verifies the new binding.
    """
    resolved = Path(path).expanduser().resolve()
    source_path = Path(global_spine_path).expanduser().resolve()
    normalized_release_id = str(release_id).strip()
    if not normalized_release_id:
        raise ValueError("router sidecar release_id is required")
    if not source_path.is_file():
        raise FileNotFoundError(f"Global spine not found: {source_path}")
    verification = verify_router_sidecar(resolved, deep=False)
    if not verification.get("ok"):
        raise ValueError(
            "cannot rebind invalid router sidecar: " + ", ".join(verification.get("errors") or [])
        )
    metadata = dict(verification.get("metadata") or {})
    if (
        expected_previous_global_spine_sha256 is not None
        and metadata.get("source_global_spine_sha256") != expected_previous_global_spine_sha256
    ):
        raise ValueError("router sidecar previous global spine SHA-256 mismatch")
    if (
        expected_previous_release_id is not None
        and metadata.get("release_id") != expected_previous_release_id
    ):
        raise ValueError("router sidecar previous release_id mismatch")

    with _connect_immutable_readonly(source_path) as source:
        source_metadata = _read_metadata(source)
    source_sha256 = immutable_file_sha256(source_path)
    rebound_metadata = {
        "release_id": normalized_release_id,
        "source_global_spine_sha256": source_sha256,
        "source_global_spine_schema_version": source_metadata.get("schema_version"),
        "source_manifest_hash": source_metadata.get("source_manifest_hash"),
        "source_created_at": source_metadata.get("created_at"),
    }
    rebound_metadata["build_fingerprint_sha256"] = _json_sha256(
        {
            "builder_version": metadata.get("builder_version"),
            "content_sha256": metadata.get("content_sha256"),
            "ranking_profile_sha256": metadata.get("ranking_profile_sha256"),
            "release_id": normalized_release_id,
            "schema_sql_sha256": metadata.get("schema_sql_sha256"),
            "schema_version": metadata.get("schema_version"),
            "source_global_spine_sha256": source_sha256,
        }
    )
    with sqlite3.connect(resolved) as conn:
        _write_metadata(conn, rebound_metadata)
        conn.commit()
    remove_immutable_sqlite_cache_seal(resolved)
    rebound = verify_router_sidecar(
        resolved,
        expected_global_spine_sha256=source_sha256,
        expected_release_id=normalized_release_id,
        deep=False,
    )
    if not rebound.get("ok"):
        raise ValueError(
            "rebound router sidecar is invalid: "
            + ", ".join(rebound.get("errors") or [])
        )
    inherited = {
        **rebound,
        "integrity_check": "ok",
        "integrity_source": "inherited_immutable_cache_seal",
        "verification_mode": "router-sidecar-deep-sealed-source-rebind",
    }
    write_immutable_sqlite_cache_seal(
        resolved,
        kind="router_sidecar",
        cache_key=str(rebound_metadata.get("build_fingerprint_sha256") or ""),
        verification=inherited,
        metadata=rebound.get("metadata") or {},
        counts=rebound.get("counts") or {},
        source_path=resolved,
        details={"inheritance": "controlled-global-spine-source-rebind"},
    )
    return verify_router_sidecar(
        resolved,
        expected_global_spine_sha256=source_sha256,
        expected_release_id=normalized_release_id,
        deep=True,
    )


class RouterSidecar:
    """One read-only connection to an immutable Serving Index V5 artifact.

    Instances are intentionally not shared across threads.  A caller can keep
    one instance per store lease, matching the immutable release lifecycle.
    """

    def __init__(
        self,
        path: Path | str,
        *,
        coherence_path: Path | str | None = None,
        require_coherence: bool = False,
    ):
        self.path = Path(path).expanduser().resolve()
        if not self.path.is_file():
            raise FileNotFoundError(f"Router sidecar not found: {self.path}")
        uri = f"file:{quote(self.path.as_posix(), safe='/')}?mode=ro&immutable=1"
        # Store leases are exclusive but can move between worker threads after
        # returning to the pool, so thread affinity must not be pinned here.
        self._conn = sqlite3.connect(uri, uri=True, check_same_thread=False)
        self._conn.row_factory = sqlite3.Row
        self._conn.execute("PRAGMA query_only = ON")
        self._metadata = _read_metadata(self._conn)
        self._profile = self._load_active_profile()
        resolved_coherence_path = (
            Path(coherence_path).expanduser().resolve()
            if coherence_path is not None
            else self.path.parent / ROUTER_COHERENCE_RELATIVE_PATH.name
        )
        self._coherence: RouterCoherence | None = None
        if resolved_coherence_path.is_file():
            coherence = RouterCoherence(resolved_coherence_path)
            coherence_metadata = coherence.metadata
            if coherence_metadata.get("source_global_spine_sha256") != self._metadata.get(
                "source_global_spine_sha256"
            ):
                coherence.close()
                self._conn.close()
                raise ValueError("Router coherence source-spine binding mismatch")
            if coherence_metadata.get("release_id") != self._metadata.get("release_id"):
                coherence.close()
                self._conn.close()
                raise ValueError("Router coherence release binding mismatch")
            self._coherence = coherence
        elif require_coherence:
            self._conn.close()
            raise FileNotFoundError(f"Router coherence index not found: {resolved_coherence_path}")
        self._term_stat_cache: dict[str, _QueryTermStat] = {}
        self._alias_rows_cache: OrderedDict[
            tuple[str, tuple[str, ...], int], tuple[dict[str, Any], ...]
        ] = OrderedDict()

    def __enter__(self) -> RouterSidecar:
        return self

    def __exit__(self, exc_type: Any, exc: Any, traceback: Any) -> None:
        self.close()

    @property
    def metadata(self) -> Mapping[str, Any]:
        return dict(self._metadata)

    @property
    def ranking_profile(self) -> Mapping[str, Any]:
        return dict(self._profile)

    @property
    def coherence_available(self) -> bool:
        return self._coherence is not None

    def close(self) -> None:
        if self._coherence is not None:
            self._coherence.close()
            self._coherence = None
        self._conn.close()

    def lookup_alias(
        self,
        text: str,
        *,
        kinds: Sequence[str] | None = None,
        limit: int = 20,
    ) -> list[dict[str, Any]]:
        """Return exact normalized alias matches; this never performs fuzzy lookup."""
        alias_norm = normalize_router_text(text)
        if not alias_norm or limit <= 0:
            return []
        params: list[Any] = [alias_norm]
        sql = "SELECT * FROM alias_lookup WHERE alias_norm = ?"
        normalized_kinds = tuple(sorted({str(kind) for kind in kinds or () if str(kind)}))
        if normalized_kinds:
            placeholders = ", ".join("?" for _ in normalized_kinds)
            sql += f" AND alias_kind IN ({placeholders})"
            params.extend(normalized_kinds)
        sql += " ORDER BY priority DESC, ticker, routing_unit_id, canonical_key LIMIT ?"
        params.append(int(limit))
        return [dict(row) for row in self._conn.execute(sql, params).fetchall()]

    def search(
        self,
        query: str,
        *,
        ticker: str | Sequence[str] | None = None,
        facet_filters: Mapping[str, Sequence[str]] | None = None,
        limit: int | None = None,
        restrict_to_resolved: bool = False,
        explicit_entity_scope: bool = False,
    ) -> dict[str, Any]:
        """Route with coverage-first FTS and gated auxiliary retrieval channels."""
        terms = _lexical_terms(query, limit=int(self._profile["max_query_terms"]))
        resolved_limit = int(limit or self._profile["default_limit"])
        if not terms or resolved_limit <= 0:
            return self._empty_search_result(query)
        candidate_limit = int(self._profile["candidate_limit"])
        requested_tickers = _normalize_tickers(ticker)
        alias_terms = _query_alias_candidates(
            query,
            terms=terms,
            max_ngram_tokens=int(self._profile["alias"]["max_ngram_tokens"]),
        )
        term_stats = self._query_term_stats(alias_terms, lexical_terms=terms)
        company_alias_enabled = bool(
            explicit_entity_scope
            or restrict_to_resolved
            or self._profile["alias"]["allow_company_alias_without_explicit_scope"]
        )
        alias_rows = self._query_alias_rows(
            query,
            terms=terms,
            term_stats=term_stats,
            explicit_entity_scope=company_alias_enabled,
        )
        company_alias_kinds = {"ticker", "company", "company_token"}
        resolved_tickers = sorted(
            {
                str(row["ticker"])
                for row in alias_rows
                if company_alias_enabled and str(row["alias_kind"]) in company_alias_kinds
            }
        )
        scoped_tickers = requested_tickers
        if not scoped_tickers and restrict_to_resolved and resolved_tickers:
            scoped_tickers = tuple(resolved_tickers)

        channels: dict[str, list[str]] = {}
        channels["exact_alias"] = self._alias_candidates(
            alias_rows,
            tickers=scoped_tickers,
            limit=candidate_limit,
            explicit_entity_scope=company_alias_enabled,
        )
        lexical_term_stats = {term: term_stats[term] for term in terms if term in term_stats}
        fts_rows = self._fts_candidates(
            terms,
            term_stats=lexical_term_stats,
            tickers=scoped_tickers,
            limit=candidate_limit,
        )
        channels["fielded_fts"] = [str(row["routing_unit_id"]) for row in fts_rows]
        facet_units = self._facet_candidates(
            alias_rows,
            facet_filters=facet_filters,
            tickers=scoped_tickers,
            limit=candidate_limit,
        )
        channels["facet"] = facet_units
        minimum_primary = int(self._profile["bigram"]["minimum_primary_results"])
        if len(fts_rows) < minimum_primary or any(_HANGUL_RE.fullmatch(term) for term in terms):
            channels["short_token"] = self._short_token_candidates(
                terms,
                tickers=scoped_tickers,
                limit=candidate_limit,
            )
        else:
            channels["short_token"] = []

        hard_facet_units = self._hard_facet_units(
            facet_filters,
            tickers=scoped_tickers,
        )
        ranked_units = self._fuse_channels(
            channels,
            allowed_units=hard_facet_units,
            limit=candidate_limit,
            primary_scores={
                str(row["routing_unit_id"]): float(row["primary_score"]) for row in fts_rows
            },
        )
        if not ranked_units and scoped_tickers:
            ranked_units = [
                (str(row["routing_unit_id"]), 0.0, {})
                for row in self._units_for_tickers(scoped_tickers, limit=candidate_limit)
            ]
        unit_headers = self._hydrate_unit_headers(ranked_units[:candidate_limit])
        micro_config = self._profile["micro_rerank"]
        coherence_config = (
            self._coherence.profile.get("query") if self._coherence is not None else {}
        )
        coarse_pool_limit = min(
            candidate_limit,
            max(
                resolved_limit,
                resolved_limit * int(micro_config["candidate_ticker_multiplier"]),
                resolved_limit * int(coherence_config.get("candidate_ticker_multiplier") or 1),
            ),
        )
        coarse_ticker_candidates = self._aggregate_tickers(
            unit_headers,
            limit=coarse_pool_limit,
        )
        micro_candidates, micro_diagnostics, micro_rows = self._micro_rerank_tickers(
            terms=terms,
            term_stats=lexical_term_stats,
            coarse_candidates=coarse_ticker_candidates,
            protected_tickers=resolved_tickers,
            limit=(coarse_pool_limit if self._coherence is not None else resolved_limit),
        )
        ticker_candidates, coherence_diagnostics, coherence_rows = self._coherence_rerank_tickers(
            terms=terms,
            coarse_candidates=micro_candidates,
            protected_tickers=resolved_tickers,
            scoped_tickers=scoped_tickers,
            candidate_generation_allowed=hard_facet_units is None,
            limit=resolved_limit,
        )
        unit_rows = self._hydrate_units(
            ranked_units[:resolved_limit],
            primary_diagnostics={str(row["routing_unit_id"]): row for row in fts_rows},
        )
        minimum_should_match = int(fts_rows[0]["minimum_should_match"]) if fts_rows else 0
        coverage_candidate_count = int(fts_rows[0]["coverage_candidate_count"]) if fts_rows else 0
        coverage_query_count = int(fts_rows[0]["coverage_query_count"]) if fts_rows else 0
        rrf_k = float(self._profile["rrf_k"])
        configured_primary_scale = float(self._profile["fusion"]["primary_rrf_scale"])
        return {
            "contract_version": "router-search/v3",
            "query": query,
            "query_terms": terms,
            "requested_tickers": list(requested_tickers),
            "resolved_tickers": resolved_tickers,
            "scope_tickers": list(scoped_tickers),
            "exact_aliases": alias_rows,
            "ticker_candidates": ticker_candidates,
            "routing_units": unit_rows,
            "micro_routing_units": micro_rows,
            "micro_rerank": micro_diagnostics,
            "coherence_units": coherence_rows,
            "coherence_rerank": coherence_diagnostics,
            "query_term_stats": [
                term_stats[term].as_dict(
                    facet_eligible=self._facet_term_is_eligible(term_stats[term])
                )
                for term in alias_terms
                if term in term_stats
            ],
            "lexical_diagnostics": {
                "term_stats_source": "build_time_term_stats",
                "coverage_mode": "materialized_term_posting",
                "coverage_candidate_count": coverage_candidate_count,
                "coverage_query_count": coverage_query_count,
                "full_term_posting_materialized": True,
            },
            "fusion_diagnostics": {
                "mode": "rrf_scale_calibrated_v1",
                "rrf_k": rrf_k,
                "configured_primary_rrf_scale": configured_primary_scale,
                "primary_contribution_scale": configured_primary_scale / (rrf_k + 1.0),
                "legacy_primary_contribution_scale": 1.0,
                "raw_primary_score_retained": True,
            },
            "minimum_should_match": minimum_should_match,
            "graph_expansion_used": False,
            "graph_expansion_reason": "no_high_confidence_seed_expansion",
            "ranking_profile_id": self._profile["profile_id"],
            "ranking_profile_sha256": self._metadata.get("ranking_profile_sha256"),
            "release_id": self._metadata.get("release_id"),
        }

    def graph_neighbors(
        self,
        ticker: str,
        *,
        terms: Sequence[str] | None = None,
        limit: int = 20,
    ) -> list[dict[str, Any]]:
        """Return deterministic conditional graph priors for one ticker."""
        normalized_ticker = str(ticker).strip().upper()
        if not normalized_ticker or limit <= 0:
            return []
        params: list[Any] = [normalized_ticker, normalized_ticker]
        sql = """
            SELECT * FROM graph_prior
            WHERE (from_ticker = ? OR to_ticker = ?)
        """
        normalized_terms = sorted({normalize_router_text(term) for term in terms or () if term})
        if normalized_terms:
            placeholders = ", ".join("?" for _ in normalized_terms)
            sql += f" AND shared_key IN ({placeholders})"
            params.extend(normalized_terms)
        sql += " ORDER BY weight DESC, from_ticker, to_ticker, shared_key LIMIT ?"
        params.append(int(limit))
        return [dict(row) for row in self._conn.execute(sql, params).fetchall()]

    def _load_active_profile(self) -> dict[str, Any]:
        row = self._conn.execute(
            """
            SELECT config_json, config_sha256
            FROM ranking_profile
            WHERE active = 1
            ORDER BY profile_id
            LIMIT 1
            """
        ).fetchone()
        if row is None:
            raise RuntimeError("router sidecar has no active ranking profile")
        profile = json.loads(str(row["config_json"]))
        actual_hash = _json_sha256(profile)
        if actual_hash != str(row["config_sha256"]):
            raise RuntimeError("router sidecar ranking profile hash mismatch")
        if actual_hash != self._metadata.get("ranking_profile_sha256"):
            raise RuntimeError("router sidecar metadata ranking profile hash mismatch")
        return profile

    def _empty_search_result(self, query: str) -> dict[str, Any]:
        return {
            "contract_version": "router-search/v3",
            "query": query,
            "query_terms": [],
            "requested_tickers": [],
            "resolved_tickers": [],
            "scope_tickers": [],
            "exact_aliases": [],
            "ticker_candidates": [],
            "routing_units": [],
            "micro_routing_units": [],
            "micro_rerank": {
                "enabled": bool(self._profile["micro_rerank"]["enabled"]),
                "applied": False,
                "reason": "empty_query",
                "fallback": "preserve_coarse_order",
            },
            "query_term_stats": [],
            "lexical_diagnostics": {
                "term_stats_source": "build_time_term_stats",
                "coverage_mode": "materialized_term_posting",
                "coverage_candidate_count": 0,
                "coverage_query_count": 0,
                "full_term_posting_materialized": True,
            },
            "fusion_diagnostics": {
                "mode": "rrf_scale_calibrated_v1",
                "rrf_k": float(self._profile["rrf_k"]),
                "configured_primary_rrf_scale": float(self._profile["fusion"]["primary_rrf_scale"]),
                "primary_contribution_scale": float(self._profile["fusion"]["primary_rrf_scale"])
                / (float(self._profile["rrf_k"]) + 1.0),
                "legacy_primary_contribution_scale": 1.0,
                "raw_primary_score_retained": True,
            },
            "minimum_should_match": 0,
            "graph_expansion_used": False,
            "graph_expansion_reason": "no_high_confidence_seed_expansion",
            "ranking_profile_id": self._profile["profile_id"],
            "ranking_profile_sha256": self._metadata.get("ranking_profile_sha256"),
            "release_id": self._metadata.get("release_id"),
        }

    def _query_term_stats(
        self,
        terms: Sequence[str],
        *,
        lexical_terms: Sequence[str],
    ) -> dict[str, _QueryTermStat]:
        counts = self._metadata.get("counts")
        total_units = int(counts.get("routing_unit") or 0) if isinstance(counts, Mapping) else 0
        total_tickers = int(counts.get("ticker_profile") or 0) if isinstance(counts, Mapping) else 0
        if total_units <= 0:
            total_units = int(self._conn.execute("SELECT COUNT(*) FROM routing_unit").fetchone()[0])
        if total_tickers <= 0:
            total_tickers = int(
                self._conn.execute("SELECT COUNT(*) FROM ticker_profile").fetchone()[0]
            )

        lexical_term_set = {
            normalize_router_text(term) for term in lexical_terms if normalize_router_text(term)
        }
        normalized_terms = [
            term for term in dict.fromkeys(normalize_router_text(value) for value in terms) if term
        ]
        missing_terms = [term for term in normalized_terms if term not in self._term_stat_cache]
        rows_by_term: dict[str, sqlite3.Row] = {}
        if missing_terms:
            placeholders = ", ".join("?" for _ in missing_terms)
            rows_by_term = {
                str(row["term_norm"]): row
                for row in self._conn.execute(
                    f"""
                    SELECT term_norm, fts_unit_df, fts_ticker_df,
                           alias_unit_df, alias_ticker_df
                    FROM term_stats
                    WHERE term_norm IN ({placeholders})
                    """,
                    missing_terms,
                ).fetchall()
            }
        for term in missing_terms:
            row = rows_by_term.get(term)
            fts_unit_df = int(row["fts_unit_df"] or 0) if row is not None else 0
            fts_ticker_df = int(row["fts_ticker_df"] or 0) if row is not None else 0
            alias_unit_df = int(row["alias_unit_df"] or 0) if row is not None else 0
            alias_ticker_df = int(row["alias_ticker_df"] or 0) if row is not None else 0
            if term not in lexical_term_set and alias_unit_df <= 0 and alias_ticker_df <= 0:
                continue
            effective_unit_df = max(fts_unit_df, alias_unit_df)
            effective_ticker_df = max(fts_ticker_df, alias_ticker_df)
            self._term_stat_cache[term] = _QueryTermStat(
                term=term,
                fts_unit_df=fts_unit_df,
                fts_ticker_df=fts_ticker_df,
                alias_unit_df=alias_unit_df,
                alias_ticker_df=alias_ticker_df,
                information=max(
                    0.0,
                    math.log((total_units + 1) / (effective_unit_df + 1))
                    + math.log((total_tickers + 1) / (effective_ticker_df + 1)),
                ),
                unit_df_ratio=effective_unit_df / max(1, total_units),
                ticker_df_ratio=effective_ticker_df / max(1, total_tickers),
            )

        result: dict[str, _QueryTermStat] = {}
        for term in normalized_terms:
            cached_stat = self._term_stat_cache.get(term)
            if cached_stat is not None:
                result[term] = cached_stat
        return result

    def _facet_term_is_eligible(self, stat: _QueryTermStat) -> bool:
        config = self._profile["alias"]
        return bool(
            (stat.fts_unit_df > 0 or stat.alias_unit_df > 0)
            and stat.information >= float(config["facet_min_information"])
            and stat.unit_df_ratio <= float(config["facet_max_unit_df_ratio"])
            and stat.ticker_df_ratio <= float(config["facet_max_ticker_df_ratio"])
        )

    def _query_alias_rows(
        self,
        query: str,
        *,
        terms: Sequence[str],
        term_stats: Mapping[str, _QueryTermStat],
        explicit_entity_scope: bool,
    ) -> list[dict[str, Any]]:
        alias_config = self._profile["alias"]
        candidates = _query_alias_candidates(
            query,
            terms=terms,
            max_ngram_tokens=int(alias_config["max_ngram_tokens"]),
        )
        if not candidates:
            return []
        company_alias_kinds = ("ticker", "company", "company_token")
        facet_alias_kinds = (
            "topic",
            "factor",
            "metric",
            "entity",
            "counterparty",
            "mechanism",
        )
        per_term_cap = int(alias_config["per_query_term_cap"])
        selected: dict[tuple[Any, ...], dict[str, Any]] = {}
        for candidate in candidates:
            stat = term_stats.get(candidate)
            include_facets = bool(stat and self._facet_term_is_eligible(stat))
            kinds: tuple[str, ...] = ()
            if explicit_entity_scope and include_facets:
                kinds = (*company_alias_kinds, *facet_alias_kinds)
            elif explicit_entity_scope:
                kinds = company_alias_kinds
            elif include_facets:
                kinds = facet_alias_kinds
            if not kinds:
                continue
            for payload in self._alias_rows_for_term(
                candidate,
                kinds=kinds,
                limit=per_term_cap,
            ):
                key = (
                    payload["alias_norm"],
                    payload["alias_kind"],
                    payload["canonical_key"],
                    payload["ticker"],
                    payload["routing_unit_id"],
                    payload["source_kind"],
                )
                selected[key] = payload
        return list(selected.values())

    def _alias_rows_for_term(
        self,
        alias_norm: str,
        *,
        kinds: tuple[str, ...],
        limit: int,
    ) -> tuple[dict[str, Any], ...]:
        cache_key = (alias_norm, kinds, int(limit))
        cached = self._alias_rows_cache.get(cache_key)
        if cached is not None:
            self._alias_rows_cache.move_to_end(cache_key)
            return cached
        placeholders = ", ".join("?" for _ in kinds)
        rows = tuple(
            dict(row)
            for row in self._conn.execute(
                f"""
                SELECT a.*
                FROM alias_lookup AS a
                LEFT JOIN routing_unit AS u
                  ON u.routing_unit_id = a.routing_unit_id
                WHERE a.alias_norm = ?
                  AND a.alias_kind IN ({placeholders})
                ORDER BY a.priority DESC,
                         COALESCE(u.materiality_max, 0.0) DESC,
                         a.alias_kind,
                         a.ticker,
                         a.routing_unit_id,
                         a.canonical_key
                LIMIT ?
                """,
                (alias_norm, *kinds, int(limit)),
            ).fetchall()
        )
        self._alias_rows_cache[cache_key] = rows
        self._alias_rows_cache.move_to_end(cache_key)
        if len(self._alias_rows_cache) > 512:
            self._alias_rows_cache.popitem(last=False)
        return rows

    def _alias_candidates(
        self,
        alias_rows: Sequence[Mapping[str, Any]],
        *,
        tickers: Sequence[str],
        limit: int,
        explicit_entity_scope: bool,
    ) -> list[str]:
        if not explicit_entity_scope:
            return []
        ticker_filter = set(tickers)
        unit_priority: dict[str, int] = {}
        company_tickers: dict[str, int] = {}
        for row in alias_rows:
            if str(row["alias_kind"]) not in {"ticker", "company", "company_token"}:
                continue
            row_ticker = str(row["ticker"])
            if ticker_filter and row_ticker not in ticker_filter:
                continue
            priority = int(row["priority"])
            unit_id = str(row["routing_unit_id"])
            if unit_id:
                unit_priority[unit_id] = max(unit_priority.get(unit_id, -1), priority)
            else:
                company_tickers[row_ticker] = max(company_tickers.get(row_ticker, -1), priority)
        if company_tickers:
            rows = self._units_for_tickers(tuple(sorted(company_tickers)), limit=limit)
            for row in rows:
                unit_id = str(row["routing_unit_id"])
                priority = company_tickers[str(row["ticker"])]
                unit_priority[unit_id] = max(unit_priority.get(unit_id, -1), priority)
        return [
            unit_id
            for unit_id, _priority in sorted(
                unit_priority.items(),
                key=lambda item: (-item[1], item[0]),
            )[:limit]
        ]

    def _fts_candidates(
        self,
        terms: Sequence[str],
        *,
        term_stats: Mapping[str, _QueryTermStat],
        tickers: Sequence[str],
        limit: int,
    ) -> list[dict[str, Any]]:
        indexed_terms = [
            term
            for term in dict.fromkeys(terms)
            if term in term_stats and term_stats[term].fts_unit_df > 0
        ]
        if not indexed_terms:
            return []
        match_query = " OR ".join(_fts_quote(term) for term in indexed_terms)
        weights = self._profile["fts_field_weights"]
        lexical_config = self._profile["lexical"]
        params: list[Any] = [match_query]
        ticker_sql = ""
        if tickers:
            placeholders = ", ".join("?" for _ in tickers)
            ticker_sql = f" AND ticker IN ({placeholders})"
            params.extend(tickers)
        lexical_limit = max(
            int(limit),
            int(limit) * int(lexical_config["candidate_multiplier"]),
        )
        params.append(lexical_limit)
        rows = self._conn.execute(
            f"""
            SELECT rowid AS fts_rowid, routing_unit_id, ticker,
                   bm25(
                       routing_fts,
                       0.0, 0.0,
                       {float(weights["company_text"])},
                       {float(weights["topic_text"])},
                       {float(weights["factor_text"])},
                       {float(weights["metric_text"])},
                       {float(weights["entity_text"])},
                       {float(weights["mechanism_text"])}
                   ) AS lexical_score
            FROM routing_fts
            WHERE routing_fts MATCH ? {ticker_sql}
            ORDER BY lexical_score ASC, routing_unit_id
            LIMIT ?
            """,
            params,
        ).fetchall()
        if not rows:
            return []

        matched_terms_by_unit: defaultdict[str, set[str]] = defaultdict(set)
        candidate_rowids = [int(row["fts_rowid"]) for row in rows]
        candidate_rowid_set = set(candidate_rowids)
        candidate_units_by_rowid = {
            int(row["fts_rowid"]): str(row["routing_unit_id"]) for row in rows
        }
        term_placeholders = ", ".join("?" for _ in indexed_terms)
        coverage_query_count = 1
        for matched in self._conn.execute(
            f"""
            SELECT term_norm, fts_rowid
            FROM routing_term_posting
            WHERE term_norm IN ({term_placeholders})
            ORDER BY term_norm, fts_rowid
            """,
            indexed_terms,
        ):
            fts_rowid = int(matched["fts_rowid"])
            if fts_rowid not in candidate_rowid_set:
                continue
            unit_id = candidate_units_by_rowid.get(fts_rowid)
            if unit_id is not None:
                matched_terms_by_unit[unit_id].add(str(matched["term_norm"]))

        min_should_match = min(
            len(indexed_terms),
            max(
                1,
                math.ceil(len(indexed_terms) * float(lexical_config["min_should_match_ratio"])),
            ),
        )
        term_information = {
            term: max(
                0.0,
                math.log(
                    (int(self._metadata["counts"]["routing_unit"]) + 1)
                    / (term_stats[term].fts_unit_df + 1)
                ),
            )
            for term in indexed_terms
        }
        total_information = sum(term_information.values()) or float(len(indexed_terms))
        strengths = [max(0.0, -float(row["lexical_score"])) for row in rows]
        min_strength = min(strengths)
        strength_range = max(strengths) - min_strength
        component_weight = sum(
            float(lexical_config[key])
            for key in ("coverage_weight", "information_coverage_weight", "bm25_weight")
        )
        ranked: list[dict[str, Any]] = []
        for row, strength in zip(rows, strengths, strict=True):
            payload = dict(row)
            unit_id = str(payload["routing_unit_id"])
            matched_terms = sorted(matched_terms_by_unit.get(unit_id, set()))
            coverage_count = len(matched_terms)
            coverage_ratio = coverage_count / len(indexed_terms)
            information_coverage = (
                sum(term_information[term] for term in matched_terms) / total_information
            )
            bm25_normalized = (
                (strength - min_strength) / strength_range if strength_range > 0 else 1.0
            )
            primary_score = (
                float(lexical_config["coverage_weight"]) * coverage_ratio
                + float(lexical_config["information_coverage_weight"]) * information_coverage
                + float(lexical_config["bm25_weight"]) * bm25_normalized
            ) / component_weight
            strict_match = coverage_count >= min_should_match
            if not strict_match:
                primary_score *= float(lexical_config["relaxed_penalty"])
            payload.update(
                {
                    "primary_score": primary_score,
                    "matched_terms": matched_terms,
                    "term_coverage_count": coverage_count,
                    "term_coverage_ratio": coverage_ratio,
                    "information_coverage": information_coverage,
                    "bm25_normalized": bm25_normalized,
                    "strict_match": strict_match,
                    "minimum_should_match": min_should_match,
                    "term_stats_source": "build_time_term_stats",
                    "coverage_mode": "materialized_term_posting",
                    "coverage_candidate_count": len(candidate_rowids),
                    "coverage_query_count": coverage_query_count,
                }
            )
            ranked.append(payload)
        return sorted(
            ranked,
            key=lambda row: (
                not bool(row["strict_match"]),
                -float(row["primary_score"]),
                float(row["lexical_score"]),
                str(row["routing_unit_id"]),
            ),
        )[: int(limit)]

    def _facet_candidates(
        self,
        alias_rows: Sequence[Mapping[str, Any]],
        *,
        facet_filters: Mapping[str, Sequence[str]] | None,
        tickers: Sequence[str],
        limit: int,
    ) -> list[str]:
        facet_kinds = {"topic", "factor", "metric", "entity", "counterparty", "mechanism"}
        scores: Counter[str] = Counter()
        ticker_filter = set(tickers)
        for row in alias_rows:
            if str(row["alias_kind"]) not in facet_kinds:
                continue
            if ticker_filter and str(row["ticker"]) not in ticker_filter:
                continue
            unit_id = str(row["routing_unit_id"])
            if unit_id:
                scores[unit_id] += 1
        hard_units = self._hard_facet_units(facet_filters, tickers=tickers)
        if hard_units is not None:
            for unit_id in hard_units:
                scores[unit_id] += len(facet_filters or {}) + 1
        return [
            unit_id
            for unit_id, _score in sorted(scores.items(), key=lambda item: (-item[1], item[0]))[
                :limit
            ]
        ]

    def _hard_facet_units(
        self,
        facet_filters: Mapping[str, Sequence[str]] | None,
        *,
        tickers: Sequence[str],
    ) -> set[str] | None:
        requested: list[tuple[str, str]] = []
        for kind, values in sorted((facet_filters or {}).items()):
            for value in values:
                key = normalize_router_text(value)
                if key:
                    requested.append((str(kind), key))
        if not requested:
            return None
        intersection: set[str] | None = None
        for kind, key in requested:
            canonical_rows = self._conn.execute(
                """
                SELECT DISTINCT canonical_key
                FROM alias_lookup
                WHERE alias_kind = ? AND alias_norm = ?
                ORDER BY canonical_key
                """,
                (kind, key),
            ).fetchall()
            canonical_keys = [str(row[0]) for row in canonical_rows] or [key]
            key_placeholders = ", ".join("?" for _ in canonical_keys)
            params: list[Any] = [kind, *canonical_keys]
            ticker_sql = ""
            if tickers:
                placeholders = ", ".join("?" for _ in tickers)
                ticker_sql = f" AND ticker IN ({placeholders})"
                params.extend(tickers)
            rows = self._conn.execute(
                f"""
                SELECT DISTINCT routing_unit_id
                FROM facet_posting
                WHERE facet_kind = ? AND facet_key IN ({key_placeholders}) {ticker_sql}
                """,
                params,
            ).fetchall()
            current = {str(row[0]) for row in rows}
            intersection = current if intersection is None else intersection & current
            if not intersection:
                return set()
        return intersection or set()

    def _short_token_candidates(
        self,
        terms: Sequence[str],
        *,
        tickers: Sequence[str],
        limit: int,
    ) -> list[str]:
        bigram_config = self._profile["bigram"]
        hangul_terms = [term for term in terms if _HANGUL_RE.fullmatch(term)]
        candidate_terms = (
            hangul_terms
            if hangul_terms and bigram_config["hangul_only_when_hangul_present"]
            else list(terms)
        )
        rescue_by_term = {
            term: tuple(sorted(set(_rescue_terms(term))))
            for term in candidate_terms
            if _rescue_terms(term)
        }
        if not rescue_by_term:
            return []
        weights = bigram_config["field_weights"]
        matched_ngrams: defaultdict[str, defaultdict[str, set[str]]] = defaultdict(
            lambda: defaultdict(set)
        )
        raw_scores: defaultdict[str, float] = defaultdict(float)
        for term, rescue_terms in rescue_by_term.items():
            placeholders = ", ".join("?" for _ in rescue_terms)
            params: list[Any] = list(rescue_terms)
            ticker_sql = ""
            if tickers:
                ticker_placeholders = ", ".join("?" for _ in tickers)
                ticker_sql = f" AND ticker IN ({ticker_placeholders})"
                params.extend(tickers)
            rows = self._conn.execute(
                f"""
                SELECT term_norm, routing_unit_id, field_name, occurrence_count
                FROM short_token_posting
                WHERE term_norm IN ({placeholders}) {ticker_sql}
                ORDER BY routing_unit_id, field_name, term_norm
                """,
                params,
            ).fetchall()
            for row in rows:
                unit_id = str(row["routing_unit_id"])
                matched_ngrams[unit_id][term].add(str(row["term_norm"]))
                raw_scores[unit_id] += float(weights.get(str(row["field_name"]), 1.0)) * math.log1p(
                    int(row["occurrence_count"])
                )

        minimum_ratio = float(bigram_config["minimum_ngram_coverage_ratio"])
        scored: list[tuple[str, int, float, float]] = []
        for unit_id, matched_by_term in matched_ngrams.items():
            coverage_values = [
                len(matched_by_term.get(term, set())) / len(rescue_terms)
                for term, rescue_terms in rescue_by_term.items()
            ]
            matched_term_count = sum(coverage >= minimum_ratio for coverage in coverage_values)
            if matched_term_count <= 0:
                continue
            scored.append(
                (
                    unit_id,
                    matched_term_count,
                    sum(coverage_values),
                    raw_scores[unit_id],
                )
            )
        return [
            unit_id
            for unit_id, _matched_count, _coverage, _score in sorted(
                scored,
                key=lambda item: (-item[1], -item[2], -item[3], item[0]),
            )[:limit]
        ]

    def _fuse_channels(
        self,
        channels: Mapping[str, Sequence[str]],
        *,
        allowed_units: set[str] | None,
        limit: int,
        primary_scores: Mapping[str, float],
    ) -> list[tuple[str, float, dict[str, float]]]:
        rrf_k = float(self._profile["rrf_k"])
        channel_weights = self._profile["channel_weights"]
        totals: defaultdict[str, float] = defaultdict(float)
        contributions: defaultdict[str, dict[str, float]] = defaultdict(dict)
        primary_weight = float(channel_weights["fielded_fts"])
        primary_scale = float(self._profile["fusion"]["primary_rrf_scale"]) / (rrf_k + 1.0)
        for unit_id, score in primary_scores.items():
            if allowed_units is not None and unit_id not in allowed_units:
                continue
            contribution = primary_weight * max(0.0, float(score)) * primary_scale
            totals[unit_id] += contribution
            contributions[unit_id]["fielded_fts"] = contribution
        for channel, unit_ids in sorted(channels.items()):
            if channel == "fielded_fts":
                continue
            weight = float(channel_weights.get(channel, 0.0))
            if weight <= 0:
                continue
            for rank, unit_id in enumerate(dict.fromkeys(unit_ids), start=1):
                if allowed_units is not None and unit_id not in allowed_units:
                    continue
                contribution = weight / (rrf_k + rank)
                totals[unit_id] += contribution
                contributions[unit_id][channel] = contribution
        return [
            (unit_id, score, contributions[unit_id])
            for unit_id, score in sorted(totals.items(), key=lambda item: (-item[1], item[0]))[
                :limit
            ]
        ]

    def _units_for_tickers(self, tickers: Sequence[str], *, limit: int) -> list[sqlite3.Row]:
        if not tickers:
            return []
        placeholders = ", ".join("?" for _ in tickers)
        return self._conn.execute(
            f"""
            SELECT routing_unit_id, ticker, materiality_max
            FROM routing_unit
            WHERE ticker IN ({placeholders})
            ORDER BY COALESCE(materiality_max, 0.0) DESC, routing_unit_id
            LIMIT ?
            """,
            [*tickers, int(limit)],
        ).fetchall()

    def _hydrate_unit_headers(
        self,
        ranked: Sequence[tuple[str, float, Mapping[str, float]]],
    ) -> list[dict[str, Any]]:
        if not ranked:
            return []
        ids = [unit_id for unit_id, _score, _channels in ranked]
        placeholders = ", ".join("?" for _ in ids)
        rows = self._conn.execute(
            f"""
            SELECT routing_unit_id, ticker, company_name, topic_family_label
            FROM routing_unit
            WHERE routing_unit_id IN ({placeholders})
            """,
            ids,
        ).fetchall()
        by_id = {str(row["routing_unit_id"]): dict(row) for row in rows}
        headers: list[dict[str, Any]] = []
        for unit_id, score, contributions in ranked:
            row = by_id.get(unit_id)
            if row is None:
                continue
            row["score"] = score
            row["channel_scores"] = dict(contributions)
            headers.append(row)
        return headers

    def _hydrate_units(
        self,
        ranked: Sequence[tuple[str, float, Mapping[str, float]]],
        *,
        primary_diagnostics: Mapping[str, Mapping[str, Any]],
    ) -> list[dict[str, Any]]:
        if not ranked:
            return []
        ids = [unit_id for unit_id, _score, _channels in ranked]
        placeholders = ", ".join("?" for _ in ids)
        rows = self._conn.execute(
            f"SELECT * FROM routing_unit WHERE routing_unit_id IN ({placeholders})",
            ids,
        ).fetchall()
        by_id = {str(row["routing_unit_id"]): dict(row) for row in rows}
        hydrated: list[dict[str, Any]] = []
        for unit_id, score, contributions in ranked:
            row = by_id.get(unit_id)
            if row is None:
                continue
            row["score"] = score
            row["channel_scores"] = dict(contributions)
            row["source_object_ids"] = json.loads(row.pop("source_object_ids_json"))
            row["evidence_grades"] = json.loads(row.pop("evidence_grades_json"))
            primary = primary_diagnostics.get(unit_id)
            if primary is not None:
                row["lexical_score"] = float(primary["lexical_score"])
                row["primary_score"] = float(primary["primary_score"])
                row["matched_terms"] = list(primary["matched_terms"])
                row["term_coverage_count"] = int(primary["term_coverage_count"])
                row["term_coverage_ratio"] = float(primary["term_coverage_ratio"])
                row["information_coverage"] = float(primary["information_coverage"])
                row["strict_match"] = bool(primary["strict_match"])
            hydrated.append(row)
        return hydrated

    def _coherence_rerank_tickers(
        self,
        *,
        terms: Sequence[str],
        coarse_candidates: Sequence[Mapping[str, Any]],
        protected_tickers: Sequence[str],
        scoped_tickers: Sequence[str],
        candidate_generation_allowed: bool,
        limit: int,
    ) -> tuple[list[dict[str, Any]], dict[str, Any], list[dict[str, Any]]]:
        """Fuse coarse rank with source-object term coherence."""
        coarse = [dict(row) for row in coarse_candidates]

        def fallback(
            reason: str,
        ) -> tuple[list[dict[str, Any]], dict[str, Any], list[dict[str, Any]]]:
            return (
                coarse[:limit],
                {
                    "available": self._coherence is not None,
                    "applied": False,
                    "reason": reason,
                    "fallback": "preserve_coarse_order",
                    "candidate_pool_size": len(coarse),
                },
                [],
            )

        if self._coherence is None:
            return fallback("coherence_index_absent")
        coherence_scope = tuple(scoped_tickers)
        if not candidate_generation_allowed and not coherence_scope:
            coherence_scope = tuple(
                str(row.get("ticker") or "").strip().upper()
                for row in coarse
                if str(row.get("ticker") or "").strip()
            )
        result = self._coherence.search_terms(terms, tickers=coherence_scope)
        if not result.get("applied"):
            return fallback(str(result.get("reason") or "coherence_not_applied"))
        coherence_rows = [
            dict(row) for row in result.get("candidates") or [] if isinstance(row, Mapping)
        ]
        if not coherence_rows:
            return fallback("coherence_candidates_empty")
        config = self._coherence.profile["query"]
        coherence_weight = float(config["coherence_weight"])
        coarse_weight = float(config["coarse_weight"])
        weight_total = coherence_weight + coarse_weight or 1.0
        coherence_by_ticker = {str(row["ticker"]): row for row in coherence_rows}
        coarse_by_ticker = {str(row["ticker"]): dict(row) for row in coarse}
        missing_tickers = sorted(set(coherence_by_ticker) - set(coarse_by_ticker))
        if not candidate_generation_allowed:
            missing_tickers = []
        if missing_tickers:
            placeholders = ", ".join("?" for _ in missing_tickers)
            profiles = {
                str(row["ticker"]): dict(row)
                for row in self._conn.execute(
                    f"""
                    SELECT ticker, company_name
                    FROM ticker_profile
                    WHERE ticker IN ({placeholders})
                    """,
                    missing_tickers,
                ).fetchall()
            }
            for ticker in missing_tickers:
                profile = profiles.get(ticker)
                if profile is None:
                    continue
                coarse_by_ticker[ticker] = {
                    "ticker": ticker,
                    "company_name": str(profile.get("company_name") or ticker),
                    "score": 0.0,
                    "best_topic_family": "",
                    "routing_unit_ids": [],
                    "matched_unit_count": 0,
                }

        coarse_ranks = {str(row["ticker"]): rank for rank, row in enumerate(coarse, start=1)}
        fused: list[dict[str, Any]] = []
        for ticker, base in coarse_by_ticker.items():
            coarse_rank = coarse_ranks.get(ticker)
            coarse_rank_score = 1.0 / math.log2(coarse_rank + 1) if coarse_rank else 0.0
            coherent = coherence_by_ticker.get(ticker)
            coherence_score = float(coherent.get("coherence_score") or 0.0) if coherent else 0.0
            final_score = (
                coherence_weight * coherence_score + coarse_weight * coarse_rank_score
            ) / weight_total
            payload = dict(base)
            payload.update(
                {
                    "coarse_rank": coarse_rank,
                    "coarse_score": float(base.get("score") or 0.0),
                    "coarse_rank_score": coarse_rank_score,
                    "coherence_score": coherence_score,
                    "source_coherent_match": coherent is not None,
                    "coherence_source_locator_rowid": (
                        int(coherent["source_locator_rowid"]) if coherent else None
                    ),
                    "coherence_matched_terms": (
                        list(coherent.get("matched_terms") or []) if coherent else []
                    ),
                    "score": final_score,
                }
            )
            fused.append(payload)
        protected = set(protected_tickers)
        fused.sort(
            key=lambda row: (
                str(row["ticker"]) not in protected,
                -float(row["score"]),
                int(row["coarse_rank"]) if row.get("coarse_rank") else len(coarse) + 1,
                str(row["ticker"]),
            )
        )
        return (
            fused[:limit],
            {
                "available": True,
                "applied": True,
                "reason": str(result.get("reason") or "source_coherent_term_pairs"),
                "fallback": "preserve_coarse_order",
                "candidate_pool_size": len(fused),
                "candidate_generation_allowed": candidate_generation_allowed,
                "coherence_weight": coherence_weight,
                "coarse_weight": coarse_weight,
                "pair_query_count": int(result.get("pair_query_count") or 0),
                "truncated_pair_count": int(result.get("truncated_pair_count") or 0),
                "matched_document_count": int(result.get("matched_document_count") or 0),
                "matched_ticker_count": int(result.get("matched_ticker_count") or 0),
                "usable_terms": list(result.get("usable_terms") or []),
                "protected_tickers": sorted(protected),
            },
            coherence_rows[: max(limit, 20)],
        )

    def _micro_rerank_tickers(
        self,
        *,
        terms: Sequence[str],
        term_stats: Mapping[str, _QueryTermStat],
        coarse_candidates: Sequence[Mapping[str, Any]],
        protected_tickers: Sequence[str],
        limit: int,
    ) -> tuple[list[dict[str, Any]], dict[str, Any], list[dict[str, Any]]]:
        """Rerank, but never generate or remove, coarse ticker candidates."""
        coarse = [dict(row) for row in coarse_candidates]
        config = self._profile["micro_rerank"]

        def fallback(
            reason: str,
        ) -> tuple[list[dict[str, Any]], dict[str, Any], list[dict[str, Any]]]:
            return (
                coarse[:limit],
                {
                    "enabled": bool(config["enabled"]),
                    "applied": False,
                    "reason": reason,
                    "fallback": "preserve_coarse_order",
                    "candidate_pool_size": len(coarse),
                    "matched_ticker_count": 0,
                },
                [],
            )

        if not config["enabled"]:
            return fallback("disabled_by_profile")
        if not coarse:
            return fallback("no_coarse_candidates")
        unique_terms = list(dict.fromkeys(terms))
        if len(unique_terms) < int(config["minimum_query_terms"]):
            return fallback("insufficient_query_terms")
        counts = self._metadata.get("counts")
        total_tickers = int(counts.get("ticker_profile") or 0) if isinstance(counts, Mapping) else 0
        if total_tickers >= int(config["anchor_gate_min_tickers"]):
            anchor_terms = [
                term
                for term in unique_terms
                if term in term_stats
                and term_stats[term].fts_ticker_df > 0
                and term_stats[term].ticker_df_ratio <= float(config["anchor_max_ticker_df_ratio"])
                and term_stats[term].information >= float(config["anchor_min_information"])
            ]
            if len(anchor_terms) < int(config["minimum_anchor_terms"]):
                return fallback("insufficient_selective_anchor")
        micro_count = (
            int(counts.get("micro_routing_unit") or 0) if isinstance(counts, Mapping) else 0
        )
        if micro_count <= 0:
            table_exists = self._conn.execute(
                """
                SELECT 1 FROM sqlite_master
                WHERE type = 'table' AND name = 'micro_routing_unit'
                """
            ).fetchone()
            if table_exists is None:
                return fallback("micro_index_absent")
            micro_count = int(
                self._conn.execute("SELECT COUNT(*) FROM micro_routing_unit").fetchone()[0]
            )
        if micro_count <= 0:
            return fallback("micro_index_empty")

        parent_ids = sorted(
            {
                str(unit_id)
                for candidate in coarse
                for unit_id in candidate.get("routing_unit_ids") or ()
                if str(unit_id)
            }
        )
        if not parent_ids:
            return fallback("coarse_topic_families_absent")
        micro_rows = self._micro_candidates(
            unique_terms,
            term_stats=term_stats,
            parent_ids=parent_ids,
        )
        coherent_rows = [row for row in micro_rows if bool(row["coherent_match"])]
        if not coherent_rows:
            return fallback("no_source_coherent_match")

        grouped: defaultdict[str, list[dict[str, Any]]] = defaultdict(list)
        for row in coherent_rows:
            grouped[str(row["ticker"])].append(row)
        units_per_ticker = int(config["units_per_ticker"])
        decay = [float(value) for value in config["score_decay"]]
        ticker_micro: dict[str, dict[str, Any]] = {}
        for ticker, rows in grouped.items():
            selected = sorted(
                rows,
                key=lambda row: (
                    -float(row["micro_score"]),
                    str(row["micro_unit_id"]),
                ),
            )[:units_per_ticker]
            applied_decay = decay[: len(selected)]
            normalizer = sum(applied_decay) or 1.0
            ticker_micro[ticker] = {
                "score": sum(
                    float(row["micro_score"]) * applied_decay[index]
                    for index, row in enumerate(selected)
                )
                / normalizer,
                "micro_unit_ids": [str(row["micro_unit_id"]) for row in selected],
                "source_kinds": sorted({str(row["source_kind"]) for row in selected}),
                "matched_terms": sorted(
                    {term for row in selected for term in row.get("matched_terms") or ()}
                ),
            }

        coarse_scores = [float(row.get("score") or 0.0) for row in coarse]
        minimum_coarse = min(coarse_scores)
        coarse_range = max(coarse_scores) - minimum_coarse
        coarse_weight = float(config["coarse_weight"])
        micro_weight = float(config["micro_weight"])
        weight_total = coarse_weight + micro_weight or 1.0
        reranked: list[dict[str, Any]] = []
        for coarse_rank, row in enumerate(coarse, start=1):
            ticker = str(row["ticker"])
            coarse_score = float(row.get("score") or 0.0)
            normalized_coarse = (
                (coarse_score - minimum_coarse) / coarse_range if coarse_range > 0 else 1.0
            )
            micro = ticker_micro.get(ticker)
            micro_score = float(micro["score"]) if micro is not None else 0.0
            payload = dict(row)
            payload.update(
                {
                    "coarse_rank": coarse_rank,
                    "coarse_score": coarse_score,
                    "micro_score": micro_score,
                    "source_coherent_match": micro is not None,
                    "score": (coarse_weight * normalized_coarse + micro_weight * micro_score)
                    / weight_total,
                    "micro_unit_ids": list(micro["micro_unit_ids"]) if micro else [],
                    "micro_source_kinds": list(micro["source_kinds"]) if micro else [],
                    "micro_matched_terms": list(micro["matched_terms"]) if micro else [],
                }
            )
            reranked.append(payload)
        coherence_gate = bool(config["coherence_gate"])
        protected = set(protected_tickers)
        reranked.sort(
            key=lambda row: (
                str(row["ticker"]) not in protected,
                not bool(row["source_coherent_match"]) if coherence_gate else False,
                -float(row["score"]),
                int(row["coarse_rank"]),
                str(row["ticker"]),
            )
        )
        diagnostics_rows = [
            {
                **{key: value for key, value in row.items() if key != "source_object_ids_json"},
                "source_object_ids": json.loads(row["source_object_ids_json"]),
            }
            for row in sorted(
                coherent_rows,
                key=lambda row: (
                    -float(row["micro_score"]),
                    str(row["micro_unit_id"]),
                ),
            )[: max(limit, 20)]
        ]
        return (
            reranked[:limit],
            {
                "enabled": True,
                "applied": True,
                "reason": "source_coherent_micro_match",
                "fallback": "preserve_coarse_order",
                "candidate_pool_size": len(coarse),
                "matched_ticker_count": len(ticker_micro),
                "protected_tickers": sorted(protected),
                "coherence_gate": coherence_gate,
                "minimum_coherent_terms": int(config["minimum_coherent_terms"]),
                "micro_unit_candidate_count": len(micro_rows),
                "coherent_micro_unit_count": len(coherent_rows),
            },
            diagnostics_rows,
        )

    def _micro_candidates(
        self,
        terms: Sequence[str],
        *,
        term_stats: Mapping[str, _QueryTermStat],
        parent_ids: Sequence[str],
    ) -> list[dict[str, Any]]:
        config = self._profile["micro_rerank"]
        weights = self._profile["fts_field_weights"]
        placeholders = ", ".join("?" for _ in parent_ids)
        scope_match = " OR ".join(
            f"routing_unit_id : {_fts_quote(parent_id)}" for parent_id in parent_ids
        )
        base_select = f"""
            SELECT m.*,
                   bm25(
                       micro_routing_fts,
                       0.0, 0.0, 0.0,
                       {float(weights["topic_text"])},
                       {float(weights["factor_text"])},
                       {float(weights["metric_text"])},
                       {float(weights["entity_text"])},
                       {float(weights["mechanism_text"])}
                   ) AS lexical_score
            FROM micro_routing_fts
            JOIN micro_routing_unit AS m
              ON m.micro_unit_id = micro_routing_fts.micro_unit_id
            WHERE micro_routing_fts MATCH ?
              AND micro_routing_fts.routing_unit_id IN ({placeholders})
            ORDER BY lexical_score ASC, m.micro_unit_id
            LIMIT ?
        """
        query_specs: list[tuple[str, int]] = [
            (
                " OR ".join(_fts_quote(term) for term in terms),
                int(config["micro_unit_candidate_limit"]),
            )
        ]
        if len(terms) > 1:
            query_specs.append(
                (
                    " AND ".join(_fts_quote(term) for term in terms),
                    int(config["pair_query_limit"]),
                )
            )
            informative_terms = sorted(
                terms,
                key=lambda term: (
                    -float(term_stats[term].information) if term in term_stats else 0.0,
                    term,
                ),
            )
            for left, right in list(combinations(informative_terms, 2))[
                : int(config["max_pair_queries"])
            ]:
                query_specs.append(
                    (
                        f"{_fts_quote(left)} AND {_fts_quote(right)}",
                        int(config["pair_query_limit"]),
                    )
                )

        candidates: dict[str, dict[str, Any]] = {}
        for term_match_query, query_limit in dict.fromkeys(query_specs):
            match_query = f"({scope_match}) AND ({term_match_query})"
            rows = self._conn.execute(
                base_select,
                [match_query, *parent_ids, query_limit],
            ).fetchall()
            for raw_row in rows:
                row = dict(raw_row)
                unit_id = str(row["micro_unit_id"])
                existing = candidates.get(unit_id)
                if existing is None or float(row["lexical_score"]) < float(
                    existing["lexical_score"]
                ):
                    candidates[unit_id] = row
        if not candidates:
            return []

        term_information = {
            term: max(
                0.0,
                float(term_stats[term].information) if term in term_stats else 1.0,
            )
            for term in terms
        }
        if not any(term_information.values()):
            term_information = dict.fromkeys(terms, 1.0)
        total_information = sum(term_information.values())
        strengths = [max(0.0, -float(row["lexical_score"])) for row in candidates.values()]
        minimum_strength = min(strengths)
        strength_range = max(strengths) - minimum_strength
        minimum_should_match = min(
            len(terms),
            max(
                int(config["minimum_coherent_terms"]),
                math.ceil(len(terms) * float(config["min_should_match_ratio"])),
            ),
        )
        component_weight = sum(
            float(config[key])
            for key in ("coverage_weight", "information_coverage_weight", "bm25_weight")
        )
        ranked: list[dict[str, Any]] = []
        for row, strength in zip(candidates.values(), strengths, strict=True):
            indexed_text_terms = {
                normalize_router_text(token)
                for field_name in (
                    "topic_text",
                    "factor_text",
                    "metric_text",
                    "entity_text",
                    "mechanism_text",
                )
                for token in _WORD_RE.findall(str(row.get(field_name) or ""))
                if normalize_router_text(token)
            }
            matched_terms = [term for term in terms if term in indexed_text_terms]
            coverage_count = len(matched_terms)
            coverage_ratio = coverage_count / len(terms)
            information_coverage = (
                sum(term_information[term] for term in matched_terms) / total_information
            )
            bm25_normalized = (
                (strength - minimum_strength) / strength_range if strength_range > 0 else 1.0
            )
            micro_score = (
                float(config["coverage_weight"]) * coverage_ratio
                + float(config["information_coverage_weight"]) * information_coverage
                + float(config["bm25_weight"]) * bm25_normalized
            ) / component_weight
            row.update(
                {
                    "micro_score": micro_score,
                    "matched_terms": matched_terms,
                    "term_coverage_count": coverage_count,
                    "term_coverage_ratio": coverage_ratio,
                    "information_coverage": information_coverage,
                    "bm25_normalized": bm25_normalized,
                    "minimum_should_match": minimum_should_match,
                    "coherent_match": coverage_count >= minimum_should_match,
                }
            )
            ranked.append(row)
        return sorted(
            ranked,
            key=lambda row: (
                not bool(row["coherent_match"]),
                -float(row["micro_score"]),
                str(row["micro_unit_id"]),
            ),
        )

    def _aggregate_tickers(
        self,
        units: Sequence[Mapping[str, Any]],
        *,
        limit: int,
    ) -> list[dict[str, Any]]:
        units_per_ticker = int(self._profile["aggregation"]["units_per_ticker"])
        score_decay = [float(value) for value in self._profile["aggregation"]["score_decay"]]
        grouped: defaultdict[str, list[Mapping[str, Any]]] = defaultdict(list)
        for row in units:
            grouped[str(row["ticker"])].append(row)
        candidates: list[dict[str, Any]] = []
        for ticker, ticker_units in sorted(grouped.items()):
            selected = sorted(
                ticker_units,
                key=lambda row: (-float(row["score"]), str(row["routing_unit_id"])),
            )[:units_per_ticker]
            candidates.append(
                {
                    "ticker": ticker,
                    "company_name": str(selected[0]["company_name"]),
                    "score": sum(
                        float(row["score"]) * score_decay[index]
                        for index, row in enumerate(selected)
                    ),
                    "best_topic_family": str(selected[0]["topic_family_label"]),
                    "routing_unit_ids": [str(row["routing_unit_id"]) for row in selected],
                    "matched_unit_count": len(ticker_units),
                }
            )
        return sorted(candidates, key=lambda row: (-float(row["score"]), row["ticker"]))[:limit]


def normalize_router_text(value: Any) -> str:
    """Normalize exact aliases/facet keys without domain-specific rewrites."""
    text = unicodedata.normalize("NFKC", str(value or "")).casefold()
    normalized = "".join(char if char.isalnum() else " " for char in text)
    return " ".join(normalized.split())


def _collect_company_names(
    conn: sqlite3.Connection,
    *,
    release_root: Path | None = None,
) -> dict[str, str]:
    counts: defaultdict[str, Counter[str]] = defaultdict(Counter)
    tickers: set[str] = set()
    for table in ("global_document_catalog", "global_object_locator"):
        rows = conn.execute(
            f"""
            SELECT ticker, company_name, COUNT(*) AS row_count
            FROM {table}
            GROUP BY ticker, company_name
            ORDER BY ticker, row_count DESC, company_name
            """
        ).fetchall()
        for row in rows:
            ticker = str(row["ticker"] or "").strip().upper()
            if not ticker:
                continue
            tickers.add(ticker)
            company_name = str(row["company_name"] or "").strip()
            if company_name:
                counts[ticker][company_name] += int(row["row_count"])
    for table in (
        "global_topic_spine",
        "global_factor_spine",
        "global_metric_spine",
        "global_entity_spine",
    ):
        rows = conn.execute(f"SELECT DISTINCT ticker FROM {table} ORDER BY ticker").fetchall()
        tickers.update(str(row[0]).strip().upper() for row in rows if str(row[0]).strip())
    result: dict[str, str] = {}
    for ticker in sorted(tickers):
        names = counts.get(ticker)
        indexed_name = (
            sorted(names.items(), key=lambda item: (-item[1], item[0]))[0][0] if names else ""
        )
        source_name = (
            _company_name_from_release(release_root, ticker)
            if release_root is not None and (not indexed_name or indexed_name == ticker)
            else ""
        )
        result[ticker] = source_name or indexed_name or ticker
    return result


def _company_name_from_release(release_root: Path, ticker: str) -> str:
    company_root = release_root / "companies" / ticker / "sources"
    if not company_root.is_dir():
        return ""
    candidates = sorted(company_root.glob("*/*/clean.md"), reverse=True)
    for path in candidates:
        raw_path = path.with_name("raw.html")
        if raw_path.is_file():
            try:
                with raw_path.open("r", encoding="utf-8", errors="ignore") as handle:
                    raw_prefix = handle.read(2 * 1024 * 1024)
            except OSError:
                raw_prefix = ""
            raw_match = _SEC_INLINE_XBRL_REGISTRANT_RE.search(raw_prefix)
            if raw_match:
                raw_name = html.unescape(_HTML_TAG_RE.sub(" ", raw_match.group(1)))
                name = " ".join(raw_name.split()).strip()
                if name and len(name) <= 200:
                    return name
        try:
            with path.open("r", encoding="utf-8", errors="ignore") as handle:
                prefix = handle.read(2 * 1024 * 1024)
        except OSError:
            continue
        match = _SEC_REGISTRANT_NAME_RE.search(prefix)
        if not match:
            continue
        name = " ".join(match.group(1).split()).strip()
        if name and len(name) <= 200:
            return name
    return ""


def _collect_routing_units(
    conn: sqlite3.Connection,
    *,
    facet_target: sqlite3.Connection,
    company_names: Mapping[str, str],
    overview_family_key: str,
    max_values_per_field: int,
    max_source_object_ids: int,
    max_value_chars: int,
) -> dict[tuple[str, str], _RoutingUnitAccumulator]:
    units: dict[tuple[str, str], _RoutingUnitAccumulator] = {}

    def get_unit(ticker: Any, family: Any, family_label: Any = None) -> _RoutingUnitAccumulator:
        normalized_ticker = str(ticker or "").strip().upper()
        raw_family = str(family or overview_family_key).strip() or overview_family_key
        family_key = normalize_router_text(raw_family) or overview_family_key
        key = (normalized_ticker, family_key)
        if key not in units:
            units[key] = _RoutingUnitAccumulator(
                ticker=normalized_ticker,
                company_name=company_names.get(normalized_ticker, normalized_ticker),
                family_key=family_key,
            )
        if family_label:
            units[key].family_labels.add(str(family_label))
        elif raw_family != overview_family_key:
            units[key].family_labels.add(raw_family)
        return units[key]

    def add_text_values(target: set[str], *values: Any) -> None:
        _add_bounded_values(
            target,
            *values,
            limit=max_values_per_field,
            max_value_chars=max_value_chars,
        )

    def add_source_ids(target: set[str], *values: Any) -> None:
        _add_bounded_values(
            target,
            *values,
            limit=max_source_object_ids,
            max_value_chars=512,
        )

    for ticker in sorted(company_names):
        get_unit(ticker, overview_family_key, "Company overview")

    for row in conn.execute("SELECT * FROM global_topic_spine ORDER BY ticker, topic_id"):
        unit = get_unit(row["ticker"], row["topic_family"], row["topic_family"])
        unit_id = _routing_unit_id(unit.ticker, unit.family_key)
        object_ids = _json_values(row["source_object_ids"]) or [""]
        unit.topic_count += 1
        add_text_values(unit.topics, row["topic_key"], row["topic_label"], row["topic_summary"])
        factor_values = _json_values(row["factor_terms"])
        metric_values = _json_values(row["metric_terms"])
        entity_values = _json_values(row["entity_terms"])
        mechanism_values = [
            *_json_values(row["mechanism_terms"]),
            *_json_values(row["impact_channels"]),
        ]
        add_text_values(unit.factors, *factor_values)
        add_text_values(unit.metrics, *metric_values)
        add_text_values(unit.entities, *entity_values)
        add_text_values(
            unit.mechanisms,
            *mechanism_values,
        )
        add_source_ids(unit.source_object_ids, *object_ids)
        _add_values(unit.evidence_grades, row["evidence_grade"])
        unit.materiality_max = _max_float(unit.materiality_max, row["materiality"])
        _write_facet_values(
            facet_target,
            kind="topic",
            values=(row["topic_key"], row["topic_label"]),
            ticker=unit.ticker,
            unit_id=unit_id,
            source_kind="topic",
            object_ids=object_ids,
            evidence_grade=row["evidence_grade"],
            materiality=row["materiality"],
        )
        for facet_kind, values in (
            ("factor", factor_values),
            ("metric", metric_values),
            ("entity", entity_values),
            ("mechanism", mechanism_values),
        ):
            for value in values:
                _write_facet_values(
                    facet_target,
                    kind=facet_kind,
                    values=(value,),
                    ticker=unit.ticker,
                    unit_id=unit_id,
                    source_kind="topic_projection",
                    object_ids=object_ids,
                    evidence_grade=row["evidence_grade"],
                    materiality=row["materiality"],
                )

    for row in conn.execute(
        """
        SELECT * FROM global_factor_spine
        ORDER BY ticker, factor_family, factor_key, object_id
        """
    ):
        unit = get_unit(row["ticker"], row["factor_family"], row["factor_family"])
        unit.factor_count += 1
        add_text_values(unit.factors, row["factor_key"], row["factor_label"], row["factor_family"])
        add_text_values(unit.mechanisms, row["impact_channel"], row["effect_direction"])
        add_source_ids(unit.source_object_ids, row["object_id"])
        _add_values(unit.evidence_grades, row["evidence_grade"])
        unit.materiality_max = _max_float(unit.materiality_max, row["materiality"])
        _write_facet_values(
            facet_target,
            kind="factor",
            values=(row["factor_key"], row["factor_label"], row["factor_family"]),
            ticker=unit.ticker,
            unit_id=_routing_unit_id(unit.ticker, unit.family_key),
            source_kind="factor",
            object_ids=(str(row["object_id"] or ""),),
            evidence_grade=row["evidence_grade"],
            materiality=row["materiality"],
        )

    for row in conn.execute(
        """
        SELECT * FROM global_metric_spine
        ORDER BY ticker, canonical_metric_key, object_id
        """
    ):
        unit = get_unit(row["ticker"], overview_family_key, "Company overview")
        unit.metric_count += 1
        add_text_values(unit.metrics, row["canonical_metric_key"], row["metric_name"], row["unit"])
        add_source_ids(unit.source_object_ids, row["object_id"])
        _write_facet_values(
            facet_target,
            kind="metric",
            values=(row["canonical_metric_key"], row["metric_name"]),
            ticker=unit.ticker,
            unit_id=_routing_unit_id(unit.ticker, unit.family_key),
            source_kind="metric",
            object_ids=(str(row["object_id"] or ""),),
        )

    for row in conn.execute(
        """
        SELECT * FROM global_entity_spine
        ORDER BY ticker, entity_key, object_id
        """
    ):
        unit = get_unit(row["ticker"], overview_family_key, "Company overview")
        unit.entity_count += 1
        entity_values = (
            row["entity_key"],
            row["canonical_name"],
            *_json_values(row["aliases"]),
        )
        add_text_values(
            unit.entities,
            *entity_values,
        )
        add_source_ids(unit.source_object_ids, row["object_id"])
        _write_facet_values(
            facet_target,
            kind="entity",
            values=entity_values,
            ticker=unit.ticker,
            unit_id=_routing_unit_id(unit.ticker, unit.family_key),
            source_kind="entity",
            object_ids=(str(row["object_id"] or ""),),
        )

    for row in conn.execute(
        """
        SELECT * FROM global_counterparty_spine
        ORDER BY ticker, counterparty_key, object_id
        """
    ):
        unit = get_unit(row["ticker"], overview_family_key, "Company overview")
        unit.entity_count += 1
        counterparty_values = (row["counterparty_key"], row["counterparty_name"])
        add_text_values(unit.entities, *counterparty_values)
        add_text_values(unit.mechanisms, row["relationship_type"], row["agreement_type"])
        add_source_ids(unit.source_object_ids, row["object_id"])
        _add_values(unit.evidence_grades, row["evidence_grade"])
        unit.materiality_max = _max_float(unit.materiality_max, row["materiality"])
        _write_facet_values(
            facet_target,
            kind="counterparty",
            values=counterparty_values,
            ticker=unit.ticker,
            unit_id=_routing_unit_id(unit.ticker, unit.family_key),
            source_kind="counterparty",
            object_ids=(str(row["object_id"] or ""),),
            evidence_grade=row["evidence_grade"],
            materiality=row["materiality"],
        )
    return units


def _write_ranking_profile(
    conn: sqlite3.Connection,
    profile: Mapping[str, Any],
    profile_sha256: str,
) -> None:
    conn.execute(
        """
        INSERT INTO ranking_profile(
            profile_id, profile_format, profile_version,
            config_json, config_sha256, active
        ) VALUES (?, ?, ?, ?, ?, 1)
        """,
        (
            profile["profile_id"],
            profile["format"],
            int(profile["version"]),
            _json_dumps(profile),
            profile_sha256,
        ),
    )


def _write_routing_units(
    conn: sqlite3.Connection,
    *,
    units: Mapping[tuple[str, str], _RoutingUnitAccumulator],
    unit_ids: Mapping[tuple[str, str], str],
    profile: Mapping[str, Any],
) -> None:
    config = profile["build"]
    max_values = int(config["max_values_per_field"])
    max_value_chars = int(config["max_value_chars"])
    max_source_ids = int(config["max_source_object_ids"])
    for key in sorted(units):
        unit = units[key]
        unit_id = unit_ids[key]
        family_label = (
            _join_values(
                unit.family_labels,
                max_values=1,
                max_value_chars=max_value_chars,
            )
            or unit.family_key
        )
        company_text = _join_values(
            (unit.ticker, unit.company_name),
            max_values=max_values,
            max_value_chars=max_value_chars,
        )
        topic_text = _join_values(
            unit.topics,
            max_values=max_values,
            max_value_chars=max_value_chars,
        )
        factor_text = _join_values(
            unit.factors,
            max_values=max_values,
            max_value_chars=max_value_chars,
        )
        metric_text = _join_values(
            unit.metrics,
            max_values=max_values,
            max_value_chars=max_value_chars,
        )
        entity_text = _join_values(
            unit.entities,
            max_values=max_values,
            max_value_chars=max_value_chars,
        )
        mechanism_text = _join_values(
            unit.mechanisms,
            max_values=max_values,
            max_value_chars=max_value_chars,
        )
        row = (
            unit_id,
            unit.ticker,
            unit.company_name,
            unit.family_key,
            family_label,
            company_text,
            topic_text,
            factor_text,
            metric_text,
            entity_text,
            mechanism_text,
            _json_dumps(sorted(unit.source_object_ids)[:max_source_ids]),
            _json_dumps(sorted(unit.evidence_grades)),
            unit.materiality_max,
            unit.topic_count,
            unit.factor_count,
            unit.metric_count,
            unit.entity_count,
        )
        conn.execute(
            """
            INSERT INTO routing_unit(
                routing_unit_id, ticker, company_name,
                topic_family_key, topic_family_label,
                company_text, topic_text, factor_text, metric_text,
                entity_text, mechanism_text, source_object_ids_json,
                evidence_grades_json, materiality_max,
                topic_count, factor_count, metric_count, entity_count
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            row,
        )
        conn.execute(
            """
            INSERT INTO routing_fts(
                routing_unit_id, ticker, company_text, topic_text,
                factor_text, metric_text, entity_text, mechanism_text
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            """,
            (
                unit_id,
                unit.ticker,
                company_text,
                topic_text,
                factor_text,
                metric_text,
                entity_text,
                mechanism_text,
            ),
        )


def _write_micro_routing_units(
    source: sqlite3.Connection,
    target: sqlite3.Connection,
    *,
    profile: Mapping[str, Any],
    overview_family_key: str,
) -> None:
    """Stream source-coherent micro units without retaining the corpus in RAM."""
    config = profile["build"]
    enabled_kinds = set(config["micro_source_kinds"])
    max_values = int(config["max_values_per_field"])
    max_value_chars = int(config["max_value_chars"])
    max_source_ids = int(config["max_source_object_ids"])

    def write(accumulator: _MicroUnitAccumulator) -> None:
        ticker = accumulator.ticker
        if not ticker:
            return
        family_key = accumulator.family_key or overview_family_key
        routing_unit_id = _routing_unit_id(ticker, family_key)
        micro_unit_id = _micro_unit_id(
            accumulator.source_kind,
            ticker,
            family_key,
            accumulator.source_cluster_key,
        )
        topic_text = _join_values(
            accumulator.topics,
            max_values=max_values,
            max_value_chars=max_value_chars,
        )
        factor_text = _join_values(
            accumulator.factors,
            max_values=max_values,
            max_value_chars=max_value_chars,
        )
        metric_text = _join_values(
            accumulator.metrics,
            max_values=max_values,
            max_value_chars=max_value_chars,
        )
        entity_text = _join_values(
            accumulator.entities,
            max_values=max_values,
            max_value_chars=max_value_chars,
        )
        mechanism_text = _join_values(
            accumulator.mechanisms,
            max_values=max_values,
            max_value_chars=max_value_chars,
        )
        source_ids = sorted(accumulator.source_object_ids)[:max_source_ids]
        target.execute(
            """
            INSERT INTO micro_routing_unit(
                micro_unit_id, routing_unit_id, ticker, topic_family_key,
                source_kind, source_cluster_key, topic_text, factor_text,
                metric_text, entity_text, mechanism_text,
                source_object_ids_json, evidence_grade, materiality
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            (
                micro_unit_id,
                routing_unit_id,
                ticker,
                family_key,
                accumulator.source_kind,
                accumulator.source_cluster_key,
                topic_text,
                factor_text,
                metric_text,
                entity_text,
                mechanism_text,
                _json_dumps(source_ids),
                _join_values(
                    accumulator.evidence_grades,
                    max_values=8,
                    max_value_chars=64,
                ),
                accumulator.materiality,
            ),
        )
        target.execute(
            """
            INSERT INTO micro_routing_fts(
                micro_unit_id, routing_unit_id, ticker, topic_text,
                factor_text, metric_text, entity_text, mechanism_text
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            """,
            (
                micro_unit_id,
                routing_unit_id,
                ticker,
                topic_text,
                factor_text,
                metric_text,
                entity_text,
                mechanism_text,
            ),
        )

    if "topic" in enabled_kinds:
        for row in source.execute("SELECT * FROM global_topic_spine ORDER BY ticker, topic_id"):
            ticker = str(row["ticker"] or "").strip().upper()
            raw_family = str(row["topic_family"] or overview_family_key).strip()
            family_key = normalize_router_text(raw_family) or overview_family_key
            topic_id = str(row["topic_id"] or "").strip()
            if not ticker or not topic_id:
                continue
            unit = _MicroUnitAccumulator(
                ticker=ticker,
                family_key=family_key,
                source_kind="topic",
                source_cluster_key=topic_id,
            )
            _add_bounded_values(
                unit.topics,
                row["topic_key"],
                row["topic_label"],
                raw_family,
                row["topic_summary"],
                limit=max_values,
                max_value_chars=max_value_chars,
            )
            _add_bounded_values(
                unit.factors,
                *_json_values(row["factor_terms"]),
                limit=max_values,
                max_value_chars=max_value_chars,
            )
            _add_bounded_values(
                unit.metrics,
                *_json_values(row["metric_terms"]),
                limit=max_values,
                max_value_chars=max_value_chars,
            )
            _add_bounded_values(
                unit.entities,
                *_json_values(row["entity_terms"]),
                limit=max_values,
                max_value_chars=max_value_chars,
            )
            _add_bounded_values(
                unit.mechanisms,
                *_json_values(row["mechanism_terms"]),
                *_json_values(row["impact_channels"]),
                limit=max_values,
                max_value_chars=max_value_chars,
            )
            _add_bounded_values(
                unit.source_object_ids,
                *_json_values(row["source_object_ids"]),
                limit=max_source_ids,
                max_value_chars=512,
            )
            _add_values(unit.evidence_grades, row["evidence_grade"])
            unit.materiality = _float_or_none(row["materiality"])
            write(unit)

    if "factor" in enabled_kinds:
        current: _MicroUnitAccumulator | None = None
        current_key: tuple[str, str, str] | None = None
        rows = source.execute(
            """
            SELECT * FROM global_factor_spine
            ORDER BY ticker, factor_family, object_id, factor_key, factor_label
            """
        )
        for row in rows:
            ticker = str(row["ticker"] or "").strip().upper()
            raw_family = str(row["factor_family"] or overview_family_key).strip()
            family_key = normalize_router_text(raw_family) or overview_family_key
            object_id = str(row["object_id"] or "").strip()
            if not ticker or not object_id:
                continue
            key = (ticker, family_key, object_id)
            if key != current_key:
                if current is not None:
                    write(current)
                current = _MicroUnitAccumulator(
                    ticker=ticker,
                    family_key=family_key,
                    source_kind="factor",
                    source_cluster_key=object_id,
                )
                current_key = key
            assert current is not None
            _add_bounded_values(
                current.topics,
                raw_family,
                row["benchmark"],
                limit=max_values,
                max_value_chars=max_value_chars,
            )
            _add_bounded_values(
                current.factors,
                row["factor_key"],
                row["factor_label"],
                limit=max_values,
                max_value_chars=max_value_chars,
            )
            _add_bounded_values(
                current.mechanisms,
                row["impact_channel"],
                row["effect_direction"],
                limit=max_values,
                max_value_chars=max_value_chars,
            )
            _add_bounded_values(
                current.source_object_ids,
                object_id,
                limit=max_source_ids,
                max_value_chars=512,
            )
            _add_values(current.evidence_grades, row["evidence_grade"])
            current.materiality = _max_float(current.materiality, row["materiality"])
        if current is not None:
            write(current)

    if "metric" in enabled_kinds:
        current = None
        current_metric_key: tuple[str, str] | None = None
        rows = source.execute(
            """
            SELECT * FROM global_metric_spine
            ORDER BY ticker, object_id, canonical_metric_key, metric_name, unit
            """
        )
        for row in rows:
            ticker = str(row["ticker"] or "").strip().upper()
            object_id = str(row["object_id"] or "").strip()
            if not ticker or not object_id:
                continue
            key = (ticker, object_id)
            if key != current_metric_key:
                if current is not None:
                    write(current)
                current = _MicroUnitAccumulator(
                    ticker=ticker,
                    family_key=overview_family_key,
                    source_kind="metric",
                    source_cluster_key=object_id,
                )
                current_metric_key = key
            assert current is not None
            _add_bounded_values(
                current.metrics,
                row["canonical_metric_key"],
                row["metric_name"],
                row["unit"],
                limit=max_values,
                max_value_chars=max_value_chars,
            )
            _add_bounded_values(
                current.mechanisms,
                row["trend_direction"],
                limit=max_values,
                max_value_chars=max_value_chars,
            )
            _add_bounded_values(
                current.source_object_ids,
                object_id,
                limit=max_source_ids,
                max_value_chars=512,
            )
        if current is not None:
            write(current)

    if "counterparty" in enabled_kinds:
        current = None
        current_counterparty_key: tuple[str, str] | None = None
        rows = source.execute(
            """
            SELECT * FROM global_counterparty_spine
            ORDER BY ticker, object_id, counterparty_key, counterparty_name
            """
        )
        for row in rows:
            ticker = str(row["ticker"] or "").strip().upper()
            object_id = str(row["object_id"] or "").strip()
            if not ticker or not object_id:
                continue
            key = (ticker, object_id)
            if key != current_counterparty_key:
                if current is not None:
                    write(current)
                current = _MicroUnitAccumulator(
                    ticker=ticker,
                    family_key=overview_family_key,
                    source_kind="counterparty",
                    source_cluster_key=object_id,
                )
                current_counterparty_key = key
            assert current is not None
            _add_bounded_values(
                current.entities,
                row["counterparty_key"],
                row["counterparty_name"],
                limit=max_values,
                max_value_chars=max_value_chars,
            )
            _add_bounded_values(
                current.mechanisms,
                row["relationship_type"],
                row["agreement_type"],
                *_json_values(row["affected_channels"]),
                limit=max_values,
                max_value_chars=max_value_chars,
            )
            _add_bounded_values(
                current.source_object_ids,
                object_id,
                limit=max_source_ids,
                max_value_chars=512,
            )
            _add_values(current.evidence_grades, row["evidence_grade"])
            current.materiality = _max_float(current.materiality, row["materiality"])
        if current is not None:
            write(current)


def _write_ticker_profiles_and_company_aliases(
    conn: sqlite3.Connection,
    *,
    company_names: Mapping[str, str],
    units: Mapping[tuple[str, str], _RoutingUnitAccumulator],
    unit_ids: Mapping[tuple[str, str], str],
    profile: Mapping[str, Any],
) -> None:
    token_length = int(profile["alias"]["minimum_unique_token_length"])
    token_owners: defaultdict[str, set[str]] = defaultdict(set)
    for ticker, company_name in sorted(company_names.items()):
        for token in _lexical_terms(company_name, limit=64):
            if len(token) >= token_length:
                token_owners[token].add(ticker)
    unit_counts = Counter(ticker for ticker, _family in units)
    for ticker, company_name in sorted(company_names.items()):
        aliases = {ticker, company_name}
        unique_tokens = sorted(
            token
            for token in _lexical_terms(company_name, limit=64)
            if len(token) >= token_length and token_owners[token] == {ticker}
        )
        aliases.update(unique_tokens)
        conn.execute(
            """
            INSERT INTO ticker_profile(
                ticker, company_name, aliases_json,
                routing_unit_count, topic_family_count
            ) VALUES (?, ?, ?, ?, ?)
            """,
            (
                ticker,
                company_name,
                _json_dumps(sorted(aliases)),
                unit_counts[ticker],
                unit_counts[ticker],
            ),
        )
        _insert_alias(conn, ticker, "ticker", ticker, ticker, "", ticker, "ticker", 100)
        _insert_alias(
            conn,
            company_name,
            "company",
            ticker,
            ticker,
            "",
            company_name,
            "company_name",
            95,
        )
        for token in unique_tokens:
            _insert_alias(
                conn,
                token,
                "company_token",
                ticker,
                ticker,
                "",
                token,
                "derived_unique_token",
                70,
            )


def _write_facet_values(
    conn: sqlite3.Connection,
    *,
    kind: str,
    values: Sequence[Any],
    ticker: str,
    unit_id: str,
    source_kind: str,
    object_ids: Sequence[str],
    evidence_grade: Any = None,
    materiality: Any = None,
) -> None:
    normalized_values: dict[str, str] = {}
    for value in values:
        label = str(value or "").strip()
        key = normalize_router_text(label)
        if key and key not in normalized_values:
            normalized_values[key] = label
    if not normalized_values:
        return
    canonical_key, canonical_label = next(iter(normalized_values.items()))
    for key, label in sorted(normalized_values.items()):
        _insert_alias(
            conn,
            label,
            kind,
            canonical_key,
            ticker,
            unit_id,
            label,
            source_kind,
            60,
        )
    source_ids = sorted({str(value or "") for value in object_ids if str(value or "")})
    source_object_id = source_ids[0] if source_ids else ""
    conn.execute(
        """
        INSERT INTO facet_posting(
            facet_kind, facet_key, facet_label, ticker, routing_unit_id,
            source_kind, source_count, sample_object_id, sample_document_id,
            sample_period, sample_document_type, evidence_grade, materiality_max
        ) VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(facet_kind, facet_key, ticker, routing_unit_id) DO UPDATE SET
            source_count = facet_posting.source_count + 1,
            materiality_max = CASE
                WHEN facet_posting.materiality_max IS NULL
                    THEN excluded.materiality_max
                WHEN excluded.materiality_max IS NULL
                    THEN facet_posting.materiality_max
                ELSE MAX(facet_posting.materiality_max, excluded.materiality_max)
            END,
            evidence_grade = CASE
                WHEN facet_posting.evidence_grade = ''
                    THEN excluded.evidence_grade
                ELSE facet_posting.evidence_grade
            END
        """,
        (
            kind,
            canonical_key,
            canonical_label,
            ticker,
            unit_id,
            source_kind,
            source_object_id,
            "",
            "",
            "",
            str(evidence_grade or ""),
            _float_or_none(materiality),
        ),
    )


def _insert_alias(
    conn: sqlite3.Connection,
    raw_alias: Any,
    alias_kind: str,
    canonical_key: str,
    ticker: str,
    routing_unit_id: str,
    display_label: str,
    source_kind: str,
    priority: int,
) -> None:
    alias_norm = normalize_router_text(raw_alias)
    if not alias_norm:
        return
    conn.execute(
        """
        INSERT OR IGNORE INTO alias_lookup(
            alias_norm, alias_kind, canonical_key, ticker,
            routing_unit_id, display_label, source_kind, priority
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        """,
        (
            alias_norm,
            alias_kind,
            normalize_router_text(canonical_key),
            ticker,
            routing_unit_id,
            display_label,
            source_kind,
            priority,
        ),
    )


def _write_short_token_postings(conn: sqlite3.Connection) -> None:
    fields = (
        "company_text",
        "topic_text",
        "factor_text",
        "metric_text",
        "entity_text",
        "mechanism_text",
    )
    rows = conn.execute(
        f"""
        SELECT routing_unit_id, ticker, {", ".join(fields)}
        FROM routing_unit
        ORDER BY ticker, routing_unit_id
        """
    )
    fts_unit_df: Counter[str] = Counter()
    fts_ticker_df: Counter[str] = Counter()
    current_ticker: str | None = None
    current_ticker_terms: set[str] = set()
    for row in rows:
        ticker = str(row["ticker"])
        if current_ticker is not None and ticker != current_ticker:
            fts_ticker_df.update(current_ticker_terms)
            current_ticker_terms.clear()
        current_ticker = ticker
        unit_terms: set[str] = set()
        for field_name in fields:
            counts: Counter[str] = Counter()
            field_terms = _lexical_terms(str(row[field_name] or ""), limit=100_000)
            unit_terms.update(field_terms)
            for token in field_terms:
                counts.update(_rescue_terms(token))
            conn.executemany(
                """
                INSERT INTO short_token_posting(
                    term_norm, routing_unit_id, ticker, field_name, occurrence_count
                ) VALUES (?, ?, ?, ?, ?)
                """,
                (
                    (
                        term,
                        row["routing_unit_id"],
                        row["ticker"],
                        field_name,
                        count,
                    )
                    for term, count in sorted(counts.items())
                ),
            )
        fts_unit_df.update(unit_terms)
        current_ticker_terms.update(unit_terms)
    if current_ticker is not None:
        fts_ticker_df.update(current_ticker_terms)
    _write_term_stats(
        conn,
        python_fts_unit_df=fts_unit_df,
        python_fts_ticker_df=fts_ticker_df,
    )


def _write_term_stats(
    conn: sqlite3.Connection,
    *,
    python_fts_unit_df: Mapping[str, int],
    python_fts_ticker_df: Mapping[str, int],
) -> None:
    """Materialize exact term postings and cold-query document frequencies."""
    alias_df = {
        str(row["term_norm"]): (
            int(row["unit_count"] or 0),
            int(row["ticker_count"] or 0),
        )
        for row in conn.execute(
            """
            SELECT alias_norm AS term_norm,
                   COUNT(DISTINCT CASE
                       WHEN routing_unit_id != '' THEN routing_unit_id
                       ELSE NULL
                   END) AS unit_count,
                   COUNT(DISTINCT ticker) AS ticker_count
            FROM alias_lookup
            GROUP BY alias_norm
            ORDER BY alias_norm
            """
        )
    }
    conn.execute("DELETE FROM routing_term_posting")
    conn.execute("DROP TABLE IF EXISTS temp.routing_fts_vocab_for_postings")
    conn.execute(
        "CREATE VIRTUAL TABLE temp.routing_fts_vocab_for_postings "
        "USING fts5vocab(main, routing_fts, 'instance')"
    )
    conn.execute(
        """
        INSERT INTO routing_term_posting(term_norm, fts_rowid)
        SELECT DISTINCT term, doc
        FROM temp.routing_fts_vocab_for_postings
        ORDER BY term, doc
        """
    )
    conn.execute("DROP TABLE temp.routing_fts_vocab_for_postings")
    fts_df = {
        str(row["term_norm"]): int(row["unit_count"] or 0)
        for row in conn.execute(
            """
            SELECT term_norm, COUNT(*) AS unit_count
            FROM routing_term_posting
            GROUP BY term_norm
            ORDER BY term_norm
            """
        )
    }
    exact_ticker_df_cache: dict[str, int] = {}

    def fts_ticker_df(term: str, unit_df: int) -> int:
        python_unit_df = int(python_fts_unit_df.get(term, 0))
        python_ticker_df = int(python_fts_ticker_df.get(term, 0))
        if python_unit_df == unit_df and 0 < python_ticker_df <= unit_df:
            return python_ticker_df
        cached = exact_ticker_df_cache.get(term)
        if cached is not None:
            return cached
        row = conn.execute(
            """
            SELECT COUNT(DISTINCT ticker)
            FROM routing_fts
            WHERE routing_fts MATCH ?
            """,
            (_fts_quote(term),),
        ).fetchone()
        exact = int(row[0] or 0)
        exact_ticker_df_cache[term] = exact
        return exact

    payload: list[tuple[Any, ...]] = []
    for term in sorted(set(fts_df) | set(alias_df)):
        unit_df = int(fts_df.get(term, 0))
        alias_unit_df, alias_ticker_df = alias_df.get(term, (0, 0))
        payload.append(
            (
                term,
                unit_df,
                fts_ticker_df(term, unit_df) if unit_df else 0,
                alias_unit_df,
                alias_ticker_df,
            )
        )
    conn.executemany(
        """
        INSERT INTO term_stats(
            term_norm, fts_unit_df, fts_ticker_df,
            alias_unit_df, alias_ticker_df
        ) VALUES (?, ?, ?, ?, ?)
        """,
        payload,
    )


def _write_graph_priors(source: sqlite3.Connection, target: sqlite3.Connection) -> None:
    for row in source.execute(
        """
        SELECT * FROM global_chain_index
        ORDER BY from_ticker, to_ticker, shared_key_type, shared_key, link_type, link_id
        """
    ):
        shared_key = normalize_router_text(row["shared_key"])
        if not shared_key:
            continue
        target.execute(
            """
            INSERT INTO graph_prior(
                from_ticker, to_ticker, shared_key_type, shared_key,
                link_type, weight, confidence, evidence_grade,
                materiality, explanation
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(
                from_ticker, to_ticker, shared_key_type, shared_key, link_type
            ) DO UPDATE SET
                confidence = CASE
                    WHEN graph_prior.confidence IS NULL THEN excluded.confidence
                    WHEN excluded.confidence IS NULL THEN graph_prior.confidence
                    ELSE MAX(graph_prior.confidence, excluded.confidence)
                END,
                materiality = CASE
                    WHEN graph_prior.materiality IS NULL THEN excluded.materiality
                    WHEN excluded.materiality IS NULL THEN graph_prior.materiality
                    ELSE MAX(graph_prior.materiality, excluded.materiality)
                END,
                evidence_grade = CASE
                    WHEN excluded.weight > graph_prior.weight
                      OR (
                          excluded.weight = graph_prior.weight
                          AND excluded.explanation < graph_prior.explanation
                      )
                    THEN excluded.evidence_grade
                    ELSE graph_prior.evidence_grade
                END,
                explanation = CASE
                    WHEN excluded.weight > graph_prior.weight
                      OR (
                          excluded.weight = graph_prior.weight
                          AND excluded.explanation < graph_prior.explanation
                      )
                    THEN excluded.explanation
                    ELSE graph_prior.explanation
                END,
                weight = MAX(graph_prior.weight, excluded.weight)
            """,
            (
                str(row["from_ticker"]).upper(),
                str(row["to_ticker"]).upper(),
                str(row["shared_key_type"]),
                shared_key,
                str(row["link_type"]),
                float(row["weight"]),
                _float_or_none(row["confidence"]),
                str(row["evidence_grade"] or ""),
                _float_or_none(row["materiality"]),
                str(row["explanation_template"] or ""),
            ),
        )


def _profile_verification_errors(
    conn: sqlite3.Connection,
    metadata: Mapping[str, Any],
) -> list[str]:
    errors: list[str] = []
    rows = conn.execute(
        "SELECT profile_id, config_json, config_sha256, active FROM ranking_profile ORDER BY profile_id"
    ).fetchall()
    active_rows = [row for row in rows if int(row["active"]) == 1]
    if len(active_rows) != 1:
        errors.append("router_sidecar_active_ranking_profile_count_mismatch")
    for row in rows:
        try:
            config = json.loads(str(row["config_json"]))
        except json.JSONDecodeError:
            errors.append(f"router_sidecar_ranking_profile_invalid_json:{row['profile_id']}")
            continue
        actual = _json_sha256(config)
        if actual != str(row["config_sha256"]):
            errors.append(f"router_sidecar_ranking_profile_hash_mismatch:{row['profile_id']}")
        if int(row["active"]) == 1:
            if actual != metadata.get("ranking_profile_sha256"):
                errors.append("router_sidecar_metadata_ranking_profile_hash_mismatch")
            if str(row["profile_id"]) != metadata.get("ranking_profile_id"):
                errors.append("router_sidecar_metadata_ranking_profile_id_mismatch")
            expected_coarse_hash = metadata.get("coarse_profile_sha256")
            if (
                expected_coarse_hash is not None
                and _coarse_profile_sha256(config) != expected_coarse_hash
            ):
                errors.append("router_sidecar_metadata_coarse_profile_hash_mismatch")
    return errors


def _sidecar_counts(conn: sqlite3.Connection) -> dict[str, int]:
    return {
        table: int(conn.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0])
        for table in ROUTER_SIDECAR_TABLES
        if table != "metadata"
    }


def _content_sha256(conn: sqlite3.Connection) -> str:
    digest = hashlib.sha256()
    for table in _CONTENT_HASH_TABLES:
        columns = [str(row[1]) for row in conn.execute(f"PRAGMA table_info({table})")]
        if not columns:
            continue
        column_sql = ", ".join(columns)
        order_sql = _CONTENT_HASH_ORDER_BY[table]
        digest.update(f"table:{table}\n".encode("utf-8"))
        for row in conn.execute(f"SELECT {column_sql} FROM {table} ORDER BY {order_sql}"):
            digest.update(_json_dumps(list(row)).encode("utf-8"))
            digest.update(b"\n")
    return digest.hexdigest()


def _write_metadata(conn: sqlite3.Connection, metadata: Mapping[str, Any]) -> None:
    conn.executemany(
        "INSERT OR REPLACE INTO metadata(key, value_json) VALUES (?, ?)",
        ((key, _json_dumps(value)) for key, value in sorted(metadata.items())),
    )


def _read_metadata(conn: sqlite3.Connection) -> dict[str, Any]:
    try:
        rows = conn.execute("SELECT key, value_json FROM metadata ORDER BY key").fetchall()
    except sqlite3.Error:
        return {}
    metadata: dict[str, Any] = {}
    for row in rows:
        key = str(row[0])
        try:
            metadata[key] = json.loads(str(row[1]))
        except json.JSONDecodeError:
            metadata[key] = str(row[1])
    return metadata


def _query_alias_candidates(
    query: str,
    *,
    terms: Sequence[str],
    max_ngram_tokens: int,
) -> list[str]:
    candidates = {normalize_router_text(query), *terms}
    for width in range(2, min(max_ngram_tokens, len(terms)) + 1):
        for start in range(0, len(terms) - width + 1):
            candidates.add(" ".join(terms[start : start + width]))
    return sorted(candidate for candidate in candidates if candidate)


def _lexical_terms(value: str, *, limit: int) -> list[str]:
    normalized = unicodedata.normalize("NFKC", str(value or "")).casefold()
    return list(dict.fromkeys(_WORD_RE.findall(normalized)))[:limit]


def _rescue_terms(token: str) -> list[str]:
    normalized = normalize_router_text(token).replace(" ", "")
    if not normalized:
        return []
    if _HANGUL_RE.fullmatch(normalized):
        if len(normalized) == 1:
            return [normalized]
        return [normalized[index : index + 2] for index in range(len(normalized) - 1)]
    return [normalized] if len(normalized) <= 2 else []


def _fts_quote(term: str) -> str:
    return '"' + term.replace('"', '""') + '"'


def _normalize_tickers(value: str | Sequence[str] | None) -> tuple[str, ...]:
    if value is None:
        return ()
    values = (value,) if isinstance(value, str) else value
    return tuple(sorted({str(item).strip().upper() for item in values if str(item).strip()}))


def _routing_unit_id(ticker: str, family_key: str) -> str:
    digest = hashlib.sha256(f"{ticker}\0{family_key}".encode("utf-8")).hexdigest()[:24]
    return f"route:{ticker}:{digest}"


def _micro_unit_id(
    source_kind: str,
    ticker: str,
    family_key: str,
    source_cluster_key: str,
) -> str:
    identity = "\0".join((source_kind, ticker, family_key, source_cluster_key))
    digest = hashlib.sha256(identity.encode("utf-8")).hexdigest()[:32]
    return f"micro:{source_kind}:{digest}"


def _json_values(value: Any) -> list[str]:
    if value is None:
        return []
    parsed: Any = value
    if isinstance(value, str):
        stripped = value.strip()
        if not stripped:
            return []
        try:
            parsed = json.loads(stripped)
        except json.JSONDecodeError:
            return [stripped]
    if isinstance(parsed, Mapping):
        values: Iterable[Any] = parsed.values()
    elif isinstance(parsed, Sequence) and not isinstance(parsed, (str, bytes, bytearray)):
        values = parsed
    else:
        values = (parsed,)
    return sorted({str(item).strip() for item in values if str(item).strip()})


def _add_values(target: set[str], *values: Any) -> None:
    for value in values:
        text = str(value or "").strip()
        if text:
            target.add(text)


def _add_bounded_values(
    target: set[str],
    *values: Any,
    limit: int,
    max_value_chars: int,
) -> None:
    """Keep the first deterministic distinct values from an ordered source scan."""
    for value in values:
        text = str(value or "").strip()[:max_value_chars]
        if not text or text in target or len(target) >= limit:
            continue
        target.add(text)


def _max_float(current: float | None, value: Any) -> float | None:
    parsed = _float_or_none(value)
    if parsed is None:
        return current
    return parsed if current is None else max(current, parsed)


def _float_or_none(value: Any) -> float | None:
    try:
        return float(value) if value is not None else None
    except (TypeError, ValueError):
        return None


def _join_values(
    values: Iterable[str],
    *,
    max_values: int,
    max_value_chars: int,
) -> str:
    ordered = sorted(
        {str(value).strip() for value in values if str(value).strip()}, key=str.casefold
    )
    return " ; ".join(value[:max_value_chars] for value in ordered[:max_values])


def _json_dumps(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def _json_sha256(value: Any) -> str:
    return hashlib.sha256(_json_dumps(value).encode("utf-8")).hexdigest()


def _coarse_profile_sha256(profile: Mapping[str, Any]) -> str:
    """Hash every non-micro setting that can affect coarse build or ranking."""
    coarse_profile = {
        key: value
        for key, value in profile.items()
        if key not in {"profile_id", "version", "micro_rerank"}
    }
    build = coarse_profile.get("build")
    if isinstance(build, Mapping):
        coarse_profile["build"] = {
            key: value for key, value in build.items() if not str(key).startswith("micro_")
        }
    return _json_sha256(coarse_profile)


def _file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def immutable_file_sha256(path: Path | str) -> str:
    """Hash an immutable artifact once for each stable stat signature."""
    resolved = Path(path).expanduser().resolve()
    sealed_digest = read_spine_verification_sha256(resolved)
    if sealed_digest is not None:
        return sealed_digest
    for _attempt in range(2):
        before = resolved.stat()
        stat_key = (
            int(before.st_dev),
            int(before.st_ino),
            int(before.st_size),
            int(before.st_mtime_ns),
        )
        digest = _immutable_file_sha256_for_stat(str(resolved), *stat_key)
        after = resolved.stat()
        after_key = (
            int(after.st_dev),
            int(after.st_ino),
            int(after.st_size),
            int(after.st_mtime_ns),
        )
        if after_key == stat_key:
            try:
                record_spine_verification_sha256(resolved, digest)
            except (OSError, RuntimeError, ValueError):
                # Most immutable artifacts are not spine databases and do not
                # carry a deep-verification seal. The stat-bound in-process
                # cache remains the fallback for those files.
                pass
            return digest
    raise RuntimeError(f"immutable artifact changed while hashing: {resolved}")


@lru_cache(maxsize=4096)
def _immutable_file_sha256_for_stat(
    path: str,
    device: int,
    inode: int,
    size: int,
    mtime_ns: int,
) -> str:
    del device, inode, size, mtime_ns
    return _file_sha256(Path(path))


def _cleanup_sqlite_files(path: Path) -> None:
    for candidate in (
        path,
        Path(f"{path}-wal"),
        Path(f"{path}-shm"),
        Path(f"{path}-journal"),
        immutable_sqlite_cache_seal_path(path),
    ):
        candidate.unlink(missing_ok=True)
