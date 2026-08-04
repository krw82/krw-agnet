"""Deep verification for v3 global spine + company shard releases."""

from __future__ import annotations

import json
import sqlite3
from pathlib import Path
from collections.abc import Mapping
from typing import Any

from krw_capability_runtime.agent_index.cache_seal import read_immutable_sqlite_cache_sha256
from krw_capability_runtime.agent_index.chart_series import (
    CHART_SERIES_RELATIVE_PATH,
    verify_chart_series_index,
)
from krw_capability_runtime.agent_index.router_sidecar import (
    ROUTER_SIDECAR_RELATIVE_PATH,
    immutable_file_sha256,
    verify_router_sidecar,
)
from krw_capability_runtime.agent_index.router_coherence import (
    ROUTER_COHERENCE_RELATIVE_PATH,
    verify_router_coherence,
)
from krw_capability_runtime.agent_index.source_artifact_sqlite import (
    SOURCE_ARTIFACT_SQLITE_BUILDER_VERSION,
    SOURCE_ARTIFACT_SQLITE_SCHEMA_VERSION,
    verify_source_artifact_sqlite,
)
from krw_capability_runtime.agent_index.metric_dictionary import metric_dictionary_binding_errors
from krw_capability_runtime.agent_index.spine_builder import (
    COMPANY_SHARD_SCHEMA_VERSION,
    SHARD_QUALITY_SUMMARY_FORMAT_VERSION,
    SPINE_PROJECTION_VERSION,
    _company_shard_quality_summary,
)
from krw_capability_runtime.agent_index.spine_schema import (
    GLOBAL_SPINE_BUILDER_VERSION,
    GLOBAL_SPINE_LAYOUT,
    GLOBAL_SPINE_REQUIRED_METADATA_KEYS,
    GLOBAL_SPINE_SCHEMA_VERSION,
    GLOBAL_SPINE_TABLES,
    read_global_spine_metadata,
    verify_global_spine_schema,
)


def verify_spine_shard_release(
    release_root: Path,
    *,
    manifest_path: Path | None = None,
    require_manifest: bool = True,
    sample_limit: int = 20,
    deep: bool = True,
) -> dict[str, Any]:
    """Verify a v3 release without any monolith dependency."""
    root = release_root.expanduser().resolve()
    manifest_file = (
        manifest_path.expanduser().resolve()
        if manifest_path is not None
        else root / "manifest.json"
    )
    errors: list[str] = []
    warnings: list[str] = []
    counts: dict[str, int] = {}
    try:
        manifest = json.loads(manifest_file.read_text(encoding="utf-8"))
    except FileNotFoundError:
        manifest = {}
        if require_manifest:
            errors.append("manifest_missing")
    except json.JSONDecodeError as exc:
        manifest = {}
        errors.append(f"manifest_invalid_json:{exc}")

    if manifest:
        if manifest.get("format") != "krw-ontology-release/v3":
            errors.append("manifest_format_unsupported")
        if manifest.get("index_layout") != GLOBAL_SPINE_LAYOUT:
            errors.append("manifest_index_layout_unsupported")
        if manifest.get("monolith_required") is not False:
            errors.append("manifest_monolith_required_not_false")
        errors.extend(_manifest_serving_binding_errors(manifest))

    global_spine_path = _manifest_file_path(
        root, manifest, "global_spine", default="indexes/global_spine.sqlite"
    )
    router_sidecar_path = _manifest_file_path(
        root,
        manifest,
        "router_sidecar",
        default=ROUTER_SIDECAR_RELATIVE_PATH.as_posix(),
    )
    router_coherence_path = _optional_manifest_file_path(
        root,
        manifest,
        "router_coherence",
        default=ROUTER_COHERENCE_RELATIVE_PATH.as_posix(),
    )
    shard_manifest_path = _manifest_file_path(
        root, manifest, "shard_manifest", default="indexes/shard_manifest.json"
    )
    company_shards_dir = _manifest_dir_path(
        root, manifest, "company_shards", default="indexes/companies"
    )
    chart_series_path = _optional_manifest_file_path(
        root,
        manifest,
        "chart_series",
        default=CHART_SERIES_RELATIVE_PATH.as_posix(),
    )

    if manifest and deep:
        errors.extend(_manifest_file_digest_errors(manifest, "global_spine", global_spine_path))
        errors.extend(_manifest_file_digest_errors(manifest, "router_sidecar", router_sidecar_path))
        if router_coherence_path is not None and isinstance(
            ((manifest.get("indexes") or {}).get("router_coherence")),
            Mapping,
        ):
            errors.extend(
                _manifest_file_digest_errors(
                    manifest,
                    "router_coherence",
                    router_coherence_path,
                )
            )
        errors.extend(_manifest_file_digest_errors(manifest, "shard_manifest", shard_manifest_path))
    chart_series_verification: dict[str, Any] | None = None
    chart_series_output = (
        ((manifest.get("indexes") or {}).get("chart_series") or {}) if manifest else {}
    )
    chart_series_required = bool(
        isinstance(chart_series_output, Mapping) and chart_series_output.get("required") is True
    )
    if chart_series_path is not None:
        chart_series_issues: list[str] = []
        if manifest and deep and isinstance(chart_series_output, Mapping):
            chart_series_issues.extend(
                _optional_manifest_file_digest_errors(manifest, "chart_series", chart_series_path)
            )
        if chart_series_path.is_file():
            chart_series_verification = verify_chart_series_index(chart_series_path)
            chart_series_issues.extend(
                str(error) for error in chart_series_verification.get("errors") or []
            )
        elif chart_series_required:
            chart_series_issues.append("chart_series_missing")
        if chart_series_issues:
            if chart_series_required:
                errors.extend(f"chart_series:{issue}" for issue in chart_series_issues)
            else:
                warnings.extend(f"chart_series:{issue}" for issue in chart_series_issues)
    spine_verification = (
        verify_global_spine_schema(global_spine_path)
        if deep
        else _verify_global_spine_schema_light(global_spine_path)
    )
    if not spine_verification["ok"]:
        errors.extend(f"global_spine:{error}" for error in spine_verification["errors"])
    errors.extend(
        f"global_spine:{error}"
        for error in _serving_metadata_binding_errors(spine_verification.get("metadata") or {})
    )
    router_sidecar_verification = verify_router_sidecar(
        router_sidecar_path,
        expected_global_spine_sha256=_manifest_output_sha256(manifest, "global_spine"),
        expected_release_id=(
            str(manifest.get("release_id")) if manifest.get("release_id") else None
        ),
        expected_ranking_profile_sha256=_manifest_output_value(
            manifest,
            "router_sidecar",
            "ranking_profile_sha256",
        ),
        expected_build_fingerprint_sha256=_manifest_output_value(
            manifest,
            "router_sidecar",
            "build_fingerprint_sha256",
        ),
        deep=deep,
    )
    if not router_sidecar_verification["ok"]:
        errors.extend(f"router_sidecar:{error}" for error in router_sidecar_verification["errors"])
    router_coherence_verification: dict[str, Any] | None = None
    router_coherence_output = (
        ((manifest.get("indexes") or {}).get("router_coherence") or {}) if manifest else {}
    )
    if isinstance(router_coherence_output, Mapping) and router_coherence_output:
        if router_coherence_path is None:
            errors.append("router_coherence:router_coherence_missing")
        else:
            router_coherence_verification = verify_router_coherence(
                router_coherence_path,
                expected_global_spine_sha256=_manifest_output_sha256(
                    manifest,
                    "global_spine",
                ),
                expected_release_id=(
                    str(manifest.get("release_id")) if manifest.get("release_id") else None
                ),
                expected_profile_sha256=_manifest_output_value(
                    manifest,
                    "router_coherence",
                    "profile_sha256",
                ),
                deep=deep,
                require_trusted_seal=not deep,
            )
            if not router_coherence_verification["ok"]:
                errors.extend(
                    f"router_coherence:{error}" for error in router_coherence_verification["errors"]
                )

    shard_manifest = _read_json(shard_manifest_path)
    if shard_manifest is None:
        errors.append("shard_manifest_missing_or_invalid")
        shard_entries: dict[str, Any] = {}
    else:
        raw_entries = shard_manifest.get("shards")
        shard_entries = raw_entries if isinstance(raw_entries, dict) else {}
        if not isinstance(raw_entries, dict):
            errors.append("shard_manifest_shards_missing")

    expected_metric_dictionary = manifest.get("metric_dictionary") if manifest else None
    if manifest:
        errors.extend(
            f"metric_dictionary:{error}"
            for error in metric_dictionary_binding_errors(expected_metric_dictionary)
        )
        manifest_company_shards = (manifest.get("indexes") or {}).get("company_shards") or {}
        nested_binding = (
            manifest_company_shards.get("metric_dictionary")
            if isinstance(manifest_company_shards, Mapping)
            else None
        )
        errors.extend(
            f"company_shards:{error}"
            for error in metric_dictionary_binding_errors(
                nested_binding,
                expected=expected_metric_dictionary
                if isinstance(expected_metric_dictionary, Mapping)
                else None,
            )
        )
    if shard_manifest is not None:
        errors.extend(
            f"shard_manifest:{error}"
            for error in metric_dictionary_binding_errors(
                shard_manifest.get("metric_dictionary"),
                expected=expected_metric_dictionary
                if isinstance(expected_metric_dictionary, Mapping)
                else None,
            )
        )

    manifest_shards = ((manifest.get("indexes") or {}).get("company_shards") or {}).get(
        "tickers"
    ) or {}
    if (
        manifest
        and isinstance(manifest_shards, dict)
        and shard_entries
        and set(manifest_shards) != set(shard_entries)
    ):
        errors.append("manifest_shard_manifest_ticker_mismatch")

    shard_results: dict[str, Any] = {}
    if global_spine_path.is_file() and spine_verification["ok"]:
        with sqlite3.connect(global_spine_path) as spine_conn:
            spine_conn.row_factory = sqlite3.Row
            if deep:
                counts.update(_global_counts(spine_conn))
                errors.extend(_global_endpoint_errors(spine_conn, sample_limit=sample_limit))
                errors.extend(
                    global_replica_consistency_errors(
                        spine_conn,
                        sample_limit=sample_limit,
                    )
                )
            for index, (ticker, entry) in enumerate(sorted(shard_entries.items()), start=1):
                shard_path = _resolve_shard_path(root, company_shards_dir, entry)
                if deep:
                    result = _verify_one_shard(
                        spine_conn,
                        shard_path,
                        ticker=str(ticker),
                        schema_name=f"shard_{index}",
                        sample_limit=sample_limit,
                        expected_sha256=_expected_shard_sha256(manifest, str(ticker), entry),
                        shard_entry=entry if isinstance(entry, Mapping) else {},
                        expected_metric_dictionary=(
                            expected_metric_dictionary
                            if isinstance(expected_metric_dictionary, Mapping)
                            else None
                        ),
                    )
                else:
                    result = _verify_one_shard_light(
                        root,
                        shard_path,
                        shard_entry=entry if isinstance(entry, Mapping) else {},
                    )
                shard_results[str(ticker)] = result
                if not result["ok"]:
                    errors.extend(f"shard:{ticker}:{error}" for error in result["errors"])
    elif shard_entries:
        errors.append("global_spine_missing")

    return {
        "ok": not errors,
        "errors": errors,
        "warnings": warnings,
        "release_root": str(root),
        "manifest_path": str(manifest_file),
        "global_spine_path": str(global_spine_path),
        "router_sidecar_path": str(router_sidecar_path),
        "router_coherence_path": (
            str(router_coherence_path) if router_coherence_path is not None else None
        ),
        "shard_manifest_path": str(shard_manifest_path),
        "company_shards_dir": str(company_shards_dir),
        "chart_series_path": str(chart_series_path) if chart_series_path is not None else None,
        "counts": counts,
        "global_spine_verification": spine_verification,
        "router_sidecar_verification": router_sidecar_verification,
        "router_coherence_verification": router_coherence_verification,
        "chart_series_verification": chart_series_verification,
        "shards": shard_results,
        "verification_mode": "spine-shard-release-deep" if deep else "spine-shard-release-light",
        "manifest_required": require_manifest,
        "deep": deep,
    }


def _serving_metadata_binding_errors(metadata: Mapping[str, Any]) -> list[str]:
    expected = {
        "schema_version": GLOBAL_SPINE_SCHEMA_VERSION,
        "builder_version": GLOBAL_SPINE_BUILDER_VERSION,
        "spine_projection_version": SPINE_PROJECTION_VERSION,
        "source_artifact_sqlite_schema_version": SOURCE_ARTIFACT_SQLITE_SCHEMA_VERSION,
        "source_artifact_sqlite_builder_version": SOURCE_ARTIFACT_SQLITE_BUILDER_VERSION,
        "company_shard_schema_version": COMPANY_SHARD_SCHEMA_VERSION,
    }
    return [
        f"serving_binding_mismatch:{key}"
        for key, value in expected.items()
        if metadata.get(key) != value
    ]


def _manifest_serving_binding_errors(manifest: Mapping[str, Any]) -> list[str]:
    errors: list[str] = []
    builder = manifest.get("builder")
    builder = builder if isinstance(builder, Mapping) else {}
    expected_builder = {
        "spine_schema_version": GLOBAL_SPINE_SCHEMA_VERSION,
        "spine_builder_version": GLOBAL_SPINE_BUILDER_VERSION,
        "spine_projection_version": SPINE_PROJECTION_VERSION,
        "source_artifact_sqlite_schema_version": SOURCE_ARTIFACT_SQLITE_SCHEMA_VERSION,
        "source_artifact_sqlite_builder_version": SOURCE_ARTIFACT_SQLITE_BUILDER_VERSION,
        "company_shard_schema_version": COMPANY_SHARD_SCHEMA_VERSION,
    }
    for key, value in expected_builder.items():
        if builder.get(key) != value:
            errors.append(f"manifest_builder_binding_mismatch:{key}")
    indexes = manifest.get("indexes")
    indexes = indexes if isinstance(indexes, Mapping) else {}
    global_spine = indexes.get("global_spine")
    global_spine = global_spine if isinstance(global_spine, Mapping) else {}
    if global_spine.get("schema_version") != GLOBAL_SPINE_SCHEMA_VERSION:
        errors.append("manifest_global_spine_schema_version_mismatch")
    company_shards = indexes.get("company_shards")
    company_shards = company_shards if isinstance(company_shards, Mapping) else {}
    if company_shards.get("schema_version") != COMPANY_SHARD_SCHEMA_VERSION:
        errors.append("manifest_company_shard_schema_version_mismatch")
    if (
        company_shards.get("source_artifact_sqlite_schema_version")
        != SOURCE_ARTIFACT_SQLITE_SCHEMA_VERSION
    ):
        errors.append("manifest_source_artifact_schema_version_mismatch")
    if (
        company_shards.get("source_artifact_sqlite_builder_version")
        != SOURCE_ARTIFACT_SQLITE_BUILDER_VERSION
    ):
        errors.append("manifest_source_artifact_builder_version_mismatch")
    return errors


def _verify_one_shard_light(
    root: Path,
    shard_path: Path,
    *,
    shard_entry: Mapping[str, Any],
) -> dict[str, Any]:
    errors: list[str] = []
    try:
        shard_path.resolve().relative_to(root)
    except ValueError:
        errors.append("shard_path_outside_root")
    if not shard_path.is_file():
        errors.append("shard_missing")
    errors.extend(_quality_summary_manifest_errors(shard_entry))
    return {
        "ok": not errors,
        "errors": errors,
        "path": str(shard_path),
        "counts": {},
        "source_artifact_sqlite_verification": None,
    }


def _verify_global_spine_schema_light(path: Path) -> dict[str, Any]:
    resolved = path.expanduser().resolve()
    errors: list[str] = []
    warnings: list[str] = []
    tables: list[str] = []
    metadata: dict[str, Any] = {}
    if not resolved.exists():
        return {
            "ok": False,
            "path": str(resolved),
            "errors": ["global_spine_missing"],
            "warnings": warnings,
            "tables": tables,
            "metadata": metadata,
            "verification_mode": "global-spine-schema-light",
        }
    try:
        with sqlite3.connect(resolved) as conn:
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
            metadata = read_global_spine_metadata(conn)
    except sqlite3.Error as exc:
        return {
            "ok": False,
            "path": str(resolved),
            "errors": [f"sqlite_error:{exc}"],
            "warnings": warnings,
            "tables": tables,
            "metadata": metadata,
            "verification_mode": "global-spine-schema-light",
        }

    table_set = set(tables)
    for table in GLOBAL_SPINE_TABLES:
        if table not in table_set:
            errors.append(f"global_spine_table_missing:{table}")
    for key in GLOBAL_SPINE_REQUIRED_METADATA_KEYS:
        if key not in metadata:
            errors.append(f"global_spine_metadata_missing:{key}")
    if metadata.get("schema_version") != GLOBAL_SPINE_SCHEMA_VERSION:
        errors.append("global_spine_schema_version_mismatch")
    if metadata.get("index_layout") != GLOBAL_SPINE_LAYOUT:
        errors.append("global_spine_index_layout_mismatch")
    return {
        "ok": not errors,
        "path": str(resolved),
        "errors": errors,
        "warnings": warnings,
        "tables": tables,
        "metadata": metadata,
        "verification_mode": "global-spine-schema-light",
    }


def _verify_one_shard(
    spine_conn: sqlite3.Connection,
    shard_path: Path,
    *,
    ticker: str,
    schema_name: str,
    sample_limit: int,
    expected_sha256: str | None,
    shard_entry: Mapping[str, Any],
    expected_metric_dictionary: Mapping[str, Any] | None,
) -> dict[str, Any]:
    errors: list[str] = []
    counts: dict[str, int] = {}
    if not shard_path.is_file():
        return {
            "ok": False,
            "errors": ["shard_missing"],
            "path": str(shard_path),
            "counts": counts,
            "source_artifact_sqlite_verification": None,
        }
    if expected_sha256 is None:
        errors.append("sha256_missing")
    elif _file_sha256(shard_path) != expected_sha256:
        errors.append("sha256_mismatch")
    errors.extend(_quality_summary_errors(shard_path, shard_entry))
    errors.extend(
        f"manifest_entry:{error}"
        for error in metric_dictionary_binding_errors(
            shard_entry.get("metric_dictionary"),
            expected=expected_metric_dictionary,
        )
    )
    source_artifact_verification = verify_source_artifact_sqlite(shard_path)
    if not source_artifact_verification["ok"]:
        errors.extend(
            f"source_artifact_sqlite:{error}" for error in source_artifact_verification["errors"]
        )
    spine_conn.execute(f"ATTACH DATABASE ? AS {schema_name}", (str(shard_path),))
    try:
        shard_binding = _attached_shard_metric_dictionary_binding(
            spine_conn,
            schema_name,
        )
        errors.extend(
            f"metadata:{error}"
            for error in metric_dictionary_binding_errors(
                shard_binding,
                expected=expected_metric_dictionary,
            )
        )
        metric_lookup_columns = {
            str(row[1])
            for row in spine_conn.execute(
                f"PRAGMA {schema_name}.table_info(metric_lookup)"
            ).fetchall()
        }
        required_metric_columns = {
            "filing_period",
            "observation_period",
            "observation_period_type",
            "observation_start_date",
            "observation_end_date",
            "observation_context_key",
            "value_numeric",
        }
        for column in sorted(required_metric_columns - metric_lookup_columns):
            errors.append(f"metric_lookup_column_missing:{column}")
        counts["shard_objects"] = _count(spine_conn, f"{schema_name}.objects")
        counts["shard_documents"] = _count(spine_conn, f"{schema_name}.documents")
        counts["shard_edges"] = _count(spine_conn, f"{schema_name}.edges")
        counts["shard_quality_events"] = _count(spine_conn, f"{schema_name}.quality_events")
        if required_metric_columns.issubset(metric_lookup_columns):
            invalid_metric_contexts = int(
                spine_conn.execute(
                    f"""
                    SELECT COUNT(*)
                    FROM {schema_name}.metric_lookup
                    WHERE filing_period = ''
                       OR observation_period = ''
                       OR observation_period_type = ''
                       OR observation_context_key = ''
                    """
                ).fetchone()[0]
            )
            if invalid_metric_contexts:
                errors.append(
                    f"metric_lookup_observation_context_invalid:{invalid_metric_contexts}"
                )
            global_metric_period_mismatches = int(
                spine_conn.execute(
                    f"""
                    SELECT COUNT(*)
                    FROM {schema_name}.metric_lookup AS metric
                    JOIN global_object_replica AS replica
                      ON replica.local_object_key = metric.object_id
                     AND replica.ticker = ?
                    JOIN global_metric_spine AS spine
                      ON spine.object_id = replica.object_id
                     AND spine.ticker = replica.ticker
                    WHERE spine.period != metric.observation_period
                    """,
                    (ticker,),
                ).fetchone()[0]
            )
            if global_metric_period_mismatches:
                errors.append(
                    f"global_metric_observation_period_mismatch:{global_metric_period_mismatches}"
                )
        counts["canonical_locator_objects"] = int(
            spine_conn.execute(
                "SELECT COUNT(*) FROM global_object_locator WHERE ticker = ?",
                (ticker,),
            ).fetchone()[0]
        )
        counts["replica_objects"] = int(
            spine_conn.execute(
                "SELECT COUNT(*) FROM global_object_replica WHERE ticker = ?",
                (ticker,),
            ).fetchone()[0]
        )
        counts["replica_edges"] = int(
            spine_conn.execute(
                "SELECT COUNT(*) FROM global_edge_replica WHERE ticker = ?",
                (ticker,),
            ).fetchone()[0]
        )
        counts["document_catalog"] = int(
            spine_conn.execute(
                "SELECT COUNT(*) FROM global_document_catalog WHERE ticker = ?",
                (ticker,),
            ).fetchone()[0]
        )
        missing_locator = int(
            spine_conn.execute(
                f"""
                SELECT COUNT(*)
                FROM {schema_name}.objects AS objects
                LEFT JOIN global_object_replica AS replica
                  ON replica.local_object_key = objects.id
                 AND replica.ticker = ?
                WHERE replica.object_id IS NULL
                """,
                (ticker,),
            ).fetchone()[0]
        )
        if missing_locator:
            errors.append(f"objects_missing_replica:{missing_locator}")
            errors.extend(
                _sample_values(
                    spine_conn,
                    f"""
                    SELECT objects.id
                    FROM {schema_name}.objects AS objects
                    LEFT JOIN global_object_replica AS replica
                      ON replica.local_object_key = objects.id
                     AND replica.ticker = ?
                    WHERE replica.object_id IS NULL
                    ORDER BY objects.id
                    LIMIT ?
                    """,
                    (ticker, sample_limit),
                    prefix="object_missing_replica",
                )
            )
        missing_object = int(
            spine_conn.execute(
                f"""
                SELECT COUNT(*)
                FROM global_object_replica AS replica
                LEFT JOIN {schema_name}.objects AS objects
                  ON objects.id = replica.local_object_key
                WHERE replica.ticker = ?
                  AND objects.id IS NULL
                """,
                (ticker,),
            ).fetchone()[0]
        )
        if missing_object:
            errors.append(f"replica_missing_object:{missing_object}")
        missing_edge_replica = int(
            spine_conn.execute(
                f"""
                SELECT COUNT(*)
                FROM {schema_name}.edges AS edges
                JOIN {schema_name}.objects AS source
                  ON source.id = edges.from_id
                JOIN {schema_name}.objects AS target
                  ON target.id = edges.to_id
                LEFT JOIN global_edge_replica AS replica
                  ON replica.edge_id = CASE
                        WHEN instr(
                            ':' || upper(edges.id) || ':',
                            ':' || upper(?) || ':'
                        ) > 0
                        THEN edges.id
                        ELSE 'scoped:' || ? || ':' || edges.id
                     END
                 AND replica.ticker = ?
                WHERE replica.edge_id IS NULL
                """,
                (ticker, ticker, ticker),
            ).fetchone()[0]
        )
        if missing_edge_replica:
            errors.append(f"edges_missing_replica:{missing_edge_replica}")
        missing_shard_edge = int(
            spine_conn.execute(
                f"""
                SELECT COUNT(*)
                FROM global_edge_replica AS replica
                LEFT JOIN {schema_name}.edges AS edges
                  ON replica.edge_id = CASE
                        WHEN instr(
                            ':' || upper(edges.id) || ':',
                            ':' || upper(?) || ':'
                        ) > 0
                        THEN edges.id
                        ELSE 'scoped:' || ? || ':' || edges.id
                     END
                WHERE replica.ticker = ?
                  AND edges.id IS NULL
                """,
                (ticker, ticker, ticker),
            ).fetchone()[0]
        )
        if missing_shard_edge:
            errors.append(f"replica_missing_edge:{missing_shard_edge}")
        missing_doc = int(
            spine_conn.execute(
                f"""
                SELECT COUNT(*)
                FROM {schema_name}.documents AS docs
                LEFT JOIN global_document_catalog AS catalog
                  ON catalog.ticker = ?
                 AND catalog.document_type = docs.document_type
                 AND catalog.period = docs.period
                WHERE catalog.document_id IS NULL
                """,
                (ticker,),
            ).fetchone()[0]
        )
        if missing_doc:
            errors.append(f"documents_missing_catalog:{missing_doc}")
    finally:
        spine_conn.commit()
        spine_conn.execute(f"DETACH DATABASE {schema_name}")
    return {
        "ok": not errors,
        "errors": errors,
        "path": str(shard_path),
        "counts": counts,
        "source_artifact_sqlite_verification": source_artifact_verification,
    }


def _attached_shard_metric_dictionary_binding(
    conn: sqlite3.Connection,
    schema_name: str,
) -> Mapping[str, Any] | None:
    try:
        row = conn.execute(
            f"SELECT value FROM {schema_name}.metadata WHERE key = 'build'"
        ).fetchone()
    except sqlite3.Error:
        return None
    if row is None:
        return None
    try:
        metadata = json.loads(str(row[0]))
    except json.JSONDecodeError:
        return None
    binding = metadata.get("metric_dictionary")
    return binding if isinstance(binding, Mapping) else None


def _quality_summary_errors(shard_path: Path, shard_entry: Mapping[str, Any]) -> list[str]:
    expected = shard_entry.get("quality_summary")
    manifest_errors = _quality_summary_manifest_errors(shard_entry)
    if manifest_errors:
        return manifest_errors
    actual = _company_shard_quality_summary(shard_path)
    expected_json = json.dumps(expected, ensure_ascii=False, sort_keys=True, default=str)
    actual_json = json.dumps(actual, ensure_ascii=False, sort_keys=True, default=str)
    if expected_json != actual_json:
        return ["quality_summary_mismatch"]
    return []


def _quality_summary_manifest_errors(shard_entry: Mapping[str, Any]) -> list[str]:
    expected = shard_entry.get("quality_summary")
    if not isinstance(expected, Mapping):
        return ["quality_summary_missing"]
    if expected.get("format") != SHARD_QUALITY_SUMMARY_FORMAT_VERSION:
        return ["quality_summary_format_mismatch"]
    return []


def _manifest_file_digest_errors(manifest: dict[str, Any], role: str, path: Path) -> list[str]:
    output = ((manifest.get("indexes") or {}).get(role) or {}) if manifest else {}
    expected = output.get("sha256") if isinstance(output, dict) else None
    if not isinstance(expected, str) or not expected:
        return [f"{role}_sha256_missing"]
    if not path.is_file():
        return []
    return [f"{role}_sha256_mismatch"] if _file_sha256(path) != expected else []


def _manifest_output_sha256(manifest: Mapping[str, Any], role: str) -> str | None:
    output = ((manifest.get("indexes") or {}).get(role) or {}) if manifest else {}
    expected = output.get("sha256") if isinstance(output, Mapping) else None
    return str(expected) if isinstance(expected, str) and expected else None


def _manifest_output_value(
    manifest: Mapping[str, Any],
    role: str,
    key: str,
) -> str | None:
    output = ((manifest.get("indexes") or {}).get(role) or {}) if manifest else {}
    value = output.get(key) if isinstance(output, Mapping) else None
    return str(value) if isinstance(value, str) and value else None


def _optional_manifest_file_digest_errors(
    manifest: dict[str, Any], role: str, path: Path
) -> list[str]:
    output = ((manifest.get("indexes") or {}).get(role) or {}) if manifest else {}
    if not isinstance(output, dict):
        return []
    expected = output.get("sha256")
    if not isinstance(expected, str) or not expected:
        return [f"{role}_sha256_missing"]
    if not path.is_file():
        return [f"{role}_missing"]
    return [f"{role}_sha256_mismatch"] if _file_sha256(path) != expected else []


def _expected_shard_sha256(manifest: dict[str, Any], ticker: str, shard_entry: Any) -> str | None:
    manifest_entry = (
        (
            (
                ((manifest.get("indexes") or {}).get("company_shards") or {}).get("tickers") or {}
            ).get(ticker)
        )
        if manifest
        else None
    )
    if isinstance(manifest_entry, dict):
        expected = manifest_entry.get("sha256")
        if isinstance(expected, str) and expected:
            return expected
    if isinstance(shard_entry, dict):
        expected = shard_entry.get("sha256")
        if isinstance(expected, str) and expected:
            return expected
    return None


def _file_sha256(path: Path) -> str:
    cached = read_immutable_sqlite_cache_sha256(path)
    return cached if cached is not None else immutable_file_sha256(path)


def _global_endpoint_errors(conn: sqlite3.Connection, *, sample_limit: int) -> list[str]:
    errors: list[str] = []
    edge_from_missing = int(
        conn.execute(
            """
            SELECT COUNT(*)
            FROM global_edge_spine AS edge
            LEFT JOIN global_object_locator AS locator
              ON locator.object_id = edge.from_object_id
            WHERE locator.object_id IS NULL
            """
        ).fetchone()[0]
    )
    edge_to_missing = int(
        conn.execute(
            """
            SELECT COUNT(*)
            FROM global_edge_spine AS edge
            LEFT JOIN global_object_locator AS locator
              ON locator.object_id = edge.to_object_id
            WHERE locator.object_id IS NULL
            """
        ).fetchone()[0]
    )
    if edge_from_missing:
        errors.append(f"edge_from_endpoint_missing:{edge_from_missing}")
    if edge_to_missing:
        errors.append(f"edge_to_endpoint_missing:{edge_to_missing}")
    chain_from_missing = int(
        conn.execute(
            """
            SELECT COUNT(*)
            FROM global_chain_index AS link
            LEFT JOIN global_object_locator AS locator
              ON locator.object_id = link.from_object_id
            WHERE link.from_object_id IS NOT NULL
              AND locator.object_id IS NULL
            """
        ).fetchone()[0]
    )
    chain_to_missing = int(
        conn.execute(
            """
            SELECT COUNT(*)
            FROM global_chain_index AS link
            LEFT JOIN global_object_locator AS locator
              ON locator.object_id = link.to_object_id
            WHERE link.to_object_id IS NOT NULL
              AND locator.object_id IS NULL
            """
        ).fetchone()[0]
    )
    if chain_from_missing:
        errors.append(f"chain_from_endpoint_missing:{chain_from_missing}")
    if chain_to_missing:
        errors.append(f"chain_to_endpoint_missing:{chain_to_missing}")
    if errors:
        errors.extend(
            _sample_values(
                conn,
                """
                SELECT edge.edge_id
                FROM global_edge_spine AS edge
                LEFT JOIN global_object_locator AS from_locator
                  ON from_locator.object_id = edge.from_object_id
                LEFT JOIN global_object_locator AS to_locator
                  ON to_locator.object_id = edge.to_object_id
                WHERE from_locator.object_id IS NULL
                   OR to_locator.object_id IS NULL
                ORDER BY edge.edge_id
                LIMIT ?
                """,
                (sample_limit,),
                prefix="edge_endpoint_sample",
            )
        )
    return errors


def global_replica_consistency_errors(
    conn: sqlite3.Connection,
    *,
    sample_limit: int = 20,
) -> list[str]:
    """Verify occurrence-level object/edge/chain identities in a global spine."""
    checks = (
        (
            "object_replica_orphan_canonical",
            """
            SELECT replica.object_id AS id
            FROM global_object_replica AS replica
            LEFT JOIN global_object_locator AS canonical
              ON canonical.object_id = replica.object_id
            WHERE canonical.object_id IS NULL
            """,
        ),
        (
            "edge_replica_orphan_canonical",
            """
            SELECT replica.edge_id AS id
            FROM global_edge_replica AS replica
            LEFT JOIN global_edge_spine AS canonical
              ON canonical.edge_id = replica.edge_id
            WHERE canonical.edge_id IS NULL
            """,
        ),
        (
            "edge_replica_from_occurrence_missing",
            """
            SELECT edge.edge_id AS id
            FROM global_edge_replica AS edge
            LEFT JOIN global_object_replica AS endpoint
              ON endpoint.object_id = edge.from_object_id
             AND endpoint.ticker = edge.ticker
            WHERE endpoint.object_id IS NULL
            """,
        ),
        (
            "edge_replica_to_occurrence_missing",
            """
            SELECT edge.edge_id AS id
            FROM global_edge_replica AS edge
            LEFT JOIN global_object_replica AS endpoint
              ON endpoint.object_id = edge.to_object_id
             AND endpoint.ticker = edge.ticker
            WHERE endpoint.object_id IS NULL
            """,
        ),
        (
            "chain_from_occurrence_missing",
            """
            SELECT link.link_id AS id
            FROM global_chain_index AS link
            LEFT JOIN global_object_replica AS endpoint
              ON endpoint.object_id = link.from_object_id
             AND endpoint.ticker = link.from_ticker
            WHERE link.from_object_id IS NOT NULL
              AND endpoint.object_id IS NULL
            """,
        ),
        (
            "chain_to_occurrence_missing",
            """
            SELECT link.link_id AS id
            FROM global_chain_index AS link
            LEFT JOIN global_object_replica AS endpoint
              ON endpoint.object_id = link.to_object_id
             AND endpoint.ticker = link.to_ticker
            WHERE link.to_object_id IS NOT NULL
              AND endpoint.object_id IS NULL
            """,
        ),
    )
    errors: list[str] = []
    for code, select_sql in checks:
        count = int(conn.execute(f"SELECT COUNT(*) FROM ({select_sql}) AS failures").fetchone()[0])
        if not count:
            continue
        errors.append(f"{code}:{count}")
        errors.extend(
            _sample_values(
                conn,
                select_sql + " ORDER BY id LIMIT ?",
                (max(1, int(sample_limit)),),
                prefix=f"{code}_sample",
            )
        )
    return errors


def _global_counts(conn: sqlite3.Connection) -> dict[str, int]:
    tables = (
        "global_object_locator",
        "global_object_replica",
        "global_document_catalog",
        "global_edge_spine",
        "global_edge_replica",
        "global_factor_spine",
        "global_topic_spine",
        "global_metric_spine",
        "global_entity_spine",
        "global_counterparty_spine",
        "global_chain_index",
    )
    return {table: _count(conn, table) for table in tables}


def _manifest_file_path(root: Path, manifest: dict[str, Any], role: str, *, default: str) -> Path:
    raw = (((manifest.get("indexes") or {}).get(role) or {}).get("path")) if manifest else None
    if not isinstance(raw, str) or not raw:
        raw = default
    candidate = Path(raw)
    return (
        candidate.expanduser().resolve()
        if candidate.is_absolute()
        else (root / candidate).resolve()
    )


def _optional_manifest_file_path(
    root: Path, manifest: dict[str, Any], role: str, *, default: str
) -> Path | None:
    output = ((manifest.get("indexes") or {}).get(role) or {}) if manifest else {}
    raw = output.get("path") if isinstance(output, Mapping) else None
    if not isinstance(raw, str) or not raw:
        fallback = (root / default).resolve()
        return fallback if fallback.exists() else None
    candidate = Path(raw)
    return (
        candidate.expanduser().resolve()
        if candidate.is_absolute()
        else (root / candidate).resolve()
    )


def _manifest_dir_path(root: Path, manifest: dict[str, Any], role: str, *, default: str) -> Path:
    raw = (((manifest.get("indexes") or {}).get(role) or {}).get("dir")) if manifest else None
    if not isinstance(raw, str) or not raw:
        raw = default
    candidate = Path(raw)
    return (
        candidate.expanduser().resolve()
        if candidate.is_absolute()
        else (root / candidate).resolve()
    )


def _resolve_shard_path(root: Path, company_shards_dir: Path, entry: Mapping[str, Any]) -> Path:
    raw = entry.get("path") or entry.get("shard_path")
    if not isinstance(raw, str) or not raw:
        return company_shards_dir / "<missing>"
    candidate = Path(raw)
    if candidate.is_absolute():
        return candidate.expanduser().resolve()
    if candidate.parts and candidate.parts[0] == "indexes":
        return (root / candidate).resolve()
    return (root / "indexes" / candidate).resolve()


def _read_json(path: Path) -> dict[str, Any] | None:
    try:
        parsed = json.loads(path.read_text(encoding="utf-8"))
    except (FileNotFoundError, json.JSONDecodeError):
        return None
    return parsed if isinstance(parsed, dict) else None


def _count(conn: sqlite3.Connection, table_name: str) -> int:
    return int(conn.execute(f"SELECT COUNT(*) FROM {table_name}").fetchone()[0])


def _sample_values(
    conn: sqlite3.Connection,
    query: str,
    params: tuple[Any, ...],
    *,
    prefix: str,
) -> list[str]:
    return [f"{prefix}:{row[0]}" for row in conn.execute(query, params).fetchall()]
