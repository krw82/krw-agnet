"""Author-sharded read index for reviewed guru ontology artifacts."""

from __future__ import annotations

from collections import OrderedDict, defaultdict
from datetime import datetime, timezone
from functools import lru_cache
import hashlib
import json
from pathlib import Path
import re
import sqlite3
from threading import RLock
from typing import Any, Mapping, Sequence

from krw_capability_runtime.guru.context_taxonomy import context_tags_for_text
from krw_capability_runtime.guru.workspace import guru_root


GURU_INDEX_SCHEMA_VERSION = "krw-guru-shard-index/v2"
GURU_INDEX_DIRNAME = "indexes"
GURU_SHARD_DIRNAME = "shards"
GURU_SHARD_MANIFEST_FILENAME = "guru_shard_manifest.json"
GURU_SEARCH_TEXT_VERSION = "krw-guru-search-text/v3"
GURU_RANKING_PROJECTION_VERSION = "krw-guru-ranking-projection/v3"

REVIEWED_FILES = {
    "guru_objects": "guru_objects.jsonl",
    "consultation_objects": "consultation_objects.jsonl",
    "data_needs": "data_needs.jsonl",
    "relationships": "relationships.jsonl",
    "corpus_metadata": "corpus_metadata.jsonl",
    "rejected_candidates": "rejected_candidates.jsonl",
}
OBJECT_FAMILY_BY_FILE = {
    "guru_objects": "guru_object",
    "consultation_objects": "consultation_object",
    "data_needs": "data_need",
    "corpus_metadata": "corpus_metadata",
}
FAMILY_BY_OBJECT_FAMILY = {value: key for key, value in OBJECT_FAMILY_BY_FILE.items()}
_FTS_TOKEN_RE = re.compile(r"[0-9A-Za-z가-힣_]+")
_COMPACT_ROW_CACHE_MAX_ENTRIES = 64
_COMPACT_ROW_CACHE: OrderedDict[tuple[Any, ...], tuple[dict[str, Any], ...]] = OrderedDict()
_COMPACT_ROW_CACHE_LOCK = RLock()
_RANKING_PROJECTION_FIELDS = (
    "reviewed_id",
    "author_key",
    "object_type",
    "object_origin",
    "intent_family",
    "decision_stage",
    "data_need_family",
    "data_need_key",
    "requires_company_data",
    "requires_portfolio_data",
    "requires_user_context",
    "company_data_hooks",
    "portfolio_data_hooks",
    "related_reviewed_ids",
    "applicability",
    "specificity",
    "answer_role",
    "supporting_span_ids",
    "confidence",
)


class _GuruIndexUnavailable(RuntimeError):
    """Internal signal that callers must use the canonical JSONL fallback."""


def build_guru_shard_index(
    root: str | Path | None = None,
    *,
    index_dir: str | Path | None = None,
) -> dict[str, Any]:
    """Build per-author SQLite shards from reviewed guru JSONL artifacts."""
    root_path = guru_root(root)
    index_path = _index_dir(root_path, index_dir)
    shard_dir = index_path / GURU_SHARD_DIRNAME
    reviewed_dir = root_path / "reviewed"
    index_path.mkdir(parents=True, exist_ok=True)
    shard_dir.mkdir(parents=True, exist_ok=True)

    files = {
        family: _read_jsonl(reviewed_dir / filename) for family, filename in REVIEWED_FILES.items()
    }
    grouped: dict[str, dict[str, list[dict[str, Any]]]] = defaultdict(_empty_group)
    object_id_to_authors: dict[str, set[str]] = defaultdict(set)
    skipped_rows: list[dict[str, Any]] = []

    for family, object_family in OBJECT_FAMILY_BY_FILE.items():
        for row in files[family]:
            if not isinstance(row, dict):
                continue
            reviewed_id = _clean_scalar(row.get("reviewed_id"))
            author_key = _clean_scalar(row.get("author_key"))
            if not reviewed_id or not author_key:
                skipped_rows.append(
                    {
                        "family": family,
                        "reviewed_id": reviewed_id,
                        "reason": "missing_reviewed_id_or_author_key",
                    }
                )
                continue
            payload = dict(row)
            payload["object_family"] = object_family
            grouped[author_key][family].append(payload)
            object_id_to_authors[reviewed_id].add(author_key)

    relationship_rows = [row for row in files["relationships"] if isinstance(row, dict)]
    for relationship in relationship_rows:
        authors = set()
        for endpoint_key in ("from_id", "to_id"):
            endpoint = _clean_scalar(relationship.get(endpoint_key))
            if endpoint:
                authors.update(object_id_to_authors.get(endpoint, set()))
        if not authors:
            for endpoint_key in ("from_id", "to_id"):
                endpoint_author = _author_from_reviewed_id(relationship.get(endpoint_key))
                if endpoint_author:
                    authors.add(endpoint_author)
        for author_key in authors:
            grouped[author_key]["relationships"].append(dict(relationship))

    authors: dict[str, Any] = {}
    for author_key in sorted(grouped):
        shard_path = shard_dir / f"{author_key}.sqlite"
        shard_payload = grouped[author_key]
        fts_enabled = _write_author_shard(
            shard_path,
            author_key=author_key,
            rows_by_family=shard_payload,
        )
        authors[author_key] = {
            "author_key": author_key,
            "shard_path": str(shard_path),
            "sha256": _sha256_file(shard_path),
            "fts_enabled": fts_enabled,
            "search_text_version": GURU_SEARCH_TEXT_VERSION,
            "ranking_projection_version": GURU_RANKING_PROJECTION_VERSION,
            "counts": {
                family: len(shard_payload[family])
                for family in (
                    "guru_objects",
                    "consultation_objects",
                    "data_needs",
                    "corpus_metadata",
                    "relationships",
                )
            },
        }

    manifest = {
        "schema_version": GURU_INDEX_SCHEMA_VERSION,
        "search_text_version": GURU_SEARCH_TEXT_VERSION,
        "ranking_projection_version": GURU_RANKING_PROJECTION_VERSION,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "root": str(root_path),
        "reviewed_dir": str(reviewed_dir),
        "index_dir": str(index_path),
        "shard_dir": str(shard_dir),
        "manifest_path": str(index_path / GURU_SHARD_MANIFEST_FILENAME),
        "authors": authors,
        "source_files": {
            family: _source_file_record(
                reviewed_dir / filename,
                rows=len(files[family]),
            )
            for family, filename in REVIEWED_FILES.items()
        },
        "skipped_rows": skipped_rows,
        "source_of_truth": "reviewed_jsonl",
        "runtime_policy": {
            "preferred": "author_shard",
            "fallback": "reviewed_jsonl",
            "notes": [
                "Shard files are read indexes only.",
                "Reviewed JSONL remains the canonical ontology store.",
            ],
        },
    }
    _write_json_atomic(index_path / GURU_SHARD_MANIFEST_FILENAME, manifest)
    return manifest


def guru_index_manifest_path(
    root: str | Path | None = None,
    *,
    index_dir: str | Path | None = None,
) -> Path:
    root_path = guru_root(root)
    return _index_dir(root_path, index_dir) / GURU_SHARD_MANIFEST_FILENAME


def guru_index_status(
    root: str | Path | None = None,
    *,
    index_dir: str | Path | None = None,
) -> dict[str, Any]:
    """Return lightweight shard-index availability metadata."""
    root_path = guru_root(root)
    manifest_path = guru_index_manifest_path(root_path, index_dir=index_dir)
    manifest = _read_json(manifest_path)
    if not manifest:
        return {
            "present": False,
            "usable": False,
            "runtime_mode": "reviewed_jsonl",
            "manifest_path": str(manifest_path),
            "reason": "manifest_not_found",
        }
    authors = manifest.get("authors") if isinstance(manifest, Mapping) else {}
    if not isinstance(authors, Mapping):
        authors = {}
    missing = [
        author_key
        for author_key, author_payload in authors.items()
        if not isinstance(author_payload, Mapping)
        or not Path(str(author_payload.get("shard_path", ""))).is_file()
    ]
    schema_matches = manifest.get("schema_version") == GURU_INDEX_SCHEMA_VERSION
    search_text_matches = manifest.get("search_text_version") == GURU_SEARCH_TEXT_VERSION
    projection_matches = (
        manifest.get("ranking_projection_version") == GURU_RANKING_PROJECTION_VERSION
    )
    source_mismatches = _source_file_mismatches(root_path, manifest)
    usable = (
        not missing
        and not source_mismatches
        and bool(authors)
        and schema_matches
        and search_text_matches
        and projection_matches
    )
    reason = None
    if not schema_matches:
        reason = "schema_version_mismatch"
    elif not search_text_matches:
        reason = "search_text_version_mismatch"
    elif not projection_matches:
        reason = "ranking_projection_version_mismatch"
    elif source_mismatches:
        reason = "source_hash_mismatch"
    elif missing:
        reason = "missing_shards"
    elif not authors:
        reason = "authors_missing"
    return {
        "present": True,
        "usable": usable,
        "runtime_mode": "author_shard" if usable else "reviewed_jsonl",
        "schema_version": manifest.get("schema_version"),
        "search_text_version": manifest.get("search_text_version"),
        "ranking_projection_version": manifest.get("ranking_projection_version"),
        "generated_at": manifest.get("generated_at"),
        "manifest_path": str(manifest_path),
        "index_dir": manifest.get("index_dir"),
        "shard_dir": manifest.get("shard_dir"),
        "authors": {
            key: {
                "shard_path": payload.get("shard_path"),
                "counts": payload.get("counts", {}),
                "fts_enabled": bool(payload.get("fts_enabled")),
                "search_text_version": payload.get("search_text_version"),
                "ranking_projection_version": payload.get("ranking_projection_version"),
            }
            for key, payload in authors.items()
            if isinstance(payload, Mapping)
        },
        "missing_shards": missing,
        "source_mismatches": source_mismatches,
        "reason": reason,
        "source_of_truth": manifest.get("source_of_truth", "reviewed_jsonl"),
    }


def load_guru_index_bundle(
    root: str | Path | None = None,
    *,
    author_keys: Sequence[str] | None = None,
) -> dict[str, Any] | None:
    """Load reviewed artifacts from author shards, returning ``None`` on fallback."""
    root_path = guru_root(root)
    manifest = _read_json(guru_index_manifest_path(root_path))
    if (
        not manifest
        or manifest.get("schema_version") != GURU_INDEX_SCHEMA_VERSION
        or manifest.get("search_text_version") != GURU_SEARCH_TEXT_VERSION
        or manifest.get("ranking_projection_version") != GURU_RANKING_PROJECTION_VERSION
        or _source_file_mismatches(root_path, manifest)
    ):
        return None
    authors_manifest = manifest.get("authors")
    if not isinstance(authors_manifest, Mapping) or not authors_manifest:
        return None

    selected_authors = _clean_list(author_keys) or sorted(authors_manifest)
    shard_paths: list[Path] = []
    for author_key in selected_authors:
        author_payload = authors_manifest.get(author_key)
        if not isinstance(author_payload, Mapping):
            return None
        shard_path = Path(str(author_payload.get("shard_path", "")))
        if not shard_path.is_file():
            return None
        shard_paths.append(shard_path)

    files: dict[str, list[dict[str, Any]]] = {family: [] for family in REVIEWED_FILES}
    relationships_by_key: dict[str, dict[str, Any]] = {}
    for shard_path in shard_paths:
        shard_rows, shard_relationships = _read_author_shard(shard_path)
        for object_family, rows in shard_rows.items():
            family = FAMILY_BY_OBJECT_FAMILY.get(object_family)
            if family:
                files[family].extend(rows)
        for relationship in shard_relationships:
            relationships_by_key[_relationship_key(relationship)] = relationship

    files["relationships"] = list(relationships_by_key.values())
    searchable: list[dict[str, Any]] = []
    for family in ("guru_objects", "consultation_objects", "data_needs", "corpus_metadata"):
        for row in files[family]:
            payload = dict(row)
            payload["object_family"] = OBJECT_FAMILY_BY_FILE[family]
            searchable.append(payload)
    objects_by_id = {str(row["reviewed_id"]): row for row in searchable if row.get("reviewed_id")}
    relationships_by_id: dict[str, list[dict[str, Any]]] = {}
    for relationship in files["relationships"]:
        for key in ("from_id", "to_id"):
            object_id = relationship.get(key)
            if object_id:
                relationships_by_id.setdefault(str(object_id), []).append(relationship)

    return {
        **files,
        "objects_by_id": objects_by_id,
        "relationships_by_id": relationships_by_id,
        "_index": {
            "enabled": True,
            "runtime_mode": "author_shard",
            "schema_version": manifest.get("schema_version"),
            "manifest_path": str(guru_index_manifest_path(root_path)),
            "author_keys": selected_authors,
        },
    }


def scan_guru_index_compact_rows(
    root: str | Path | None,
    *,
    query: str,
    author_keys: Sequence[str] | None = None,
    object_families: Sequence[str] | None = None,
    object_types: Sequence[str] | None = None,
    intent_family: str | None = None,
    decision_stage: str | None = None,
    requires_company_data: bool | None = None,
    fts_shadow_limit: int = 64,
    projection_profile: str = "context",
) -> dict[str, Any] | None:
    """Return every structurally eligible compact scoring row.

    This is the correctness path used by the Guru MCP. FTS5 runs only as an
    observable lexical shadow and never limits rows seen by the exact Python
    scorer. Full payload JSON is point-loaded after final selection.
    """
    root_path = guru_root(root)
    resolved_shadow_limit = max(1, int(fts_shadow_limit))
    if projection_profile not in {"search", "context"}:
        raise ValueError("projection_profile must be 'search' or 'context'")
    try:
        manifest, selected_shards = _selected_author_shards(
            root_path,
            author_keys=author_keys,
        )
        match_expression = _fts_match_expression(query)
        rows_by_id: dict[str, dict[str, Any]] = {}
        shard_diagnostics: list[dict[str, Any]] = []
        for author_key, shard_path, author_payload in selected_shards:
            shard_result = _scan_author_shard_compact_rows(
                shard_path,
                author_key=author_key,
                manifest_fts_enabled=bool(author_payload.get("fts_enabled")),
                match_expression=match_expression,
                object_families=object_families,
                object_types=object_types,
                intent_family=intent_family,
                decision_stage=decision_stage,
                requires_company_data=requires_company_data,
                fts_shadow_limit=resolved_shadow_limit,
                projection_profile=projection_profile,
            )
            for row in shard_result["rows"]:
                reviewed_id = str(row.get("reviewed_id") or "")
                if reviewed_id:
                    rows_by_id[reviewed_id] = row
            shard_diagnostics.append(shard_result["diagnostics"])
    except (
        _GuruIndexUnavailable,
        json.JSONDecodeError,
        OSError,
        sqlite3.DatabaseError,
        TypeError,
        ValueError,
    ):
        return None

    diagnostics = _exact_scan_diagnostics(shard_diagnostics)
    return {
        "rows": [rows_by_id[key] for key in sorted(rows_by_id)],
        "_index": {
            "enabled": True,
            "runtime_mode": "author_shard",
            "schema_version": manifest.get("schema_version"),
            "ranking_projection_version": manifest.get("ranking_projection_version"),
            "manifest_path": str(guru_index_manifest_path(root_path)),
            "author_keys": [author_key for author_key, _path, _payload in selected_shards],
            "candidate_generation": diagnostics,
        },
    }


def load_guru_index_objects_by_id(
    root: str | Path | None,
    *,
    reviewed_ids: Sequence[str],
    author_keys: Sequence[str] | None = None,
) -> dict[str, dict[str, Any]] | None:
    """Load only requested indexed object payloads, or ``None`` for JSONL fallback."""
    requested_ids = _clean_list(reviewed_ids)
    if not requested_ids:
        return {}
    root_path = guru_root(root)
    try:
        _manifest, selected_shards = _selected_author_shards(
            root_path,
            author_keys=author_keys,
        )
        objects_by_id: dict[str, dict[str, Any]] = {}
        for author_key, shard_path, _author_payload in selected_shards:
            conn = _open_author_shard(shard_path, author_key=author_key)
            try:
                for reviewed_id, payload in _load_payloads_by_id(
                    conn,
                    requested_ids,
                ).items():
                    objects_by_id[reviewed_id] = payload
            finally:
                conn.close()
        return {key: objects_by_id[key] for key in sorted(objects_by_id)}
    except (
        _GuruIndexUnavailable,
        json.JSONDecodeError,
        OSError,
        sqlite3.DatabaseError,
        TypeError,
        ValueError,
    ):
        return None


def load_guru_index_relationships_by_id(
    root: str | Path | None,
    *,
    reviewed_ids: Sequence[str],
    author_keys: Sequence[str] | None = None,
) -> dict[str, list[dict[str, Any]]] | None:
    """Load relationships touching only the requested indexed object ids."""
    requested_ids = _clean_list(reviewed_ids)
    if not requested_ids:
        return {}
    root_path = guru_root(root)
    requested_set = set(requested_ids)
    try:
        _manifest, selected_shards = _selected_author_shards(
            root_path,
            author_keys=author_keys,
        )
        relationships_by_key: dict[str, dict[str, Any]] = {}
        for author_key, shard_path, _author_payload in selected_shards:
            conn = _open_author_shard(shard_path, author_key=author_key)
            try:
                placeholders = ", ".join("?" for _ in requested_ids)
                sql = f"""
                    SELECT relationship_key, payload_json
                    FROM relationships
                    WHERE from_id IN ({placeholders})
                       OR to_id IN ({placeholders})
                    ORDER BY relationship_key
                """
                for row in conn.execute(sql, [*requested_ids, *requested_ids]):
                    payload = json.loads(str(row["payload_json"]))
                    relationships_by_key[str(row["relationship_key"])] = payload
            finally:
                conn.close()
    except (
        _GuruIndexUnavailable,
        json.JSONDecodeError,
        OSError,
        sqlite3.DatabaseError,
        TypeError,
        ValueError,
    ):
        return None

    relationships_by_id: dict[str, list[dict[str, Any]]] = {
        reviewed_id: [] for reviewed_id in requested_ids
    }
    for relationship_key in sorted(relationships_by_key):
        relationship = relationships_by_key[relationship_key]
        for endpoint_key in ("from_id", "to_id"):
            endpoint = str(relationship.get(endpoint_key) or "")
            if endpoint in requested_set:
                relationships_by_id[endpoint].append(relationship)
    return relationships_by_id


def _selected_author_shards(
    root_path: Path,
    *,
    author_keys: Sequence[str] | None,
) -> tuple[dict[str, Any], list[tuple[str, Path, Mapping[str, Any]]]]:
    manifest_path = guru_index_manifest_path(root_path)
    manifest = _read_json(manifest_path)
    if manifest.get("schema_version") != GURU_INDEX_SCHEMA_VERSION:
        raise _GuruIndexUnavailable("manifest_schema_mismatch")
    if manifest.get("search_text_version") != GURU_SEARCH_TEXT_VERSION:
        raise _GuruIndexUnavailable("manifest_search_text_version_mismatch")
    if manifest.get("ranking_projection_version") != GURU_RANKING_PROJECTION_VERSION:
        raise _GuruIndexUnavailable("manifest_ranking_projection_version_mismatch")
    source_mismatches = _source_file_mismatches(root_path, manifest)
    if source_mismatches:
        raise _GuruIndexUnavailable("manifest_source_hash_mismatch:" + ",".join(source_mismatches))
    authors_manifest = manifest.get("authors")
    if not isinstance(authors_manifest, Mapping) or not authors_manifest:
        raise _GuruIndexUnavailable("manifest_authors_missing")
    requested_authors = _clean_list(author_keys)
    selected_authors = requested_authors or sorted(str(key) for key in authors_manifest)
    selected_shards: list[tuple[str, Path, Mapping[str, Any]]] = []
    for author_key in selected_authors:
        author_payload = authors_manifest.get(author_key)
        if not isinstance(author_payload, Mapping):
            raise _GuruIndexUnavailable(f"author_missing:{author_key}")
        shard_path = Path(str(author_payload.get("shard_path") or ""))
        if not shard_path.is_file():
            raise _GuruIndexUnavailable(f"shard_missing:{author_key}")
        selected_shards.append((author_key, shard_path, author_payload))
    return manifest, selected_shards


def _scan_author_shard_compact_rows(
    shard_path: Path,
    *,
    author_key: str,
    manifest_fts_enabled: bool,
    match_expression: str | None,
    object_families: Sequence[str] | None,
    object_types: Sequence[str] | None,
    intent_family: str | None,
    decision_stage: str | None,
    requires_company_data: bool | None,
    fts_shadow_limit: int,
    projection_profile: str,
) -> dict[str, Any]:
    conn = _open_author_shard(shard_path, author_key=author_key)
    try:
        clauses, parameters = _object_filter_sql(
            author_key=author_key,
            object_families=object_families,
            object_types=object_types,
            intent_family=intent_family,
            decision_stage=decision_stage,
            requires_company_data=requires_company_data,
        )
        where_sql = " AND ".join(clauses)
        compact_rows, compact_cache_hit = _cached_compact_projection_rows(
            conn,
            shard_path=shard_path,
            where_sql=where_sql,
            parameters=parameters,
            projection_profile=projection_profile,
        )

        fallback_reasons: list[str] = []
        fts_available = manifest_fts_enabled and _fts_table_available(conn)
        fts_shadow_ids: list[str] = []
        if match_expression and fts_available:
            try:
                fts_shadow_ids = _query_fts_candidate_ids(
                    conn,
                    match_expression=match_expression,
                    where_sql=where_sql,
                    parameters=parameters,
                    limit=fts_shadow_limit,
                )
            except sqlite3.OperationalError as exc:
                fts_available = False
                fallback_reasons.append(f"fts_runtime_error:{_compact_sqlite_error(exc)}")
        elif not match_expression:
            fallback_reasons.append("empty_or_nonlexical_query")
        else:
            fallback_reasons.append("fts_unavailable")

        return {
            "rows": compact_rows,
            "diagnostics": {
                "author_key": author_key,
                "mode": "exact_compact_scan",
                "candidate_limit": None,
                "candidate_count": len(compact_rows),
                "structural_total": len(compact_rows),
                "candidate_exhaustive": True,
                "exact_compact_row_count": len(compact_rows),
                "payload_json_row_count": 0,
                "projection_profile": projection_profile,
                "compact_cache_hit": compact_cache_hit,
                "fts_available": fts_available,
                "fts_shadow_limit": fts_shadow_limit,
                "fts_shadow_count": len(fts_shadow_ids),
                "fallback_reasons": fallback_reasons,
            },
        }
    finally:
        conn.close()


def _cached_compact_projection_rows(
    conn: sqlite3.Connection,
    *,
    shard_path: Path,
    where_sql: str,
    parameters: Sequence[Any],
    projection_profile: str,
) -> tuple[list[dict[str, Any]], bool]:
    stat = shard_path.stat()
    cache_key = (
        str(shard_path),
        stat.st_size,
        stat.st_mtime_ns,
        GURU_RANKING_PROJECTION_VERSION,
        projection_profile,
        where_sql,
        tuple(parameters),
    )
    with _COMPACT_ROW_CACHE_LOCK:
        cached = _COMPACT_ROW_CACHE.get(cache_key)
        if cached is not None:
            _COMPACT_ROW_CACHE.move_to_end(cache_key)
            return [dict(row) for row in cached], True

    rows = _query_compact_projection_rows(
        conn,
        where_sql=where_sql,
        parameters=parameters,
        projection_profile=projection_profile,
    )
    with _COMPACT_ROW_CACHE_LOCK:
        _COMPACT_ROW_CACHE[cache_key] = tuple(rows)
        _COMPACT_ROW_CACHE.move_to_end(cache_key)
        while len(_COMPACT_ROW_CACHE) > _COMPACT_ROW_CACHE_MAX_ENTRIES:
            _COMPACT_ROW_CACHE.popitem(last=False)
    return [dict(row) for row in rows], False


def _query_compact_projection_rows(
    conn: sqlite3.Connection,
    *,
    where_sql: str,
    parameters: Sequence[Any],
    projection_profile: str,
) -> list[dict[str, Any]]:
    compact_rows: list[dict[str, Any]] = []
    if projection_profile == "search":
        for row in conn.execute(
            f"""
            SELECT
                o.reviewed_id,
                o.object_family,
                o.author_key,
                o.ranking_object_type,
                o.intent_family,
                o.decision_stage,
                o.requires_company_data,
                o.ranking_text
            FROM objects AS o
            WHERE {where_sql}
            ORDER BY o.reviewed_id
            """,
            parameters,
        ):
            projection = {
                "reviewed_id": str(row["reviewed_id"]),
                "author_key": str(row["author_key"]),
                "_index_object_family": str(row["object_family"]),
                "_ranking_text": str(row["ranking_text"]),
                "requires_company_data": bool(row["requires_company_data"]),
            }
            for source_key, target_key in (
                ("ranking_object_type", "object_type"),
                ("intent_family", "intent_family"),
                ("decision_stage", "decision_stage"),
            ):
                value = row[source_key]
                if value is not None:
                    projection[target_key] = str(value)
            compact_rows.append(projection)
        return compact_rows

    for row in conn.execute(
        f"""
        SELECT o.reviewed_id, o.object_family, o.ranking_text, o.ranking_json
        FROM objects AS o
        WHERE {where_sql}
        ORDER BY o.reviewed_id
        """,
        parameters,
    ):
        projection = json.loads(str(row["ranking_json"]))
        projection["_index_object_family"] = str(row["object_family"])
        projection["_ranking_text"] = str(row["ranking_text"])
        compact_rows.append(projection)
    return compact_rows


def _open_author_shard(shard_path: Path, *, author_key: str) -> sqlite3.Connection:
    conn = sqlite3.connect(f"file:{shard_path}?mode=ro", uri=True)
    conn.row_factory = sqlite3.Row
    try:
        metadata = {
            str(row["key"]): json.loads(str(row["value"]))
            for row in conn.execute("SELECT key, value FROM metadata")
        }
        if metadata.get("schema_version") != GURU_INDEX_SCHEMA_VERSION:
            raise _GuruIndexUnavailable(f"shard_schema_mismatch:{author_key}")
        if metadata.get("search_text_version") != GURU_SEARCH_TEXT_VERSION:
            raise _GuruIndexUnavailable(f"shard_search_text_version_mismatch:{author_key}")
        if metadata.get("ranking_projection_version") != GURU_RANKING_PROJECTION_VERSION:
            raise _GuruIndexUnavailable(f"shard_ranking_projection_version_mismatch:{author_key}")
        if metadata.get("author_key") != author_key:
            raise _GuruIndexUnavailable(f"shard_author_mismatch:{author_key}")
    except Exception:
        conn.close()
        raise
    return conn


def _object_filter_sql(
    *,
    author_key: str,
    object_families: Sequence[str] | None,
    object_types: Sequence[str] | None,
    intent_family: str | None,
    decision_stage: str | None,
    requires_company_data: bool | None,
) -> tuple[list[str], list[Any]]:
    clauses = ["o.author_key = ?"]
    parameters: list[Any] = [author_key]
    for column, values in (
        ("object_family", _clean_list(object_families)),
        ("object_type", _clean_list(object_types)),
    ):
        if values:
            placeholders = ", ".join("?" for _ in values)
            clauses.append(f"o.{column} IN ({placeholders})")
            parameters.extend(values)
    if intent_family:
        clauses.append("o.intent_family = ?")
        parameters.append(str(intent_family))
    if decision_stage:
        clauses.append("o.decision_stage = ?")
        parameters.append(str(decision_stage))
    if requires_company_data is not None:
        clauses.append("o.requires_company_data = ?")
        parameters.append(int(bool(requires_company_data)))
    return clauses, parameters


def _fts_match_expression(query: str) -> str | None:
    tokens: list[str] = []
    for raw_token in _FTS_TOKEN_RE.findall(str(query or "").lower()):
        for candidate in raw_token.split("_"):
            token = candidate.strip()
            if token and token not in tokens:
                tokens.append(token)
    if not tokens:
        return None
    return " OR ".join(f'"{token}"*' if len(token) >= 2 else f'"{token}"' for token in tokens)


def _fts_table_available(conn: sqlite3.Connection) -> bool:
    row = conn.execute("SELECT sql FROM sqlite_master WHERE name = 'objects_fts'").fetchone()
    sql = str(row[0] or "").lower() if row is not None else ""
    return "virtual table" in sql and "fts5" in sql


def _query_fts_candidate_ids(
    conn: sqlite3.Connection,
    *,
    match_expression: str,
    where_sql: str,
    parameters: Sequence[Any],
    limit: int,
) -> list[str]:
    sql = f"""
        SELECT o.reviewed_id, bm25(objects_fts) AS fts_rank
        FROM objects_fts
        JOIN objects AS o ON o.reviewed_id = objects_fts.reviewed_id
        WHERE objects_fts MATCH ? AND {where_sql}
        ORDER BY fts_rank ASC, o.reviewed_id ASC
        LIMIT ?
    """
    return [
        str(row["reviewed_id"]) for row in conn.execute(sql, [match_expression, *parameters, limit])
    ]


def _load_payloads_by_id(
    conn: sqlite3.Connection,
    reviewed_ids: Sequence[str],
) -> dict[str, dict[str, Any]]:
    if not reviewed_ids:
        return {}
    placeholders = ", ".join("?" for _ in reviewed_ids)
    rows = conn.execute(
        f"""
        SELECT reviewed_id, payload_json
        FROM objects
        WHERE reviewed_id IN ({placeholders})
        """,
        list(reviewed_ids),
    )
    payloads: dict[str, dict[str, Any]] = {}
    for row in rows:
        payload = json.loads(str(row["payload_json"]))
        payload.pop("object_family", None)
        payloads[str(row["reviewed_id"])] = payload
    return payloads


def _exact_scan_diagnostics(
    shards: Sequence[Mapping[str, Any]],
) -> dict[str, Any]:
    reasons = sorted(
        {
            str(reason)
            for shard in shards
            for reason in shard.get("fallback_reasons") or []
            if reason
        }
    )
    return {
        "mode": "exact_compact_scan",
        "candidate_count": sum(int(shard.get("candidate_count") or 0) for shard in shards),
        "structural_total": sum(int(shard.get("structural_total") or 0) for shard in shards),
        "candidate_exhaustive": True,
        "exact_compact_row_count": sum(
            int(shard.get("exact_compact_row_count") or 0) for shard in shards
        ),
        "payload_json_row_count": 0,
        "compact_cache_hit_shards": sum(bool(shard.get("compact_cache_hit")) for shard in shards),
        "compact_cache_all_hit": bool(shards)
        and all(bool(shard.get("compact_cache_hit")) for shard in shards),
        "fts_shadow_count": sum(int(shard.get("fts_shadow_count") or 0) for shard in shards),
        "fallback_reasons": reasons,
        "shards": [dict(shard) for shard in shards],
    }


def _compact_sqlite_error(exc: sqlite3.OperationalError) -> str:
    return "_".join(str(exc).strip().lower().split())[:120] or "operational_error"


def _write_author_shard(
    shard_path: Path,
    *,
    author_key: str,
    rows_by_family: Mapping[str, Sequence[dict[str, Any]]],
) -> bool:
    tmp_path = shard_path.with_name(f".{shard_path.name}.tmp")
    if tmp_path.exists():
        tmp_path.unlink()
    conn = sqlite3.connect(tmp_path)
    fts_enabled = False
    try:
        conn.executescript(
            """
            PRAGMA journal_mode=OFF;
            CREATE TABLE metadata (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE objects (
                reviewed_id TEXT PRIMARY KEY,
                author_key TEXT NOT NULL,
                object_family TEXT NOT NULL,
                object_type TEXT,
                object_origin TEXT,
                intent_family TEXT,
                decision_stage TEXT,
                requires_company_data INTEGER NOT NULL DEFAULT 0,
                requires_portfolio_data INTEGER NOT NULL DEFAULT 0,
                label_ko TEXT,
                label_en TEXT,
                summary_ko TEXT,
                ranking_object_type TEXT,
                ranking_text TEXT NOT NULL,
                ranking_json TEXT NOT NULL,
                payload_json TEXT NOT NULL
            );
            CREATE TABLE relationships (
                relationship_key TEXT PRIMARY KEY,
                from_id TEXT,
                to_id TEXT,
                relation_type TEXT,
                explanation_ko TEXT,
                payload_json TEXT NOT NULL
            );
            CREATE TABLE source_anchors (
                reviewed_id TEXT NOT NULL,
                span_id TEXT NOT NULL,
                author_key TEXT NOT NULL,
                object_family TEXT NOT NULL,
                PRIMARY KEY (reviewed_id, span_id)
            );
            CREATE INDEX idx_objects_author_family ON objects(author_key, object_family);
            CREATE INDEX idx_objects_intent ON objects(intent_family);
            CREATE INDEX idx_objects_stage ON objects(decision_stage);
            CREATE INDEX idx_objects_company ON objects(requires_company_data);
            CREATE INDEX idx_relationships_from ON relationships(from_id);
            CREATE INDEX idx_relationships_to ON relationships(to_id);
            CREATE INDEX idx_source_anchors_span ON source_anchors(span_id);
            """
        )
        try:
            conn.execute("CREATE VIRTUAL TABLE objects_fts USING fts5(reviewed_id UNINDEXED, text)")
            fts_enabled = True
        except sqlite3.OperationalError:
            conn.execute("CREATE TABLE objects_fts (reviewed_id TEXT PRIMARY KEY, text TEXT)")

        for family, object_family in OBJECT_FAMILY_BY_FILE.items():
            for row in rows_by_family.get(family, []):
                payload = dict(row)
                reviewed_id = _clean_scalar(payload.get("reviewed_id"))
                if not reviewed_id:
                    continue
                conn.execute(
                    """
                    INSERT OR REPLACE INTO objects (
                        reviewed_id,
                        author_key,
                        object_family,
                        object_type,
                        object_origin,
                        intent_family,
                        decision_stage,
                        requires_company_data,
                        requires_portfolio_data,
                        label_ko,
                        label_en,
                        summary_ko,
                        ranking_object_type,
                        ranking_text,
                        ranking_json,
                        payload_json
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        reviewed_id,
                        author_key,
                        object_family,
                        _object_type(payload, object_family),
                        _clean_scalar(payload.get("object_origin")),
                        _clean_scalar(payload.get("intent_family")),
                        _clean_scalar(payload.get("decision_stage")),
                        int(bool(payload.get("requires_company_data"))),
                        int(_boolish(payload.get("requires_portfolio_data"))),
                        _clean_scalar(payload.get("label_ko")),
                        _clean_scalar(payload.get("label_en")),
                        _clean_scalar(payload.get("summary_ko")),
                        _clean_scalar(payload.get("object_type")),
                        guru_ranking_text(payload),
                        _json_dumps(_ranking_projection(payload, object_family)),
                        _json_dumps(payload),
                    ),
                )
                conn.execute(
                    "INSERT OR REPLACE INTO objects_fts (reviewed_id, text) VALUES (?, ?)",
                    (reviewed_id, _search_text(payload)),
                )
                for span_id in _clean_list(payload.get("supporting_span_ids")):
                    conn.execute(
                        """
                        INSERT OR IGNORE INTO source_anchors (
                            reviewed_id, span_id, author_key, object_family
                        ) VALUES (?, ?, ?, ?)
                        """,
                        (reviewed_id, span_id, author_key, object_family),
                    )

        for relationship in rows_by_family.get("relationships", []):
            relationship_key = _relationship_key(relationship)
            conn.execute(
                """
                INSERT OR REPLACE INTO relationships (
                    relationship_key,
                    from_id,
                    to_id,
                    relation_type,
                    explanation_ko,
                    payload_json
                ) VALUES (?, ?, ?, ?, ?, ?)
                """,
                (
                    relationship_key,
                    _clean_scalar(relationship.get("from_id")),
                    _clean_scalar(relationship.get("to_id")),
                    _clean_scalar(relationship.get("relation_type")),
                    _clean_scalar(relationship.get("explanation_ko")),
                    _json_dumps(relationship),
                ),
            )

        metadata = {
            "schema_version": GURU_INDEX_SCHEMA_VERSION,
            "search_text_version": GURU_SEARCH_TEXT_VERSION,
            "ranking_projection_version": GURU_RANKING_PROJECTION_VERSION,
            "author_key": author_key,
            "generated_at": datetime.now(timezone.utc).isoformat(),
            "fts_enabled": fts_enabled,
        }
        for key, value in metadata.items():
            conn.execute(
                "INSERT OR REPLACE INTO metadata (key, value) VALUES (?, ?)",
                (key, _json_dumps(value)),
            )
        conn.commit()
    finally:
        conn.close()
    tmp_path.replace(shard_path)
    return fts_enabled


def _read_author_shard(
    shard_path: Path,
) -> tuple[dict[str, list[dict[str, Any]]], list[dict[str, Any]]]:
    rows_by_family: dict[str, list[dict[str, Any]]] = {
        family: [] for family in OBJECT_FAMILY_BY_FILE.values()
    }
    relationships: list[dict[str, Any]] = []
    conn = sqlite3.connect(f"file:{shard_path}?mode=ro", uri=True)
    try:
        conn.row_factory = sqlite3.Row
        for row in conn.execute(
            "SELECT object_family, payload_json FROM objects ORDER BY reviewed_id"
        ):
            payload = json.loads(row["payload_json"])
            payload["object_family"] = row["object_family"]
            rows_by_family[row["object_family"]].append(payload)
        for row in conn.execute("SELECT payload_json FROM relationships ORDER BY relationship_key"):
            relationships.append(json.loads(row["payload_json"]))
    finally:
        conn.close()
    return rows_by_family, relationships


def _index_dir(root_path: Path, index_dir: str | Path | None) -> Path:
    if index_dir is None:
        return root_path / GURU_INDEX_DIRNAME
    return Path(index_dir).expanduser().resolve()


def _empty_group() -> dict[str, list[dict[str, Any]]]:
    return {family: [] for family in REVIEWED_FILES}


def _read_jsonl(path: Path) -> list[dict[str, Any]]:
    if not path.is_file():
        return []
    rows = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        payload = json.loads(line)
        if isinstance(payload, dict):
            rows.append(payload)
    return rows


def _read_json(path: Path) -> dict[str, Any]:
    if not path.is_file():
        return {}
    payload = json.loads(path.read_text(encoding="utf-8"))
    return payload if isinstance(payload, dict) else {}


def _write_json_atomic(path: Path, payload: Mapping[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = path.with_name(f".{path.name}.tmp")
    tmp_path.write_text(_json_dumps(payload) + "\n", encoding="utf-8")
    tmp_path.replace(path)


def _json_dumps(payload: Any) -> str:
    return json.dumps(payload, ensure_ascii=False, sort_keys=True)


def _clean_scalar(value: Any) -> str | None:
    if value is None:
        return None
    text = str(value).strip()
    return text or None


def _clean_list(value: Any) -> list[str]:
    if value is None:
        return []
    if isinstance(value, str):
        values = [value]
    elif isinstance(value, Sequence):
        values = list(value)
    else:
        values = [value]
    cleaned = []
    for item in values:
        text = _clean_scalar(item)
        if text and text not in cleaned:
            cleaned.append(text)
    return cleaned


def _object_type(row: Mapping[str, Any], object_family: str) -> str | None:
    return (
        _clean_scalar(row.get("object_type"))
        or _clean_scalar(row.get("data_need_family"))
        or object_family
    )


def _boolish(value: Any) -> bool:
    if isinstance(value, bool):
        return value
    if value is None:
        return False
    if isinstance(value, (int, float)):
        return bool(value)
    return str(value).strip().lower() in {"1", "true", "yes", "y"}


def guru_ranking_text(row: Mapping[str, Any]) -> str:
    """Return the canonical text consumed by Guru ranking functions."""
    parts: list[str] = []
    for key in (
        "author_key",
        "object_type",
        "label_ko",
        "label_en",
        "summary_ko",
        "body_ko",
        "question_pattern_ko",
        "intent_family",
        "decision_stage",
        "answer_section_type",
        "data_need_family",
        "data_need_key",
    ):
        value = row.get(key)
        if isinstance(value, str):
            parts.append(value)
    for key in (
        "example_questions_ko",
        "intent_tags",
        "answer_sections",
        "required_context",
        "company_data_hooks",
    ):
        value = row.get(key)
        if isinstance(value, list):
            parts.extend(str(item) for item in value)
    for key in ("applicability", "specificity", "answer_role"):
        value = row.get(key)
        if isinstance(value, Mapping):
            for nested in value.values():
                if isinstance(nested, str):
                    parts.append(nested)
                elif isinstance(nested, list):
                    parts.extend(str(item) for item in nested)
    return " ".join(parts).lower()


def _ranking_projection(row: Mapping[str, Any], object_family: str) -> dict[str, Any]:
    projection = {
        key: row[key]
        for key in _RANKING_PROJECTION_FIELDS
        if key in row and row.get(key) is not None
    }
    projection["_index_object_family"] = object_family
    specificity = row.get("specificity")
    source_tags = ""
    if isinstance(specificity, Mapping):
        values = specificity.get("source_case_tags")
        if isinstance(values, Sequence) and not isinstance(values, str):
            source_tags = " ".join(str(value).lower() for value in values)
    projection["_ranking_domain_tags"] = sorted(
        context_tags_for_text(f"{guru_ranking_text(row)} {source_tags}")
    )
    return projection


def _search_text(row: Mapping[str, Any]) -> str:
    parts: list[str] = []
    for key in (
        "reviewed_id",
        "author_key",
        "object_family",
        "object_type",
        "label_ko",
        "label_en",
        "summary_ko",
        "summary_en",
        "body_ko",
        "question_pattern_ko",
        "answer_guidance_ko",
        "answer_section_type",
        "data_need_key",
        "data_need_family",
        "intent_family",
        "decision_stage",
    ):
        value = row.get(key)
        if isinstance(value, str) and value.strip():
            parts.append(value.strip())
    for key in (
        "example_questions_ko",
        "intent_tags",
        "answer_sections",
        "required_context",
        "company_data_hooks",
        "portfolio_data_hooks",
    ):
        value = row.get(key)
        if isinstance(value, Sequence) and not isinstance(value, str):
            parts.extend(str(item) for item in value if item)
    for key in ("applicability", "specificity", "answer_role"):
        value = row.get(key)
        if isinstance(value, Mapping):
            parts.append(_json_dumps(value))
    return "\n".join(parts)


def _relationship_key(relationship: Mapping[str, Any]) -> str:
    explicit = _clean_scalar(relationship.get("relationship_id"))
    if explicit:
        return explicit
    raw = _json_dumps(
        {
            "from_id": relationship.get("from_id"),
            "to_id": relationship.get("to_id"),
            "relation_type": relationship.get("relation_type"),
            "explanation_ko": relationship.get("explanation_ko"),
        }
    )
    return "relationship:" + hashlib.sha1(raw.encode("utf-8")).hexdigest()[:20]


def _author_from_reviewed_id(value: Any) -> str | None:
    text = _clean_scalar(value)
    if not text:
        return None
    parts = text.split(":")
    if len(parts) >= 3 and parts[0] == "guru":
        return parts[1]
    return None


def _source_file_record(path: Path, *, rows: int) -> dict[str, Any]:
    if not path.is_file():
        return {
            "path": str(path),
            "exists": False,
            "rows": rows,
            "size": 0,
            "mtime_ns": None,
            "sha256": None,
        }
    stat = path.stat()
    return {
        "path": str(path),
        "exists": True,
        "rows": rows,
        "size": stat.st_size,
        "mtime_ns": stat.st_mtime_ns,
        "sha256": _sha256_file_for_stat(str(path), stat.st_size, stat.st_mtime_ns),
    }


def _source_file_mismatches(root_path: Path, manifest: Mapping[str, Any]) -> list[str]:
    source_files = manifest.get("source_files")
    if not isinstance(source_files, Mapping):
        return ["source_files_manifest_missing"]
    mismatches: list[str] = []
    reviewed_dir = root_path / "reviewed"
    for family, filename in REVIEWED_FILES.items():
        expected = source_files.get(family)
        if not isinstance(expected, Mapping):
            mismatches.append(f"{family}:manifest_missing")
            continue
        path = reviewed_dir / filename
        exists = path.is_file()
        if bool(expected.get("exists")) != exists:
            mismatches.append(f"{family}:existence")
            continue
        if not exists:
            continue
        expected_sha = str(expected.get("sha256") or "")
        if not expected_sha:
            mismatches.append(f"{family}:sha256_missing")
            continue
        stat = path.stat()
        actual_sha = _sha256_file_for_stat(str(path), stat.st_size, stat.st_mtime_ns)
        if actual_sha != expected_sha:
            mismatches.append(f"{family}:sha256")
    return mismatches


@lru_cache(maxsize=256)
def _sha256_file_for_stat(path: str, size: int, mtime_ns: int) -> str:
    del size, mtime_ns
    digest = hashlib.sha256()
    with Path(path).open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _sha256_file(path: Path) -> str | None:
    if not path.is_file():
        return None
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()
