"""Compact all-fragment collision preflight for global-spine builds."""

from __future__ import annotations

import json
import os
import sqlite3
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from collections import Counter
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import Any

from krw_capability_runtime.agent_index.spine_schema import verify_spine_fragment_schema
from krw_capability_runtime.agent_index.semantic_identity import (
    GLOBAL_CANONICAL_ENTITY_PREFIXES,
    GLOBAL_TAXONOMY_TERM_PREFIX,
    is_global_object_identity,
    semantic_object_hash,
)


SPINE_SEMANTIC_PREFLIGHT_FORMAT_VERSION = "krw-ontology-semantic-preflight/v1"
COMPANY_IDENTITY_PREFLIGHT_FORMAT_VERSION = "krw-ontology-company-identity-preflight/v1"

_GLOBAL_OBJECT_SCOPE_SQL = (
    "(incoming.object_type = 'TaxonomyTerm' "
    f"AND incoming.object_id LIKE '{GLOBAL_TAXONOMY_TERM_PREFIX}%') OR "
    "(incoming.object_type = 'CanonicalEntity' AND ("
    + " OR ".join(
        f"incoming.object_id LIKE '{prefix}%'" for prefix in GLOBAL_CANONICAL_ENTITY_PREFIXES
    )
    + "))"
)

_CLAIM_TABLES: Mapping[str, tuple[tuple[str, ...], tuple[str, ...], str]] = {
    "global_object_locator": (
        ("object_id",),
        ("semantic_hash", "object_type", "local_object_key"),
        f"({_GLOBAL_OBJECT_SCOPE_SQL})",
    ),
    "global_edge_spine": (
        ("edge_id",),
        (
            "semantic_hash",
            "from_object_id",
            "to_object_id",
            "relation_type",
            "edge_scope",
            "source_object_type",
            "target_object_type",
        ),
        "(incoming.edge_scope = 'cross_company' OR "
        "instr(upper(incoming.edge_id), ':' || upper(incoming.ticker) || ':') = 0)",
    ),
}

_LOCAL_SCOPE_RULES: Mapping[str, tuple[str, str]] = {
    "global_document_catalog": ("document_id", "1"),
    "global_object_locator": (
        "object_id",
        f"NOT ({_GLOBAL_OBJECT_SCOPE_SQL})",
    ),
    "global_topic_spine": ("topic_id", "1"),
}


def preflight_company_shard_identities(
    shards: Mapping[str, Path],
    *,
    report_path: Path | None = None,
    workers: int = 8,
) -> dict[str, Any]:
    """Validate identity/reference contracts before emitting large fragments."""
    started_at = time.perf_counter()
    normalized = tuple(
        sorted(
            (
                str(ticker).strip().upper(),
                path.expanduser().resolve(),
            )
            for ticker, path in shards.items()
        )
    )
    if not normalized:
        raise ValueError("at least one company shard is required")
    conflicts: list[dict[str, Any]] = []
    warnings: list[dict[str, Any]] = []
    claims: dict[str, tuple[str, str, str, str]] = {}
    all_claims: list[dict[str, str]] = []
    worker_count = max(1, min(int(workers), len(normalized)))
    with ThreadPoolExecutor(max_workers=worker_count) as pool:
        futures = {
            pool.submit(_audit_company_shard_identity, ticker, path): (ticker, path)
            for ticker, path in normalized
        }
        for future in as_completed(futures):
            ticker, path = futures[future]
            result = future.result()
            conflicts.extend(result["conflicts"])
            warnings.extend(result["warnings"])
            for claim in result["claims"]:
                all_claims.append({**claim, "ticker": ticker, "path": str(path)})
    for claim in sorted(
        all_claims,
        key=lambda row: (row["object_id"], row["ticker"], row["path"]),
    ):
        key = claim["object_id"]
        first = claims.get(key)
        if first is None:
            claims[key] = (
                claim["semantic_hash"],
                claim["object_type"],
                claim["ticker"],
                claim["path"],
            )
            continue
        if first[0] == claim["semantic_hash"] and first[1] == claim["object_type"]:
            continue
        fields = []
        if first[0] != claim["semantic_hash"]:
            fields.append("semantic_hash")
        if first[1] != claim["object_type"]:
            fields.append("object_type")
        conflicts.append(
            {
                "table": "global_object_locator",
                "key": {"object_id": claim["object_id"]},
                "kind": "semantic",
                "fields": fields,
                "first_fragment": first[3],
                "conflicting_fragment": claim["path"],
                "first_ticker": first[2],
                "conflicting_ticker": claim["ticker"],
                "first_semantic": {
                    "semantic_hash": first[0],
                    "object_type": first[1],
                },
                "conflicting_semantic": {
                    "semantic_hash": claim["semantic_hash"],
                    "object_type": claim["object_type"],
                },
            }
        )
    conflicts.sort(
        key=lambda row: (
            str(row.get("table") or ""),
            json.dumps(row.get("key") or {}, sort_keys=True),
            str(row.get("conflicting_fragment") or ""),
        )
    )
    warnings.sort(key=lambda row: (str(row.get("ticker") or ""), str(row.get("kind") or "")))
    warning_kinds: Counter[str] = Counter()
    for warning in warnings:
        warning_kinds[str(warning["kind"])] += int(warning.get("count") or 0)
    result: dict[str, Any] = {
        "format": COMPANY_IDENTITY_PREFLIGHT_FORMAT_VERSION,
        "ok": not conflicts,
        "shard_count": len(normalized),
        "worker_count": worker_count,
        "claim_row_count": len(all_claims),
        "unique_claim_count": len(claims),
        "conflict_count": len(conflicts),
        "warning_count": sum(int(row.get("count") or 0) for row in warnings),
        "warning_kinds": dict(warning_kinds),
        "conflicts": conflicts,
        "warnings": warnings,
        "elapsed_ms": int((time.perf_counter() - started_at) * 1000),
    }
    if report_path is not None:
        resolved_report = report_path.expanduser().resolve()
        _write_json_atomic(resolved_report, result)
        result["report_path"] = str(resolved_report)
    return result


def _audit_company_shard_identity(ticker: str, path: Path) -> dict[str, Any]:
    if not path.is_file():
        raise FileNotFoundError(f"company shard not found: {path}")
    conflicts: list[dict[str, Any]] = []
    warnings: list[dict[str, Any]] = []
    claims: list[dict[str, str]] = []
    with sqlite3.connect(path) as conn:
        conn.row_factory = sqlite3.Row
        conn.execute("PRAGMA query_only=ON")
        blocking_queries = {
            "factor_missing_object": (
                "factor_lookup",
                "SELECT l.object_id AS identity FROM factor_lookup l "
                "LEFT JOIN objects o ON o.id=l.object_id WHERE o.id IS NULL",
            ),
            "metric_missing_object": (
                "metric_lookup",
                "SELECT l.object_id AS identity FROM metric_lookup l "
                "LEFT JOIN objects o ON o.id=l.object_id WHERE o.id IS NULL",
            ),
            "agreement_missing_object": (
                "agreement_lookup",
                "SELECT l.object_id AS identity FROM agreement_lookup l "
                "LEFT JOIN objects o ON o.id=l.object_id WHERE o.id IS NULL",
            ),
            "topic_primary_missing_object": (
                "company_topic_index",
                "SELECT l.primary_object_id AS identity FROM company_topic_index l "
                "LEFT JOIN objects o ON o.id=l.primary_object_id "
                "WHERE l.primary_object_id IS NOT NULL AND l.primary_object_id != '' "
                "AND o.id IS NULL",
            ),
            "document_identity_collision": (
                "documents",
                "SELECT doc_type_key || ':' || period AS identity FROM documents "
                "GROUP BY doc_type_key,period HAVING COUNT(*) > 1",
            ),
            "topic_identity_collision": (
                "company_topic_index",
                "SELECT topic_id AS identity FROM company_topic_index "
                "GROUP BY topic_id HAVING COUNT(*) > 1",
            ),
            "object_projection_collision": (
                "objects",
                "SELECT a.id AS identity FROM objects a JOIN objects b "
                "ON a.id = 'scoped:' || ? || ':' || b.id WHERE a.id != b.id "
                "AND instr(':' || upper(b.id) || ':', ':' || upper(?) || ':') = 0",
            ),
            "edge_projection_collision": (
                "edges",
                "SELECT a.id AS identity FROM edges a JOIN edges b "
                "ON a.id = 'scoped:' || ? || ':' || b.id WHERE a.id != b.id "
                "AND instr(':' || upper(b.id) || ':', ':' || upper(?) || ':') = 0",
            ),
            "topic_projection_collision": (
                "company_topic_index",
                "SELECT a.topic_id AS identity FROM company_topic_index a "
                "JOIN company_topic_index b "
                "ON a.topic_id = 'scoped:' || ? || ':' || b.topic_id "
                "WHERE a.topic_id != b.topic_id "
                "AND instr(':' || upper(b.topic_id) || ':', ':' || upper(?) || ':') = 0",
            ),
        }
        for kind, (table_name, sql) in blocking_queries.items():
            parameters = tuple(ticker for _ in range(sql.count("?")))
            try:
                rows = conn.execute(sql, parameters).fetchall()
            except sqlite3.Error as exc:
                conflicts.append(
                    _company_preflight_conflict(
                        ticker=ticker,
                        path=path,
                        table=table_name,
                        kind="sqlite",
                        identity=kind,
                        fields=[str(exc)],
                    )
                )
                continue
            conflicts.extend(
                _company_preflight_conflict(
                    ticker=ticker,
                    path=path,
                    table=table_name,
                    kind="reference",
                    identity=str(row["identity"] or ""),
                    fields=[kind],
                )
                for row in rows
            )
        for table_name in (
            "documents",
            "objects",
            "edges",
            "factor_lookup",
            "metric_lookup",
            "agreement_lookup",
            "company_topic_index",
        ):
            try:
                rows = conn.execute(
                    f"""
                    SELECT DISTINCT ticker AS identity
                    FROM {table_name}
                    WHERE upper(COALESCE(ticker, ''))
                          NOT IN ('', 'UNKNOWN', 'N/A', 'NA', 'NONE', 'NULL', ?)
                    """,
                    (ticker,),
                ).fetchall()
            except sqlite3.Error as exc:
                conflicts.append(
                    _company_preflight_conflict(
                        ticker=ticker,
                        path=path,
                        table=table_name,
                        kind="sqlite",
                        identity="ticker_scope",
                        fields=[str(exc)],
                    )
                )
                continue
            conflicts.extend(
                _company_preflight_conflict(
                    ticker=ticker,
                    path=path,
                    table=table_name,
                    kind="reference",
                    identity=str(row["identity"] or ""),
                    fields=["ticker_scope_mismatch"],
                )
                for row in rows
            )
        warning_queries = {
            "dangling_edges_skipped": (
                """
                SELECT COUNT(DISTINCT e.id)
                FROM edges e
                LEFT JOIN objects source ON source.id=e.from_id
                LEFT JOIN objects target ON target.id=e.to_id
                WHERE source.id IS NULL OR target.id IS NULL
                """
            ),
            "topic_source_refs_skipped": (
                """
                SELECT COUNT(*)
                FROM company_topic_index topic
                JOIN json_each(topic.source_object_ids) item
                LEFT JOIN objects object ON object.id=item.value
                WHERE object.id IS NULL
                """
            ),
        }
        for kind, sql in warning_queries.items():
            try:
                count = int(conn.execute(sql).fetchone()[0] or 0)
            except sqlite3.Error as exc:
                conflicts.append(
                    _company_preflight_conflict(
                        ticker=ticker,
                        path=path,
                        table="company_shard",
                        kind="sqlite",
                        identity=kind,
                        fields=[str(exc)],
                    )
                )
                continue
            if count:
                warnings.append({"ticker": ticker, "kind": kind, "count": count})
        for row in conn.execute(
            "SELECT id,type,json FROM objects WHERE type IN ('TaxonomyTerm','CanonicalEntity')"
        ):
            try:
                payload = json.loads(str(row["json"] or "{}"))
            except json.JSONDecodeError as exc:
                conflicts.append(
                    _company_preflight_conflict(
                        ticker=ticker,
                        path=path,
                        table="objects",
                        kind="json",
                        identity=str(row["id"]),
                        fields=[str(exc)],
                    )
                )
                continue
            if not isinstance(payload, Mapping):
                payload = {}
            if not is_global_object_identity(row["id"], row["type"], payload):
                continue
            claims.append(
                {
                    "object_id": str(row["id"]),
                    "object_type": str(row["type"] or ""),
                    "semantic_hash": semantic_object_hash(
                        payload,
                        object_id=str(row["id"]),
                        object_type=str(row["type"] or ""),
                    ),
                }
            )
    return {"conflicts": conflicts, "warnings": warnings, "claims": claims}


def _company_preflight_conflict(
    *,
    ticker: str,
    path: Path,
    table: str,
    kind: str,
    identity: str,
    fields: list[str],
) -> dict[str, Any]:
    return {
        "table": table,
        "key": {"identity": identity},
        "kind": kind,
        "fields": fields,
        "first_fragment": str(path),
        "conflicting_fragment": str(path),
        "first_ticker": ticker,
        "conflicting_ticker": ticker,
        "first_semantic": {},
        "conflicting_semantic": {},
    }


def preflight_spine_fragments(
    fragments: Sequence[Path],
    *,
    report_path: Path | None = None,
) -> dict[str, Any]:
    """Scan every fragment and return every shared-ID semantic conflict.

    A compact temporary claim database is used so memory is bounded even for a
    production release. Ticker-scoped primary keys remain strict in the merge;
    only deliberately global object/edge identities are accumulated here. The
    global spine itself is not allocated.
    """
    started_at = time.perf_counter()
    resolved_fragments = tuple(
        sorted((path.expanduser().resolve() for path in fragments), key=lambda path: path.name)
    )
    if not resolved_fragments:
        raise ValueError("at least one spine fragment is required")
    resolved_report = report_path.expanduser().resolve() if report_path is not None else None
    temp_parent = (
        resolved_report.parent if resolved_report is not None else resolved_fragments[0].parent
    )
    temp_parent.mkdir(parents=True, exist_ok=True)
    claims_path = temp_parent / f".semantic-preflight.{os.getpid()}.{time.time_ns()}.sqlite"
    table_counts: dict[str, int] = {table: 0 for table in _CLAIM_TABLES}
    equivalent_counts: dict[str, int] = {table: 0 for table in _CLAIM_TABLES}
    try:
        with sqlite3.connect(claims_path) as conn:
            conn.row_factory = sqlite3.Row
            conn.execute("PRAGMA journal_mode=OFF")
            conn.execute("PRAGMA synchronous=OFF")
            conn.execute("PRAGMA temp_store=FILE")
            conn.execute(
                """
                CREATE TABLE claims(
                    table_name TEXT NOT NULL,
                    key_json TEXT NOT NULL,
                    semantic_json TEXT NOT NULL,
                    first_fragment TEXT NOT NULL,
                    PRIMARY KEY(table_name, key_json)
                ) WITHOUT ROWID
                """
            )
            conn.execute(
                """
                CREATE TABLE conflicts(
                    table_name TEXT NOT NULL,
                    key_json TEXT NOT NULL,
                    conflict_kind TEXT NOT NULL,
                    first_fragment TEXT NOT NULL,
                    conflicting_fragment TEXT NOT NULL,
                    first_semantic_json TEXT NOT NULL,
                    conflicting_semantic_json TEXT NOT NULL,
                    PRIMARY KEY(table_name, key_json, conflicting_fragment)
                ) WITHOUT ROWID
                """
            )
            for index, fragment in enumerate(resolved_fragments, start=1):
                verification = verify_spine_fragment_schema(
                    fragment,
                    deep=True,
                    trust_seal=True,
                )
                if not verification.get("ok"):
                    errors = ", ".join(verification.get("errors") or [])
                    raise RuntimeError(f"spine fragment failed verification: {fragment}: {errors}")
                schema_name = f"preflight_fragment_{index}"
                conn.execute(f"ATTACH DATABASE ? AS {schema_name}", (str(fragment),))
                try:
                    fragment_ticker = _fragment_ticker(conn, schema_name, fragment)
                    for table_name, (key_column, local_scope_sql) in _LOCAL_SCOPE_RULES.items():
                        conn.execute(
                            f"""
                            INSERT OR IGNORE INTO conflicts(
                                table_name, key_json, conflict_kind,
                                first_fragment, conflicting_fragment,
                                first_semantic_json, conflicting_semantic_json
                            )
                            SELECT ?, json_object('{key_column}', incoming.{key_column}),
                                   'scope', ?, ?, '{{}}',
                                   json_object(
                                       'required_ticker', ?,
                                       'actual_identity', incoming.{key_column}
                                   )
                            FROM {schema_name}.{table_name} AS incoming
                            WHERE {local_scope_sql}
                              AND instr(
                                  ':' || upper(incoming.{key_column}) || ':',
                                  ':' || upper(?) || ':'
                              ) = 0
                            """,
                            (
                                table_name,
                                str(fragment),
                                str(fragment),
                                fragment_ticker,
                                fragment_ticker,
                            ),
                        )
                    for table_name, (
                        key_columns,
                        semantic_columns,
                        shared_scope_sql,
                    ) in _CLAIM_TABLES.items():
                        key_json = _json_object_sql("incoming", key_columns)
                        semantic_json = (
                            _json_object_sql("incoming", semantic_columns)
                            if semantic_columns
                            else "'{}'"
                        )
                        incoming_sql = (
                            f"SELECT {key_json} AS key_json, "
                            f"{semantic_json} AS semantic_json "
                            f"FROM {schema_name}.{table_name} AS incoming "
                            f"WHERE {shared_scope_sql}"
                        )
                        row_count = conn.execute(
                            f"SELECT COUNT(*) FROM {schema_name}.{table_name} AS incoming "
                            f"WHERE {shared_scope_sql}"
                        ).fetchone()[0]
                        table_counts[table_name] += int(row_count)
                        equivalent_counts[table_name] += int(
                            conn.execute(
                                f"""
                                SELECT COUNT(*)
                                FROM ({incoming_sql}) AS incoming_claim
                                JOIN claims AS existing
                                  ON existing.table_name = ?
                                 AND existing.key_json = incoming_claim.key_json
                                 AND existing.semantic_json = incoming_claim.semantic_json
                                """,
                                (table_name,),
                            ).fetchone()[0]
                        )
                        conflict_kind = "semantic"
                        mismatch_sql = "existing.semantic_json != incoming_claim.semantic_json"
                        conn.execute(
                            f"""
                            INSERT OR IGNORE INTO conflicts(
                                table_name, key_json, conflict_kind,
                                first_fragment, conflicting_fragment,
                                first_semantic_json, conflicting_semantic_json
                            )
                            SELECT ?, incoming_claim.key_json, ?, existing.first_fragment, ?,
                                   existing.semantic_json, incoming_claim.semantic_json
                            FROM ({incoming_sql}) AS incoming_claim
                            JOIN claims AS existing
                              ON existing.table_name = ?
                             AND existing.key_json = incoming_claim.key_json
                            WHERE {mismatch_sql}
                            """,
                            (table_name, conflict_kind, str(fragment), table_name),
                        )
                        conn.execute(
                            f"""
                            INSERT OR IGNORE INTO claims(
                                table_name, key_json, semantic_json, first_fragment
                            )
                            SELECT ?, key_json, semantic_json, ?
                            FROM ({incoming_sql})
                            """,
                            (table_name, str(fragment)),
                        )
                    conn.commit()
                except BaseException:
                    conn.rollback()
                    raise
                finally:
                    conn.execute(f"DETACH DATABASE {schema_name}")
            conflicts = [
                _conflict_payload(row)
                for row in conn.execute(
                    """
                SELECT table_name, key_json, conflict_kind, first_fragment,
                       conflicting_fragment, first_semantic_json,
                       conflicting_semantic_json
                FROM conflicts
                ORDER BY table_name, key_json, conflicting_fragment
                """
                )
            ]
            unique_claim_count = int(conn.execute("SELECT COUNT(*) FROM claims").fetchone()[0])
        result: dict[str, Any] = {
            "format": SPINE_SEMANTIC_PREFLIGHT_FORMAT_VERSION,
            "ok": not conflicts,
            "fragment_count": len(resolved_fragments),
            "claim_row_count": sum(table_counts.values()),
            "unique_claim_count": unique_claim_count,
            "equivalent_collision_count": sum(equivalent_counts.values()),
            "table_counts": table_counts,
            "equivalent_collision_counts": equivalent_counts,
            "conflict_count": len(conflicts),
            "conflicts": conflicts,
            "elapsed_ms": int((time.perf_counter() - started_at) * 1000),
        }
        if resolved_report is not None:
            _write_json_atomic(resolved_report, result)
            result["report_path"] = str(resolved_report)
        return result
    finally:
        for suffix in ("", "-wal", "-shm"):
            Path(str(claims_path) + suffix).unlink(missing_ok=True)


def _json_object_sql(alias: str, columns: tuple[str, ...]) -> str:
    arguments = ", ".join(f"'{column}', {alias}.{column}" for column in columns)
    return f"json_object({arguments})"


def _conflict_payload(row: sqlite3.Row) -> dict[str, Any]:
    first = json.loads(row["first_semantic_json"])
    conflicting = json.loads(row["conflicting_semantic_json"])
    fields = sorted(
        key for key in set(first) | set(conflicting) if first.get(key) != conflicting.get(key)
    )
    return {
        "table": row["table_name"],
        "key": json.loads(row["key_json"]),
        "kind": row["conflict_kind"],
        "fields": (
            fields
            if row["conflict_kind"] == "semantic"
            else ["identity_scope"]
            if row["conflict_kind"] == "scope"
            else ["primary_key"]
        ),
        "first_fragment": row["first_fragment"],
        "conflicting_fragment": row["conflicting_fragment"],
        "first_semantic": first,
        "conflicting_semantic": conflicting,
    }


def _fragment_ticker(
    conn: sqlite3.Connection,
    schema_name: str,
    fragment: Path,
) -> str:
    row = conn.execute(
        f"SELECT value_json FROM {schema_name}.metadata WHERE key = 'ticker'"
    ).fetchone()
    try:
        ticker = str(json.loads(row[0])) if row is not None else ""
    except (json.JSONDecodeError, TypeError):
        ticker = ""
    ticker = ticker.strip().upper()
    if not ticker:
        raise RuntimeError(f"spine fragment ticker metadata missing: {fragment}")
    return ticker


def _write_json_atomic(path: Path, payload: Mapping[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = path.parent / f".{path.name}.{os.getpid()}.{time.time_ns()}.tmp"
    try:
        tmp_path.write_text(
            json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        os.replace(tmp_path, path)
    finally:
        tmp_path.unlink(missing_ok=True)
