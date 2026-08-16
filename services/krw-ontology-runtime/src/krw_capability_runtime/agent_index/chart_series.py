"""Chart-ready metric series sidecar for v4 release indexes."""

from __future__ import annotations

import hashlib
import json
import os
import re
import sqlite3
import time
from collections.abc import Iterable, Mapping, Sequence
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from krw_capability_runtime.agent_index.semantic_identity import (
    project_local_identity,
    project_object_identity,
)

CHART_SERIES_SCHEMA_VERSION = "krw-ontology-chart-series/v2"
CHART_SERIES_BUILDER_VERSION = "chart-series-builder/v3"
CHART_SERIES_RELATIVE_PATH = Path("indexes") / "chart_series.sqlite"
CHART_SERIES_TABLES = (
    "chart_series_metadata",
    "chart_series",
    "chart_series_points",
)


def chart_series_runtime_enabled() -> bool:
    """Return whether the optional presentation sidecar may be used.

    Production is deliberately fail-closed: the sealed local launcher also
    exports ``0``.  A local developer can opt in explicitly with ``1`` when
    working on the presentation path, but an omitted or inherited environment
    variable must never turn charts on by accident.
    """
    return os.environ.get("KRW_CHART_SERIES_ENABLED", "0").strip().lower() in {
        "1",
        "true",
        "on",
        "yes",
    }

_PERIODIC_METRICS = {
    "adjusted_free_cash_flow",
    "capital_expenditures",
    "capex",
    "eps",
    "free_cash_flow",
    "gross_margin",
    "ma_cash_outflow",
    "ma_related_costs",
    "net_income",
    "net_sales",
    "operating_cash_flow",
    "operating_income",
    "operating_margin",
    "research_and_development",
    "selling_general_and_admin",
    "revenue",
    "share_repurchase",
    "stock_based_compensation",
}
_POINT_IN_TIME_METRICS = {
    "cash_and_cash_equivalents",
    "cash_and_equivalents",
    "total_assets",
    "total_debt",
    "total_liabilities",
}
CHART_SAFE_CANONICAL_METRICS = frozenset(_PERIODIC_METRICS | _POINT_IN_TIME_METRICS)
_CHART_METRIC_ALIASES = {
    "capex": "capital_expenditures",
    "cash_and_equivalents": "cash_and_cash_equivalents",
}


@dataclass(frozen=True)
class ChartSeriesBuildResult:
    path: Path
    counts: Mapping[str, int]
    verification: Mapping[str, Any]
    elapsed_ms: int


def build_chart_series_index(
    release_root: Path | str,
    *,
    shard_manifest_path: Path | str | None = None,
    output_path: Path | str | None = None,
    release_id: str | None = None,
    source_manifest_hash: str | None = None,
) -> ChartSeriesBuildResult:
    """Build indexes/chart_series.sqlite from company shard metric_lookup tables."""
    started_at = time.perf_counter()
    root = Path(release_root).expanduser().resolve()
    manifest_path = (
        Path(shard_manifest_path).expanduser().resolve()
        if shard_manifest_path is not None
        else root / "indexes" / "shard_manifest.json"
    )
    target_path = (
        Path(output_path).expanduser().resolve()
        if output_path is not None
        else root / CHART_SERIES_RELATIVE_PATH
    )
    shard_manifest = _read_json(manifest_path)
    shards = (
        shard_manifest.get("shards") if isinstance(shard_manifest.get("shards"), Mapping) else {}
    )

    target_path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = _temporary_sqlite_path(target_path)
    if tmp_path.exists():
        tmp_path.unlink()
    try:
        with sqlite3.connect(tmp_path) as conn:
            conn.row_factory = sqlite3.Row
            conn.execute("PRAGMA journal_mode = WAL")
            conn.execute("PRAGMA synchronous = NORMAL")
            create_chart_series_schema(conn)
            series_points: dict[tuple[str, str], dict[str, Any]] = {}
            series_meta: dict[str, dict[str, Any]] = {}
            for ticker, entry in sorted(shards.items()):
                if not isinstance(entry, Mapping):
                    continue
                shard_path = _resolve_shard_path(root, entry)
                if not shard_path.is_file():
                    continue
                _collect_shard_chart_points(
                    shard_path,
                    ticker=str(ticker).upper(),
                    series_meta=series_meta,
                    series_points=series_points,
                )
            _write_chart_series_rows(conn, series_meta=series_meta, series_points=series_points)
            counts = {
                "series": _count(conn, "chart_series"),
                "points": _count(conn, "chart_series_points"),
                "tickers": int(
                    conn.execute("SELECT COUNT(DISTINCT ticker) FROM chart_series").fetchone()[0]
                ),
            }
            _write_metadata(
                conn,
                {
                    "schema_version": CHART_SERIES_SCHEMA_VERSION,
                    "builder_version": CHART_SERIES_BUILDER_VERSION,
                    "release_id": release_id,
                    "source_manifest_hash": source_manifest_hash,
                    "built_at": datetime.now(timezone.utc).isoformat(),
                    "counts": counts,
                },
            )
            conn.commit()
            conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        os.replace(tmp_path, target_path)
    finally:
        _cleanup_sqlite_sidecars(tmp_path)

    verification = verify_chart_series_index(target_path)
    if not verification.get("ok"):
        target_path.unlink(missing_ok=True)
        raise RuntimeError(
            "chart series failed verification: "
            + ", ".join(str(error) for error in verification.get("errors") or [])
        )
    return ChartSeriesBuildResult(
        path=target_path,
        counts=verification.get("counts") or {},
        verification=verification,
        elapsed_ms=int((time.perf_counter() - started_at) * 1000),
    )


def create_chart_series_schema(conn: sqlite3.Connection) -> None:
    conn.executescript(
        """
        CREATE TABLE IF NOT EXISTS chart_series_metadata (
            key TEXT PRIMARY KEY,
            value_json TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS chart_series (
            series_key TEXT PRIMARY KEY,
            ticker TEXT NOT NULL,
            canonical_metric TEXT NOT NULL,
            metric_name TEXT,
            label TEXT NOT NULL,
            unit TEXT,
            scope_kind TEXT NOT NULL,
            scope_key TEXT NOT NULL,
            scope_label TEXT NOT NULL,
            scope_dimension TEXT,
            composition_eligible INTEGER NOT NULL DEFAULT 0,
            currency TEXT,
            period_type TEXT NOT NULL,
            basis TEXT NOT NULL,
            duration TEXT NOT NULL,
            source_class TEXT NOT NULL,
            statement_family TEXT NOT NULL,
            point_count INTEGER NOT NULL,
            first_period TEXT,
            last_period TEXT,
            first_sort_key INTEGER,
            last_sort_key INTEGER,
            quality_flags_json TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS chart_series_points (
            series_key TEXT NOT NULL,
            ticker TEXT NOT NULL,
            period TEXT NOT NULL,
            period_basis TEXT,
            period_start TEXT,
            period_end TEXT,
            fiscal_year INTEGER,
            fiscal_quarter INTEGER,
            period_sort_key INTEGER NOT NULL,
            document_type TEXT,
            document_period TEXT,
            source_document_id TEXT,
            source_sort_key INTEGER,
            value REAL NOT NULL,
            formatted_value TEXT,
            currency TEXT,
            object_id TEXT NOT NULL,
            trace_status TEXT,
            metric_lineage_status TEXT,
            PRIMARY KEY (series_key, period),
            FOREIGN KEY (series_key) REFERENCES chart_series(series_key)
        );

        CREATE INDEX IF NOT EXISTS idx_chart_series_ticker_metric
            ON chart_series(ticker, canonical_metric, scope_kind, period_type);
        CREATE INDEX IF NOT EXISTS idx_chart_series_ticker_scope
            ON chart_series(ticker, scope_kind, scope_key);
        CREATE INDEX IF NOT EXISTS idx_chart_series_points_series_sort
            ON chart_series_points(series_key, period_sort_key);
        CREATE INDEX IF NOT EXISTS idx_chart_series_points_ticker_metric_period
            ON chart_series_points(ticker, period_sort_key);
        """
    )


def verify_chart_series_index(path: Path | str, *, deep: bool = True) -> dict[str, Any]:
    resolved = Path(path).expanduser().resolve()
    errors: list[str] = []
    warnings: list[str] = []
    counts: dict[str, int] = {}
    metadata: dict[str, Any] = {}
    tables: list[str] = []
    if not resolved.exists():
        return {
            "ok": False,
            "errors": ["chart_series_missing"],
            "warnings": warnings,
            "path": str(resolved),
            "tables": tables,
            "metadata": metadata,
            "counts": counts,
            "verification_mode": "chart-series",
        }
    if not resolved.is_file():
        return {
            "ok": False,
            "errors": ["chart_series_not_file"],
            "warnings": warnings,
            "path": str(resolved),
            "tables": tables,
            "metadata": metadata,
            "counts": counts,
            "verification_mode": "chart-series",
        }
    try:
        with sqlite3.connect(resolved) as conn:
            conn.row_factory = sqlite3.Row
            tables = sorted(
                str(row[0])
                for row in conn.execute(
                    "SELECT name FROM sqlite_master WHERE type IN ('table', 'view')"
                ).fetchall()
            )
            table_set = set(tables)
            for table in CHART_SERIES_TABLES:
                if table not in table_set:
                    errors.append(f"table_missing:{table}")
            if not errors:
                metadata = read_chart_series_metadata(conn)
                if metadata.get("schema_version") != CHART_SERIES_SCHEMA_VERSION:
                    errors.append("schema_version_mismatch")
                if metadata.get("builder_version") != CHART_SERIES_BUILDER_VERSION:
                    errors.append("builder_version_mismatch")
                counts = {
                    "series": _count(conn, "chart_series"),
                    "points": _count(conn, "chart_series_points"),
                    "tickers": int(
                        conn.execute("SELECT COUNT(DISTINCT ticker) FROM chart_series").fetchone()[
                            0
                        ]
                    ),
                }
                if deep:
                    orphan_points = int(
                        conn.execute(
                            """
                            SELECT COUNT(*)
                            FROM chart_series_points AS point
                            LEFT JOIN chart_series AS series
                              ON series.series_key = point.series_key
                            WHERE series.series_key IS NULL
                            """
                        ).fetchone()[0]
                    )
                    if orphan_points:
                        errors.append(f"orphan_points:{orphan_points}")
                    mismatched_point_counts = int(
                        conn.execute(
                            """
                            SELECT COUNT(*)
                            FROM chart_series AS series
                            LEFT JOIN (
                                SELECT series_key, COUNT(*) AS point_count
                                FROM chart_series_points
                                GROUP BY series_key
                            ) AS actual
                              ON actual.series_key = series.series_key
                            WHERE series.point_count != COALESCE(actual.point_count, 0)
                            """
                        ).fetchone()[0]
                    )
                    if mismatched_point_counts:
                        errors.append(f"series_point_count_mismatch:{mismatched_point_counts}")
    except sqlite3.Error as exc:
        errors.append(f"sqlite_error:{exc}")
    return {
        "ok": not errors,
        "errors": errors,
        "warnings": warnings,
        "path": str(resolved),
        "tables": tables,
        "metadata": metadata,
        "counts": counts,
        "verification_mode": "chart-series-deep" if deep else "chart-series-light",
    }


def read_chart_series_metadata(conn: sqlite3.Connection) -> dict[str, Any]:
    rows = conn.execute("SELECT key, value_json FROM chart_series_metadata").fetchall()
    metadata: dict[str, Any] = {}
    for row in rows:
        try:
            metadata[str(row["key"])] = json.loads(str(row["value_json"]))
        except (json.JSONDecodeError, TypeError):
            metadata[str(row["key"])] = row["value_json"]
    return metadata


def query_chart_series_pack(
    path: Path | str,
    *,
    tickers: Sequence[str],
    chart_clauses: Sequence[Mapping[str, Any]] | None = None,
    limit_series: int = 20,
    limit_points: int = 12,
    release_id: str | None = None,
    source_manifest_hash: str | None = None,
) -> dict[str, Any] | None:
    """Return a presentation-channel metric pack from the chart sidecar.

    Chart eligibility is derived only from validated SearchPlan clauses.  A
    natural-language question is deliberately not inspected here: a company
    description or risk question must never cause a broad metric scan merely
    because it contains a word such as "trend".  The emitted pack is private
    presentation data (MCP ``_meta``), never model-visible ResearchState.
    """
    resolved = Path(path).expanduser().resolve()
    if not resolved.is_file():
        return None
    ticker_values = [str(ticker).upper() for ticker in tickers if str(ticker or "").strip()]
    if not ticker_values:
        return None
    eligible_clauses = _eligible_chart_clauses(chart_clauses, ticker_values)
    if not eligible_clauses:
        return None
    metric_candidates = _unique(
        metric
        for clause in eligible_clauses
        for metric in clause["metrics"]
    )
    scope_candidates = _unique(
        _slug(dimension)
        for clause in eligible_clauses
        for dimension in clause["metric_dimensions"]
        if _slug(dimension)
    )
    dimension_requested = any(
        clause["metric_scope"] in {"dimensioned", "any"} or clause["metric_dimensions"]
        for clause in eligible_clauses
    )
    try:
        with sqlite3.connect(resolved) as conn:
            conn.row_factory = sqlite3.Row
            # Query one ticker/metric pair at a time.  A single global LIMIT
            # can fill the result with one company's segments and silently
            # remove another ticker or the total row required to reconcile a
            # composition.  The pair bound keeps the sidecar lookup bounded
            # without making the result dependent on unrelated rows.
            pair_scopes: dict[tuple[str, str], set[str]] = {}
            coverage_pairs: list[tuple[str, str]] = []
            seen_coverage_pairs: set[tuple[str, str]] = set()
            for clause in eligible_clauses:
                dimensions = {
                    _slug(dimension)
                    for dimension in clause["metric_dimensions"]
                    if _slug(dimension)
                }
                for ticker in clause["tickers"]:
                    for metric in clause["metrics"]:
                        pair = (ticker, metric)
                        pair_scopes.setdefault(pair, set()).update(dimensions)
                        if pair not in seen_coverage_pairs:
                            coverage_pairs.append(pair)
                            seen_coverage_pairs.add(pair)

            pair_limit = max(24, min(96, max(1, int(limit_series)) * 8))
            rows_by_key: dict[str, sqlite3.Row] = {}
            for (ticker, metric), requested_scopes in sorted(pair_scopes.items()):
                scope_order = "CASE WHEN scope_kind = 'company_total' THEN 0 ELSE 1 END"
                scope_params: list[Any] = []
                if requested_scopes:
                    placeholders = ",".join("?" for _ in requested_scopes)
                    scope_order = (
                        f"CASE WHEN scope_key IN ({placeholders}) THEN 0 "
                        "WHEN scope_kind = 'company_total' THEN 1 ELSE 2 END"
                    )
                    scope_params.extend(sorted(requested_scopes))
                pair_rows = conn.execute(
                    f"""
                    SELECT *
                    FROM chart_series
                    WHERE ticker = ? AND canonical_metric = ?
                    ORDER BY {scope_order}, point_count DESC, last_sort_key DESC,
                             scope_kind, scope_key, series_key
                    LIMIT ?
                    """,
                    [ticker, metric, *scope_params, pair_limit],
                ).fetchall()
                for row in pair_rows:
                    key = str(row["series_key"] or "")
                    if key:
                        rows_by_key.setdefault(key, row)

            rows = list(rows_by_key.values())
            rows = _select_chart_series_rows(
                rows,
                limit=max(1, min(int(limit_series), 20)),
                metric_candidates=metric_candidates,
                scope_candidates=scope_candidates,
                dimension_requested=dimension_requested,
                coverage_pairs=coverage_pairs,
            )
            if not rows and not metric_candidates:
                return None
            if not rows and metric_candidates:
                return None
            series: list[dict[str, Any]] = []
            for row in rows:
                points = _chart_series_points(
                    conn,
                    str(row["series_key"]),
                    limit=max(1, min(int(limit_points), 48)),
                )
                if not points:
                    continue
                series.append(
                    {
                        "series_key": row["series_key"],
                        "label": row["label"],
                        "ticker": row["ticker"],
                        "metric_role": "metric",
                        "metric_name": row["metric_name"],
                        "canonical_metric": row["canonical_metric"],
                        "unit": row["unit"],
                        "scope": {
                            "kind": row["scope_kind"],
                            "key": row["scope_key"],
                            "label": row["scope_label"],
                            "dimension": _col(row, "scope_dimension"),
                            "composition_eligible": bool(_col(row, "composition_eligible")),
                        },
                        "currency": _col(row, "currency"),
                        "basis": row["basis"],
                        "duration": row["duration"],
                        "source_class": row["source_class"],
                        "statement_family": row["statement_family"],
                        "period_type": row["period_type"],
                        "points": points,
                        "periods": [point["period"] for point in points],
                        "point_count": len(points),
                    }
                )
            if not series:
                return None
            pack = {
                "schema_version": 2,
                "mode": "chart_series_sidecar",
                "chart_clauses": eligible_clauses,
                "result_count": sum(series_item["point_count"] for series_item in series),
                "series": series,
                "roles": ["metric"],
                "quality": {
                    "chart_safe": True,
                    "source": "chart_series_sidecar",
                    "missing_parts": [],
                },
                "render_hints": {
                    "prefer_chart": True,
                    "prefer_indexed_axis": True,
                    "hide_raw_y_axis_amounts": True,
                    "do_not_requery_per_metric": True,
                },
                "diagnostics": {
                    "metric_candidates": metric_candidates,
                    "sidecar_schema_version": CHART_SERIES_SCHEMA_VERSION,
                    "eligibility_source": "validated_search_plan_clauses",
                },
            }
            if release_id:
                pack["release_id"] = str(release_id)
            if source_manifest_hash:
                pack["source_manifest_hash"] = str(source_manifest_hash)
            return pack
    except sqlite3.Error:
        return None


def chart_series_index_status(path: Path | str) -> dict[str, Any]:
    resolved = Path(path).expanduser().resolve()
    if not resolved.exists():
        return {"available": False, "path": str(resolved), "reason": "missing"}
    verification = verify_chart_series_index(resolved, deep=False)
    return {
        "available": bool(verification.get("ok")),
        "path": str(resolved),
        "reason": None if verification.get("ok") else "verification_failed",
        "verification": verification,
    }


def _eligible_chart_clauses(
    clauses: Sequence[Mapping[str, Any]] | None,
    plan_tickers: Sequence[str],
) -> list[dict[str, Any]]:
    """Return only deterministic, metric-backed chart intents from the plan.

    This intentionally does not infer intent from ``question`` or from the
    presence of arbitrary sidecar rows.  A trend needs an explicit temporal
    window, a comparison needs at least two effective tickers, and a
    composition needs an explicit dimensioned metric clause.  The returned
    values are raw validated clause intent: Python only decides whether a
    clause is worth querying, while the Rust compiler remains the single
    authority for the final trend/comparison/composition decision.
    """
    if not clauses:
        return []
    plan_scope = _unique(str(ticker).strip().upper() for ticker in plan_tickers)
    eligible: list[dict[str, Any]] = []
    for raw in clauses:
        if not isinstance(raw, Mapping):
            continue
        metrics = _unique(
            _canonical_chart_metric(metric)
            for metric in raw.get("metrics") or []
            if _canonical_chart_metric(metric) is not None
        )
        if not metrics:
            continue
        clause_tickers = _unique(
            str(ticker).strip().upper()
            for ticker in raw.get("tickers") or []
            if str(ticker or "").strip()
        )
        effective_tickers = [ticker for ticker in clause_tickers if ticker in plan_scope]
        if not effective_tickers:
            effective_tickers = list(plan_scope)
        dimensions = _unique(
            str(dimension).strip()
            for dimension in raw.get("metric_dimensions") or []
            if str(dimension or "").strip()
        )
        metric_scope = str(raw.get("metric_scope") or "company_total").strip()
        raw_window = str(raw.get("calculation_window") or "").strip()
        window = raw_window if raw_window in {"period_over_period", "year_over_year"} else None
        if not (
            window
            or len(effective_tickers) >= 2
            or (metric_scope in {"dimensioned", "any"} and dimensions)
        ):
            continue
        eligible.append(
            {
                "clause_id": str(raw.get("clause_id") or "").strip(),
                "required": bool(raw.get("required", True)),
                "metrics": metrics,
                "tickers": effective_tickers,
                "metric_scope": metric_scope,
                "metric_dimensions": dimensions,
                "calculation_window": window,
            }
        )
    return [clause for clause in eligible if clause["clause_id"]]


def _collect_shard_chart_points(
    shard_path: Path,
    *,
    ticker: str,
    series_meta: dict[str, dict[str, Any]],
    series_points: dict[tuple[str, str], dict[str, Any]],
) -> None:
    try:
        with sqlite3.connect(shard_path) as conn:
            conn.row_factory = sqlite3.Row
            tables = {
                str(row[0])
                for row in conn.execute(
                    "SELECT name FROM sqlite_master WHERE type IN ('table', 'view')"
                ).fetchall()
            }
            if not {"metric_lookup", "objects"}.issubset(tables):
                return
            rows = conn.execute(
                """
                SELECT
                    metric_lookup.*,
                    objects.id AS object_id_from_objects,
                    objects.json AS object_json,
                    objects.text AS object_text,
                    objects.review_status AS review_status
                FROM metric_lookup
                JOIN objects ON objects.id = metric_lookup.object_id
                WHERE metric_lookup.value_text IS NOT NULL
                  AND metric_lookup.value_text != ''
                  AND metric_lookup.canonical_metric IS NOT NULL
                  AND metric_lookup.canonical_metric != ''
                  AND (objects.review_status IS NULL OR objects.review_status != 'rejected')
                ORDER BY metric_lookup.ticker, metric_lookup.period, metric_lookup.canonical_metric, metric_lookup.object_id
                """
            ).fetchall()
    except sqlite3.Error:
        return

    for row in rows:
        point = _chart_point_from_row(row, ticker=ticker)
        if point is None:
            continue
        series_key = point["series_key"]
        period = point["period"]
        series_meta.setdefault(series_key, point["series"])
        point_key = (series_key, period)
        existing = series_points.get(point_key)
        if existing is None or _point_quality_score(point) > _point_quality_score(existing):
            series_points[point_key] = point


def _chart_point_from_row(row: sqlite3.Row, *, ticker: str) -> dict[str, Any] | None:
    obj = _json_object(row["object_json"])
    value = _number(row["value_text"])
    if value is None:
        value = _number(obj.get("value"))
    if value is None:
        return None
    classification = _classify_metric(
        row["canonical_metric"],
        metric_name=row["metric_name"],
        text=" ".join(
            str(part or "")
            for part in (
                row["metric_alias_text"],
                row["metric_name"],
                row["text"],
                row["object_text"],
            )
        ),
    )
    if classification is None:
        return None
    period_info = _period_info(row, obj)
    if period_info is None:
        return None
    scope_kind, scope_key, scope_label = _scope(row)
    unit = str(row["unit"] or obj.get("unit") or "").strip() or None
    currency = _currency(row, obj, unit)
    basis = _basis(classification["canonical_metric"], row, obj)
    duration = _duration(classification["canonical_metric"], row, obj)
    period_type = "quarterly" if period_info["fiscal_quarter"] else "annual"
    source_class = _source_class(classification["canonical_metric"], basis, row)
    statement_family = _statement_family(classification["canonical_metric"], source_class)
    series_key = "|".join(
        [
            ticker,
            classification["canonical_metric"],
            f"{scope_kind}:{scope_key}",
            currency or "currencyless",
            unit or "unitless",
            period_type,
            basis,
            duration,
            source_class,
        ]
    )
    label = _series_label(classification["canonical_metric"], scope_kind, scope_label)
    document_period = str(row["period"] or obj.get("period") or "")
    point = {
        "series_key": series_key,
        "period": period_info["period"],
        "period_basis": period_info["basis"],
        "period_start": period_info["start"],
        "period_end": period_info["end"],
        "fiscal_year": period_info["fiscal_year"],
        "fiscal_quarter": period_info["fiscal_quarter"],
        "period_sort_key": period_info["sort_key"],
        "document_type": row["document_type"],
        "document_period": document_period,
        "source_document_id": (
            project_local_identity(obj["source_document_id"], ticker=ticker)
            if obj.get("source_document_id")
            else None
        ),
        "source_sort_key": _period_sort_key(document_period),
        "value": value,
        "formatted_value": _format_value(value, unit),
        "currency": currency,
        "object_id": project_object_identity(
            row["object_id"],
            row["object_type"],
            ticker=ticker,
            payload=obj,
        ),
        "trace_status": row["trace_status"],
        "metric_lineage_status": row["metric_lineage_status"],
        "series": {
            "series_key": series_key,
            "ticker": ticker,
            "canonical_metric": classification["canonical_metric"],
            "metric_name": row["metric_name"],
            "label": label,
            "unit": unit,
            "scope_kind": scope_kind,
            "scope_key": scope_key,
            "scope_label": scope_label,
            "scope_dimension": scope_kind if scope_kind != "company_total" else None,
            "composition_eligible": bool(scope_kind != "company_total" and value >= 0),
            "currency": currency,
            "period_type": period_type,
            "basis": basis,
            "duration": duration,
            "source_class": source_class,
            "statement_family": statement_family,
            "quality_flags": classification["quality_flags"],
        },
    }
    return point


def _canonical_chart_metric(value: Any) -> str | None:
    metric = _slug(value)
    metric = _CHART_METRIC_ALIASES.get(metric, metric)
    return metric if metric in CHART_SAFE_CANONICAL_METRICS else None


def _write_chart_series_rows(
    conn: sqlite3.Connection,
    *,
    series_meta: Mapping[str, Mapping[str, Any]],
    series_points: Mapping[tuple[str, str], Mapping[str, Any]],
) -> None:
    points_by_series: dict[str, list[Mapping[str, Any]]] = {}
    for point in series_points.values():
        points_by_series.setdefault(str(point["series_key"]), []).append(point)
    for series_key, meta in sorted(series_meta.items()):
        points = sorted(
            points_by_series.get(series_key, []),
            key=lambda point: (int(point["period_sort_key"]), str(point["period"])),
        )
        if not points:
            continue
        quality_flags = list(meta.get("quality_flags") or [])
        conn.execute(
            """
            INSERT INTO chart_series (
                series_key, ticker, canonical_metric, metric_name, label, unit,
                scope_kind, scope_key, scope_label, scope_dimension, composition_eligible,
                currency, period_type, basis, duration,
                source_class, statement_family, point_count, first_period, last_period,
                first_sort_key, last_sort_key, quality_flags_json
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            (
                series_key,
                meta.get("ticker"),
                meta.get("canonical_metric"),
                meta.get("metric_name"),
                meta.get("label"),
                meta.get("unit"),
                meta.get("scope_kind"),
                meta.get("scope_key"),
                meta.get("scope_label"),
                meta.get("scope_dimension"),
                int(bool(meta.get("composition_eligible"))),
                meta.get("currency"),
                meta.get("period_type"),
                meta.get("basis"),
                meta.get("duration"),
                meta.get("source_class"),
                meta.get("statement_family"),
                len(points),
                points[0]["period"],
                points[-1]["period"],
                points[0]["period_sort_key"],
                points[-1]["period_sort_key"],
                json.dumps(quality_flags, ensure_ascii=False, sort_keys=True),
            ),
        )
        for point in points:
            conn.execute(
                """
                INSERT INTO chart_series_points (
                    series_key, ticker, period, period_basis, period_start, period_end,
                    fiscal_year, fiscal_quarter,
                    period_sort_key, document_type, document_period, source_document_id,
                    source_sort_key, value, formatted_value, currency, object_id, trace_status,
                    metric_lineage_status
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    series_key,
                    meta.get("ticker"),
                    point.get("period"),
                    point.get("period_basis"),
                    point.get("period_start"),
                    point.get("period_end"),
                    point.get("fiscal_year"),
                    point.get("fiscal_quarter"),
                    point.get("period_sort_key"),
                    point.get("document_type"),
                    point.get("document_period"),
                    point.get("source_document_id"),
                    point.get("source_sort_key"),
                    point.get("value"),
                    point.get("formatted_value"),
                    point.get("currency"),
                    point.get("object_id"),
                    point.get("trace_status"),
                    point.get("metric_lineage_status"),
                ),
            )


def _write_metadata(conn: sqlite3.Connection, metadata: Mapping[str, Any]) -> None:
    for key, value in metadata.items():
        conn.execute(
            """
            INSERT OR REPLACE INTO chart_series_metadata (key, value_json)
            VALUES (?, ?)
            """,
            (str(key), json.dumps(value, ensure_ascii=False, sort_keys=True, default=str)),
        )


def _col(row: sqlite3.Row, name: str, default: Any = None) -> Any:
    """Read a column that older sidecar schemas may not carry.

    The chart sidecar is optional presentation data and its sqlite file can
    persist across schema revisions (`CREATE TABLE IF NOT EXISTS` never adds
    columns). Reading a v2-only column from a v1 sidecar must degrade to the
    default instead of raising - a stale presentation cache must never kill
    the research query it decorates (production incident 2026-08-16:
    period_basis IndexError aborted every query_context call).
    """
    try:
        return row[name]
    except IndexError:
        return default


def _chart_series_points(
    conn: sqlite3.Connection, series_key: str, *, limit: int
) -> list[dict[str, Any]]:
    rows = conn.execute(
        """
        SELECT *
        FROM chart_series_points
        WHERE series_key = ?
        ORDER BY period_sort_key DESC, period DESC
        LIMIT ?
        """,
        (series_key, limit),
    ).fetchall()
    points = [
        {
            "period": row["period"],
            "period_basis": _col(row, "period_basis"),
            "period_start": _col(row, "period_start"),
            "period_end": _col(row, "period_end"),
            "fiscal_year": row["fiscal_year"],
            "fiscal_quarter": row["fiscal_quarter"],
            "period_sort_key": row["period_sort_key"],
            "document_type": row["document_type"],
            "document_period": row["document_period"],
            "source_document_id": row["source_document_id"],
            "value": row["value"],
            "formatted_value": row["formatted_value"],
            "currency": _col(row, "currency"),
            "object_id": row["object_id"],
            "evidence_ref": row["object_id"],
        }
        for row in reversed(rows)
    ]
    return points


def _classify_metric(raw_metric: Any, *, metric_name: Any, text: str) -> dict[str, Any] | None:
    canonical = _slug(raw_metric) or _slug(metric_name)
    haystack = " ".join([canonical, _slug(metric_name), _slug(text), str(text or "").lower()])
    quality_flags: list[str] = []
    if "business_combination_and_other_related_cost" in haystack or (
        "business combination" in haystack and "related cost" in haystack
    ):
        return {
            "canonical_metric": "ma_related_costs",
            "quality_flags": ["ma_costs_not_cash_acquisition"],
        }
    if (
        "business_combination" in haystack
        or "business combinations" in haystack
        or "acquisition" in haystack
        or "acquisitions" in haystack
        or "m_and_a" in haystack
    ) and ("cash" in haystack or "net_of_cash" in haystack or "purchase" in haystack):
        return {"canonical_metric": "ma_cash_outflow", "quality_flags": []}
    if "adjusted_free_cash_flow" in haystack or "adjusted_fcf" in haystack:
        return {"canonical_metric": "adjusted_free_cash_flow", "quality_flags": ["non_gaap"]}
    aliases = (
        ("research_and_development", ("research_and_development", "r_d", "research_development")),
        (
            "stock_based_compensation",
            ("stock_based_compensation", "share_based_compensation", "sbc"),
        ),
        ("share_repurchase", ("share_repurchase", "stock_repurchase", "repurchase", "buyback")),
        ("capital_expenditures", ("capital_expenditure", "capital_expenditures", "capex")),
        ("operating_cash_flow", ("operating_cash_flow", "net_cash_provided_by_operating")),
        ("free_cash_flow", ("free_cash_flow", "fcf")),
        ("cash_and_cash_equivalents", ("cash_and_cash_equivalents", "cash_and_equivalents")),
        ("total_debt", ("total_debt", "long_term_debt", "short_term_debt")),
        ("operating_income", ("operating_income", "income_from_operations")),
        ("net_income", ("net_income", "net_earnings")),
        ("gross_margin", ("gross_margin",)),
        ("operating_margin", ("operating_margin",)),
        ("eps", ("eps", "earnings_per_share")),
        ("revenue", ("revenue",)),
        ("net_sales", ("net_sales", "sales")),
    )
    for target, needles in aliases:
        if canonical == target or any(needle in haystack for needle in needles):
            if target in CHART_SAFE_CANONICAL_METRICS:
                return {"canonical_metric": target, "quality_flags": quality_flags}
    if canonical in CHART_SAFE_CANONICAL_METRICS:
        return {"canonical_metric": canonical, "quality_flags": quality_flags}
    return None


def _period_info(row: sqlite3.Row, obj: Mapping[str, Any]) -> dict[str, Any] | None:
    context = obj.get("context") if isinstance(obj.get("context"), Mapping) else {}
    period_end = obj.get("period_end") or context.get("period_end") or context.get("end_date")
    fiscal_year = _int_or_none(obj.get("fiscal_year"))
    if fiscal_year is None:
        fiscal_year = _int_or_none(context.get("fiscal_year"))
    if fiscal_year is None:
        fiscal_year = _int_or_none(row["fiscal_year"])
    if fiscal_year is None and period_end:
        fiscal_year = _year_from_date(str(period_end))
    if fiscal_year is None:
        fiscal_year = _year_from_period(str(row["period"] or ""))
    if fiscal_year is None:
        return None

    fiscal_quarter = _int_or_none(obj.get("fiscal_quarter"))
    if fiscal_quarter is None:
        fiscal_quarter = _int_or_none(context.get("fiscal_quarter"))
    if fiscal_quarter is None:
        fiscal_quarter = _int_or_none(row["fiscal_quarter"])
    if fiscal_quarter is None and period_end and str(row["document_type"] or "").upper() == "10-Q":
        fiscal_quarter = _quarter_from_date(str(period_end))
    if fiscal_quarter is None:
        fiscal_quarter = _quarter_from_period(str(row["period"] or ""))

    raw_period = str(row["period"] or obj.get("period") or "").strip().upper()
    raw_match = re.fullmatch(r"(FY|CY)((?:19|20)\d{2})(?:Q([1-4]))?", raw_period)
    if raw_match:
        # A canonical FY/CY label is authoritative. Do not let a stale
        # document date or row-level quarter metadata turn an annual label
        # into a quarterly point.
        fiscal_year = int(raw_match.group(2))
        fiscal_quarter = int(raw_match.group(3)) if raw_match.group(3) else None
        period = raw_period
        basis = raw_match.group(1)
    elif fiscal_quarter:
        period = f"CY{fiscal_year}Q{fiscal_quarter}"
        basis = "CY"
    else:
        period = f"CY{fiscal_year}"
        basis = "CY"
    period_start = obj.get("period_start") or context.get("period_start") or context.get("start_date")
    period_end_value = period_end or obj.get("period_end") or context.get("end_date")
    return {
        "period": period,
        "basis": basis,
        "start": str(period_start).strip() if period_start else None,
        "end": str(period_end_value).strip() if period_end_value else None,
        "fiscal_year": fiscal_year,
        "fiscal_quarter": fiscal_quarter,
        "sort_key": _period_sort_key(period),
    }


def _currency(row: sqlite3.Row, obj: Mapping[str, Any], unit: str | None) -> str | None:
    context = obj.get("context") if isinstance(obj.get("context"), Mapping) else {}
    for candidate in (
        obj.get("currency"),
        context.get("currency"),
        context.get("currency_code"),
    ):
        value = str(candidate or "").strip().upper()
        if re.fullmatch(r"[A-Z]{3}", value):
            return value
    unit_text = str(unit or "").upper()
    match = re.search(r"\b(USD|EUR|GBP|JPY|CNY|KRW|CAD|AUD|CHF)\b", unit_text)
    return match.group(1) if match else None


def _scope(row: sqlite3.Row) -> tuple[str, str, str]:
    if int(row["is_company_total"] or 0):
        ticker = str(row["ticker"] or "").strip().upper()
        return "company_total", ticker or "company_total", "Company total"
    for kind, column in (
        ("segment", "segment_name"),
        ("product", "product_name"),
        ("geography", "geography_name"),
    ):
        value = str(row[column] or "").strip()
        if value:
            key = _slug(value) or _short_hash(value)
            return kind, key, value
    dimensions = _json_object(row["dimensions_json"])
    if dimensions:
        first_key = sorted(dimensions)[0]
        first_value = str(dimensions.get(first_key) or "").strip()
        if first_value:
            kind = _slug(first_key) or "dimension"
            return kind, _slug(first_value) or _short_hash(first_value), first_value
        encoded = json.dumps(dimensions, ensure_ascii=False, sort_keys=True)
        return "dimension", _short_hash(encoded), encoded
    ticker = str(row["ticker"] or "").strip().upper()
    return "company_total", ticker or "company_total", "Company total"


def _basis(metric: str, row: sqlite3.Row, obj: Mapping[str, Any]) -> str:
    text = " ".join(
        str(part or "").lower()
        for part in (metric, row["metric_name"], row["text"], row["object_text"])
    )
    if (
        "non-gaap" in text
        or "non gaap" in text
        or "adjusted" in text
        or metric == "adjusted_free_cash_flow"
    ):
        return "non_gaap_adjusted"
    if obj.get("type") == "XBRLFact" or "(xbrl)" in text or "xbrl" in text:
        return "gaap_xbrl"
    if metric in {"gross_margin", "operating_margin"}:
        return "company_reported_ratio"
    return "company_reported"


def _duration(metric: str, row: sqlite3.Row, obj: Mapping[str, Any]) -> str:
    context = obj.get("context") if isinstance(obj.get("context"), Mapping) else {}
    raw_period_type = str(obj.get("period_type") or context.get("period_type") or "").lower()
    text = " ".join(
        str(part or "").lower() for part in (row["metric_name"], row["text"], row["object_text"])
    )
    if "ttm" in text or "trailing twelve" in text:
        return "ttm"
    if "ytd" in text or "year to date" in text:
        return "ytd"
    if raw_period_type == "instant" or metric in _POINT_IN_TIME_METRICS:
        return "point_in_time"
    return "period"


def _source_class(metric: str, basis: str, row: sqlite3.Row) -> str:
    text = " ".join(
        str(part or "").lower()
        for part in (metric, row["metric_name"], row["text"], row["object_text"])
    )
    if metric == "ma_related_costs":
        return "fcf_reconciliation"
    if metric == "ma_cash_outflow":
        return "cash_flow_statement"
    if metric == "adjusted_free_cash_flow":
        return "company_non_gaap_metric"
    if basis == "gaap_xbrl":
        return "xbrl"
    if "cash flow" in text or metric in {
        "operating_cash_flow",
        "free_cash_flow",
        "capital_expenditures",
        "share_repurchase",
    }:
        return "cash_flow_statement"
    if metric in _POINT_IN_TIME_METRICS:
        return "balance_sheet"
    if metric in {"gross_margin", "operating_margin"}:
        return "ratio_metric"
    return "company_reported_metric"


def _statement_family(metric: str, source_class: str) -> str:
    if source_class in {"cash_flow_statement", "fcf_reconciliation", "company_non_gaap_metric"}:
        return "cash_flow"
    if source_class == "balance_sheet" or metric in _POINT_IN_TIME_METRICS:
        return "balance_sheet"
    if metric in {"gross_margin", "operating_margin"}:
        return "ratio"
    return "income_statement"


def _chart_series_relevance_key(
    row: sqlite3.Row,
    *,
    metric_candidates: Sequence[str],
    scope_candidates: Sequence[str],
    dimension_requested: bool,
) -> tuple[int, int, int, int, int, str, str]:
    metric = str(row["canonical_metric"] or "")
    scope_key = str(row["scope_key"] or "")
    scope_kind = str(row["scope_kind"] or "")
    metric_rank = metric_candidates.index(metric) if metric in metric_candidates else 999
    if scope_candidates and scope_key in set(scope_candidates):
        scope_rank = 0
    elif dimension_requested and scope_kind != "company_total":
        scope_rank = 1
    elif scope_kind == "company_total":
        scope_rank = 0 if not dimension_requested else 2
    else:
        scope_rank = 3
    period_rank = 0 if row["period_type"] == "annual" else 1
    point_rank = -int(row["point_count"] or 0)
    recency_rank = -int(row["last_sort_key"] or 0)
    return (
        metric_rank,
        scope_rank,
        period_rank,
        point_rank,
        recency_rank,
        str(row["ticker"] or ""),
        str(row["label"] or ""),
    )


def _select_chart_series_rows(
    rows: Sequence[sqlite3.Row],
    *,
    limit: int,
    metric_candidates: Sequence[str],
    scope_candidates: Sequence[str],
    dimension_requested: bool,
    coverage_pairs: Sequence[tuple[str, str]] = (),
) -> list[sqlite3.Row]:
    ordered = sorted(
        rows,
        key=lambda row: _chart_series_relevance_key(
            row,
            metric_candidates=metric_candidates,
            scope_candidates=scope_candidates,
            dimension_requested=dimension_requested,
        ),
    )
    selected: list[sqlite3.Row] = []
    seen_series: set[str] = set()
    # Ticker is part of the identity.  Segment labels such as "Other" or
    # "Services" legitimately recur across companies and must not collapse
    # a multi-company comparison into the first ticker encountered.
    seen_scope_metric: set[tuple[str, str, str, str]] = set()

    def add(row: sqlite3.Row) -> bool:
        if len(selected) >= limit:
            return False
        series_key = str(row["series_key"] or "")
        if not series_key or series_key in seen_series:
            return False
        scope_metric_key = (
            str(row["ticker"] or ""),
            str(row["canonical_metric"] or ""),
            str(row["scope_kind"] or ""),
            str(row["scope_key"] or ""),
        )
        if scope_metric_key in seen_scope_metric:
            return False
        selected.append(row)
        seen_series.add(series_key)
        seen_scope_metric.add(scope_metric_key)
        return True

    # Preserve at least one best row for every requested ticker/metric pair
    # while capacity allows.  Without this pass, a crowded company's segment
    # rows can outrank a later metric and make the latter disappear before the
    # Rust compiler ever sees it.
    for ticker, metric in coverage_pairs:
        candidates = [
            row
            for row in ordered
            if str(row["ticker"] or "") == ticker
            and str(row["canonical_metric"] or "") == metric
        ]
        candidates.sort(
            key=lambda row: (
                0 if str(row["scope_kind"] or "") == "company_total" else 1,
                -int(row["point_count"] or 0),
                -int(row["last_sort_key"] or 0),
                str(row["scope_kind"] or ""),
                str(row["scope_key"] or ""),
            )
        )
        if candidates:
            add(candidates[0])

    if not dimension_requested:
        for row in ordered:
            add(row)
        return selected

    scope_set = set(scope_candidates)

    # A composition needs the same-period company total as a reconciliation
    # anchor. Reserve it before optional breakdown rows so a broad sidecar
    # cannot fill the bounded pack with segments and silently omit the total.
    for metric in metric_candidates:
        for row in ordered:
            if (
                str(row["canonical_metric"] or "") == metric
                and str(row["scope_kind"] or "") == "company_total"
            ):
                add(row)
                break

    for row in ordered:
        if str(row["scope_key"] or "") in scope_set:
            add(row)

    selected_metrics = {str(row["canonical_metric"] or "") for row in selected}
    for metric in metric_candidates:
        if metric in selected_metrics:
            continue
        for row in ordered:
            if str(row["canonical_metric"] or "") != metric:
                continue
            if add(row):
                selected_metrics.add(metric)
                break

    for row in ordered:
        add(row)

    return selected


def _series_label(metric: str, scope_kind: str, scope_label: str) -> str:
    metric_label = {
        "adjusted_free_cash_flow": "Adjusted FCF",
        "capital_expenditures": "CapEx",
        "cash_and_cash_equivalents": "Cash & equivalents",
        "cash_and_equivalents": "Cash & equivalents",
        "eps": "EPS",
        "free_cash_flow": "Free cash flow",
        "gross_margin": "Gross margin",
        "ma_cash_outflow": "M&A cash outflow",
        "ma_related_costs": "M&A-related costs",
        "net_income": "Net income",
        "net_sales": "Net sales",
        "operating_cash_flow": "Operating cash flow",
        "operating_income": "Operating income",
        "operating_margin": "Operating margin",
        "research_and_development": "R&D",
        "revenue": "Revenue",
        "selling_general_and_admin": "SG&A",
        "share_repurchase": "Share repurchase",
        "stock_based_compensation": "SBC",
        "total_assets": "Total assets",
        "total_debt": "Total debt",
        "total_liabilities": "Total liabilities",
    }.get(metric, metric.replace("_", " ").title())
    if scope_kind == "company_total":
        return metric_label
    return f"{scope_label} {metric_label}"


def _point_quality_score(point: Mapping[str, Any]) -> tuple[int, int, int, int, str]:
    source_sort = int(point.get("source_sort_key") or 0)
    trace_score = 0
    if point.get("metric_lineage_status") == "traceable_metric_lineage":
        trace_score += 20
    if point.get("trace_status") in {"traceable", "traceable_metric_lineage"}:
        trace_score += 10
    document_score = 5 if point.get("document_type") in {"10-K", "10-Q"} else 0
    exact_period_score = (
        5
        if _period_sort_key(str(point.get("document_period") or ""))
        == int(point.get("period_sort_key") or 0)
        else 0
    )
    return (
        source_sort,
        trace_score,
        document_score,
        exact_period_score,
        str(point.get("object_id") or ""),
    )


def _format_value(value: float, unit: str | None) -> str:
    unit_text = str(unit or "")
    if unit_text in {"USD", "usd"}:
        absolute = abs(value)
        if absolute >= 1_000_000_000:
            return f"${value / 1_000_000_000:.1f}B"
        if absolute >= 1_000_000:
            return f"${value / 1_000_000:.1f}M"
        return f"${value:,.0f}"
    if "percent" in unit_text.lower() or unit_text == "%":
        return f"{value:.1f}%"
    return f"{value:g}"


def _resolve_shard_path(root: Path, entry: Mapping[str, Any]) -> Path:
    raw = entry.get("path") or entry.get("shard_path")
    if not isinstance(raw, str) or not raw:
        return root / "indexes" / "companies" / "<missing>"
    candidate = Path(raw)
    if candidate.is_absolute():
        return candidate.expanduser().resolve()
    if candidate.parts and candidate.parts[0] == "indexes":
        return (root / candidate).resolve()
    return (root / "indexes" / candidate).resolve()


def _read_json(path: Path) -> dict[str, Any]:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (FileNotFoundError, json.JSONDecodeError):
        return {}
    return payload if isinstance(payload, dict) else {}


def _json_object(value: Any) -> dict[str, Any]:
    if isinstance(value, Mapping):
        return dict(value)
    try:
        payload = json.loads(str(value or "{}"))
    except json.JSONDecodeError:
        return {}
    return payload if isinstance(payload, dict) else {}


def _number(value: Any) -> float | None:
    if value is None:
        return None
    if isinstance(value, (int, float)):
        return float(value)
    text = str(value).strip().replace(",", "")
    if not text:
        return None
    try:
        return float(text)
    except ValueError:
        return None


def _int_or_none(value: Any) -> int | None:
    if value in (None, ""):
        return None
    try:
        return int(value)
    except (TypeError, ValueError):
        return None


def _year_from_date(value: str) -> int | None:
    match = re.search(r"((?:19|20)\d{2})", value)
    return int(match.group(1)) if match else None


def _quarter_from_date(value: str) -> int | None:
    match = re.search(r"(?:19|20)\d{2}-(\d{2})-\d{2}", value)
    if not match:
        return None
    month = int(match.group(1))
    if month <= 3:
        return 1
    if month <= 6:
        return 2
    if month <= 9:
        return 3
    return 4


def _year_from_period(value: str) -> int | None:
    match = re.search(r"((?:19|20)\d{2})", value.upper())
    return int(match.group(1)) if match else None


def _quarter_from_period(value: str) -> int | None:
    match = re.search(r"Q([1-4])", value.upper())
    return int(match.group(1)) if match else None


def _period_sort_key(period: str) -> int:
    match = re.search(r"(?:CY|FY)?((?:19|20)\d{2})(?:Q([1-4]))?", period.upper())
    if not match:
        return 0
    return int(match.group(1)) * 10 + int(match.group(2) or 0)


def _slug(value: Any) -> str:
    text = str(value or "").strip()
    text = re.sub(r"(?<=[a-z0-9])(?=[A-Z])", "_", text)
    text = text.lower()
    text = text.replace("&", " and ")
    text = re.sub(r"[^a-z0-9]+", "_", text)
    return re.sub(r"_+", "_", text).strip("_")


def _short_hash(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()[:12]


def _unique(values: Iterable[str]) -> list[str]:
    seen: set[str] = set()
    result: list[str] = []
    for value in values:
        if not value or value in seen:
            continue
        seen.add(value)
        result.append(value)
    return result


def _count(conn: sqlite3.Connection, table: str) -> int:
    return int(conn.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0])


def _temporary_sqlite_path(path: Path) -> Path:
    return path.with_name(f".{path.name}.{os.getpid()}.tmp")


def _cleanup_sqlite_sidecars(path: Path) -> None:
    for candidate in (
        path,
        path.with_suffix(path.suffix + "-wal"),
        path.with_suffix(path.suffix + "-shm"),
    ):
        try:
            if candidate.exists():
                candidate.unlink()
        except OSError:
            pass
