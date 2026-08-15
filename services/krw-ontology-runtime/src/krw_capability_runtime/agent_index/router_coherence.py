"""Compact source-coherence retrieval for the coarse ontology Router.

The index stores no source text. FTS5 keeps only token postings while a small
integer table maps immutable Global Spine locator rowids to tickers. This lets
the Router test whether selective query terms occur in the same source object
without duplicating the ontology or opening hundreds of company shards.
"""

from __future__ import annotations

import hashlib
import json
import math
import os
import sqlite3
import time
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from itertools import combinations
from pathlib import Path
from typing import Any
from urllib.parse import quote

from krw_capability_runtime.agent_index.cache_seal import (
    assert_trusted_immutable_copy,
    read_immutable_sqlite_cache_seal,
    read_immutable_sqlite_cache_sha256,
    remove_immutable_sqlite_cache_seal,
    write_immutable_sqlite_cache_seal,
)
from krw_capability_runtime.agent_index.spine_schema import read_spine_verification_sha256


ROUTER_COHERENCE_SCHEMA_VERSION = "krw-ontology-router-coherence/v1"
ROUTER_COHERENCE_BUILDER_VERSION = "router-coherence-builder/v1"
ROUTER_COHERENCE_PROFILE_FILENAME = "router_coherence_v1.json"
ROUTER_COHERENCE_RELATIVE_PATH = Path("indexes") / "router_coherence.sqlite"
ROUTER_COHERENCE_SEMANTIC_CACHE_VERSION = "router-coherence-semantic-cache/v1"

_SCHEMA_SQL = """
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

CREATE TABLE IF NOT EXISTS coherence_ticker (
    ticker_id INTEGER PRIMARY KEY,
    ticker TEXT NOT NULL UNIQUE
);

CREATE TABLE IF NOT EXISTS coherence_unit (
    unit_id INTEGER PRIMARY KEY,
    ticker_id INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_coherence_unit_ticker
    ON coherence_unit(ticker_id, unit_id);

CREATE VIRTUAL TABLE IF NOT EXISTS coherence_fts USING fts5(
    text,
    content = '',
    detail = none,
    columnsize = 0,
    tokenize = 'unicode61 remove_diacritics 2'
);

CREATE VIRTUAL TABLE IF NOT EXISTS coherence_vocab
    USING fts5vocab(coherence_fts, 'row');

CREATE TABLE IF NOT EXISTS coherence_term_stats (
    term_norm TEXT PRIMARY KEY,
    document_df INTEGER NOT NULL CHECK(document_df > 0)
) WITHOUT ROWID;
"""

ROUTER_COHERENCE_TABLES = (
    "metadata",
    "ranking_profile",
    "coherence_ticker",
    "coherence_unit",
    "coherence_fts",
    "coherence_vocab",
    "coherence_term_stats",
)
ROUTER_COHERENCE_REQUIRED_METADATA_KEYS = (
    "schema_version",
    "builder_version",
    "release_id",
    "source_global_spine_sha256",
    "profile_id",
    "profile_sha256",
    "schema_sql_sha256",
    "semantic_cache_key",
    "build_fingerprint_sha256",
    "counts",
)


@dataclass(frozen=True)
class RouterCoherenceBuildResult:
    path: Path
    counts: Mapping[str, int]
    metadata: Mapping[str, Any]
    verification: Mapping[str, Any]
    elapsed_ms: int


def load_router_coherence_profile(path: Path | str | None = None) -> dict[str, Any]:
    profile_path = (
        Path(path).expanduser().resolve()
        if path is not None
        else Path(__file__).with_name(ROUTER_COHERENCE_PROFILE_FILENAME)
    )
    try:
        profile = json.loads(profile_path.read_text(encoding="utf-8"))
    except FileNotFoundError as exc:
        raise FileNotFoundError(f"Router coherence profile not found: {profile_path}") from exc
    except json.JSONDecodeError as exc:
        raise ValueError(f"Invalid Router coherence profile JSON: {profile_path}") from exc
    if not isinstance(profile, dict):
        raise ValueError("router coherence profile must be a JSON object")
    _validate_profile(profile)
    return profile


def router_coherence_semantic_cache_key(
    *,
    source_global_spine_sha256: str,
    profile: Mapping[str, Any],
) -> str:
    payload = {
        "builder_version": ROUTER_COHERENCE_BUILDER_VERSION,
        "cache_version": ROUTER_COHERENCE_SEMANTIC_CACHE_VERSION,
        "profile": dict(profile),
        "schema_sql_sha256": hashlib.sha256(_SCHEMA_SQL.encode("utf-8")).hexdigest(),
        "schema_version": ROUTER_COHERENCE_SCHEMA_VERSION,
        "source_global_spine_sha256": source_global_spine_sha256,
    }
    return _json_sha256(payload)


def build_router_coherence(
    global_spine_path: Path | str,
    output_path: Path | str | None = None,
    *,
    release_id: str | None = None,
    profile_path: Path | str | None = None,
) -> RouterCoherenceBuildResult:
    """Build one deterministic contentless FTS derivative from Global Spine."""
    started_at = time.perf_counter()
    source_path = Path(global_spine_path).expanduser().resolve()
    if not source_path.is_file():
        raise FileNotFoundError(f"Global spine not found: {source_path}")
    target_path = (
        Path(output_path).expanduser().resolve()
        if output_path is not None
        else source_path.parent / ROUTER_COHERENCE_RELATIVE_PATH.name
    )
    profile = load_router_coherence_profile(profile_path)
    profile_sha256 = _json_sha256(profile)
    schema_sha256 = hashlib.sha256(_SCHEMA_SQL.encode("utf-8")).hexdigest()
    source_sha256 = read_spine_verification_sha256(source_path) or _file_sha256(source_path)
    semantic_cache_key = router_coherence_semantic_cache_key(
        source_global_spine_sha256=source_sha256,
        profile=profile,
    )
    object_types = tuple(str(value) for value in profile["build"]["object_types"])
    placeholders = ", ".join("?" for _ in object_types)
    source_uri = f"file:{quote(source_path.as_posix(), safe='/')}?mode=ro&immutable=1"
    target_path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = target_path.parent / f".{target_path.name}.{os.getpid()}.{time.time_ns()}.tmp"
    _cleanup_sqlite_files(tmp_path)
    metadata: dict[str, Any] = {}
    counts: dict[str, int] = {}
    resolved_release_id = release_id
    try:
        with sqlite3.connect(tmp_path, uri=True) as conn:
            conn.row_factory = sqlite3.Row
            conn.execute("PRAGMA page_size=32768")
            conn.execute("PRAGMA journal_mode=OFF")
            conn.execute("PRAGMA synchronous=OFF")
            conn.execute("PRAGMA temp_store=FILE")
            conn.execute("PRAGMA cache_size=-1048576")
            conn.execute("ATTACH DATABASE ? AS source", (source_uri,))
            conn.executescript(_SCHEMA_SQL)
            source_metadata = _read_metadata(conn, schema="source")
            resolved_release_id = resolved_release_id or source_metadata.get("release_id")
            conn.execute(
                f"""
                INSERT INTO coherence_ticker(ticker)
                SELECT DISTINCT ticker
                FROM source.global_object_locator
                WHERE object_type IN ({placeholders})
                  AND (COALESCE(compact_label, '') != ''
                       OR COALESCE(compact_summary, '') != '')
                ORDER BY ticker
                """,
                object_types,
            )
            conn.execute(
                f"""
                INSERT INTO coherence_unit(unit_id, ticker_id)
                SELECT locator.rowid, ticker.ticker_id
                FROM source.global_object_locator AS locator
                JOIN coherence_ticker AS ticker USING(ticker)
                WHERE locator.object_type IN ({placeholders})
                  AND (COALESCE(locator.compact_label, '') != ''
                       OR COALESCE(locator.compact_summary, '') != '')
                ORDER BY locator.rowid
                """,
                object_types,
            )
            conn.execute(
                f"""
                INSERT INTO coherence_fts(rowid, text)
                SELECT rowid,
                       COALESCE(compact_label, '') || ' ' || COALESCE(compact_summary, '')
                FROM source.global_object_locator
                WHERE object_type IN ({placeholders})
                  AND (COALESCE(compact_label, '') != ''
                       OR COALESCE(compact_summary, '') != '')
                ORDER BY rowid
                """,
                object_types,
            )
            conn.execute("INSERT INTO coherence_fts(coherence_fts) VALUES('optimize')")
            conn.execute(
                """
                INSERT INTO coherence_term_stats(term_norm, document_df)
                SELECT term, doc
                FROM coherence_vocab
                ORDER BY term
                """
            )
            counts = _counts(conn)
            build_fingerprint = _json_sha256(
                {
                    "builder_version": ROUTER_COHERENCE_BUILDER_VERSION,
                    "counts": counts,
                    "profile_sha256": profile_sha256,
                    "release_id": resolved_release_id,
                    "schema_sql_sha256": schema_sha256,
                    "schema_version": ROUTER_COHERENCE_SCHEMA_VERSION,
                    "semantic_cache_key": semantic_cache_key,
                    "source_global_spine_sha256": source_sha256,
                }
            )
            metadata = {
                "schema_version": ROUTER_COHERENCE_SCHEMA_VERSION,
                "builder_version": ROUTER_COHERENCE_BUILDER_VERSION,
                "release_id": resolved_release_id,
                "source_global_spine_sha256": source_sha256,
                "source_global_spine_schema_version": source_metadata.get("schema_version"),
                "source_manifest_hash": source_metadata.get("source_manifest_hash"),
                "profile_id": profile["profile_id"],
                "profile_sha256": profile_sha256,
                "schema_sql_sha256": schema_sha256,
                "semantic_cache_key": semantic_cache_key,
                "build_fingerprint_sha256": build_fingerprint,
                "counts": counts,
            }
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
            _write_metadata(conn, metadata)
            conn.commit()
            conn.execute("ANALYZE main")
            conn.commit()
            conn.execute("DETACH DATABASE source")
        verification = verify_router_coherence(
            tmp_path,
            expected_global_spine_sha256=source_sha256,
            expected_release_id=str(resolved_release_id) if resolved_release_id else None,
            expected_profile_sha256=profile_sha256,
            deep=True,
        )
        if not verification["ok"]:
            raise RuntimeError(
                "router coherence failed verification: "
                + ", ".join(str(error) for error in verification["errors"])
            )
        remove_immutable_sqlite_cache_seal(target_path)
        os.replace(tmp_path, target_path)
        write_immutable_sqlite_cache_seal(
            target_path,
            kind="router_coherence",
            cache_key=semantic_cache_key,
            verification=verification,
            metadata=metadata,
            counts=counts,
            source_path=source_path,
        )
    finally:
        _cleanup_sqlite_files(tmp_path)
    return RouterCoherenceBuildResult(
        path=target_path,
        counts=counts,
        metadata=metadata,
        verification={**verification, "path": str(target_path)},
        elapsed_ms=max(0, int((time.perf_counter() - started_at) * 1000)),
    )


def verify_router_coherence(
    path: Path | str,
    *,
    expected_global_spine_sha256: str | None = None,
    expected_release_id: str | None = None,
    expected_profile_sha256: str | None = None,
    deep: bool = True,
    require_trusted_seal: bool = False,
) -> dict[str, Any]:
    """Verify the coherence schema and source/profile/release bindings."""
    resolved = Path(path).expanduser().resolve()
    errors: list[str] = []
    metadata: dict[str, Any] = {}
    counts: dict[str, int] = {}
    tables: list[str] = []
    seal, seal_status = read_immutable_sqlite_cache_seal(
        resolved,
        kind="router_coherence",
    )
    if not resolved.is_file():
        return {
            "ok": False,
            "errors": ["router_coherence_missing"],
            "path": str(resolved),
            "metadata": metadata,
            "counts": counts,
            "tables": tables,
            "seal_status": seal_status,
        }
    seal_trusted = seal_status == "valid"
    integrity_check: str | None = None
    integrity_source = "none"
    if deep and seal_trusted:
        raw_metadata = seal.get("metadata")
        raw_counts = seal.get("counts")
        metadata = dict(raw_metadata) if isinstance(raw_metadata, Mapping) else {}
        counts = _integer_mapping(raw_counts)
        tables = list(ROUTER_COHERENCE_TABLES)
        integrity_check = "ok"
        integrity_source = "immutable_cache_seal"
    else:
        if require_trusted_seal and not seal_trusted:
            errors.append(f"router_coherence_verification_seal_required:{seal_status}")
        try:
            with _connect_immutable_readonly(resolved) as conn:
                conn.row_factory = sqlite3.Row
                if deep and not require_trusted_seal:
                    integrity = conn.execute("PRAGMA integrity_check").fetchall()
                    integrity_check = (
                        "ok" if [tuple(row) for row in integrity] == [("ok",)] else repr(integrity)
                    )
                    integrity_source = "sqlite_integrity_check"
                    if integrity_check != "ok":
                        errors.append(f"sqlite_integrity_check_failed:{integrity!r}")
                tables = sorted(
                    str(row[0])
                    for row in conn.execute(
                        "SELECT name FROM sqlite_master WHERE type IN ('table', 'view')"
                    )
                )
                metadata = _read_metadata(conn)
                metadata_counts = metadata.get("counts")
                counts = _counts(conn) if deep else _integer_mapping(metadata_counts)
                profile_rows = conn.execute(
                    "SELECT * FROM ranking_profile WHERE active = 1 ORDER BY profile_id"
                ).fetchall()
                if len(profile_rows) != 1:
                    errors.append("router_coherence_active_profile_count_mismatch")
                else:
                    row = profile_rows[0]
                    try:
                        profile = json.loads(str(row["config_json"]))
                        _validate_profile(profile)
                    except (json.JSONDecodeError, ValueError) as exc:
                        errors.append(f"router_coherence_profile_invalid:{exc}")
                    else:
                        actual_profile_sha = _json_sha256(profile)
                        if actual_profile_sha != str(row["config_sha256"]):
                            errors.append("router_coherence_profile_hash_mismatch")
                        if actual_profile_sha != metadata.get("profile_sha256"):
                            errors.append("router_coherence_metadata_profile_hash_mismatch")
                if deep:
                    invalid_terms = int(
                        conn.execute(
                            "SELECT COUNT(*) FROM coherence_term_stats WHERE document_df <= 0"
                        ).fetchone()[0]
                    )
                    if invalid_terms:
                        errors.append(f"router_coherence_invalid_term_stats:{invalid_terms}")
        except sqlite3.Error as exc:
            errors.append(f"router_coherence_sqlite_error:{exc}")

    table_set = set(tables)
    for table in ROUTER_COHERENCE_TABLES:
        if table not in table_set:
            errors.append(f"router_coherence_table_missing:{table}")
    for key in ROUTER_COHERENCE_REQUIRED_METADATA_KEYS:
        if key not in metadata:
            errors.append(f"router_coherence_metadata_missing:{key}")
    if metadata.get("schema_version") != ROUTER_COHERENCE_SCHEMA_VERSION:
        errors.append("router_coherence_schema_version_mismatch")
    if metadata.get("builder_version") != ROUTER_COHERENCE_BUILDER_VERSION:
        errors.append("router_coherence_builder_version_mismatch")
    if expected_global_spine_sha256 is not None and (
        metadata.get("source_global_spine_sha256") != expected_global_spine_sha256
    ):
        errors.append("router_coherence_source_global_spine_hash_mismatch")
    if expected_release_id is not None and metadata.get("release_id") != expected_release_id:
        errors.append("router_coherence_release_id_mismatch")
    if expected_profile_sha256 is not None and metadata.get("profile_sha256") != (
        expected_profile_sha256
    ):
        errors.append("router_coherence_manifest_profile_hash_mismatch")
    expected_fingerprint = _json_sha256(
        {
            "builder_version": metadata.get("builder_version"),
            "counts": counts,
            "profile_sha256": metadata.get("profile_sha256"),
            "release_id": metadata.get("release_id"),
            "schema_sql_sha256": metadata.get("schema_sql_sha256"),
            "schema_version": metadata.get("schema_version"),
            "semantic_cache_key": metadata.get("semantic_cache_key"),
            "source_global_spine_sha256": metadata.get("source_global_spine_sha256"),
        }
    )
    if metadata.get("build_fingerprint_sha256") != expected_fingerprint:
        errors.append("router_coherence_build_fingerprint_mismatch")
    if deep and isinstance(metadata.get("counts"), Mapping) and dict(metadata["counts"]) != counts:
        errors.append("router_coherence_counts_mismatch")
    return {
        "ok": not errors,
        "errors": list(dict.fromkeys(errors)),
        "path": str(resolved),
        "metadata": metadata,
        "counts": counts,
        "tables": tables,
        "deep": deep,
        "integrity_check": integrity_check,
        "integrity_source": integrity_source,
        "seal_status": seal_status,
        "seal_trusted": seal_trusted,
        "verification_mode": (
            "router-coherence-deep-sealed"
            if deep and seal_trusted
            else "router-coherence-deep"
            if deep
            else "router-coherence-light"
        ),
    }


def rebind_router_coherence_release(
    path: Path | str,
    *,
    release_id: str,
    expected_global_spine_sha256: str | None = None,
    expected_previous_release_id: str | None = None,
    trusted_source_path: Path | str,
) -> dict[str, Any]:
    """Rebind a trusted coherence copy to a materialized release ID."""
    resolved = Path(path).expanduser().resolve()
    trusted_source = Path(trusted_source_path).expanduser().resolve()
    normalized_release_id = str(release_id).strip()
    if not normalized_release_id:
        raise ValueError("router coherence release_id is required")
    _source_seal, source_seal_status = read_immutable_sqlite_cache_seal(
        trusted_source,
        kind="router_coherence",
    )
    if source_seal_status != "valid":
        raise ValueError(
            "cannot inherit router coherence verification from an untrusted source: "
            f"{source_seal_status}"
        )
    source_verification = verify_router_coherence(
        trusted_source,
        expected_global_spine_sha256=expected_global_spine_sha256,
        expected_release_id=expected_previous_release_id,
        deep=True,
        require_trusted_seal=True,
    )
    if not source_verification.get("ok"):
        raise ValueError(
            "cannot inherit invalid router coherence verification: "
            + ", ".join(source_verification.get("errors") or [])
        )
    assert_trusted_immutable_copy(
        trusted_source,
        resolved,
        cached_source_sha256=read_immutable_sqlite_cache_sha256(trusted_source),
        role="router coherence index",
    )

    verification = verify_router_coherence(
        resolved,
        expected_global_spine_sha256=expected_global_spine_sha256,
        deep=False,
    )
    if not verification.get("ok"):
        raise ValueError(
            "cannot rebind invalid router coherence index: "
            + ", ".join(verification.get("errors") or [])
        )
    metadata = dict(verification.get("metadata") or {})
    if (
        expected_previous_release_id is not None
        and metadata.get("release_id") != expected_previous_release_id
    ):
        raise ValueError("router coherence previous release_id mismatch")
    metadata["release_id"] = normalized_release_id
    metadata["build_fingerprint_sha256"] = _json_sha256(
        {
            "builder_version": metadata.get("builder_version"),
            "counts": metadata.get("counts"),
            "profile_sha256": metadata.get("profile_sha256"),
            "release_id": normalized_release_id,
            "schema_sql_sha256": metadata.get("schema_sql_sha256"),
            "schema_version": metadata.get("schema_version"),
            "semantic_cache_key": metadata.get("semantic_cache_key"),
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
    rebound = verify_router_coherence(
        resolved,
        expected_global_spine_sha256=expected_global_spine_sha256,
        expected_release_id=normalized_release_id,
        deep=False,
    )
    if not rebound.get("ok"):
        raise ValueError(
            "rebound router coherence index is invalid: " + ", ".join(rebound.get("errors") or [])
        )
    inherited = {
        **rebound,
        "integrity_check": "ok",
        "integrity_source": "inherited_immutable_cache_seal",
        "verification_mode": "router-coherence-deep-sealed-inherited-rebind",
    }
    write_immutable_sqlite_cache_seal(
        resolved,
        kind="router_coherence",
        cache_key=str(metadata.get("semantic_cache_key") or ""),
        verification=inherited,
        metadata=rebound.get("metadata") or {},
        counts=rebound.get("counts") or {},
        source_path=trusted_source,
        details={"inheritance": "controlled-release-id-rebind"},
    )
    return verify_router_coherence(
        resolved,
        expected_global_spine_sha256=expected_global_spine_sha256,
        expected_release_id=normalized_release_id,
        deep=True,
        require_trusted_seal=True,
    )


def rebind_router_coherence_source(
    path: Path | str,
    *,
    global_spine_path: Path | str,
    release_id: str,
    expected_previous_global_spine_sha256: str | None = None,
    expected_previous_release_id: str | None = None,
) -> dict[str, Any]:
    """Rebind a trusted coherence copy to a byte-distinct global spine.

    Release materialization re-seals the global spine after rebasing its
    release-bound paths.  The semantic coherence rows remain identical, but
    the global file hash necessarily changes.  The ordinary release-id rebind
    deliberately rejects that situation, so this boundary verifies the old
    binding first and then writes the new source hash atomically.
    """
    resolved = Path(path).expanduser().resolve()
    source_path = Path(global_spine_path).expanduser().resolve()
    normalized_release_id = str(release_id).strip()
    if not normalized_release_id:
        raise ValueError("router coherence release_id is required")
    if not source_path.is_file():
        raise FileNotFoundError(f"Global spine not found: {source_path}")

    source_seal, source_seal_status = read_immutable_sqlite_cache_seal(
        resolved,
        kind="router_coherence",
    )
    if source_seal_status != "valid":
        raise ValueError(
            "cannot rebind router coherence from an untrusted candidate: "
            f"{source_seal_status}"
        )
    previous = verify_router_coherence(
        resolved,
        expected_global_spine_sha256=expected_previous_global_spine_sha256,
        expected_release_id=expected_previous_release_id,
        deep=True,
        require_trusted_seal=True,
    )
    if not previous.get("ok"):
        raise ValueError(
            "cannot rebind invalid router coherence: "
            + ", ".join(previous.get("errors") or [])
        )

    with _connect_immutable_readonly(source_path) as source:
        source_metadata = _read_metadata(source)
    source_sha256 = _file_sha256(source_path)
    metadata = dict(previous.get("metadata") or {})
    metadata.update(
        {
            "release_id": normalized_release_id,
            "source_global_spine_sha256": source_sha256,
            "source_global_spine_schema_version": source_metadata.get("schema_version"),
            "source_manifest_hash": source_metadata.get("source_manifest_hash"),
            "source_created_at": source_metadata.get("created_at"),
        }
    )
    metadata["build_fingerprint_sha256"] = _json_sha256(
        {
            "builder_version": metadata.get("builder_version"),
            "counts": metadata.get("counts"),
            "profile_sha256": metadata.get("profile_sha256"),
            "release_id": normalized_release_id,
            "schema_sql_sha256": metadata.get("schema_sql_sha256"),
            "schema_version": metadata.get("schema_version"),
            "semantic_cache_key": metadata.get("semantic_cache_key"),
            "source_global_spine_sha256": source_sha256,
        }
    )
    with sqlite3.connect(resolved) as conn:
        _write_metadata(conn, metadata)
        conn.commit()
    remove_immutable_sqlite_cache_seal(resolved)

    rebound = verify_router_coherence(
        resolved,
        expected_global_spine_sha256=source_sha256,
        expected_release_id=normalized_release_id,
        deep=False,
    )
    if not rebound.get("ok"):
        raise ValueError(
            "rebound router coherence is invalid: "
            + ", ".join(rebound.get("errors") or [])
        )
    inherited = {
        **rebound,
        "integrity_check": "ok",
        "integrity_source": "inherited_immutable_cache_seal",
        "verification_mode": "router-coherence-deep-sealed-source-rebind",
    }
    write_immutable_sqlite_cache_seal(
        resolved,
        kind="router_coherence",
        cache_key=str(metadata.get("semantic_cache_key") or ""),
        verification=inherited,
        metadata=rebound.get("metadata") or {},
        counts=rebound.get("counts") or {},
        source_path=resolved,
        details={"inheritance": "controlled-global-spine-source-rebind"},
    )
    return verify_router_coherence(
        resolved,
        expected_global_spine_sha256=source_sha256,
        expected_release_id=normalized_release_id,
        deep=True,
        require_trusted_seal=True,
    )


class RouterCoherence:
    """Read-only source-coherent candidate scorer."""

    def __init__(self, path: Path | str) -> None:
        self.path = Path(path).expanduser().resolve()
        if not self.path.is_file():
            raise FileNotFoundError(f"Router coherence index not found: {self.path}")
        self._conn = _connect_immutable_readonly(self.path)
        self._conn.row_factory = sqlite3.Row
        self._conn.execute("PRAGMA query_only=ON")
        self._metadata = _read_metadata(self._conn)
        row = self._conn.execute(
            "SELECT config_json FROM ranking_profile WHERE active = 1 ORDER BY profile_id LIMIT 1"
        ).fetchone()
        if row is None:
            self._conn.close()
            raise ValueError("Router coherence active ranking profile is missing")
        profile = json.loads(str(row["config_json"]))
        _validate_profile(profile)
        self._profile = profile

    def __enter__(self) -> RouterCoherence:
        return self

    def __exit__(self, exc_type: Any, exc: Any, traceback: Any) -> None:
        self.close()

    @property
    def metadata(self) -> Mapping[str, Any]:
        return dict(self._metadata)

    @property
    def profile(self) -> Mapping[str, Any]:
        return dict(self._profile)

    def close(self) -> None:
        self._conn.close()

    def search_terms(
        self,
        terms: Sequence[str],
        *,
        tickers: Sequence[str] = (),
    ) -> dict[str, Any]:
        config = self._profile["query"]
        unique_terms = list(dict.fromkeys(str(term).casefold() for term in terms if str(term)))
        if len(unique_terms) < int(config["minimum_query_terms"]):
            return self._empty_result("insufficient_query_terms")
        placeholders = ", ".join("?" for _ in unique_terms)
        rows = self._conn.execute(
            f"""
            SELECT term_norm AS term, document_df AS doc
            FROM coherence_term_stats
            WHERE term_norm IN ({placeholders})
            """,
            unique_terms,
        ).fetchall()
        total_units = max(1, int((self._metadata.get("counts") or {}).get("coherence_unit") or 0))
        stats = {
            str(row["term"]): {
                "document_df": int(row["doc"]),
                "information": math.log((total_units + 1) / (int(row["doc"]) + 1)),
            }
            for row in rows
        }
        usable_terms = [
            term
            for term in unique_terms
            if term in stats
            and float(stats[term]["information"]) >= float(config["minimum_information"])
        ]
        if len(usable_terms) < int(config["minimum_query_terms"]):
            return self._empty_result("insufficient_informative_terms")
        pair_specs = sorted(
            combinations(usable_terms, 2),
            key=lambda pair: (
                -float(stats[pair[0]]["information"]) - float(stats[pair[1]]["information"]),
                pair,
            ),
        )[: int(config["max_pair_queries"])]
        scope = tuple(sorted({str(ticker).strip().upper() for ticker in tickers if str(ticker)}))
        result_limit = int(config["max_pair_result_rows"])
        term_bits = {term: 1 << index for index, term in enumerate(usable_terms)}
        document_masks: dict[int, int] = {}
        document_tickers: dict[int, str] = {}
        truncated_pairs = 0
        for left, right in pair_specs:
            pair_mask = term_bits[left] | term_bits[right]
            pair_rows = self._match_rows(
                (left, right),
                tickers=scope,
                limit=result_limit,
            )
            if len(pair_rows) > result_limit and bool(config["discard_truncated_pairs"]):
                truncated_pairs += 1
                continue
            for row in pair_rows[:result_limit]:
                unit_id = int(row["unit_id"])
                document_masks[unit_id] = document_masks.get(unit_id, 0) | pair_mask
                document_tickers[unit_id] = str(row["ticker"])
        if not document_masks:
            return self._empty_result(
                "no_source_coherent_match",
                pair_query_count=len(pair_specs),
                truncated_pair_count=truncated_pairs,
            )
        total_information = sum(float(stats[term]["information"]) for term in usable_terms) or 1.0
        coverage_weight = float(config["coverage_weight"])
        information_weight = float(config["information_coverage_weight"])
        weight_total = coverage_weight + information_weight or 1.0
        information_by_bit = [float(stats[term]["information"]) for term in usable_terms]
        mask_information_cache: dict[int, float] = {}
        best_by_ticker: dict[str, tuple[float, int, int, float, float]] = {}
        for unit_id, mask in document_masks.items():
            coverage = mask.bit_count() / len(usable_terms)
            matched_information = mask_information_cache.get(mask)
            if matched_information is None:
                matched_information = sum(
                    information
                    for index, information in enumerate(information_by_bit)
                    if mask & (1 << index)
                )
                mask_information_cache[mask] = matched_information
            information_coverage = matched_information / total_information
            score = (
                coverage_weight * coverage + information_weight * information_coverage
            ) / weight_total
            ticker = document_tickers[unit_id]
            current = best_by_ticker.get(ticker)
            candidate = (score, unit_id, mask, coverage, information_coverage)
            if current is None or (-score, unit_id) < (-current[0], current[1]):
                best_by_ticker[ticker] = candidate
        candidates = [
            {
                "ticker": ticker,
                "coherence_score": value[0],
                "source_locator_rowid": value[1],
                "matched_terms": [
                    term for index, term in enumerate(usable_terms) if value[2] & (1 << index)
                ],
                "term_coverage": value[3],
                "information_coverage": value[4],
            }
            for ticker, value in best_by_ticker.items()
        ]
        candidates = sorted(
            candidates,
            key=lambda row: (
                -float(row["coherence_score"]),
                str(row["ticker"]),
                int(row["source_locator_rowid"]),
            ),
        )
        return {
            "applied": True,
            "reason": "source_coherent_term_pairs",
            "candidates": candidates,
            "usable_terms": usable_terms,
            "pair_query_count": len(pair_specs),
            "truncated_pair_count": truncated_pairs,
            "matched_document_count": len(document_masks),
            "matched_ticker_count": len(candidates),
        }

    def _match_rows(
        self,
        terms: Sequence[str],
        *,
        tickers: Sequence[str],
        limit: int,
    ) -> list[sqlite3.Row]:
        params: list[Any] = [" AND ".join(_fts_quote(term) for term in terms)]
        ticker_clause = ""
        if tickers:
            ticker_placeholders = ", ".join("?" for _ in tickers)
            ticker_clause = f" AND ticker.ticker IN ({ticker_placeholders})"
            params.extend(tickers)
        params.append(limit + 1)
        return self._conn.execute(
            f"""
            SELECT unit.unit_id, ticker.ticker
            FROM coherence_fts
            JOIN coherence_unit AS unit ON unit.unit_id = coherence_fts.rowid
            JOIN coherence_ticker AS ticker ON ticker.ticker_id = unit.ticker_id
            WHERE coherence_fts MATCH ?
            {ticker_clause}
            LIMIT ?
            """,
            params,
        ).fetchall()

    @staticmethod
    def _empty_result(
        reason: str,
        *,
        pair_query_count: int = 0,
        truncated_pair_count: int = 0,
    ) -> dict[str, Any]:
        return {
            "applied": False,
            "reason": reason,
            "candidates": [],
            "usable_terms": [],
            "pair_query_count": pair_query_count,
            "truncated_pair_count": truncated_pair_count,
            "matched_document_count": 0,
            "matched_ticker_count": 0,
        }


def _validate_profile(profile: Mapping[str, Any]) -> None:
    if profile.get("format") != "krw-ontology-router-coherence-profile/v1":
        raise ValueError("router coherence profile format is unsupported")
    if not str(profile.get("profile_id") or "").strip():
        raise ValueError("router coherence profile_id is required")
    version = profile.get("version")
    if not isinstance(version, int) or isinstance(version, bool) or version <= 0:
        raise ValueError("router coherence profile version must be a positive integer")
    build = profile.get("build")
    if not isinstance(build, Mapping):
        raise ValueError("router coherence profile build must be an object")
    object_types = build.get("object_types")
    if (
        not isinstance(object_types, Sequence)
        or isinstance(object_types, (str, bytes, bytearray))
        or not object_types
        or len(object_types) != len(set(object_types))
        or any(not str(value).strip() for value in object_types)
    ):
        raise ValueError("router coherence build.object_types must be unique non-empty strings")
    query = profile.get("query")
    if not isinstance(query, Mapping):
        raise ValueError("router coherence profile query must be an object")
    for key in (
        "candidate_ticker_multiplier",
        "max_pair_queries",
        "max_pair_result_rows",
        "minimum_query_terms",
    ):
        value = query.get(key)
        if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
            raise ValueError(f"router coherence query.{key} must be a positive integer")
    for key in (
        "coarse_weight",
        "coherence_weight",
        "coverage_weight",
        "information_coverage_weight",
        "minimum_information",
    ):
        value = query.get(key)
        if (
            not isinstance(value, (int, float))
            or isinstance(value, bool)
            or not math.isfinite(float(value))
            or float(value) < 0
        ):
            raise ValueError(f"router coherence query.{key} must be non-negative and finite")
    if not bool(float(query["coarse_weight"]) + float(query["coherence_weight"])):
        raise ValueError("router coherence fusion weights cannot both be zero")
    if not bool(float(query["coverage_weight"]) + float(query["information_coverage_weight"])):
        raise ValueError("router coherence scoring weights cannot both be zero")
    if not isinstance(query.get("discard_truncated_pairs"), bool):
        raise ValueError("router coherence query.discard_truncated_pairs must be boolean")


def _connect_immutable_readonly(path: Path) -> sqlite3.Connection:
    uri = f"file:{quote(path.resolve().as_posix(), safe='/')}?mode=ro&immutable=1"
    return sqlite3.connect(uri, uri=True, check_same_thread=False)


def _read_metadata(conn: sqlite3.Connection, *, schema: str = "main") -> dict[str, Any]:
    try:
        rows = conn.execute(f"SELECT key, value_json FROM {schema}.metadata").fetchall()
    except sqlite3.Error:
        return {}
    metadata: dict[str, Any] = {}
    for key, value_json in rows:
        try:
            metadata[str(key)] = json.loads(str(value_json))
        except json.JSONDecodeError:
            metadata[str(key)] = str(value_json)
    return metadata


def _write_metadata(conn: sqlite3.Connection, metadata: Mapping[str, Any]) -> None:
    conn.executemany(
        "INSERT OR REPLACE INTO metadata(key, value_json) VALUES (?, ?)",
        ((key, _json_dumps(value)) for key, value in sorted(metadata.items())),
    )


def _counts(conn: sqlite3.Connection) -> dict[str, int]:
    return {
        "coherence_ticker": int(
            conn.execute("SELECT COUNT(*) FROM coherence_ticker").fetchone()[0]
        ),
        "coherence_unit": int(conn.execute("SELECT COUNT(*) FROM coherence_unit").fetchone()[0]),
        "coherence_fts": int(conn.execute("SELECT COUNT(*) FROM coherence_unit").fetchone()[0]),
        "coherence_term_stats": int(
            conn.execute("SELECT COUNT(*) FROM coherence_term_stats").fetchone()[0]
        ),
    }


def _integer_mapping(value: Any) -> dict[str, int]:
    if not isinstance(value, Mapping):
        return {}
    return {
        str(key): int(item)
        for key, item in value.items()
        if isinstance(item, int) and not isinstance(item, bool)
    }


def _fts_quote(term: str) -> str:
    return '"' + term.replace('"', '""') + '"'


def _json_dumps(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def _json_sha256(value: Any) -> str:
    return hashlib.sha256(_json_dumps(value).encode("utf-8")).hexdigest()


def _file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _cleanup_sqlite_files(path: Path) -> None:
    for candidate in (
        path,
        Path(f"{path}-wal"),
        Path(f"{path}-shm"),
        Path(f"{path}-journal"),
    ):
        candidate.unlink(missing_ok=True)
