"""Schema primitives for the v3 global-spine-and-company-shards index."""

from __future__ import annotations

import hashlib
import json
import os
import re
import sqlite3
import time
from collections.abc import Mapping
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

GLOBAL_SPINE_SCHEMA_VERSION = "krw-ontology-global-spine/v4"
GLOBAL_SPINE_BUILDER_VERSION = "global-spine-builder/v5"
GLOBAL_SPINE_AGENT_GRAPH_INDEX_VERSION = "agent-graph-index/v1"
GLOBAL_SPINE_REPLICA_INVARIANT_VERSION = "global-spine-replica-invariants/v1"
SPINE_FRAGMENT_SCHEMA_VERSION = "krw-ontology-spine-fragment-schema/v2"
GLOBAL_SPINE_RELATIVE_PATH = Path("indexes") / "global_spine.sqlite"
GLOBAL_SPINE_LAYOUT = "global-spine-and-company-shards"
GLOBAL_SPINE_ROUTING_MODE = "planned-router-sidecar-only"
SPINE_VERIFICATION_SEAL_FORMAT = "krw-ontology-spine-verification-seal/v1"
SPINE_VERIFICATION_SEAL_SUFFIX = ".verify.json"
SPINE_FORBIDDEN_TABLE_PREFIXES = ("global_search_fts",)
_SHA256_RE = re.compile(r"[0-9a-f]{64}")

SPINE_FRAGMENT_TABLES = (
    "metadata",
    "global_object_locator",
    "global_document_catalog",
    "global_edge_spine",
    "global_factor_spine",
    "global_topic_spine",
    "global_metric_spine",
    "global_entity_spine",
    "global_counterparty_spine",
)

GLOBAL_SPINE_DERIVED_TABLES = (
    "global_object_replica",
    "global_edge_replica",
    "global_chain_index",
    "global_key_stats",
)

GLOBAL_SPINE_TABLES = (*SPINE_FRAGMENT_TABLES, *GLOBAL_SPINE_DERIVED_TABLES)

GLOBAL_SPINE_REQUIRED_METADATA_KEYS = (
    "schema_version",
    "builder_version",
    "index_layout",
    "routing_mode",
    "created_at",
)


def _connect_immutable_readonly(path: Path) -> sqlite3.Connection:
    return sqlite3.connect(path.resolve().as_uri() + "?mode=ro&immutable=1", uri=True)


SPINE_FRAGMENT_REQUIRED_METADATA_KEYS = (
    *GLOBAL_SPINE_REQUIRED_METADATA_KEYS,
    "fragment_schema_version",
)


_SPINE_COMMON_SCHEMA_SQL = """
    PRAGMA foreign_keys = ON;

    CREATE TABLE IF NOT EXISTS metadata (
        key TEXT PRIMARY KEY,
        value_json TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS global_object_locator (
        object_id TEXT PRIMARY KEY,
        ticker TEXT NOT NULL,
        company_name TEXT,
        document_id TEXT,
        document_type TEXT,
        period TEXT,
        filing_date TEXT,
        object_type TEXT,
        shard_id TEXT NOT NULL,
        shard_path TEXT NOT NULL,
        local_object_key TEXT,
        object_hash TEXT,
        semantic_hash TEXT,
        compact_label TEXT,
        compact_summary TEXT,
        quality_status TEXT,
        occurrence_count INTEGER NOT NULL DEFAULT 1
    );

    CREATE TABLE IF NOT EXISTS global_document_catalog (
        document_id TEXT PRIMARY KEY,
        ticker TEXT NOT NULL,
        company_name TEXT,
        document_type TEXT,
        period TEXT,
        fiscal_year INTEGER,
        fiscal_quarter INTEGER,
        filing_date TEXT,
        source_path TEXT,
        shard_id TEXT NOT NULL,
        shard_path TEXT NOT NULL,
        document_hash TEXT,
        object_count INTEGER NOT NULL DEFAULT 0,
        edge_count INTEGER NOT NULL DEFAULT 0,
        quality_event_count INTEGER NOT NULL DEFAULT 0,
        quality_status TEXT
    );

    CREATE TABLE IF NOT EXISTS global_edge_spine (
        edge_id TEXT PRIMARY KEY,
        ticker TEXT NOT NULL DEFAULT '',
        document_id TEXT,
        document_type TEXT,
        period TEXT,
        shard_id TEXT NOT NULL DEFAULT '',
        shard_path TEXT NOT NULL DEFAULT '',
        from_object_id TEXT NOT NULL,
        to_object_id TEXT NOT NULL,
        from_ticker TEXT,
        to_ticker TEXT,
        relation_type TEXT NOT NULL,
        edge_scope TEXT NOT NULL,
        source_object_type TEXT,
        target_object_type TEXT,
        confidence REAL,
        evidence_grade TEXT,
        materiality REAL,
        recency_score REAL,
        shard_hint TEXT,
        compact_reason TEXT,
        semantic_hash TEXT,
        occurrence_count INTEGER NOT NULL DEFAULT 1
    );

    CREATE TABLE IF NOT EXISTS global_factor_spine (
        factor_key TEXT NOT NULL,
        factor_label TEXT,
        factor_family TEXT,
        benchmark TEXT,
        ticker TEXT NOT NULL,
        object_id TEXT NOT NULL,
        document_id TEXT,
        impact_channel TEXT,
        effect_direction TEXT,
        materiality REAL,
        evidence_grade TEXT,
        shard_id TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS global_topic_spine (
        topic_id TEXT PRIMARY KEY,
        topic_key TEXT NOT NULL,
        topic_label TEXT,
        topic_family TEXT,
        topic_summary TEXT,
        ticker TEXT NOT NULL,
        source_object_ids TEXT,
        factor_terms TEXT,
        metric_terms TEXT,
        entity_terms TEXT,
        mechanism_terms TEXT,
        impact_channels TEXT,
        evidence_grade TEXT,
        materiality REAL,
        shard_id TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS global_metric_spine (
        canonical_metric_key TEXT NOT NULL,
        metric_name TEXT,
        unit TEXT,
        dimensions_hash TEXT,
        ticker TEXT NOT NULL,
        object_id TEXT NOT NULL,
        document_id TEXT,
        period TEXT,
        document_type TEXT,
        value_normalized REAL,
        trend_direction TEXT,
        confidence REAL,
        shard_id TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS global_entity_spine (
        entity_key TEXT NOT NULL,
        entity_type TEXT NOT NULL,
        canonical_name TEXT,
        aliases TEXT,
        ticker_scope TEXT,
        ticker TEXT NOT NULL,
        object_id TEXT NOT NULL,
        document_id TEXT,
        confidence REAL,
        shard_id TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS global_counterparty_spine (
        counterparty_key TEXT NOT NULL,
        counterparty_name TEXT,
        relationship_type TEXT,
        ticker TEXT NOT NULL,
        object_id TEXT NOT NULL,
        document_id TEXT,
        agreement_type TEXT,
        affected_channels TEXT,
        materiality REAL,
        evidence_grade TEXT,
        shard_id TEXT NOT NULL
    );
"""


_GLOBAL_SPINE_DERIVED_SCHEMA_SQL = """
    CREATE TABLE IF NOT EXISTS global_object_replica (
        object_id TEXT NOT NULL,
        ticker TEXT NOT NULL,
        document_id TEXT NOT NULL,
        document_type TEXT NOT NULL,
        period TEXT NOT NULL,
        shard_id TEXT NOT NULL,
        shard_path TEXT NOT NULL,
        object_type TEXT,
        local_object_key TEXT,
        object_hash TEXT,
        semantic_hash TEXT,
        quality_status TEXT,
        PRIMARY KEY (
            object_id, ticker, document_id, document_type, period,
            shard_id, shard_path
        )
    ) WITHOUT ROWID;

    CREATE TABLE IF NOT EXISTS global_edge_replica (
        edge_id TEXT NOT NULL,
        ticker TEXT NOT NULL,
        document_id TEXT NOT NULL,
        document_type TEXT NOT NULL,
        period TEXT NOT NULL,
        shard_id TEXT NOT NULL,
        shard_path TEXT NOT NULL,
        from_object_id TEXT NOT NULL,
        to_object_id TEXT NOT NULL,
        relation_type TEXT NOT NULL,
        semantic_hash TEXT,
        confidence REAL,
        evidence_grade TEXT,
        PRIMARY KEY (
            edge_id, ticker, document_id, document_type, period,
            shard_id, shard_path
        )
    ) WITHOUT ROWID;

    CREATE TABLE IF NOT EXISTS global_chain_index (
        link_id TEXT PRIMARY KEY,
        link_type TEXT NOT NULL,
        from_ticker TEXT NOT NULL,
        to_ticker TEXT NOT NULL,
        shared_key TEXT NOT NULL,
        shared_key_type TEXT NOT NULL,
        from_object_id TEXT,
        to_object_id TEXT,
        weight REAL NOT NULL,
        confidence REAL,
        evidence_grade TEXT,
        materiality REAL,
        generic_penalty REAL,
        recency_score REAL,
        explanation_template TEXT
    );

    CREATE TABLE IF NOT EXISTS global_key_stats (
        key_type TEXT NOT NULL,
        key TEXT NOT NULL,
        ticker_count INTEGER NOT NULL DEFAULT 0,
        object_count INTEGER NOT NULL DEFAULT 0,
        document_count INTEGER NOT NULL DEFAULT 0,
        idf_score REAL,
        generic INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (key_type, key)
    );

"""


_GLOBAL_SPINE_SECONDARY_INDEX_SQL = """
    CREATE INDEX IF NOT EXISTS idx_global_object_locator_ticker
        ON global_object_locator(ticker);
    CREATE INDEX IF NOT EXISTS idx_global_object_locator_type
        ON global_object_locator(object_type);
    CREATE INDEX IF NOT EXISTS idx_global_object_locator_doc
        ON global_object_locator(ticker, document_type, period);

    CREATE INDEX IF NOT EXISTS idx_global_document_catalog_ticker_period
        ON global_document_catalog(ticker, document_type, period);
    CREATE INDEX IF NOT EXISTS idx_global_document_catalog_filing_date
        ON global_document_catalog(filing_date);

    CREATE INDEX IF NOT EXISTS idx_global_edge_spine_from
        ON global_edge_spine(from_object_id);
    CREATE INDEX IF NOT EXISTS idx_global_edge_spine_to
        ON global_edge_spine(to_object_id);
    CREATE INDEX IF NOT EXISTS idx_global_edge_spine_tickers
        ON global_edge_spine(from_ticker, to_ticker);
    CREATE INDEX IF NOT EXISTS idx_global_edge_spine_scope
        ON global_edge_spine(edge_scope, relation_type);
    CREATE INDEX IF NOT EXISTS idx_global_edge_spine_from_relation_to
        ON global_edge_spine(from_object_id, relation_type, to_object_id);
    CREATE INDEX IF NOT EXISTS idx_global_edge_spine_to_relation_from
        ON global_edge_spine(to_object_id, relation_type, from_object_id);

    CREATE INDEX IF NOT EXISTS idx_global_factor_spine_key
        ON global_factor_spine(factor_key);
    CREATE INDEX IF NOT EXISTS idx_global_factor_spine_ticker
        ON global_factor_spine(ticker);
    CREATE INDEX IF NOT EXISTS idx_global_factor_spine_family
        ON global_factor_spine(factor_family);

    CREATE INDEX IF NOT EXISTS idx_global_topic_spine_key
        ON global_topic_spine(topic_key);
    CREATE INDEX IF NOT EXISTS idx_global_topic_spine_ticker
        ON global_topic_spine(ticker);

    CREATE INDEX IF NOT EXISTS idx_global_metric_spine_key
        ON global_metric_spine(canonical_metric_key);
    CREATE INDEX IF NOT EXISTS idx_global_metric_spine_ticker_period
        ON global_metric_spine(ticker, period);

    CREATE INDEX IF NOT EXISTS idx_global_entity_spine_key
        ON global_entity_spine(entity_key);
    CREATE INDEX IF NOT EXISTS idx_global_entity_spine_type
        ON global_entity_spine(entity_type);
    CREATE INDEX IF NOT EXISTS idx_global_entity_spine_ticker
        ON global_entity_spine(ticker);

    CREATE INDEX IF NOT EXISTS idx_global_counterparty_spine_key
        ON global_counterparty_spine(counterparty_key);
    CREATE INDEX IF NOT EXISTS idx_global_counterparty_spine_ticker
        ON global_counterparty_spine(ticker);

    CREATE INDEX IF NOT EXISTS idx_global_object_replica_ticker_doc
        ON global_object_replica(ticker, document_id);
    CREATE UNIQUE INDEX IF NOT EXISTS idx_global_object_replica_ticker_local_key
        ON global_object_replica(ticker, local_object_key);
    CREATE INDEX IF NOT EXISTS idx_global_edge_replica_ticker_doc
        ON global_edge_replica(ticker, document_id);
    CREATE INDEX IF NOT EXISTS idx_global_edge_replica_ticker_from_confidence_to
        ON global_edge_replica(
            ticker, from_object_id, confidence DESC, to_object_id
        );
    CREATE INDEX IF NOT EXISTS idx_global_edge_replica_ticker_to_confidence_from
        ON global_edge_replica(
            ticker, to_object_id, confidence DESC, from_object_id
        );

    CREATE INDEX IF NOT EXISTS idx_global_chain_index_from
        ON global_chain_index(from_ticker);
    CREATE INDEX IF NOT EXISTS idx_global_chain_index_to
        ON global_chain_index(to_ticker);
    CREATE INDEX IF NOT EXISTS idx_global_chain_index_shared_key
        ON global_chain_index(shared_key_type, shared_key);
    CREATE INDEX IF NOT EXISTS idx_global_chain_index_weight
        ON global_chain_index(weight DESC);
    CREATE INDEX IF NOT EXISTS idx_global_chain_index_from_object_weight
        ON global_chain_index(from_object_id, weight DESC, to_object_id);
    CREATE INDEX IF NOT EXISTS idx_global_chain_index_to_object_weight
        ON global_chain_index(to_object_id, weight DESC, from_object_id);
    CREATE INDEX IF NOT EXISTS idx_global_chain_index_from_ticker_weight
        ON global_chain_index(from_ticker, weight DESC, to_ticker);
    CREATE INDEX IF NOT EXISTS idx_global_chain_index_to_ticker_weight
        ON global_chain_index(to_ticker, weight DESC, from_ticker);
    CREATE INDEX IF NOT EXISTS idx_global_chain_index_from_ticker_object_weight
        ON global_chain_index(
            from_ticker, from_object_id, weight DESC, to_ticker, to_object_id
        );
    CREATE INDEX IF NOT EXISTS idx_global_chain_index_to_ticker_object_weight
        ON global_chain_index(
            to_ticker, to_object_id, weight DESC, from_ticker, from_object_id
        );
"""


GLOBAL_SPINE_ALLOWED_SECONDARY_INDEXES = tuple(
    re.findall(
        r"CREATE\s+(?:UNIQUE\s+)?INDEX\s+IF\s+NOT\s+EXISTS\s+([A-Za-z0-9_]+)",
        _GLOBAL_SPINE_SECONDARY_INDEX_SQL,
        flags=re.IGNORECASE,
    )
)


def create_global_spine_schema(
    conn: sqlite3.Connection,
    *,
    include_secondary_indexes: bool = True,
) -> None:
    """Create the v3 global spine schema in an open SQLite connection."""
    conn.executescript(_SPINE_COMMON_SCHEMA_SQL)
    conn.executescript(_GLOBAL_SPINE_DERIVED_SCHEMA_SQL)
    if include_secondary_indexes:
        conn.executescript(_GLOBAL_SPINE_SECONDARY_INDEX_SQL)


def create_spine_fragment_schema(conn: sqlite3.Connection) -> None:
    """Create the lean, non-serving schema used by immutable merge fragments."""
    conn.executescript(_SPINE_COMMON_SCHEMA_SQL)


def write_global_spine_metadata(
    conn: sqlite3.Connection, metadata: Mapping[str, Any] | None = None
) -> None:
    """Write required and caller-provided metadata rows."""
    payload: dict[str, Any] = {
        "schema_version": GLOBAL_SPINE_SCHEMA_VERSION,
        "builder_version": GLOBAL_SPINE_BUILDER_VERSION,
        "index_layout": GLOBAL_SPINE_LAYOUT,
        "routing_mode": GLOBAL_SPINE_ROUTING_MODE,
        "agent_graph_index_version": GLOBAL_SPINE_AGENT_GRAPH_INDEX_VERSION,
        "replica_invariant_version": GLOBAL_SPINE_REPLICA_INVARIANT_VERSION,
        "created_at": datetime.now(timezone.utc).isoformat(),
    }
    if metadata:
        payload.update(dict(metadata))
    conn.executemany(
        "INSERT OR REPLACE INTO metadata(key, value_json) VALUES(?, ?)",
        ((key, json.dumps(value, sort_keys=True)) for key, value in sorted(payload.items())),
    )


def read_global_spine_metadata(conn: sqlite3.Connection) -> dict[str, Any]:
    """Read metadata rows from an open global spine connection."""
    try:
        rows = conn.execute("SELECT key, value_json FROM metadata").fetchall()
    except sqlite3.Error:
        return {}
    metadata: dict[str, Any] = {}
    for key, value_json in rows:
        try:
            metadata[str(key)] = json.loads(str(value_json))
        except json.JSONDecodeError:
            metadata[str(key)] = str(value_json)
    return metadata


def initialize_global_spine_database(
    path: Path,
    *,
    metadata: Mapping[str, Any] | None = None,
    replace: bool = False,
) -> Path:
    """Create a global spine database at ``path`` and write required metadata."""
    resolved = path.expanduser().resolve()
    resolved.parent.mkdir(parents=True, exist_ok=True)
    if replace:
        resolved.unlink(missing_ok=True)
    with sqlite3.connect(resolved) as conn:
        create_global_spine_schema(conn)
        write_global_spine_metadata(conn, metadata)
    return resolved


def verify_global_spine_schema(
    path: Path,
    *,
    deep: bool = True,
    trust_seal: bool = True,
    require_trusted_seal: bool = False,
) -> dict[str, Any]:
    """Verify a global spine, reusing a matching immutable deep-verification seal."""
    result = _verify_spine_database(
        path,
        kind="global_spine",
        required_tables=GLOBAL_SPINE_TABLES,
        required_metadata_keys=GLOBAL_SPINE_REQUIRED_METADATA_KEYS,
        forbidden_table_prefixes=SPINE_FORBIDDEN_TABLE_PREFIXES,
        allowed_secondary_indexes=GLOBAL_SPINE_ALLOWED_SECONDARY_INDEXES,
        required_secondary_indexes=GLOBAL_SPINE_ALLOWED_SECONDARY_INDEXES,
        deep=deep,
        trust_seal=trust_seal,
        require_trusted_seal=require_trusted_seal,
    )
    errors = list(result["errors"])
    metadata = result["metadata"]
    if "global_spine_missing" in errors:
        return {
            **result,
            "replica_invariants": _skipped_global_replica_invariants("global_spine_missing"),
        }
    if metadata.get("routing_mode") != GLOBAL_SPINE_ROUTING_MODE:
        errors.append("global_spine_routing_mode_mismatch")
    if metadata.get("replica_invariant_version") != GLOBAL_SPINE_REPLICA_INVARIANT_VERSION:
        errors.append("global_spine_replica_invariant_version_mismatch")

    required_replica_tables = {
        "global_object_locator",
        "global_object_replica",
        "global_edge_spine",
        "global_edge_replica",
    }
    if result.get("seal_trusted"):
        sealed_invariants = result.get("sealed_replica_invariants")
        if _valid_global_replica_invariants(sealed_invariants):
            replica_invariants = {
                **dict(sealed_invariants),
                "source": "immutable_seal",
            }
        else:
            replica_invariants = _skipped_global_replica_invariants("invalid_invariant_seal")
            errors.append("global_spine_replica_invariant_seal_invalid")
    elif deep and required_replica_tables.issubset(set(result.get("tables") or ())):
        replica_invariants = _verify_global_replica_invariants(path)
        errors.extend(str(error) for error in replica_invariants["errors"])
    else:
        reason = "quick_verification" if not deep else "schema_incomplete"
        replica_invariants = _skipped_global_replica_invariants(reason)
    return {
        **result,
        "ok": not errors,
        "errors": errors,
        "replica_invariants": replica_invariants,
    }


_GLOBAL_REPLICA_INVARIANT_SPECS = (
    (
        "objects",
        "global_object_locator",
        "global_object_replica",
        "object_id",
    ),
    (
        "edges",
        "global_edge_spine",
        "global_edge_replica",
        "edge_id",
    ),
)


def _verify_global_replica_invariants(path: Path) -> dict[str, Any]:
    """Scan canonical/replica pairs once per kind and report the first violation."""
    started_at = time.perf_counter()
    checks: dict[str, Any] = {}
    errors: list[str] = []
    resolved = path.expanduser().resolve()
    try:
        with _connect_immutable_readonly(resolved) as conn:
            conn.row_factory = sqlite3.Row
            conn.execute("PRAGMA query_only=ON")
            for kind, canonical_table, replica_table, key_column in _GLOBAL_REPLICA_INVARIANT_SPECS:
                check_started_at = time.perf_counter()
                violation = conn.execute(
                    f"""
                    SELECT
                        canonical.{key_column} AS semantic_id,
                        canonical.occurrence_count AS occurrence_count,
                        COUNT(replica.{key_column}) AS replica_count,
                        COALESCE(
                            MAX(
                                CASE
                                    WHEN replica.{key_column} IS NOT NULL
                                     AND canonical.semantic_hash
                                         IS NOT replica.semantic_hash
                                    THEN 1
                                    ELSE 0
                                END
                            ),
                            0
                        ) AS semantic_hash_mismatch
                    FROM {canonical_table} AS canonical
                    LEFT JOIN {replica_table} AS replica
                      ON replica.{key_column} = canonical.{key_column}
                    GROUP BY canonical.{key_column}
                    HAVING COUNT(replica.{key_column}) = 0
                        OR canonical.occurrence_count
                            != COUNT(replica.{key_column})
                        OR semantic_hash_mismatch != 0
                    LIMIT 1
                    """
                ).fetchone()
                violation_payload = dict(violation) if violation is not None else None
                check_ok = violation_payload is None
                checks[kind] = {
                    "ok": check_ok,
                    "canonical_table": canonical_table,
                    "replica_table": replica_table,
                    "elapsed_ms": max(
                        0,
                        int((time.perf_counter() - check_started_at) * 1000),
                    ),
                    "violation": violation_payload,
                }
                if not check_ok:
                    errors.append(
                        f"global_spine_{kind}_replica_invariant_failed:"
                        f"key={violation_payload['semantic_id']}:"
                        f"occurrence_count={violation_payload['occurrence_count']}:"
                        f"replica_count={violation_payload['replica_count']}:"
                        "semantic_hash_mismatch="
                        f"{violation_payload['semantic_hash_mismatch']}"
                    )
    except sqlite3.Error as exc:
        errors.append(f"global_spine_replica_invariant_sqlite_error:{exc}")
    return {
        "version": GLOBAL_SPINE_REPLICA_INVARIANT_VERSION,
        "ok": not errors,
        "source": "sqlite_query",
        "checks": checks,
        "errors": errors,
        "elapsed_ms": max(0, int((time.perf_counter() - started_at) * 1000)),
    }


def _skipped_global_replica_invariants(reason: str) -> dict[str, Any]:
    return {
        "version": GLOBAL_SPINE_REPLICA_INVARIANT_VERSION,
        "ok": None,
        "source": "skipped",
        "reason": reason,
        "checks": {},
        "errors": [],
        "elapsed_ms": 0,
    }


def _valid_global_replica_invariants(value: Any) -> bool:
    return bool(
        isinstance(value, Mapping)
        and value.get("version") == GLOBAL_SPINE_REPLICA_INVARIANT_VERSION
        and value.get("ok") is True
        and isinstance(value.get("checks"), Mapping)
        and all(
            isinstance(value["checks"].get(kind), Mapping)
            and value["checks"][kind].get("ok") is True
            for kind, *_rest in _GLOBAL_REPLICA_INVARIANT_SPECS
        )
    )


def verify_spine_fragment_schema(
    path: Path,
    *,
    deep: bool = True,
    trust_seal: bool = True,
    require_trusted_seal: bool = False,
) -> dict[str, Any]:
    """Verify a lean fragment with quick/deep and immutable-seal tiers."""
    result = _verify_spine_database(
        path,
        kind="spine_fragment",
        required_tables=SPINE_FRAGMENT_TABLES,
        required_metadata_keys=SPINE_FRAGMENT_REQUIRED_METADATA_KEYS,
        forbidden_tables=GLOBAL_SPINE_DERIVED_TABLES,
        forbidden_table_prefixes=SPINE_FORBIDDEN_TABLE_PREFIXES,
        allowed_secondary_indexes=(),
        required_secondary_indexes=(),
        deep=deep,
        trust_seal=trust_seal,
        require_trusted_seal=require_trusted_seal,
    )
    errors = list(result["errors"])
    metadata = result["metadata"]
    if "spine_fragment_missing" in errors:
        return result
    if metadata.get("fragment_schema_version") != SPINE_FRAGMENT_SCHEMA_VERSION:
        errors.append("spine_fragment_schema_version_mismatch")
    return {**result, "ok": not errors, "errors": errors}


def _verify_spine_database(
    path: Path,
    *,
    kind: str,
    required_tables: tuple[str, ...],
    required_metadata_keys: tuple[str, ...],
    forbidden_tables: tuple[str, ...] = (),
    forbidden_table_prefixes: tuple[str, ...] = (),
    allowed_secondary_indexes: tuple[str, ...] = (),
    required_secondary_indexes: tuple[str, ...] = (),
    deep: bool,
    trust_seal: bool,
    require_trusted_seal: bool,
) -> dict[str, Any]:
    resolved = path.expanduser().resolve()
    errors: list[str] = []
    warnings: list[str] = []
    tables: list[str] = []
    indexes: list[str] = []
    metadata: dict[str, Any] = {}
    seal_path = spine_verification_seal_path(resolved)
    seal, seal_status = _read_matching_spine_verification_seal(resolved, kind=kind)
    seal_trusted = bool(trust_seal and seal_status == "valid")
    sealed_replica_invariants = (
        dict(seal["replica_invariants"])
        if seal_trusted and isinstance(seal.get("replica_invariants"), Mapping)
        else None
    )
    verification_mode = "deep" if deep else "quick"
    integrity_check: str | None = None
    integrity_source = "none"
    if not resolved.exists():
        return {
            "ok": False,
            "kind": kind,
            "path": str(resolved),
            "errors": [f"{kind}_missing"],
            "warnings": warnings,
            "tables": tables,
            "indexes": indexes,
            "metadata": metadata,
            "verification_mode": verification_mode,
            "integrity_check": integrity_check,
            "integrity_source": integrity_source,
            "seal_path": str(seal_path),
            "seal_status": seal_status,
            "seal_trusted": False,
            "sealed_replica_invariants": None,
            "file_identity": None,
        }
    if seal_status not in {"valid", "missing"}:
        warnings.append(f"{kind}_verification_seal_{seal_status}")
    try:
        with _connect_immutable_readonly(resolved) as conn:
            if seal_trusted:
                verification_mode = "deep-sealed" if deep else "quick-sealed"
                integrity_check = "ok"
                integrity_source = "immutable_seal"
            elif require_trusted_seal:
                verification_mode = "seal-required"
                integrity_source = "none"
                errors.append(f"{kind}_verification_seal_required:{seal_status}")
            else:
                pragma = "PRAGMA integrity_check" if deep else "PRAGMA quick_check(1)"
                integrity_rows = conn.execute(pragma).fetchall()
                integrity_check = "ok" if integrity_rows == [("ok",)] else repr(integrity_rows)
                integrity_source = "sqlite_integrity_check" if deep else "sqlite_quick_check"
                if integrity_rows != [("ok",)]:
                    errors.append(f"sqlite_integrity_check_failed:{integrity_rows!r}")
            tables = sorted(
                str(row[0])
                for row in conn.execute(
                    """
                    SELECT name
                    FROM sqlite_master
                    WHERE type IN ('table', 'view')
                    """
                ).fetchall()
            )
            indexes = sorted(
                str(row[0])
                for row in conn.execute(
                    "SELECT name FROM sqlite_master WHERE type = 'index'"
                ).fetchall()
            )
            metadata = read_global_spine_metadata(conn)
    except sqlite3.Error as exc:
        return {
            "ok": False,
            "kind": kind,
            "path": str(resolved),
            "errors": [f"sqlite_error:{exc}"],
            "warnings": warnings,
            "tables": tables,
            "indexes": indexes,
            "metadata": metadata,
            "verification_mode": verification_mode,
            "integrity_check": integrity_check,
            "integrity_source": integrity_source,
            "seal_path": str(seal_path),
            "seal_status": seal_status,
            "seal_trusted": seal_trusted,
            "sealed_replica_invariants": sealed_replica_invariants,
            "file_identity": _spine_database_identity(resolved),
        }

    table_set = set(tables)
    for table in required_tables:
        if table not in table_set:
            errors.append(f"{kind}_table_missing:{table}")
    for table in forbidden_tables:
        if table in table_set:
            errors.append(f"{kind}_forbidden_table:{table}")
    for table in tables:
        if any(table.startswith(prefix) for prefix in forbidden_table_prefixes):
            errors.append(f"{kind}_forbidden_table:{table}")
    named_indexes = {
        index_name for index_name in indexes if not index_name.startswith("sqlite_autoindex_")
    }
    allowed_index_set = set(allowed_secondary_indexes)
    for index_name in sorted(named_indexes - allowed_index_set):
        errors.append(f"{kind}_forbidden_index:{index_name}")
    for index_name in sorted(set(required_secondary_indexes) - named_indexes):
        errors.append(f"{kind}_index_missing:{index_name}")
    for key in required_metadata_keys:
        if key not in metadata:
            errors.append(f"{kind}_metadata_missing:{key}")
    if metadata.get("schema_version") != GLOBAL_SPINE_SCHEMA_VERSION:
        errors.append(
            "global_spine_schema_version_mismatch"
            if kind == "global_spine"
            else f"{kind}_global_schema_version_mismatch"
        )
    if metadata.get("builder_version") != GLOBAL_SPINE_BUILDER_VERSION:
        errors.append(f"{kind}_builder_version_mismatch")
    if metadata.get("index_layout") != GLOBAL_SPINE_LAYOUT:
        errors.append(f"{kind}_index_layout_mismatch")
    return {
        "ok": not errors,
        "kind": kind,
        "path": str(resolved),
        "errors": errors,
        "warnings": warnings,
        "tables": tables,
        "indexes": indexes,
        "metadata": metadata,
        "verification_mode": verification_mode,
        "integrity_check": integrity_check,
        "integrity_source": integrity_source,
        "seal_path": str(seal_path),
        "seal_status": seal_status,
        "seal_trusted": seal_trusted,
        "sealed_replica_invariants": sealed_replica_invariants,
        "file_identity": _spine_database_identity(resolved),
    }


def spine_verification_seal_path(path: Path) -> Path:
    """Return the adjacent immutable verification-seal path for a spine DB."""
    resolved = path.expanduser().resolve()
    return resolved.with_name(resolved.name + SPINE_VERIFICATION_SEAL_SUFFIX)


def write_spine_verification_seal(
    path: Path,
    verification: Mapping[str, Any],
    *,
    source_path: Path | None = None,
    details: Mapping[str, Any] | None = None,
    sha256: str | None = None,
) -> Path:
    """Atomically seal the current file identity after an actual or inherited deep check."""
    resolved = path.expanduser().resolve()
    if not resolved.is_file():
        raise FileNotFoundError(f"spine database missing while sealing: {resolved}")
    if not verification.get("ok"):
        raise ValueError("cannot seal a failed spine verification")
    integrity_source = str(verification.get("integrity_source") or "")
    if integrity_source not in {"sqlite_integrity_check", "immutable_seal"}:
        raise ValueError(
            f"cannot seal non-deep verification source: {integrity_source or '<missing>'}"
        )
    if sha256 is not None and (not isinstance(sha256, str) or _SHA256_RE.fullmatch(sha256) is None):
        raise ValueError("spine verification seal sha256 must be lowercase 64-hex")
    replica_invariants = verification.get("replica_invariants")
    if verification.get("kind") == "global_spine" and not _valid_global_replica_invariants(
        replica_invariants
    ):
        raise ValueError("cannot seal global spine without valid replica invariants")
    database_identity = _spine_database_identity(resolved)
    payload = {
        "format": SPINE_VERIFICATION_SEAL_FORMAT,
        "kind": str(verification.get("kind") or ""),
        "schema_version": GLOBAL_SPINE_SCHEMA_VERSION,
        "builder_version": GLOBAL_SPINE_BUILDER_VERSION,
        "fragment_schema_version": (
            SPINE_FRAGMENT_SCHEMA_VERSION if verification.get("kind") == "spine_fragment" else None
        ),
        "deep_verified": True,
        "source_integrity": integrity_source,
        "replica_invariants": (
            dict(replica_invariants) if isinstance(replica_invariants, Mapping) else None
        ),
        "database_identity": database_identity,
        "sha256": sha256,
        "sha256_database_identity": database_identity if sha256 is not None else None,
        "source_path": str(source_path.expanduser().resolve()) if source_path is not None else None,
        "details": dict(details or {}),
        "verified_at": datetime.now(timezone.utc).isoformat(),
    }
    seal_path = spine_verification_seal_path(resolved)
    return _write_spine_verification_seal_payload(seal_path, payload)


def inherit_spine_verification_seal(
    source_path: Path | str,
    target_path: Path | str,
    *,
    details: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    """Rebind a trusted deep seal to a byte-identical release copy.

    Release materialization changes the inode even when the SQLite bytes are
    cloned unchanged.  Re-running ``integrity_check`` over a multi-gigabyte
    Global Spine adds no information once the source seal and byte identity of
    the copy are proven.  The fast path uses the size and copied nanosecond
    mtime; filesystems that cannot preserve mtime fall back to one SHA-256
    comparison before inheriting the seal.
    """
    source = Path(source_path).expanduser().resolve()
    target = Path(target_path).expanduser().resolve()
    if source == target:
        raise ValueError("source and target spine paths must differ")
    source_verification = verify_global_spine_schema(
        source,
        deep=True,
        trust_seal=True,
        require_trusted_seal=True,
    )
    if not source_verification.get("ok"):
        raise ValueError(
            "cannot inherit an invalid global spine seal: "
            + ", ".join(source_verification.get("errors") or [])
        )
    if not target.is_file():
        raise FileNotFoundError(f"copied global spine missing: {target}")

    source_identity = _spine_database_identity(source) or {}
    target_identity = _spine_database_identity(target) or {}
    copy_identity_matches = bool(
        source_identity.get("size") == target_identity.get("size")
        and source_identity.get("mtime_ns") == target_identity.get("mtime_ns")
    )
    source_sha256 = read_spine_verification_sha256(source)
    if not copy_identity_matches:
        if source_sha256 is None:
            raise ValueError("source global spine seal has no SHA-256 for copy verification")
        if _spine_file_sha256(target) != source_sha256:
            raise ValueError("copied global spine SHA-256 mismatch")

    inherited = {
        **source_verification,
        "path": str(target),
        "integrity_check": "ok",
        "integrity_source": "immutable_seal",
        "verification_mode": "deep-sealed-inherited-copy",
    }
    write_spine_verification_seal(
        target,
        inherited,
        source_path=source,
        details={"inheritance": "byte-identical-release-copy", **dict(details or {})},
        sha256=source_sha256,
    )
    return verify_global_spine_schema(
        target,
        deep=False,
        trust_seal=True,
        require_trusted_seal=True,
    )


def read_spine_verification_sha256(path: Path) -> str | None:
    """Return a cached digest only while its deep seal matches this exact file identity."""
    payload, status = _read_valid_spine_verification_seal(path)
    if status != "valid":
        return None
    digest = payload.get("sha256")
    if not isinstance(digest, str) or _SHA256_RE.fullmatch(digest) is None:
        return None
    current_identity = _spine_database_identity(path.expanduser().resolve())
    if not _spine_database_identity_matches(
        payload.get("sha256_database_identity"),
        current_identity,
    ):
        return None
    return digest


def record_spine_verification_sha256(path: Path, sha256: str) -> Path:
    """Atomically attach a freshly computed digest to an unchanged deep seal."""
    if not isinstance(sha256, str) or _SHA256_RE.fullmatch(sha256) is None:
        raise ValueError("spine verification seal sha256 must be lowercase 64-hex")
    resolved = path.expanduser().resolve()
    payload, status = _read_valid_spine_verification_seal(resolved)
    if status != "valid":
        raise RuntimeError(f"cannot cache sha256 without a matching immutable seal: {status}")
    database_identity = _spine_database_identity(resolved)
    if not _spine_database_identity_matches(payload.get("database_identity"), database_identity):
        raise RuntimeError("cannot cache sha256 after spine database identity changed")
    payload = {
        **payload,
        "sha256": sha256,
        "sha256_database_identity": database_identity,
        "sha256_recorded_at": datetime.now(timezone.utc).isoformat(),
    }
    if not _spine_database_identity_matches(
        database_identity,
        _spine_database_identity(resolved),
    ):
        raise RuntimeError("cannot cache sha256 while spine database identity is changing")
    return _write_spine_verification_seal_payload(
        spine_verification_seal_path(resolved),
        payload,
    )


def _write_spine_verification_seal_payload(
    seal_path: Path,
    payload: Mapping[str, Any],
) -> Path:
    seal_path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = seal_path.with_name(f".{seal_path.name}.{os.getpid()}.{time.time_ns()}.tmp")
    try:
        tmp_path.write_text(
            json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        os.replace(tmp_path, seal_path)
    finally:
        tmp_path.unlink(missing_ok=True)
    return seal_path


def _spine_file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(8 * 1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _read_valid_spine_verification_seal(path: Path) -> tuple[dict[str, Any], str]:
    payload, status = _read_spine_verification_seal_payload(path)
    if status != "readable":
        return payload, status
    kind = payload.get("kind")
    if kind not in {"global_spine", "spine_fragment"}:
        return payload, "contract_mismatch"
    return _read_matching_spine_verification_seal(path, kind=str(kind))


def _read_matching_spine_verification_seal(
    path: Path,
    *,
    kind: str,
) -> tuple[dict[str, Any], str]:
    payload, status = _read_spine_verification_seal_payload(path)
    if status != "readable":
        return payload, status
    if not isinstance(payload, dict) or payload.get("format") != SPINE_VERIFICATION_SEAL_FORMAT:
        return {}, "invalid"
    if payload.get("kind") != kind or payload.get("deep_verified") is not True:
        return payload, "contract_mismatch"
    if payload.get("schema_version") != GLOBAL_SPINE_SCHEMA_VERSION:
        return payload, "contract_mismatch"
    if payload.get("builder_version") != GLOBAL_SPINE_BUILDER_VERSION:
        return payload, "contract_mismatch"
    if kind == "global_spine" and not _valid_global_replica_invariants(
        payload.get("replica_invariants")
    ):
        return payload, "contract_mismatch"
    if (
        kind == "spine_fragment"
        and payload.get("fragment_schema_version") != SPINE_FRAGMENT_SCHEMA_VERSION
    ):
        return payload, "contract_mismatch"
    if not _spine_database_identity_matches(
        payload.get("database_identity"),
        _spine_database_identity(path),
    ):
        return payload, "identity_mismatch"
    return payload, "valid"


def _read_spine_verification_seal_payload(path: Path) -> tuple[dict[str, Any], str]:
    seal_path = spine_verification_seal_path(path)
    try:
        payload = json.loads(seal_path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return {}, "missing"
    except (json.JSONDecodeError, OSError):
        return {}, "invalid"
    if not isinstance(payload, dict):
        return {}, "invalid"
    return payload, "readable"


def _spine_database_identity(path: Path) -> dict[str, int] | None:
    try:
        stat = path.stat()
    except FileNotFoundError:
        return None
    return {
        "device": int(stat.st_dev),
        "inode": int(stat.st_ino),
        "size": int(stat.st_size),
        "mtime_ns": int(stat.st_mtime_ns),
    }


def _spine_database_identity_matches(sealed: Any, current: Mapping[str, int] | None) -> bool:
    """Match immutable bytes across a volume remount without trusting replacement.

    ``st_dev`` is an identifier for the currently mounted filesystem, not the
    file's bytes. APFS can change it after a restart or a volume remount while
    keeping the same immutable inode, size, and nanosecond mtime. Those three
    fields still reject normal replacement, truncation, and in-place writes.
    """
    return bool(
        isinstance(sealed, Mapping)
        and current is not None
        and all(
            sealed.get(field) == current.get(field)
            for field in ("inode", "size", "mtime_ns")
        )
    )
