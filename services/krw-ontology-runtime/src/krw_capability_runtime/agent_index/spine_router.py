"""Monolith-free v3 router backed by global spine and company shards."""

from __future__ import annotations

import hashlib
import heapq
import json
import math
import os
import re
import sqlite3
import time
from collections import Counter, OrderedDict, defaultdict
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path
from typing import Any, Callable, Iterable, Mapping, Sequence

from krw_capability_runtime.agent_index.chart_series import (
    CHART_SERIES_RELATIVE_PATH,
    chart_series_index_status,
    query_chart_series_pack,
)
from krw_capability_runtime.agent_index.router_coherence import ROUTER_COHERENCE_RELATIVE_PATH
from krw_capability_runtime.agent_index.router_sidecar import (
    ROUTER_SIDECAR_RELATIVE_PATH,
    RouterSidecar,
)
from krw_capability_runtime.agent_index.semantic_identity import effective_ticker
from krw_capability_runtime.agent_index.store import (
    OntologyStore,
    filing_document_roles_from_documents,
    latest_document_anchors_from_documents,
)
from krw_capability_runtime.agent_index.spine_schema import (
    GLOBAL_SPINE_LAYOUT,
    GLOBAL_SPINE_SCHEMA_VERSION,
    GLOBAL_SPINE_TABLES,
    read_global_spine_metadata,
)

_ROUTER_FANOUT_WORKERS_ENV = "KRW_ROUTER_FANOUT_WORKERS"
_CHART_SERIES_ENABLED_ENV = "KRW_CHART_SERIES_ENABLED"
_TICKERLESS_QUERY_CONTEXT_MAX_TICKERS_ENV = "KRW_ROUTER_TICKERLESS_QUERY_CONTEXT_MAX_TICKERS"
_DEFAULT_ROUTER_FANOUT_WORKERS = 8
_MAX_ROUTER_FANOUT_WORKERS = 16
_DEFAULT_TICKERLESS_QUERY_CONTEXT_MAX_TICKERS = 5
_TRUE_ENV_VALUES = {"1", "true", "yes", "on"}
_GLOBAL_CHAIN_MAX_DEPTH = 5
_GLOBAL_CHAIN_MAX_PATHS = 32
_GLOBAL_CHAIN_MAX_EXPANSIONS = 256
_GLOBAL_CHAIN_MAX_FRONTIER = 128
_GLOBAL_CHAIN_NEIGHBORS_PER_NODE = 12
_OBJECT_REPLICA_PREVIEW_LIMIT = 20
_OBJECT_ID_LOOKUP_CHUNK_SIZE = 500
_OBJECT_ID_PROJECTION_CACHE_MAX = 50_000
_OBJECT_ID_SCALAR_FIELDS = frozenset(
    {
        "id",
        "trace_id",
        "from_id",
        "to_id",
        "source_id",
        "target_id",
    }
)
_OBJECT_ID_LIST_FIELDS = frozenset(
    {
        "affected_objects",
        "claim_ids",
        "object_ids",
        "quote_ids",
        "related_object_ids",
        "source_ids",
        "source_object_ids",
        "span_ids",
    }
)
_OCCURRENCE_TICKER_FIELDS = frozenset(
    {"ticker", "from_ticker", "neighbor_ticker", "root_ticker", "to_ticker"}
)


def _is_object_id_scalar_field(field: str) -> bool:
    normalized = str(field or "").strip().casefold()
    return normalized in _OBJECT_ID_SCALAR_FIELDS or normalized.endswith("_object_id")


def _is_object_id_list_field(field: str) -> bool:
    normalized = str(field or "").strip().casefold()
    return normalized in _OBJECT_ID_LIST_FIELDS or normalized.endswith("_object_ids")


def _prefix_upper_bound(prefix: str) -> str | None:
    """Return the exclusive BINARY-collation upper bound for a text prefix."""
    if not prefix:
        return None
    for index in range(len(prefix) - 1, -1, -1):
        codepoint = ord(prefix[index])
        if codepoint < 0x10FFFF:
            return f"{prefix[:index]}{chr(codepoint + 1)}"
    return None


class OntologySpineRouter:
    """Store-compatible facade for v3 global spine + company shard releases."""

    def __init__(self, global_spine_path: Path | str, *, check_same_thread: bool = True):
        self.index_path = Path(global_spine_path).expanduser().resolve()
        self.global_spine_path = self.index_path
        self.index_dir = self.global_spine_path.parent
        self.release_root = self.index_dir.parent
        self.check_same_thread = check_same_thread
        self._shard_manifest_path = self.index_dir / "shard_manifest.json"
        self._shard_manifest = _read_json(self._shard_manifest_path)
        self._declared_shard_paths, self._shard_paths, self._missing_shard_paths = (
            self._load_shard_paths()
        )
        self._chart_series_path = self.release_root / CHART_SERIES_RELATIVE_PATH
        self._chart_series_status = chart_series_index_status(self._chart_series_path)
        self._router_sidecar_path = self.release_root / ROUTER_SIDECAR_RELATIVE_PATH
        self._router_coherence_path = self.release_root / ROUTER_COHERENCE_RELATIVE_PATH
        self._spine_conn: sqlite3.Connection | None = None
        self._router_sidecar: RouterSidecar | None = None
        self._global_metadata_cache: dict[str, Any] | None = None
        self._shards: dict[str, OntologyStore] = {}
        self._local_to_index_id_cache: OrderedDict[tuple[str, str], str | None] = OrderedDict()
        self._closed = False

    @classmethod
    def can_open(cls, path: Path | str) -> bool:
        candidate = Path(path).expanduser()
        if candidate.name != "global_spine.sqlite" or not candidate.is_file():
            return False
        try:
            with sqlite3.connect(candidate) as conn:
                metadata = read_global_spine_metadata(conn)
                if metadata.get("schema_version") != GLOBAL_SPINE_SCHEMA_VERSION:
                    return False
                if metadata.get("index_layout") != GLOBAL_SPINE_LAYOUT:
                    return False
                tables = {
                    str(row[0])
                    for row in conn.execute(
                        """
                        SELECT name
                        FROM sqlite_master
                        WHERE type IN ('table', 'view')
                        """
                    ).fetchall()
                }
        except (OSError, sqlite3.Error):
            return False
        return set(GLOBAL_SPINE_TABLES).issubset(tables)

    @property
    def conn(self) -> sqlite3.Connection:
        return self._spine_connection()

    @property
    def shards_available(self) -> bool:
        return bool(self._shard_paths)

    def __enter__(self) -> "OntologySpineRouter":
        return self

    def __exit__(self, *_exc: object) -> None:
        self.close()

    def close(self) -> None:
        if self._closed:
            return
        for store in self._shards.values():
            store.close()
        self._shards.clear()
        if self._spine_conn is not None:
            self._spine_conn.close()
            self._spine_conn = None
        if self._router_sidecar is not None:
            self._router_sidecar.close()
            self._router_sidecar = None
        self._closed = True

    def routing_status(self) -> dict[str, Any]:
        metadata = self._global_metadata()
        raw_counts = metadata.get("counts")
        counts = raw_counts if isinstance(raw_counts, Mapping) else {}
        raw_duplicates = metadata.get("benign_duplicate_counts")
        benign_duplicates = dict(raw_duplicates) if isinstance(raw_duplicates, Mapping) else {}
        return {
            "mode": "global_spine",
            "index_layout": GLOBAL_SPINE_LAYOUT,
            "global_spine_path": str(self.global_spine_path),
            "shard_manifest_path": str(self._shard_manifest_path),
            "release_root": str(self.release_root),
            "ticker_count": len(self._declared_shard_paths),
            "available_ticker_count": len(self._shard_paths),
            "missing_shard_count": len(self._missing_shard_paths),
            "missing_shards": {
                ticker: str(path) for ticker, path in sorted(self._missing_shard_paths.items())
            },
            "open_shards": sorted(self._shards),
            "chart_series_available": bool(self._chart_series_status.get("available")),
            "chart_series_path": str(self._chart_series_path),
            "router_sidecar_available": self._router_sidecar_path.is_file(),
            "router_sidecar_path": str(self._router_sidecar_path),
            "router_coherence_available": self._router_coherence_path.is_file(),
            "router_coherence_path": str(self._router_coherence_path),
            "object_resolution": {
                "canonical_identity": "object_id",
                "occurrence_identity": "ticker+object_id",
                "shared_object_requires_ticker": True,
                "ambiguous_object_policy": "fail_closed_with_ticker_candidates",
            },
            "occurrence_counts": {
                "objects": int(counts.get("global_object_replica") or 0),
                "edges": int(counts.get("global_edge_replica") or 0),
            },
            "canonical_counts": {
                "objects": int(counts.get("global_object_locator") or 0),
                "edges": int(counts.get("global_edge_spine") or 0),
            },
            "benign_duplicate_counts": benign_duplicates,
            "fallback_enabled": False,
            "fallback": False,
        }

    def list_companies(self) -> list[str]:
        if self._declared_shard_paths:
            return sorted(self._declared_shard_paths)
        rows = self.conn.execute(
            "SELECT DISTINCT ticker FROM global_document_catalog ORDER BY ticker"
        ).fetchall()
        return [str(row[0]).upper() for row in rows]

    def _available_tickers(self, tickers: Sequence[str]) -> set[str]:
        requested = {str(ticker).upper() for ticker in tickers if str(ticker or "").strip()}
        return {ticker for ticker in requested if ticker in self._shard_paths}

    def list_documents(
        self,
        *,
        ticker: str | None = None,
        tickers: Iterable[str] | None = None,
        document_types: Iterable[str] | None = None,
    ) -> list[dict[str, Any]]:
        ticker_filter = str(ticker).upper() if ticker else None
        ticker_values = list(
            dict.fromkeys(
                str(value).strip().upper() for value in (tickers or []) if str(value or "").strip()
            )
        )
        document_type_values = [
            str(value) for value in (document_types or []) if str(value or "").strip()
        ]
        clauses: list[str] = []
        params: list[Any] = []
        if ticker_filter:
            clauses.append("ticker = ?")
            params.append(ticker_filter)
        elif ticker_values:
            placeholders = ", ".join("?" for _ in ticker_values)
            clauses.append(f"ticker IN ({placeholders})")
            params.extend(ticker_values)
        if document_type_values:
            placeholders = ", ".join("?" for _ in document_type_values)
            clauses.append(f"document_type IN ({placeholders})")
            params.extend(document_type_values)
        where = f"WHERE {' AND '.join(clauses)}" if clauses else ""
        rows = self.conn.execute(
            f"""
            SELECT ticker, document_type, period, source_path, object_count,
                   edge_count, quality_event_count, quality_status
            FROM global_document_catalog
            {where}
            ORDER BY ticker, document_type, period
            """,
            params,
        ).fetchall()
        return [
            {
                "ticker": row["ticker"],
                "document_type": row["document_type"],
                "doc_type_key": row["document_type"],
                "period": row["period"],
                "artifact_index_path": row["source_path"],
                "ontology_dir": None,
                "counts": {
                    "objects": int(row["object_count"] or 0),
                    "edges": int(row["edge_count"] or 0),
                    "quality_events": int(row["quality_event_count"] or 0),
                },
                "section_quality_status": row["quality_status"],
                "section_quality": {},
            }
            for row in rows
        ]

    def index_context(
        self,
        *,
        include_counts: bool = True,
        include_capabilities: bool = True,
        include_quality_summary: bool = True,
    ) -> dict[str, Any]:
        documents = self.list_documents()
        metadata = self._global_metadata()
        periods_by_ticker: dict[str, list[str]] = {}
        document_types: set[str] = set()
        for document in documents:
            ticker = str(document.get("ticker") or "")
            period = str(document.get("period") or "")
            if ticker and period:
                periods_by_ticker.setdefault(ticker, [])
                if period not in periods_by_ticker[ticker]:
                    periods_by_ticker[ticker].append(period)
            if document.get("document_type"):
                document_types.add(str(document["document_type"]))
        payload: dict[str, Any] = {
            "index_status": "ready",
            "index_path": str(self.global_spine_path),
            "index_layout": GLOBAL_SPINE_LAYOUT,
            "schema_version": metadata.get("schema_version"),
            "builder_version": metadata.get("builder_version"),
            "available_tickers": self.list_companies(),
            "available_document_types": sorted(document_types),
            "available_periods_by_ticker": {
                ticker: sorted(periods) for ticker, periods in periods_by_ticker.items()
            },
            "routing": self.routing_status(),
        }
        if include_counts:
            payload["object_counts"] = self._global_object_counts()
            payload["serving_counts"] = self._global_serving_counts()
            payload["serving_counts"]["available_company_shards"] = len(self._shard_paths)
            payload["serving_counts"]["missing_company_shards"] = len(self._missing_shard_paths)
        if include_capabilities:
            payload["capabilities"] = {
                "query": True,
                "retrieve": True,
                "trace": True,
                "chain": True,
                "compare": True,
                "quality": True,
                "company_context": True,
                "query_context": True,
                "global_spine": True,
                "company_shards": True,
                "chart_series": bool(self._chart_series_status.get("available")),
                "fallback": False,
            }
        if include_quality_summary:
            payload["quality_summary"] = self._quality_summary()
        return payload

    def _global_metadata(self) -> dict[str, Any]:
        if self._global_metadata_cache is not None:
            return dict(self._global_metadata_cache)
        rows = self.conn.execute("SELECT key, value_json FROM metadata").fetchall()
        metadata: dict[str, Any] = {}
        for row in rows:
            try:
                metadata[str(row["key"])] = json.loads(str(row["value_json"]))
            except (json.JSONDecodeError, TypeError):
                metadata[str(row["key"])] = row["value_json"]
        self._global_metadata_cache = metadata
        return dict(metadata)

    def company_context(self, *, ticker: str, **kwargs: Any) -> dict[str, Any]:
        normalized = str(ticker or "").upper()
        if normalized in self._missing_shard_paths:
            return self._missing_shard_payload(ticker, operation="company_context")
        payload = self._store_for_ticker(ticker).company_context(ticker=ticker, **kwargs)
        payload = self._project_shard_payload(payload, ticker=normalized)
        payload.setdefault("routing", self._route_payload("company_shard", [normalized]))
        return payload

    def topic_map(self, *, ticker: str, **kwargs: Any) -> dict[str, Any]:
        normalized = str(ticker or "").upper()
        if normalized in self._missing_shard_paths:
            payload = self._missing_shard_payload(ticker, operation="topic_map")
            payload.setdefault("ticker", normalized)
            payload.setdefault("topics", [])
            return payload
        payload = self._store_for_ticker(ticker).topic_map(ticker=ticker, **kwargs)
        payload = self._project_shard_payload(payload, ticker=normalized)
        payload.setdefault("routing", self._route_payload("company_shard", [normalized]))
        return payload

    def query(self, **kwargs: Any) -> list[dict[str, Any]]:
        bundles, _diagnostics = self.query_with_diagnostics(**kwargs)
        return bundles

    def query_with_diagnostics(self, **kwargs: Any) -> tuple[list[dict[str, Any]], dict[str, Any]]:
        return self._fanout_query(compact=False, **kwargs)

    def query_compact_with_diagnostics(
        self, **kwargs: Any
    ) -> tuple[list[dict[str, Any]], dict[str, Any]]:
        return self._fanout_query(compact=True, **kwargs)

    def route_planned_tickers(
        self,
        *,
        clauses: Sequence[Mapping[str, Any]],
        explicit_tickers: Sequence[str] | None = None,
        limit: int = 20,
    ) -> tuple[list[str], dict[str, Any]]:
        """Resolve plan scope with the derived sidecar and no spine scan fallback."""
        started_at = time.perf_counter()
        resolved_limit = max(1, int(limit))
        normalized_explicit = _normalize_tickers(explicit_tickers) or []
        if normalized_explicit:
            available = [ticker for ticker in normalized_explicit if ticker in self._shard_paths]
            return available, {
                "mode": "explicit_plan_scope",
                "requested_tickers": normalized_explicit,
                "resolved_tickers": available,
                "unknown_tickers": self._unknown_tickers(normalized_explicit),
                "missing_shards": self._missing_shards_for_tickers(normalized_explicit),
                "fallback_used": False,
                "timing_ms": {"total": int((time.perf_counter() - started_at) * 1000)},
            }

        normalized_clauses = [
            {
                "clause_id": str(clause.get("clause_id") or f"clause_{index + 1}"),
                "query": str(clause.get("query") or "").strip(),
                "required": bool(clause.get("required", True)),
            }
            for index, clause in enumerate(clauses)
            if str(clause.get("query") or "").strip()
        ]
        if not normalized_clauses:
            return [], {
                "mode": "planned_router_sidecar",
                "error": "planned_routing_clauses_missing",
                "fallback_used": False,
            }
        if not self._router_sidecar_path.is_file():
            return [], {
                "mode": "planned_router_sidecar",
                "error": "router_sidecar_unavailable",
                "router_sidecar_path": str(self._router_sidecar_path),
                "fallback_used": False,
            }

        try:
            sidecar = self._sidecar()
            rrf_k = max(1, int(sidecar.ranking_profile.get("rrf_k") or 60))
        except (KeyError, OSError, RuntimeError, TypeError, ValueError, sqlite3.Error) as exc:
            return [], {
                "mode": "planned_router_sidecar",
                "error": "router_sidecar_invalid",
                "error_type": type(exc).__name__,
                "router_sidecar_path": str(self._router_sidecar_path),
                "fallback_used": False,
                "timing_ms": {"total": int((time.perf_counter() - started_at) * 1000)},
            }
        scores: defaultdict[str, float] = defaultdict(float)
        clause_hits: Counter[str] = Counter()
        required_clause_hits: Counter[str] = Counter()
        best_rank: dict[str, int] = {}
        query_diagnostics: list[dict[str, Any]] = []
        required_rankings: list[list[str]] = []
        for clause in normalized_clauses:
            query = str(clause["query"])
            required = bool(clause["required"])
            weight = 2.0 if required else 1.0
            query_started_at = time.perf_counter()
            try:
                result = sidecar.search(query, limit=resolved_limit)
            except (KeyError, OSError, RuntimeError, TypeError, ValueError, sqlite3.Error) as exc:
                return [], {
                    "mode": "planned_router_sidecar",
                    "error": "router_sidecar_query_failed",
                    "error_type": type(exc).__name__,
                    "failed_clause_id": clause["clause_id"],
                    "router_sidecar_path": str(self._router_sidecar_path),
                    "fallback_used": False,
                    "timing_ms": {"total": int((time.perf_counter() - started_at) * 1000)},
                }
            candidates = [
                candidate
                for candidate in result.get("ticker_candidates") or []
                if isinstance(candidate, Mapping)
            ]
            seen_for_query: set[str] = set()
            for rank, candidate in enumerate(candidates, start=1):
                ticker = str(candidate.get("ticker") or "").strip().upper()
                if not ticker or ticker not in self._shard_paths or ticker in seen_for_query:
                    continue
                seen_for_query.add(ticker)
                scores[ticker] += weight / (rrf_k + rank)
                clause_hits[ticker] += 1
                if required:
                    required_clause_hits[ticker] += 1
                best_rank[ticker] = min(best_rank.get(ticker, rank), rank)
            ranked_for_clause = [
                str(candidate.get("ticker") or "").strip().upper()
                for candidate in candidates
                if str(candidate.get("ticker") or "").strip().upper() in seen_for_query
            ]
            if required:
                required_rankings.append(ranked_for_clause)
            query_diagnostics.append(
                {
                    "clause_id": clause["clause_id"],
                    "required": required,
                    "weight": weight,
                    "query": query,
                    "query_terms": list(result.get("query_terms") or []),
                    "candidate_count": len(seen_for_query),
                    "resolved_tickers": sorted(seen_for_query),
                    "score_summary": _score_summary(candidates),
                    "timing_ms": int((time.perf_counter() - query_started_at) * 1000),
                }
            )

        aggregate_ranked = sorted(
            scores,
            key=lambda ticker: (
                -required_clause_hits[ticker],
                -scores[ticker],
                -clause_hits[ticker],
                best_rank.get(ticker, resolved_limit + 1),
                ticker,
            ),
        )
        ranked: list[str] = []
        for clause_ranking in required_rankings:
            top = next((ticker for ticker in clause_ranking if ticker not in ranked), None)
            if top:
                ranked.append(top)
            if len(ranked) >= resolved_limit:
                break
        for ticker in aggregate_ranked:
            if ticker not in ranked:
                ranked.append(ticker)
            if len(ranked) >= resolved_limit:
                break
        return ranked, {
            "mode": "planned_router_sidecar",
            "resolved_tickers": ranked,
            "query_count": len(normalized_clauses),
            "required_query_count": sum(1 for clause in normalized_clauses if clause["required"]),
            "queries": query_diagnostics,
            "ranking_profile_id": sidecar.ranking_profile.get("profile_id"),
            "ranking_profile_sha256": sidecar.metadata.get("ranking_profile_sha256"),
            "build_fingerprint_sha256": sidecar.metadata.get("build_fingerprint_sha256"),
            "release_id": sidecar.metadata.get("release_id"),
            "timing_ms": {"total": int((time.perf_counter() - started_at) * 1000)},
            "fallback_used": False,
        }

    def query_planned_compact_with_diagnostics(
        self,
        *,
        retrieval_query: str,
        retrieval_terms: Iterable[str] | None = None,
        predicate_terms: Iterable[str] | None = None,
        metrics: Iterable[str] | None = None,
        metric_dimensions: Iterable[str] | None = None,
        metric_scope: str = "company_total",
        calculation_window: str | None = None,
        comparison_axes: Iterable[str] | None = None,
        tickers: Iterable[str],
        document_types: Iterable[str] | None = None,
        periods: Iterable[str] | None = None,
        object_types: Iterable[str] | None = None,
        include_rejected: bool = False,
        allow_relaxed: bool = False,
        limit: int = 20,
    ) -> tuple[list[dict[str, Any]], dict[str, Any]]:
        """Execute one SearchPlan clause across resolved shards in rank-balanced order."""
        result_limit = max(1, int(limit))
        route_tickers = _normalize_tickers(tickers) or []
        available_tickers = [ticker for ticker in route_tickers if ticker in self._shard_paths]
        shard_diagnostics: dict[str, Any] = {}

        def load_query(_ticker: str, store: OntologyStore) -> dict[str, Any]:
            rows, diagnostics = store.query_planned_compact_with_diagnostics(
                retrieval_query=retrieval_query,
                retrieval_terms=retrieval_terms,
                predicate_terms=predicate_terms,
                metrics=metrics,
                metric_dimensions=metric_dimensions,
                metric_scope=metric_scope,
                calculation_window=calculation_window,
                comparison_axes=comparison_axes,
                tickers=None,
                document_types=document_types,
                periods=periods,
                object_types=object_types,
                include_rejected=include_rejected,
                allow_relaxed=allow_relaxed,
                limit=result_limit,
            )
            return {"rows": list(rows), "diagnostics": diagnostics}

        payload_by_ticker, shard_errors, worker_count = self._store_fanout(
            available_tickers,
            load_query,
        )
        rows_by_ticker: dict[str, list[dict[str, Any]]] = {}
        for ticker in available_tickers:
            payload = payload_by_ticker.get(ticker)
            if not isinstance(payload, Mapping):
                continue
            rows_by_ticker[ticker] = [
                self._project_shard_payload(dict(row), ticker=ticker)
                for row in payload.get("rows") or []
                if isinstance(row, Mapping)
            ]
            diagnostics = payload.get("diagnostics")
            if isinstance(diagnostics, Mapping):
                shard_diagnostics[ticker] = self._project_shard_payload(
                    dict(diagnostics), ticker=ticker
                )

        results: list[dict[str, Any]] = []
        row_rank = 0
        while len(results) < result_limit:
            added = False
            for ticker in available_tickers:
                rows = rows_by_ticker.get(ticker) or []
                if row_rank >= len(rows):
                    continue
                row = dict(rows[row_rank])
                row["planned_ticker_rank"] = available_tickers.index(ticker) + 1
                row["planned_within_ticker_rank"] = row_rank + 1
                results.append(row)
                added = True
                if len(results) >= result_limit:
                    break
            if not added:
                break
            row_rank += 1

        fanout = self._fanout_diagnostics(worker_count, shard_errors)
        return results, {
            "execution_mode": "planned_shard_fanout",
            "retrieval_query": retrieval_query,
            "result_count": len(results),
            "routing": self._route_payload("planned_shard_fanout", route_tickers),
            "shard_diagnostics": shard_diagnostics,
            "missing_shards": self._missing_shards_for_tickers(route_tickers),
            "unknown_tickers": self._unknown_tickers(route_tickers),
            "fallback_used": False,
            **fanout,
        }

    def query_planned_batch_with_diagnostics(
        self,
        *,
        clauses: Sequence[Mapping[str, Any]],
        tickers: Iterable[str],
        document_types: Iterable[str] | None = None,
        periods: Iterable[str] | None = None,
        include_rejected: bool = False,
        limit: int = 20,
    ) -> tuple[dict[str, dict[str, Any]], dict[str, Any]]:
        """Execute all plan clauses with one SQLite connection per routed shard."""
        started_at = time.perf_counter()
        result_limit = max(1, int(limit))
        route_tickers = _normalize_tickers(tickers) or []
        available_tickers = [ticker for ticker in route_tickers if ticker in self._shard_paths]
        normalized_clauses = [
            dict(clause)
            for clause in clauses
            if str(clause.get("clause_id") or "").strip()
            and str(clause.get("retrieval_query") or "").strip()
        ]

        def load_batch(ticker: str, store: OntologyStore) -> dict[str, Any]:
            clause_payloads: dict[str, Any] = {}
            for clause in normalized_clauses:
                clause_id = str(clause["clause_id"])
                clause_tickers = _normalize_tickers(clause.get("tickers")) or []
                if clause_tickers and ticker not in clause_tickers:
                    clause_payloads[clause_id] = {
                        "rows": [],
                        "diagnostics": {
                            "execution_mode": "planned_shard_batch",
                            "ticker_scope_skipped": True,
                        },
                    }
                    continue
                rows, diagnostics = store.query_planned_compact_with_diagnostics(
                    retrieval_query=str(clause["retrieval_query"]),
                    retrieval_terms=clause.get("retrieval_terms"),
                    predicate_terms=clause.get("predicate_terms"),
                    metrics=clause.get("metrics"),
                    metric_dimensions=clause.get("metric_dimensions"),
                    metric_scope=str(clause.get("metric_scope") or "company_total"),
                    calculation_window=(
                        str(clause.get("calculation_window") or "").strip() or None
                    ),
                    comparison_axes=clause.get("comparison_axes"),
                    tickers=None,
                    document_types=document_types,
                    periods=periods,
                    object_types=clause.get("object_types"),
                    include_rejected=include_rejected,
                    allow_relaxed=bool(clause.get("allow_relaxed")),
                    limit=result_limit,
                )
                clause_payloads[clause_id] = {
                    "rows": list(rows),
                    "diagnostics": diagnostics,
                }
            return clause_payloads

        payload_by_ticker, shard_errors, worker_count = self._store_fanout(
            available_tickers,
            load_batch,
        )
        by_clause: dict[str, dict[str, Any]] = {}
        ticker_rank = {ticker: rank for rank, ticker in enumerate(available_tickers, start=1)}
        for clause in normalized_clauses:
            clause_id = str(clause["clause_id"])
            rows_by_ticker: dict[str, list[dict[str, Any]]] = {}
            shard_diagnostics: dict[str, Any] = {}
            for ticker in available_tickers:
                ticker_payload = payload_by_ticker.get(ticker)
                if not isinstance(ticker_payload, Mapping):
                    continue
                clause_payload = ticker_payload.get(clause_id)
                if not isinstance(clause_payload, Mapping):
                    continue
                rows_by_ticker[ticker] = [
                    self._project_shard_payload(dict(row), ticker=ticker)
                    for row in clause_payload.get("rows") or []
                    if isinstance(row, Mapping)
                ]
                diagnostics = clause_payload.get("diagnostics")
                if isinstance(diagnostics, Mapping):
                    shard_diagnostics[ticker] = self._project_shard_payload(
                        dict(diagnostics), ticker=ticker
                    )

            rows: list[dict[str, Any]] = []
            selected_by_ticker: Counter[str] = Counter()
            pre_truncation_count = sum(len(value) for value in rows_by_ticker.values())
            row_rank = 0
            while len(rows) < result_limit:
                added = False
                for ticker in available_tickers:
                    ticker_rows = rows_by_ticker.get(ticker) or []
                    if row_rank >= len(ticker_rows):
                        continue
                    row = dict(ticker_rows[row_rank])
                    row["planned_ticker_rank"] = ticker_rank[ticker]
                    row["planned_within_ticker_rank"] = row_rank + 1
                    rows.append(row)
                    selected_by_ticker[ticker] += 1
                    added = True
                    if len(rows) >= result_limit:
                        break
                if not added:
                    break
                row_rank += 1
            by_clause[clause_id] = {
                "rows": rows,
                "diagnostics": {
                    "execution_mode": "planned_shard_batch",
                    "retrieval_query": clause["retrieval_query"],
                    "result_count": len(rows),
                    "pre_truncation_count": pre_truncation_count,
                    "omitted_count": max(0, pre_truncation_count - len(rows)),
                    "omitted_tickers": [
                        ticker
                        for ticker, ticker_rows in rows_by_ticker.items()
                        if len(ticker_rows) > selected_by_ticker[ticker]
                    ],
                    "truncation_possible": any(
                        len(ticker_rows) >= result_limit for ticker_rows in rows_by_ticker.values()
                    ),
                    "shard_diagnostics": shard_diagnostics,
                },
            }

        failed_tickers = sorted(shard_errors)
        successful_tickers = [ticker for ticker in available_tickers if ticker not in shard_errors]
        fanout = self._fanout_diagnostics(worker_count, shard_errors)
        routing = self._route_payload("planned_shard_batch", route_tickers)
        routing.update(
            {
                "resolved_tickers": successful_tickers,
                "failed_tickers": failed_tickers,
            }
        )
        routing.update(fanout)
        return by_clause, {
            "execution_mode": "planned_shard_batch",
            "clause_count": len(normalized_clauses),
            "ticker_count": len(available_tickers),
            "shard_open_count": len(payload_by_ticker),
            "routing": routing,
            "missing_shards": self._missing_shards_for_tickers(route_tickers),
            "unknown_tickers": self._unknown_tickers(route_tickers),
            "failed_tickers": failed_tickers,
            "warnings": ["ticker_shard_query_failed"] if failed_tickers else [],
            "pre_truncation_count": sum(
                int(payload["diagnostics"].get("pre_truncation_count") or 0)
                for payload in by_clause.values()
            ),
            "omitted_count": sum(
                int(payload["diagnostics"].get("omitted_count") or 0)
                for payload in by_clause.values()
            ),
            "truncation_possible": any(
                bool(payload["diagnostics"].get("truncation_possible"))
                for payload in by_clause.values()
            ),
            "timing_ms": {"total": int((time.perf_counter() - started_at) * 1000)},
            "fallback_used": False,
            **fanout,
        }

    def query_context(
        self,
        *,
        question: str,
        ticker: str | None = None,
        tickers: Iterable[str] | None = None,
        limit_tickers: int = 5,
        **kwargs: Any,
    ) -> dict[str, Any]:
        scoped_tickers = _normalize_tickers([ticker] if ticker else tickers)
        requested_limit_tickers = max(1, int(limit_tickers or 5))
        candidate_limit = (
            requested_limit_tickers
            if scoped_tickers
            else _tickerless_query_context_limit(requested_limit_tickers)
        )
        candidate_tickers = self._candidate_tickers(
            question,
            explicit_tickers=scoped_tickers,
            limit=candidate_limit,
        )
        candidate_routing = self._candidate_routing_diagnostics(
            explicit_tickers=scoped_tickers,
            candidate_tickers=candidate_tickers,
        )
        route_tickers = scoped_tickers or candidate_tickers
        contexts: list[dict[str, Any]] = []

        def load_context(candidate: str, store: OntologyStore) -> dict[str, Any]:
            context = store.query_context(
                question=question,
                tickers=[candidate],
                **kwargs,
            )
            context.setdefault("routing", self._route_payload("company_shard", [candidate]))
            return context

        context_by_ticker, shard_errors, worker_count = self._store_fanout(
            candidate_tickers, load_context
        )
        for candidate in candidate_tickers:
            context = context_by_ticker.get(candidate)
            if isinstance(context, Mapping):
                contexts.append(self._project_shard_payload(dict(context), ticker=candidate))
        if len(contexts) == 1:
            payload = contexts[0]
            payload["routing"] = self._route_payload("company_shard", route_tickers)
            payload["routing"]["candidate_routing"] = candidate_routing
            payload["routing"].update(self._fanout_diagnostics(worker_count, shard_errors))
            payload["routing"].update(
                _candidate_budget_payload(
                    scoped=bool(scoped_tickers),
                    requested_limit=requested_limit_tickers,
                    effective_limit=candidate_limit,
                )
            )
            self._attach_missing_release_parts(payload, route_tickers)
            _attach_current_document_anchors(
                payload,
                requested_tickers=route_tickers,
                available_tickers=candidate_tickers,
                documents=self.list_documents(),
            )
            _attach_spine_cross_company_pack(
                payload,
                question=question,
                requested_tickers=route_tickers,
                available_tickers=candidate_tickers,
                documents=self.list_documents(),
            )
            _attach_chart_series_pack(
                payload,
                question=question,
                requested_tickers=route_tickers,
                chart_series_path=self._chart_series_path,
                chart_series_status=self._chart_series_status,
            )
            return payload
        routing = self._route_payload("global_spine_fanout", route_tickers)
        routing["candidate_routing"] = candidate_routing
        routing.update(self._fanout_diagnostics(worker_count, shard_errors))
        routing.update(
            _candidate_budget_payload(
                scoped=bool(scoped_tickers),
                requested_limit=requested_limit_tickers,
                effective_limit=candidate_limit,
            )
        )
        payload = _merge_query_contexts(
            contexts,
            question=question,
            routing=routing,
        )
        self._attach_missing_release_parts(payload, route_tickers)
        _attach_current_document_anchors(
            payload,
            requested_tickers=route_tickers,
            available_tickers=candidate_tickers,
            documents=self.list_documents(),
        )
        _attach_spine_cross_company_pack(
            payload,
            question=question,
            requested_tickers=route_tickers,
            available_tickers=candidate_tickers,
            documents=self.list_documents(),
        )
        _attach_chart_series_pack(
            payload,
            question=question,
            requested_tickers=route_tickers,
            chart_series_path=self._chart_series_path,
            chart_series_status=self._chart_series_status,
        )
        return payload

    def discover_company_topics(
        self,
        *,
        question: str | None = None,
        tickers: Iterable[str] | None = None,
        limit_groups: int = 10,
        limit_per_group: int = 3,
        limit: int = 50,
        **kwargs: Any,
    ) -> dict[str, Any]:
        requested_tickers = _normalize_tickers(tickers)
        candidate_tickers = self._candidate_tickers(
            question or "",
            explicit_tickers=requested_tickers,
            limit=max(1, int(limit_groups or 10)),
        )
        candidate_routing = self._candidate_routing_diagnostics(
            explicit_tickers=requested_tickers,
            candidate_tickers=candidate_tickers,
        )
        route_tickers = requested_tickers or candidate_tickers
        shard_payloads: list[dict[str, Any]] = []

        def load_discovery(ticker: str, store: OntologyStore) -> dict[str, Any]:
            return store.discover_company_topics(
                question=question,
                tickers=[ticker],
                limit_groups=1,
                limit_per_group=limit_per_group,
                limit=limit,
                **kwargs,
            )

        payload_by_ticker, shard_errors, worker_count = self._store_fanout(
            candidate_tickers, load_discovery
        )
        for ticker in candidate_tickers:
            payload = payload_by_ticker.get(ticker)
            if isinstance(payload, Mapping):
                shard_payloads.append(self._project_shard_payload(dict(payload), ticker=ticker))
        if shard_payloads:
            routing = self._route_payload("global_spine", route_tickers)
            routing["candidate_routing"] = candidate_routing
            routing.update(self._fanout_diagnostics(worker_count, shard_errors))
            return _merge_discovery_payloads(
                shard_payloads,
                question=question,
                candidate_tickers=candidate_tickers,
                routing=routing,
                limit=limit,
            )

        candidates: list[dict[str, Any]] = []
        for ticker in candidate_tickers:
            rows = self._topic_rows_for_ticker(ticker, question=question, limit=limit_per_group)
            candidates.append(
                {
                    "ticker": ticker,
                    "matched_topics": rows,
                    "top_topic": rows[0] if rows else None,
                    "score": rows[0].get("score") if rows else 0,
                    "routing": self._route_payload("global_spine", [ticker]),
                }
            )
        return {
            "question": question,
            "ticker_candidates": candidates[:limit],
            "search_diagnostics": {
                "routing": {
                    **self._route_payload("global_spine", route_tickers),
                    "candidate_routing": candidate_routing,
                    **self._fanout_diagnostics(worker_count, shard_errors),
                },
                "candidate_tickers": candidate_tickers,
                "missing_shards": self._missing_shards_for_tickers(route_tickers),
                "unknown_tickers": self._unknown_tickers(route_tickers),
                "fallback_used": False,
                **self._fanout_diagnostics(worker_count, shard_errors),
            },
        }

    def get_object(
        self,
        object_id: str,
        *,
        ticker: str | None = None,
    ) -> dict[str, Any] | None:
        ticker = self._ticker_for_object(object_id, preferred_ticker=ticker)
        if ticker is None:
            return None
        if ticker in self._missing_shard_paths:
            return None
        local_object_key = self._local_object_key(object_id, ticker=ticker)
        if local_object_key is None:
            return None
        payload = self._store_for_ticker(ticker).get_object(local_object_key)
        if not isinstance(payload, Mapping):
            return None
        return self._project_shard_payload(dict(payload), ticker=ticker)

    def find_object_ids(
        self,
        prefix: str,
        *,
        ticker: str | None = None,
        limit: int = 20,
    ) -> list[dict[str, Any]]:
        candidate_prefix = str(prefix or "").strip()
        if not candidate_prefix:
            return []
        prefix_upper_bound = _prefix_upper_bound(candidate_prefix)
        if prefix_upper_bound is None:
            return []
        preferred_ticker = str(ticker or "").strip().upper()
        resolved_limit = max(1, int(limit))
        ticker_scope_sql = ""
        locator_params: list[Any] = [candidate_prefix, prefix_upper_bound]
        if preferred_ticker:
            ticker_scope_sql = """
                  AND EXISTS (
                        SELECT 1
                        FROM global_object_replica AS replica
                        WHERE replica.object_id = locator.object_id
                          AND replica.ticker = ?
                  )
            """
            locator_params.append(preferred_ticker)
        locator_params.append(resolved_limit)
        object_ids = {
            str(row["object_id"])
            for row in self.conn.execute(
                f"""
                SELECT locator.object_id
                FROM global_object_locator AS locator
                WHERE locator.object_id >= ?
                  AND locator.object_id < ?
                  {ticker_scope_sql}
                ORDER BY locator.object_id
                LIMIT ?
                """,
                locator_params,
            ).fetchall()
        }

        lookup_tickers = (
            [preferred_ticker]
            if preferred_ticker
            else sorted(
                set(self._declared_shard_paths)
                | set(self._shard_paths)
                | set(self._missing_shard_paths)
            )
        )
        for lookup_ticker in lookup_tickers:
            local_rows = self.conn.execute(
                """
                SELECT object_id
                FROM global_object_replica
                WHERE ticker = ?
                  AND local_object_key >= ?
                  AND local_object_key < ?
                ORDER BY local_object_key, object_id
                LIMIT ?
                """,
                (
                    lookup_ticker,
                    candidate_prefix,
                    prefix_upper_bound,
                    resolved_limit,
                ),
            ).fetchall()
            object_ids.update(str(row["object_id"]) for row in local_rows)

        selected_ids = sorted(object_ids)[:resolved_limit]
        if not selected_ids:
            return []
        placeholders = ",".join("?" for _ in selected_ids)
        rows = self.conn.execute(
            f"""
            SELECT object_id AS id, ticker, object_type AS type,
                   compact_label AS label, compact_summary AS text,
                   occurrence_count
            FROM global_object_locator
            WHERE object_id IN ({placeholders})
            ORDER BY object_id
            """,
            selected_ids,
        ).fetchall()
        results: list[dict[str, Any]] = []
        for row in rows:
            result = dict(row)
            locator = self._object_locator_payload(
                str(row["id"]),
                preferred_ticker or None,
            )
            result["ticker"] = locator.get("ticker") or result.get("ticker")
            result["replica_preview_tickers"] = list(
                dict.fromkeys(
                    str(item.get("ticker") or "")
                    for item in locator.get("replica_locations") or []
                    if item.get("ticker")
                )
            )
            result["replica_locations_truncated"] = bool(locator.get("replica_locations_truncated"))
            results.append(result)
        return results

    def trace(
        self,
        object_id: str,
        *,
        ticker: str | None = None,
    ) -> dict[str, Any] | None:
        if not str(ticker or "").strip():
            ticker_candidates = self._object_ticker_candidates(object_id, limit=2)
            if len(ticker_candidates) > 1:
                return self._ambiguous_object_ticker_payload(
                    object_id,
                    operation="trace",
                )
        ticker = self._ticker_for_object(object_id, preferred_ticker=ticker)
        if ticker is None:
            return None
        index_object_id = self._index_object_id(object_id, ticker=ticker)
        if index_object_id is None:
            return None
        locator = self._object_locator_payload(index_object_id, ticker)
        if ticker in self._missing_shard_paths:
            payload = self._missing_shard_payload(ticker, operation="trace")
            payload["object_id"] = index_object_id
            payload["object_locator"] = locator
            return payload
        local_object_key = self._local_object_key(index_object_id, ticker=ticker)
        if local_object_key is None:
            return None
        payload = self._store_for_ticker(ticker).trace(local_object_key)
        if payload is not None:
            payload = self._project_shard_payload(payload, ticker=ticker)
            payload.setdefault("routing", self._route_payload("object_locator", [ticker]))
            payload.setdefault("object_locator", locator)
        return payload

    def chain(
        self,
        object_id: str,
        *,
        ticker: str | None = None,
        **kwargs: Any,
    ) -> dict[str, Any] | None:
        if not str(ticker or "").strip():
            ticker_candidates = self._object_ticker_candidates(object_id, limit=2)
            if len(ticker_candidates) > 1:
                return self._ambiguous_object_ticker_payload(
                    object_id,
                    operation="chain",
                )
        ticker = self._ticker_for_object(object_id, preferred_ticker=ticker)
        if ticker is None:
            return None
        index_object_id = self._index_object_id(object_id, ticker=ticker)
        if index_object_id is None:
            return None
        locator = self._object_locator_payload(index_object_id, ticker)
        if ticker in self._missing_shard_paths:
            payload = self._missing_shard_payload(ticker, operation="chain")
            payload["object_id"] = index_object_id
            payload["object_locator"] = locator
            return payload
        local_object_key = self._local_object_key(index_object_id, ticker=ticker)
        if local_object_key is None:
            return None
        chain = self._store_for_ticker(ticker).chain(local_object_key, **kwargs)
        if chain is None:
            return None
        chain = self._project_shard_payload(chain, ticker=ticker)
        max_depth = max(
            0,
            min(int(kwargs.get("max_depth", 2)), _GLOBAL_CHAIN_MAX_DEPTH),
        )
        direction = _normalize_global_chain_direction(str(kwargs.get("direction") or "both"))
        chain["global_spine_neighbors"] = self._chain_neighbors(
            index_object_id,
            ticker=ticker,
        )
        chain["global_chain"] = self._global_chain_paths(
            index_object_id,
            max_depth=max_depth,
            direction=direction,
            root_locator=locator,
        )
        chain["routing"] = self._route_payload("object_locator", [ticker])
        chain["object_locator"] = locator
        return chain

    def bundle(self, object_id: str) -> dict[str, Any]:
        ticker = self._ticker_for_object(object_id)
        if ticker is None:
            raise KeyError(f"object not found or ambiguous in global locator: {object_id}")
        index_object_id = self._index_object_id(object_id, ticker=ticker)
        if index_object_id is None:
            raise KeyError(f"object location missing in global locator: {object_id}")
        local_object_key = self._local_object_key(index_object_id, ticker=ticker)
        if local_object_key is None:
            raise KeyError(f"object location missing in global locator: {object_id}")
        payload = self._store_for_ticker(ticker).bundle(local_object_key)
        return self._project_shard_payload(payload, ticker=ticker)

    def compare(self, *, tickers: Iterable[str], **kwargs: Any) -> dict[str, Any]:
        return self._fanout_compare(tickers=tickers, compact=False, **kwargs)

    def compare_compact(self, *, tickers: Iterable[str], **kwargs: Any) -> dict[str, Any]:
        return self._fanout_compare(tickers=tickers, compact=True, **kwargs)

    def quality(self, *, ticker: str | None = None, **kwargs: Any) -> dict[str, Any]:
        if ticker:
            if str(ticker or "").upper() in self._missing_shard_paths:
                payload = self._missing_shard_payload(ticker, operation="quality")
                return {
                    "documents": [],
                    "events": [],
                    "summary": {
                        "documents": 0,
                        "events": 0,
                        "rejected_objects": 0,
                        "batch_failures": 0,
                        "section_warnings": 0,
                        "status": "not_answerable_from_current_release",
                        "missing_parts": ["ticker_shard_missing"],
                        "missing_shards": payload["missing_shards"],
                    },
                    "routing": payload["routing"],
                }
            payload = self._project_shard_payload(
                self._store_for_ticker(ticker).quality(ticker=ticker, **kwargs),
                ticker=str(ticker).upper(),
            )
            payload["routing"] = self._route_payload("company_shard", [ticker.upper()])
            return payload
        documents = self.list_documents()
        events: list[dict[str, Any]] = []

        def load_quality(shard_ticker: str, store: OntologyStore) -> dict[str, Any]:
            return store.quality(ticker=shard_ticker, **kwargs)

        quality_by_ticker, shard_errors, worker_count = self._store_fanout(
            sorted(self._shard_paths), load_quality
        )
        for shard_ticker in sorted(self._shard_paths):
            shard_quality = quality_by_ticker.get(shard_ticker)
            if not isinstance(shard_quality, Mapping):
                continue
            projected_quality = self._project_shard_payload(
                dict(shard_quality),
                ticker=shard_ticker,
            )
            events.extend(projected_quality.get("events") or [])
        routing = self._route_payload("quality_release_scan", self.list_companies())
        routing.update(self._fanout_diagnostics(worker_count, shard_errors))
        return {
            "documents": documents,
            "events": events,
            "summary": {
                "documents": len(documents),
                "events": len(events),
                "rejected_objects": sum(
                    1 for event in events if event.get("category") == "rejected_object"
                ),
                "batch_failures": sum(
                    1 for event in events if event.get("category") == "batch_failure"
                ),
                "section_warnings": sum(
                    1 for event in events if event.get("category") == "section_quality"
                ),
                "missing_company_shards": len(self._missing_shard_paths),
                "missing_shards": {
                    ticker: str(path) for ticker, path in sorted(self._missing_shard_paths.items())
                },
                "shard_errors": dict(sorted(shard_errors.items())),
                "fanout_workers": worker_count,
            },
            "topology": self.routing_status(),
            "routing": routing,
        }

    def search_diagnostics(self, *args: Any, **kwargs: Any) -> dict[str, Any]:
        return {
            "routing": self.routing_status(),
            "fallback_used": False,
            "args": len(args),
            "kwargs": sorted(kwargs),
        }

    def __getattr__(self, name: str) -> Any:
        raise AttributeError(f"{type(self).__name__} has no fallback for {name!r}")

    def _fanout_worker_count(self, count: int) -> int:
        if count <= 0:
            return 0
        configured = _read_int_env(
            _ROUTER_FANOUT_WORKERS_ENV,
            _DEFAULT_ROUTER_FANOUT_WORKERS,
            min_value=1,
        )
        return max(1, min(count, configured, _MAX_ROUTER_FANOUT_WORKERS))

    def _store_fanout(
        self,
        tickers: Sequence[str],
        callback: Callable[[str, OntologyStore], Any],
    ) -> tuple[dict[str, Any], dict[str, str], int]:
        available_tickers = [ticker for ticker in tickers if ticker in self._shard_paths]
        worker_count = self._fanout_worker_count(len(available_tickers))
        results: dict[str, Any] = {}
        errors: dict[str, str] = {}
        if not available_tickers:
            return results, errors, worker_count
        if worker_count <= 1:
            for ticker in available_tickers:
                try:
                    results[ticker] = callback(ticker, self._store_for_ticker(ticker))
                except Exception as exc:  # pragma: no cover - defensive partial-failure path
                    errors[ticker] = _fanout_error(exc)
            return results, errors, worker_count

        def run(ticker: str) -> tuple[str, Any]:
            with OntologyStore(
                self._shard_paths[ticker],
                check_same_thread=True,
                immutable=True,
            ) as store:
                return ticker, callback(ticker, store)

        with ThreadPoolExecutor(max_workers=worker_count) as executor:
            future_to_ticker = {
                executor.submit(run, ticker): ticker for ticker in available_tickers
            }
            for future in as_completed(future_to_ticker):
                ticker = future_to_ticker[future]
                try:
                    result_ticker, payload = future.result()
                except (
                    Exception
                ) as exc:  # pragma: no cover - exercised through integration failures
                    errors[ticker] = _fanout_error(exc)
                    continue
                results[result_ticker] = payload
        return results, errors, worker_count

    def _fanout_diagnostics(self, worker_count: int, errors: Mapping[str, str]) -> dict[str, Any]:
        payload: dict[str, Any] = {
            "fanout_parallel": worker_count > 1,
            "fanout_workers": worker_count,
        }
        if errors:
            payload["shard_errors"] = dict(sorted(errors.items()))
        return payload

    def _fanout_query(
        self, *, compact: bool, **kwargs: Any
    ) -> tuple[list[dict[str, Any]], dict[str, Any]]:
        explicit_tickers = _normalize_tickers(kwargs.get("tickers"))
        topic = str(kwargs.get("topic") or kwargs.get("question") or "")
        limit = max(1, int(kwargs.get("limit") or 20))
        candidate_tickers = self._candidate_tickers(
            topic, explicit_tickers=explicit_tickers, limit=min(limit, 10)
        )
        candidate_routing = self._candidate_routing_diagnostics(
            explicit_tickers=explicit_tickers,
            candidate_tickers=candidate_tickers,
        )
        route_tickers = explicit_tickers or candidate_tickers
        results: list[dict[str, Any]] = []
        shard_diagnostics: dict[str, Any] = {}
        primary_diagnostics: dict[str, Any] = {}

        def load_query(ticker: str, store: OntologyStore) -> dict[str, Any]:
            ticker_kwargs = {**kwargs, "tickers": None, "limit": limit}
            if compact:
                rows, diagnostics = store.query_compact_with_diagnostics(**ticker_kwargs)
            else:
                rows, diagnostics = store.query_with_diagnostics(**ticker_kwargs)
            return {"rows": list(rows), "diagnostics": diagnostics}

        query_by_ticker, shard_errors, worker_count = self._store_fanout(
            candidate_tickers, load_query
        )
        for ticker in candidate_tickers:
            payload = query_by_ticker.get(ticker)
            if not isinstance(payload, Mapping):
                continue
            rows = list(payload.get("rows") or [])
            diagnostics = (
                payload.get("diagnostics")
                if isinstance(payload.get("diagnostics"), Mapping)
                else {}
            )
            diagnostics = self._project_shard_payload(dict(diagnostics), ticker=ticker)
            shard_diagnostics[ticker] = diagnostics
            if not primary_diagnostics and isinstance(diagnostics, Mapping):
                primary_diagnostics = dict(diagnostics)
            results.extend(
                self._project_shard_payload(row, ticker=ticker)
                for row in rows
                if isinstance(row, Mapping)
            )
            if len(results) >= limit:
                break
        fanout_diagnostics = self._fanout_diagnostics(worker_count, shard_errors)
        routing = self._route_payload("global_spine_fanout", route_tickers)
        routing["candidate_routing"] = candidate_routing
        routing.update(fanout_diagnostics)
        diagnostics = dict(primary_diagnostics)
        diagnostics.update(
            {
                "result_count": min(len(results), limit),
                "routing": routing,
                "shard_diagnostics": shard_diagnostics,
                "missing_shards": self._missing_shards_for_tickers(route_tickers),
                "unknown_tickers": self._unknown_tickers(route_tickers),
                "fallback_used": False,
                **fanout_diagnostics,
            }
        )
        return results[:limit], diagnostics

    def _fanout_compare(
        self, *, tickers: Iterable[str], compact: bool, **kwargs: Any
    ) -> dict[str, Any]:
        ticker_list = _normalize_tickers(tickers) or []
        results: dict[str, list[dict[str, Any]]] = {}
        evaluations: dict[str, dict[str, Any]] = {}
        contexts: dict[str, dict[str, Any]] = {}
        available_tickers: list[str] = []
        for ticker in ticker_list:
            if ticker not in self._shard_paths:
                reason = (
                    "ticker_shard_missing"
                    if ticker in self._missing_shard_paths
                    else "ticker_shard_not_found"
                )
                results[ticker] = []
                evaluations[ticker] = {
                    "status": "missing",
                    "reason": reason,
                }
                contexts[ticker] = {
                    "research_status": "not_answerable_from_current_release",
                    "missing_parts": [reason],
                    "routing": self._route_payload("compare_fanout", [ticker]),
                    "missing_shards": self._missing_shards_for_tickers([ticker]),
                    "unknown_tickers": self._unknown_tickers([ticker]),
                }
                continue
            available_tickers.append(ticker)

        def load_compare(ticker: str, store: OntologyStore) -> dict[str, Any]:
            compare_fn = store.compare_compact if compact else store.compare
            return compare_fn(tickers=[ticker], **kwargs)

        payload_by_ticker, shard_errors, worker_count = self._store_fanout(
            available_tickers, load_compare
        )
        for ticker in available_tickers:
            payload = payload_by_ticker.get(ticker)
            if not isinstance(payload, Mapping):
                results[ticker] = []
                evaluations[ticker] = {
                    "status": "error",
                    "reason": shard_errors.get(ticker, "ticker_shard_query_failed"),
                }
                contexts[ticker] = {
                    "research_status": "not_answerable_from_current_release",
                    "missing_parts": ["ticker_shard_query_failed"],
                    "routing": self._route_payload("compare_fanout", [ticker]),
                }
                continue
            results[ticker] = [
                self._project_shard_payload(row, ticker=ticker)
                for row in (payload.get("results") or {}).get(ticker) or []
                if isinstance(row, Mapping)
            ]
            evaluations[ticker] = self._project_shard_payload(
                dict((payload.get("comparison_evaluations") or {}).get(ticker) or {}),
                ticker=ticker,
            )
            contexts[ticker] = self._project_shard_payload(
                dict((payload.get("comparison_contexts") or {}).get(ticker) or {}),
                ticker=ticker,
            )
            contexts[ticker].setdefault("routing", self._route_payload("company_shard", [ticker]))
        fanout_diagnostics = self._fanout_diagnostics(worker_count, shard_errors)
        routing = self._route_payload("compare_fanout", ticker_list)
        routing.update(fanout_diagnostics)
        return {
            "mode": "metric" if kwargs.get("metric") else "topic",
            "topic": kwargs.get("topic"),
            "metric": kwargs.get("metric"),
            "tickers": ticker_list,
            "results": results,
            "comparison_evaluations": evaluations,
            "comparison_contexts": contexts,
            "compact_fast_path": compact,
            "missing_shards": self._missing_shards_for_tickers(ticker_list),
            "unknown_tickers": self._unknown_tickers(ticker_list),
            "fallback_used": False,
            "routing": routing,
            **fanout_diagnostics,
        }

    def _candidate_tickers(
        self,
        question: str,
        *,
        explicit_tickers: Sequence[str] | None,
        limit: int,
    ) -> list[str]:
        resolved_limit = max(1, int(limit))
        if explicit_tickers:
            return [ticker for ticker in explicit_tickers if ticker in self._shard_paths]
        return self._rank_candidate_tickers(question, limit=resolved_limit)

    def _rank_candidate_tickers(self, question: str, *, limit: int) -> list[str]:
        resolved_limit = max(1, int(limit))
        if not str(question or "").strip() or not self._router_sidecar_path.is_file():
            return []
        try:
            result = self._sidecar().search(question, limit=resolved_limit)
        except (KeyError, OSError, RuntimeError, TypeError, ValueError, sqlite3.Error):
            return []
        ranked: list[str] = []
        for candidate in result.get("ticker_candidates") or []:
            if not isinstance(candidate, Mapping):
                continue
            ticker = str(candidate.get("ticker") or "").strip().upper()
            if not ticker or ticker not in self._shard_paths or ticker in ranked:
                continue
            ranked.append(ticker)
            if len(ranked) >= resolved_limit:
                break
        return ranked

    def _candidate_routing_diagnostics(
        self,
        *,
        explicit_tickers: Sequence[str] | None,
        candidate_tickers: Sequence[str],
    ) -> dict[str, Any]:
        explicit = bool(explicit_tickers)
        sidecar_path = self.__dict__.get("_router_sidecar_path")
        sidecar_available = isinstance(sidecar_path, Path) and sidecar_path.is_file()
        coherence_path = self.__dict__.get("_router_coherence_path")
        coherence_available = isinstance(coherence_path, Path) and coherence_path.is_file()
        status = "resolved" if candidate_tickers else "empty"
        reason: str | None = None
        if not explicit and not sidecar_available:
            reason = "router_sidecar_unavailable"
        elif not explicit and not candidate_tickers:
            reason = "router_sidecar_no_candidates"
        payload: dict[str, Any] = {
            "source": "explicit_scope" if explicit else "router_sidecar",
            "status": status,
            "candidate_count": len(candidate_tickers),
            "router_sidecar_available": sidecar_available,
            "router_coherence_available": coherence_available,
            "fallback_used": False,
        }
        if reason:
            payload["reason"] = reason
        return payload

    def _store_for_ticker(self, ticker: str | None) -> OntologyStore:
        normalized = str(ticker or "").upper()
        if normalized in self._missing_shard_paths:
            raise FileNotFoundError(
                f"ticker shard missing: {normalized}: {self._missing_shard_paths[normalized]}"
            )
        if normalized not in self._shard_paths:
            raise KeyError(f"ticker shard not found: {normalized or '<missing>'}")
        store = self._shards.get(normalized)
        if store is None:
            store = OntologyStore(
                self._shard_paths[normalized],
                check_same_thread=self.check_same_thread,
                immutable=True,
            )
            self._shards[normalized] = store
        return store

    def _store_for_object_id(self, object_id: str) -> OntologyStore:
        ticker = self._ticker_for_object(object_id)
        if ticker is None:
            raise KeyError(f"object not found in global locator: {object_id}")
        return self._store_for_ticker(ticker)

    def _ticker_for_object(
        self,
        object_id: str,
        *,
        preferred_ticker: str | None = None,
    ) -> str | None:
        preferred = str(preferred_ticker or "").strip().upper()
        if preferred:
            row = self._replica_row_for_object(object_id, ticker=preferred)
            return str(row["ticker"]).upper() if row else None
        candidates = self._object_ticker_candidates(object_id, limit=2)
        return candidates[0] if len(candidates) == 1 else None

    def _index_object_id(self, object_id: str, *, ticker: str) -> str | None:
        normalized_ticker = str(ticker or "").strip().upper()
        candidate = str(object_id or "").strip()
        if not normalized_ticker or not candidate:
            return None
        cache_key = (normalized_ticker, candidate)
        if cache_key in self._local_to_index_id_cache:
            return self._local_to_index_id_cache[cache_key]
        row = self._replica_row_for_object(candidate, ticker=normalized_ticker)
        projected = str(row["object_id"]) if row is not None else None
        self._remember_object_id_projection(cache_key, projected)
        if row is not None:
            local_key = str(row["local_object_key"] or "").strip()
            if local_key:
                self._remember_object_id_projection((normalized_ticker, local_key), projected)
        return projected

    def _remember_object_id_projection(
        self,
        key: tuple[str, str],
        value: str | None,
    ) -> None:
        self._local_to_index_id_cache[key] = value
        self._local_to_index_id_cache.move_to_end(key)
        while len(self._local_to_index_id_cache) > _OBJECT_ID_PROJECTION_CACHE_MAX:
            self._local_to_index_id_cache.popitem(last=False)

    def _local_object_key(self, object_id: str, *, ticker: str) -> str | None:
        row = self._replica_row_for_object(object_id, ticker=str(ticker).upper())
        if row is None:
            return None
        return str(row["local_object_key"] or object_id)

    def _replica_row_for_object(
        self,
        object_id: str,
        *,
        ticker: str,
    ) -> sqlite3.Row | None:
        normalized_ticker = str(ticker or "").strip().upper()
        candidate = str(object_id or "").strip()
        if not normalized_ticker or not candidate:
            return None
        row = self.conn.execute(
            """
            SELECT object_id, ticker, local_object_key
            FROM global_object_replica
            WHERE object_id = ? AND ticker = ?
            ORDER BY document_id, document_type, period, shard_id, shard_path
            LIMIT 1
            """,
            (candidate, normalized_ticker),
        ).fetchone()
        if row is not None:
            return row
        return self.conn.execute(
            """
            SELECT object_id, ticker, local_object_key
            FROM global_object_replica
            WHERE ticker = ? AND local_object_key = ?
            ORDER BY document_id, document_type, period, shard_id, shard_path
            LIMIT 1
            """,
            (normalized_ticker, candidate),
        ).fetchone()

    def _object_ticker_candidates(
        self,
        object_id: str,
        *,
        limit: int = _OBJECT_REPLICA_PREVIEW_LIMIT + 1,
    ) -> list[str]:
        candidate = str(object_id or "").strip()
        if not candidate:
            return []
        resolved_limit = max(1, int(limit))
        rows = self.conn.execute(
            """
            SELECT DISTINCT ticker
            FROM global_object_replica
            WHERE object_id = ?
            ORDER BY ticker
            LIMIT ?
            """,
            (candidate, resolved_limit),
        ).fetchall()
        tickers = {str(row["ticker"]).upper() for row in rows}
        if len(tickers) < resolved_limit:
            lookup_tickers = sorted(
                set(self._declared_shard_paths)
                | set(self._shard_paths)
                | set(self._missing_shard_paths)
            )
            for lookup_ticker in lookup_tickers:
                row = self.conn.execute(
                    """
                    SELECT 1
                    FROM global_object_replica
                    WHERE ticker = ? AND local_object_key = ?
                    LIMIT 1
                    """,
                    (lookup_ticker, candidate),
                ).fetchone()
                if row is not None:
                    tickers.add(lookup_ticker)
                    if len(tickers) >= resolved_limit:
                        break
        return sorted(tickers)[:resolved_limit]

    def _object_id_projection_map(
        self,
        local_object_keys: Iterable[str],
        *,
        ticker: str,
    ) -> dict[str, str]:
        normalized_ticker = str(ticker or "").strip().upper()
        candidates = sorted(
            {str(value).strip() for value in local_object_keys if str(value or "").strip()}
        )
        if not normalized_ticker or not candidates:
            return {}
        resolved = {
            value: projected
            for value in candidates
            if (projected := self._local_to_index_id_cache.get((normalized_ticker, value)))
            is not None
        }
        missing = [
            value
            for value in candidates
            if (normalized_ticker, value) not in self._local_to_index_id_cache
        ]
        for offset in range(0, len(missing), _OBJECT_ID_LOOKUP_CHUNK_SIZE):
            chunk = missing[offset : offset + _OBJECT_ID_LOOKUP_CHUNK_SIZE]
            placeholders = ",".join("?" for _ in chunk)
            rows = self.conn.execute(
                f"""
                SELECT local_object_key, object_id
                FROM global_object_replica
                WHERE ticker = ?
                  AND local_object_key IN ({placeholders})
                """,
                [normalized_ticker, *chunk],
            ).fetchall()
            found: set[str] = set()
            for row in rows:
                local_key = str(row["local_object_key"] or "").strip()
                if not local_key:
                    continue
                projected = str(row["object_id"])
                found.add(local_key)
                resolved[local_key] = projected
                self._remember_object_id_projection((normalized_ticker, local_key), projected)
                self._remember_object_id_projection((normalized_ticker, projected), projected)
            for value in chunk:
                if value not in found:
                    self._remember_object_id_projection((normalized_ticker, value), None)
        return resolved

    def _project_shard_payload(self, payload: Any, *, ticker: str) -> Any:
        normalized_ticker = str(ticker or "").strip().upper()
        candidates: set[str] = set()

        def collect(value: Any, field: str | None = None) -> None:
            if isinstance(value, Mapping):
                for key, item in value.items():
                    key_text = str(key)
                    if _is_object_id_scalar_field(key_text) and isinstance(item, str):
                        candidates.add(item)
                    elif _is_object_id_list_field(key_text) and isinstance(item, (list, tuple)):
                        candidates.update(entry for entry in item if isinstance(entry, str))
                    collect(item, key_text)
                return
            if isinstance(value, (list, tuple)):
                for item in value:
                    collect(item, field)

        collect(payload)
        projection = self._object_id_projection_map(candidates, ticker=ticker)

        def rewrite(value: Any, field: str | None = None) -> Any:
            if isinstance(value, Mapping):
                rewritten: dict[Any, Any] = {}
                replaced_id: str | None = None
                for key, item in value.items():
                    key_text = str(key)
                    if _is_object_id_scalar_field(key_text) and isinstance(item, str):
                        projected = projection.get(item, item)
                        rewritten[key] = projected
                        if key_text == "id" and projected != item:
                            replaced_id = item
                    elif (
                        key_text.casefold() in _OCCURRENCE_TICKER_FIELDS
                        and isinstance(item, str)
                        and normalized_ticker
                    ):
                        rewritten[key] = effective_ticker(item, fallback=normalized_ticker)
                    elif _is_object_id_list_field(key_text) and isinstance(item, (list, tuple)):
                        rewritten[key] = [
                            projection.get(entry, entry)
                            if isinstance(entry, str)
                            else rewrite(entry)
                            for entry in item
                        ]
                    else:
                        rewritten[key] = rewrite(item, key_text)
                if replaced_id is not None and "local_object_key" not in rewritten:
                    rewritten["local_object_key"] = replaced_id
                return rewritten
            if isinstance(value, list):
                return [rewrite(item, field) for item in value]
            if isinstance(value, tuple):
                return tuple(rewrite(item, field) for item in value)
            return value

        return rewrite(payload)

    def _ambiguous_object_ticker_payload(
        self,
        object_id: str,
        *,
        operation: str,
    ) -> dict[str, Any]:
        ticker_candidates = self._object_ticker_candidates(object_id)
        ticker_candidate_count = int(
            self.conn.execute(
                """
                SELECT COUNT(DISTINCT ticker)
                FROM global_object_replica
                WHERE object_id = ? OR local_object_key = ?
                """,
                (object_id, object_id),
            ).fetchone()[0]
        )
        locator = self._object_locator_payload(
            object_id,
            ticker_candidates[0] if ticker_candidates else None,
        )
        for key in (
            "ticker",
            "company_name",
            "document_id",
            "document_type",
            "period",
            "filing_date",
            "shard_id",
            "shard_path",
            "local_object_key",
            "object_hash",
            "quality_status",
            "resolved_shard_path",
        ):
            locator[key] = None
        locator.update(
            {
                "location_selection": "ambiguous",
                "shard_available": False,
                "shard_missing": False,
            }
        )
        return {
            "error": {
                "code": "ambiguous_object_ticker",
                "message": (f"Object id has occurrences in multiple ticker shards: {object_id}"),
                "suggestion": (
                    f"Pass one ticker from ticker_candidates to krw_ontology_{operation}."
                ),
                "details": {
                    "object_id": object_id,
                    "operation": operation,
                    "ticker_candidate_count": ticker_candidate_count,
                },
            },
            "object_id": object_id,
            "ticker_candidates": ticker_candidates[:_OBJECT_REPLICA_PREVIEW_LIMIT],
            "ticker_candidates_truncated": ticker_candidate_count > _OBJECT_REPLICA_PREVIEW_LIMIT,
            "object_locator": locator,
            "fallback_used": False,
        }

    def _object_locator_payload(self, object_id: str, ticker: str | None = None) -> dict[str, Any]:
        row = self.conn.execute(
            """
            SELECT object_id, ticker, company_name, document_id, document_type, period,
                   filing_date, object_type, shard_id, shard_path, local_object_key,
                   object_hash, semantic_hash, compact_label, compact_summary,
                   quality_status, occurrence_count
            FROM global_object_locator
            WHERE object_id = ?
            """,
            (object_id,),
        ).fetchone()
        canonical = dict(row) if row else {}
        canonical_ticker = str(canonical.get("ticker") or "").upper()
        normalized = str(ticker or canonical_ticker or "").upper()
        replica_rows = self.conn.execute(
            """
            SELECT replica.object_id, replica.ticker, catalog.company_name,
                   replica.document_id, replica.document_type, replica.period,
                   catalog.filing_date, replica.object_type, replica.shard_id,
                   replica.shard_path, replica.local_object_key, replica.object_hash,
                   replica.semantic_hash, replica.quality_status
            FROM global_object_replica AS replica
            LEFT JOIN global_document_catalog AS catalog
              ON catalog.document_id = replica.document_id
            WHERE replica.object_id = ?
            ORDER BY CASE WHEN replica.ticker = ? THEN 0 ELSE 1 END,
                     replica.ticker, replica.document_id, replica.document_type,
                     replica.period, replica.shard_id, replica.shard_path
            LIMIT ?
            """,
            (object_id, normalized, _OBJECT_REPLICA_PREVIEW_LIMIT + 1),
        ).fetchall()
        replica_locations = [
            _clean_replica_location(dict(replica_row))
            for replica_row in replica_rows[:_OBJECT_REPLICA_PREVIEW_LIMIT]
        ]
        selected_location = next(
            (
                location
                for location in replica_locations
                if str(location.get("ticker") or "").upper() == normalized
            ),
            None,
        )
        if selected_location is not None:
            normalized = str(selected_location.get("ticker") or normalized).upper()
        declared_path = self._declared_shard_paths.get(normalized)
        available_path = self._shard_paths.get(normalized)
        missing_path = self._missing_shard_paths.get(normalized)
        payload = canonical or {"object_id": object_id, "ticker": normalized}
        if selected_location is not None:
            payload.update(selected_location)
        payload.update(
            {
                "ticker": normalized,
                "canonical_ticker": canonical_ticker or None,
                "location_selection": "requested_ticker" if ticker else "canonical",
                "semantic_shared": int(canonical.get("occurrence_count") or 0) > 1,
                "replica_locations": replica_locations,
                "replica_locations_truncated": len(replica_rows) > _OBJECT_REPLICA_PREVIEW_LIMIT,
                "resolved_shard_path": str(available_path or declared_path or missing_path or ""),
                "shard_available": normalized in self._shard_paths,
                "shard_missing": normalized in self._missing_shard_paths,
                "fallback_used": False,
            }
        )
        return payload

    def _topic_rows_for_ticker(
        self, ticker: str, *, question: str | None, limit: int
    ) -> list[dict[str, Any]]:
        params: list[Any] = [ticker]
        match_sql = ""
        terms = _like_terms(question or "")
        if terms:
            match_sql = (
                "AND ("
                + " OR ".join("topic_label LIKE ? OR topic_summary LIKE ?" for _ in terms)
                + ")"
            )
            params.extend(value for term in terms for value in (f"%{term}%", f"%{term}%"))
        params.append(max(1, int(limit)))
        rows = self.conn.execute(
            f"""
            SELECT topic_id, topic_key, topic_label, topic_summary, ticker,
                   evidence_grade, materiality
            FROM global_topic_spine
            WHERE ticker = ?
            {match_sql}
            ORDER BY COALESCE(materiality, 0) DESC, topic_label
            LIMIT ?
            """,
            params,
        ).fetchall()
        return [
            {
                "topic_id": row["topic_id"],
                "topic_key": row["topic_key"],
                "topic_label": row["topic_label"],
                "topic_summary": row["topic_summary"],
                "ticker": row["ticker"],
                "evidence_grade": row["evidence_grade"],
                "materiality": row["materiality"],
                "score": row["materiality"] or 0,
            }
            for row in rows
        ]

    def _chain_neighbors(
        self,
        object_id: str,
        *,
        ticker: str,
    ) -> list[dict[str, Any]]:
        rows: dict[str, dict[str, Any]] = {}
        sql = """
            SELECT link_id, link_type, from_ticker, to_ticker, shared_key,
                   shared_key_type, from_object_id, to_object_id, weight,
                   confidence, evidence_grade, materiality, generic_penalty,
                   recency_score, explanation_template
            FROM global_chain_index
            WHERE {object_column} = ? AND {ticker_column} = ?
            ORDER BY COALESCE(weight, 0) DESC, link_id
            LIMIT 20
        """
        for object_column, ticker_column in (
            ("from_object_id", "from_ticker"),
            ("to_object_id", "to_ticker"),
        ):
            for row in self.conn.execute(
                sql.format(
                    object_column=object_column,
                    ticker_column=ticker_column,
                ),
                (object_id, ticker),
            ).fetchall():
                payload = dict(row)
                payload["semantic_role"] = (
                    "routing_only_similarity"
                    if payload.get("link_type") == "similar_topic"
                    else "cross_company_association"
                )
                payload["causal_inference_allowed"] = False
                rows[str(payload["link_id"])] = payload
        return sorted(
            rows.values(),
            key=lambda row: (-float(row.get("weight") or 0.0), str(row.get("link_id") or "")),
        )[:20]

    def _global_chain_paths(
        self,
        object_id: str,
        *,
        max_depth: int,
        direction: str,
        root_locator: Mapping[str, Any] | None = None,
    ) -> dict[str, Any]:
        """Traverse local ontology edges and exact cross-company associations.

        The traversal is deliberately bounded and best-first.  Similar-topic
        links remain visible as one-hop routing candidates, but are excluded
        from evidence paths because lexical similarity is not proof of a
        relationship or causal mechanism.
        """
        root = (
            dict(root_locator)
            if root_locator is not None
            else self._global_locator_compact(object_id)
        )
        root_ticker = str((root or {}).get("ticker") or "").upper()
        if root is None or max_depth <= 0:
            return {
                "algorithm": "bounded_best_first_occurrence/v2",
                "root_object_id": object_id,
                "root_ticker": root_ticker or None,
                "max_depth": max_depth,
                "direction": direction,
                "paths": [],
                "path_count": 0,
                "cross_company_path_count": 0,
                "expanded_states": 0,
                "frontier_remaining": 0,
                "truncated": False,
                "stop_reason": "max_depth_zero" if max_depth <= 0 else "root_not_found",
                "similarity_links_in_evidence_paths": False,
            }

        root_key = (root_ticker, object_id)
        frontier: list[
            tuple[
                float,
                int,
                int,
                str,
                str,
                list[dict[str, Any]],
                frozenset[tuple[str, str]],
                tuple[str, ...],
            ]
        ] = []
        sequence = 0
        heapq.heappush(
            frontier,
            (
                -1.0,
                0,
                sequence,
                root_ticker,
                object_id,
                [],
                frozenset({root_key}),
                (root_ticker,),
            ),
        )
        locator_cache: dict[tuple[str, str], dict[str, Any]] = {root_key: root}
        paths: list[dict[str, Any]] = []
        expanded_states = 0

        while (
            frontier
            and len(paths) < _GLOBAL_CHAIN_MAX_PATHS
            and expanded_states < _GLOBAL_CHAIN_MAX_EXPANSIONS
        ):
            (
                negative_score,
                depth,
                _order,
                current_ticker,
                current_id,
                steps,
                seen_nodes,
                tickers,
            ) = heapq.heappop(frontier)
            score = -negative_score
            if steps:
                paths.append(
                    _global_chain_path_payload(
                        object_id,
                        root_ticker=root_ticker,
                        steps=steps,
                        score=score,
                        tickers=tickers,
                    )
                )
            if depth >= max_depth:
                continue
            current_key = (current_ticker, current_id)
            current_locator = locator_cache.get(current_key)
            if current_locator is None:
                current_locator = self._global_locator_compact(
                    current_id,
                    ticker=current_ticker,
                )
                if current_locator is None:
                    continue
                locator_cache[current_key] = current_locator
            expanded_states += 1
            for neighbor in self._global_graph_neighbors(
                current_id,
                ticker=current_ticker,
                current_locator=current_locator,
                direction=direction,
            ):
                neighbor_id = str(neighbor["neighbor_object_id"])
                neighbor_ticker = str(neighbor["neighbor_ticker"]).upper()
                neighbor_key = (neighbor_ticker, neighbor_id)
                if neighbor_key in seen_nodes:
                    continue
                locator = dict(neighbor["object_locator"])
                if str(locator.get("quality_status") or "").lower() == "rejected":
                    continue
                locator_cache[neighbor_key] = locator
                step = dict(neighbor["step"])
                next_steps = [*steps, step]
                next_score = score * float(neighbor["step_score"])
                next_tickers = (*tickers, neighbor_ticker)
                sequence += 1
                heapq.heappush(
                    frontier,
                    (
                        -next_score,
                        depth + 1,
                        sequence,
                        neighbor_ticker,
                        neighbor_id,
                        next_steps,
                        seen_nodes | {neighbor_key},
                        next_tickers,
                    ),
                )
            if len(frontier) > _GLOBAL_CHAIN_MAX_FRONTIER:
                frontier = heapq.nsmallest(_GLOBAL_CHAIN_MAX_FRONTIER, frontier)
                heapq.heapify(frontier)

        truncated = bool(frontier) or expanded_states >= _GLOBAL_CHAIN_MAX_EXPANSIONS
        return {
            "algorithm": "bounded_best_first_occurrence/v2",
            "root_object_id": object_id,
            "root_ticker": root_ticker,
            "max_depth": max_depth,
            "direction": direction,
            "paths": paths,
            "path_count": len(paths),
            "cross_company_path_count": sum(
                1 for path in paths if int(path.get("cross_company_hops") or 0) > 0
            ),
            "expanded_states": expanded_states,
            "frontier_remaining": len(frontier),
            "truncated": truncated,
            "stop_reason": (
                "path_limit"
                if len(paths) >= _GLOBAL_CHAIN_MAX_PATHS
                else "expansion_limit"
                if expanded_states >= _GLOBAL_CHAIN_MAX_EXPANSIONS
                else "frontier_exhausted"
            ),
            "similarity_links_in_evidence_paths": False,
            "accuracy_policy": {
                "cross_company_links_are_associations_not_causal_edges": True,
                "similar_topic_links_are_routing_only": True,
                "rejected_objects_excluded": True,
                "cycles_excluded_per_occurrence_path": True,
                "node_identity": "ticker+object_id",
            },
        }

    def _global_graph_neighbors(
        self,
        object_id: str,
        *,
        ticker: str,
        current_locator: Mapping[str, Any],
        direction: str,
    ) -> list[dict[str, Any]]:
        neighbors: list[dict[str, Any]] = []
        if direction in {"both", "outgoing"}:
            neighbors.extend(
                self._global_edge_neighbor_rows(
                    object_id,
                    ticker=ticker,
                    current_locator=current_locator,
                    outgoing=True,
                )
            )
        if direction in {"both", "incoming"}:
            neighbors.extend(
                self._global_edge_neighbor_rows(
                    object_id,
                    ticker=ticker,
                    current_locator=current_locator,
                    outgoing=False,
                )
            )
        # Cross-company shared-key links are symmetric associations; their
        # lexical from/to ordering must not make one endpoint undiscoverable.
        neighbors.extend(
            self._global_association_neighbor_rows(
                object_id,
                ticker=ticker,
                current_locator=current_locator,
            )
        )
        deduplicated: dict[tuple[str, str, str, str], dict[str, Any]] = {}
        for neighbor in neighbors:
            step = neighbor["step"]
            key = (
                str(step.get("kind") or ""),
                str(step.get("connection_id") or ""),
                str(neighbor.get("neighbor_ticker") or ""),
                str(neighbor.get("neighbor_object_id") or ""),
            )
            previous = deduplicated.get(key)
            if previous is None or float(neighbor["step_score"]) > float(previous["step_score"]):
                deduplicated[key] = neighbor
        return sorted(
            deduplicated.values(),
            key=lambda row: (
                -float(row["step_score"]),
                str(row["step"].get("connection_id") or ""),
                str(row["neighbor_ticker"]),
                str(row["neighbor_object_id"]),
            ),
        )[:_GLOBAL_CHAIN_NEIGHBORS_PER_NODE]

    def _global_edge_neighbor_rows(
        self,
        object_id: str,
        *,
        ticker: str,
        current_locator: Mapping[str, Any],
        outgoing: bool,
    ) -> list[dict[str, Any]]:
        source_column, target_column = (
            ("from_object_id", "to_object_id") if outgoing else ("to_object_id", "from_object_id")
        )
        rows = self.conn.execute(
            f"""
            SELECT replica.edge_id, replica.from_object_id, replica.to_object_id,
                   replica.ticker AS edge_occurrence_ticker,
                   replica.relation_type,
                   edge.edge_scope, edge.source_object_type, edge.target_object_type,
                   replica.confidence, replica.evidence_grade, edge.materiality,
                   edge.recency_score, edge.shard_hint, edge.compact_reason,
                   locator.object_id AS neighbor_object_id,
                   neighbor.ticker AS neighbor_ticker,
                   neighbor.document_id AS neighbor_document_id,
                   neighbor.document_type AS neighbor_document_type,
                   neighbor.period AS neighbor_period,
                   catalog.filing_date AS neighbor_filing_date,
                   locator.object_type AS neighbor_object_type,
                   locator.compact_label AS neighbor_label,
                   locator.compact_summary AS neighbor_summary,
                   COALESCE(neighbor.quality_status, locator.quality_status)
                       AS neighbor_quality_status
            FROM global_edge_replica AS replica
            JOIN global_edge_spine AS edge
              ON edge.edge_id = replica.edge_id
            JOIN global_object_replica AS neighbor
              ON neighbor.object_id = replica.{target_column}
             AND neighbor.ticker = replica.ticker
            JOIN global_object_locator AS locator
              ON locator.object_id = neighbor.object_id
            LEFT JOIN global_document_catalog AS catalog
              ON catalog.document_id = neighbor.document_id
            WHERE replica.ticker = ?
              AND replica.{source_column} = ?
            ORDER BY COALESCE(replica.confidence, 0.5) DESC,
                     replica.relation_type, replica.edge_id,
                     neighbor.document_id, neighbor.period
            LIMIT ?
            """,
            (ticker, object_id, _GLOBAL_CHAIN_NEIGHBORS_PER_NODE),
        ).fetchall()
        results: list[dict[str, Any]] = []
        for row in rows:
            locator = _locator_from_global_chain_row(row)
            temporal = _global_temporal_alignment(current_locator, locator)
            step_score = _global_edge_step_score(dict(row), temporal_score=temporal["score"])
            results.append(
                {
                    "neighbor_object_id": row["neighbor_object_id"],
                    "neighbor_ticker": row["neighbor_ticker"],
                    "object_locator": locator,
                    "step_score": step_score,
                    "step": {
                        "kind": "ontology_edge",
                        "connection_id": row["edge_id"],
                        "direction": "outgoing" if outgoing else "incoming",
                        "relation_type": row["relation_type"],
                        "from_object_id": row["from_object_id"],
                        "to_object_id": row["to_object_id"],
                        "from_ticker": ticker,
                        "to_ticker": ticker,
                        "edge_occurrence_ticker": row["edge_occurrence_ticker"],
                        "confidence": row["confidence"],
                        "evidence_grade": row["evidence_grade"],
                        "materiality": row["materiality"],
                        "recency_score": row["recency_score"],
                        "reason": row["compact_reason"],
                        "semantic_role": "ontology_relation",
                        "causal_inference_allowed": False,
                        "temporal_alignment": temporal,
                        "step_score": step_score,
                        "object": locator,
                    },
                }
            )
        return results

    def _global_association_neighbor_rows(
        self,
        object_id: str,
        *,
        ticker: str,
        current_locator: Mapping[str, Any],
    ) -> list[dict[str, Any]]:
        sql = """
            SELECT link.link_id, link.link_type, link.from_ticker, link.to_ticker,
                   link.shared_key, link.shared_key_type, link.from_object_id,
                   link.to_object_id, link.weight, link.confidence,
                   link.evidence_grade, link.materiality, link.generic_penalty,
                   link.recency_score, link.explanation_template,
                   locator.object_id AS neighbor_object_id,
                   neighbor.ticker AS neighbor_ticker,
                   neighbor.document_id AS neighbor_document_id,
                   neighbor.document_type AS neighbor_document_type,
                   neighbor.period AS neighbor_period,
                   catalog.filing_date AS neighbor_filing_date,
                   locator.object_type AS neighbor_object_type,
                   locator.compact_label AS neighbor_label,
                   locator.compact_summary AS neighbor_summary,
                   COALESCE(neighbor.quality_status, locator.quality_status)
                       AS neighbor_quality_status
            FROM global_chain_index AS link
            JOIN global_object_replica AS neighbor
              ON neighbor.object_id = link.{target_object_column}
             AND neighbor.ticker = link.{target_ticker_column}
            JOIN global_object_locator AS locator
              ON locator.object_id = neighbor.object_id
            LEFT JOIN global_document_catalog AS catalog
              ON catalog.document_id = neighbor.document_id
            WHERE link.{source_object_column} = ?
              AND link.{source_ticker_column} = ?
              AND link.link_type != 'similar_topic'
              AND COALESCE(link.generic_penalty, 0) < 0.8
            ORDER BY COALESCE(link.weight, 0) DESC, link.link_id
            LIMIT ?
        """
        rows: list[tuple[sqlite3.Row, str]] = []
        for (
            source_object_column,
            source_ticker_column,
            target_object_column,
            target_ticker_column,
            direction,
        ) in (
            (
                "from_object_id",
                "from_ticker",
                "to_object_id",
                "to_ticker",
                "undirected_from",
            ),
            (
                "to_object_id",
                "to_ticker",
                "from_object_id",
                "from_ticker",
                "undirected_to",
            ),
        ):
            query = sql.format(
                source_object_column=source_object_column,
                source_ticker_column=source_ticker_column,
                target_object_column=target_object_column,
                target_ticker_column=target_ticker_column,
            )
            rows.extend(
                (row, direction)
                for row in self.conn.execute(
                    query,
                    (object_id, ticker, _GLOBAL_CHAIN_NEIGHBORS_PER_NODE),
                ).fetchall()
            )
        results: list[dict[str, Any]] = []
        for row, association_direction in rows:
            locator = _locator_from_global_chain_row(row)
            temporal = _global_temporal_alignment(current_locator, locator)
            step_score = _global_association_step_score(dict(row), temporal_score=temporal["score"])
            results.append(
                {
                    "neighbor_object_id": row["neighbor_object_id"],
                    "neighbor_ticker": row["neighbor_ticker"],
                    "object_locator": locator,
                    "step_score": step_score,
                    "step": {
                        "kind": "cross_company_association",
                        "connection_id": row["link_id"],
                        "direction": association_direction,
                        "link_type": row["link_type"],
                        "shared_key_type": row["shared_key_type"],
                        "shared_key": row["shared_key"],
                        "from_object_id": row["from_object_id"],
                        "to_object_id": row["to_object_id"],
                        "from_ticker": row["from_ticker"],
                        "to_ticker": row["to_ticker"],
                        "weight": row["weight"],
                        "confidence": row["confidence"],
                        "evidence_grade": row["evidence_grade"],
                        "materiality": row["materiality"],
                        "generic_penalty": row["generic_penalty"],
                        "recency_score": row["recency_score"],
                        "reason": row["explanation_template"],
                        "semantic_role": "discovery_association",
                        "causal_inference_allowed": False,
                        "temporal_alignment": temporal,
                        "step_score": step_score,
                        "object": locator,
                    },
                }
            )
        return results

    def _global_locator_compact(
        self,
        object_id: str,
        *,
        ticker: str | None = None,
    ) -> dict[str, Any] | None:
        normalized_ticker = str(ticker or "").strip().upper()
        if not normalized_ticker:
            row = self.conn.execute(
                """
                SELECT object_id, ticker, document_id, document_type, period,
                       filing_date, object_type, compact_label, compact_summary,
                       quality_status
                FROM global_object_locator
                WHERE object_id = ?
                """,
                (object_id,),
            ).fetchone()
            return dict(row) if row else None
        row = self.conn.execute(
            """
            SELECT locator.object_id, replica.ticker, replica.document_id,
                   replica.document_type, replica.period, catalog.filing_date,
                   locator.object_type, locator.compact_label,
                   locator.compact_summary,
                   COALESCE(replica.quality_status, locator.quality_status)
                       AS quality_status
            FROM global_object_replica AS replica
            JOIN global_object_locator AS locator
              ON locator.object_id = replica.object_id
            LEFT JOIN global_document_catalog AS catalog
              ON catalog.document_id = replica.document_id
            WHERE replica.object_id = ? AND replica.ticker = ?
            ORDER BY replica.document_id, replica.document_type, replica.period,
                     replica.shard_id, replica.shard_path
            LIMIT 1
            """,
            (object_id, normalized_ticker),
        ).fetchone()
        return dict(row) if row else None

    def _global_object_counts(self) -> dict[str, int]:
        rows = self.conn.execute(
            """
            SELECT object_type, COUNT(*) AS count
            FROM global_object_locator
            GROUP BY object_type
            ORDER BY object_type
            """
        ).fetchall()
        return {str(row["object_type"]): int(row["count"]) for row in rows}

    def _global_serving_counts(self) -> dict[str, int]:
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
        counts = {table: _count_table(self.conn, table) for table in tables}
        counts["objects"] = counts["global_object_locator"]
        counts["documents"] = counts["global_document_catalog"]
        counts["edges"] = counts["global_edge_spine"]
        counts["quality_events"] = sum(
            int(document["counts"].get("quality_events") or 0)
            for document in self.list_documents()
            if isinstance(document.get("counts"), Mapping)
        )
        return counts

    def _quality_summary(self) -> dict[str, Any]:
        documents = self.list_documents()
        statuses = Counter(
            str(document.get("section_quality_status") or "unknown") for document in documents
        )
        return {
            "document_quality_status": dict(statuses),
            "critical_errors": 0,
            "batch_failures": 0,
        }

    def _route_payload(self, mode: str, tickers: Sequence[str]) -> dict[str, Any]:
        normalized_tickers = [ticker for ticker in tickers if ticker]
        return {
            "mode": mode,
            "index_layout": GLOBAL_SPINE_LAYOUT,
            "tickers": normalized_tickers,
            "route_tickers": normalized_tickers,
            "global_spine_path": str(self.global_spine_path),
            "shard_manifest_path": str(self._shard_manifest_path),
            "companies_dir": str(self.index_dir / "companies"),
            "missing_shards": self._missing_shards_for_tickers(tickers),
            "unknown_tickers": self._unknown_tickers(tickers),
            "fallback": False,
            "fallback_used": False,
        }

    def _spine_connection(self) -> sqlite3.Connection:
        if self._spine_conn is None:
            uri = self.global_spine_path.resolve().as_uri() + "?mode=ro&immutable=1"
            self._spine_conn = sqlite3.connect(
                uri,
                uri=True,
                check_same_thread=self.check_same_thread,
            )
            self._spine_conn.row_factory = sqlite3.Row
        return self._spine_conn

    def _sidecar(self) -> RouterSidecar:
        if self._router_sidecar is None:
            self._router_sidecar = RouterSidecar(self._router_sidecar_path)
        return self._router_sidecar

    def _load_shard_paths(self) -> tuple[dict[str, Path], dict[str, Path], dict[str, Path]]:
        raw_shards = self._shard_manifest.get("shards")
        declared_paths: dict[str, Path] = {}
        paths: dict[str, Path] = {}
        missing_paths: dict[str, Path] = {}
        if isinstance(raw_shards, Mapping):
            for ticker, raw_entry in raw_shards.items():
                if not isinstance(raw_entry, Mapping):
                    continue
                raw_path = raw_entry.get("path") or raw_entry.get("shard_path")
                if not isinstance(raw_path, str) or not raw_path:
                    continue
                path = Path(raw_path)
                if path.is_absolute():
                    resolved = path.expanduser().resolve()
                elif path.parts and path.parts[0] == "indexes":
                    resolved = (self.release_root / path).resolve()
                else:
                    resolved = (self.index_dir / path).resolve()
                normalized = str(ticker).upper()
                declared_paths[normalized] = resolved
                if resolved.exists():
                    paths[normalized] = resolved
                else:
                    missing_paths[normalized] = resolved
        return declared_paths, paths, missing_paths

    def _missing_shards_for_tickers(self, tickers: Iterable[str] | None) -> dict[str, str]:
        normalized = _normalize_tickers(tickers) or []
        return {
            ticker: str(self._missing_shard_paths[ticker])
            for ticker in normalized
            if ticker in self._missing_shard_paths
        }

    def _unknown_tickers(self, tickers: Iterable[str] | None) -> list[str]:
        normalized = _normalize_tickers(tickers) or []
        known = set(self._declared_shard_paths) | set(self._shard_paths)
        return [ticker for ticker in normalized if ticker not in known]

    def _missing_shard_payload(self, ticker: str | None, *, operation: str) -> dict[str, Any]:
        normalized = str(ticker or "").upper()
        return {
            "error": {
                "code": "ticker_shard_missing",
                "message": f"Ticker shard is declared in shard_manifest but missing on disk: {normalized}",
                "details": {
                    "ticker": normalized,
                    "expected_shard_path": str(self._missing_shard_paths.get(normalized, "")),
                    "operation": operation,
                },
            },
            "ticker": normalized,
            "missing_parts": ["ticker_shard_missing"],
            "missing_shards": self._missing_shards_for_tickers([normalized]),
            "fallback_used": False,
            "routing": self._route_payload("company_shard_missing", [normalized]),
        }

    def _attach_missing_release_parts(
        self, payload: dict[str, Any], tickers: Iterable[str] | None
    ) -> None:
        normalized = _normalize_tickers(tickers) or []
        missing_shards = self._missing_shards_for_tickers(tickers)
        unknown_tickers = self._unknown_tickers(tickers)
        if not missing_shards and not unknown_tickers:
            return
        available_tickers = [ticker for ticker in normalized if ticker in self._shard_paths]
        payload["missing_shards"] = missing_shards
        payload["unknown_tickers"] = unknown_tickers
        missing_parts = list(payload.get("missing_parts") or [])
        if missing_shards and "ticker_shard_missing" not in missing_parts:
            missing_parts.append("ticker_shard_missing")
        if unknown_tickers and "ticker_shard_not_found" not in missing_parts:
            missing_parts.append("ticker_shard_not_found")
        payload["missing_parts"] = missing_parts
        if available_tickers:
            payload["research_status"] = "partial_answerable_from_current_release"
            payload["answerability"] = {
                **(
                    payload.get("answerability")
                    if isinstance(payload.get("answerability"), Mapping)
                    else {}
                ),
                "direct_answerable": True,
                "related_context_available": True,
                "negative_answer_supported": False,
                "needs_user_clarification": False,
                "recommended_answer_mode": "partial_answerable_from_current_release",
            }
            autonomy = (
                dict(payload.get("agent_autonomy"))
                if isinstance(payload.get("agent_autonomy"), Mapping)
                else {}
            )
            if autonomy.get("mode") == "blocked_by_release_integrity":
                autonomy.pop("allowed_next_tools", None)
                autonomy.pop("max_additional_tool_calls", None)
            autonomy.setdefault("mode", "bounded")
            autonomy.setdefault("may_continue_research", True)
            payload["agent_autonomy"] = autonomy
            payload["partial_answerability"] = {
                "available_tickers": available_tickers,
                "missing_tickers": [
                    ticker for ticker in normalized if ticker not in available_tickers
                ],
                "instruction": (
                    "Answer using available tickers only, state missing tickers briefly, "
                    "and do not treat one missing shard as a failure for the whole basket."
                ),
            }
            return

        payload.setdefault("research_status", "not_answerable_from_current_release")
        payload.setdefault(
            "answerability",
            {
                "direct_answerable": False,
                "related_context_available": False,
                "negative_answer_supported": False,
                "needs_user_clarification": False,
                "recommended_answer_mode": "not_answerable_from_current_release",
            },
        )
        payload.setdefault("recommended_tools", [])
        payload.setdefault("do_not_call", ["broad_retrieve", "krw_ontology_retrieve"])
        payload.setdefault(
            "agent_autonomy",
            {
                "mode": "blocked_by_release_integrity",
                "allowed_next_tools": [],
                "max_additional_tool_calls": 0,
            },
        )


def _merge_query_contexts(
    contexts: Sequence[Mapping[str, Any]],
    *,
    question: str,
    routing: Mapping[str, Any],
) -> dict[str, Any]:
    documents: list[Any] = []
    objects: list[Any] = []
    bundles: list[Any] = []
    warnings: list[str] = []
    for context in contexts:
        documents.extend(context.get("documents") or [])
        objects.extend(context.get("objects") or [])
        bundles.extend(context.get("bundles") or context.get("evidence") or [])
        quality = context.get("quality") if isinstance(context.get("quality"), Mapping) else {}
        warnings.extend(str(item) for item in quality.get("warnings") or [])
    return {
        "question": question,
        "documents": documents,
        "objects": objects,
        "bundles": bundles,
        "quality": {"warnings": warnings},
        "routing": dict(routing),
    }


def _attach_current_document_anchors(
    payload: dict[str, Any],
    *,
    requested_tickers: Sequence[str] | None,
    available_tickers: Sequence[str],
    documents: Sequence[Mapping[str, Any]],
) -> None:
    requested = [ticker for ticker in (requested_tickers or []) if ticker]
    available = [ticker for ticker in available_tickers if ticker]
    anchor_tickers = available or requested
    roles = filing_document_roles_from_documents(documents, tickers=anchor_tickers)
    anchors = latest_document_anchors_from_documents(documents, tickers=anchor_tickers)
    if not anchors and not roles:
        return
    policy = (
        "Use current_driver for latest/current changes and annual_baseline for business mix, "
        "segment structure, and long-term baseline. Only 10-Q and 10-K filings are used for these roles."
    )
    payload["current_document_anchors"] = anchors
    payload["current_document_anchor_policy"] = policy
    payload["filing_document_roles"] = roles
    payload["filing_document_role_policy"] = policy
    research_pack = payload.setdefault("research_pack", {})
    if isinstance(research_pack, dict):
        research_pack["current_document_anchors"] = anchors
        research_pack["current_document_anchor_policy"] = policy
        research_pack["filing_document_roles"] = roles
        research_pack["filing_document_role_policy"] = policy
    routing = payload.get("routing")
    if isinstance(routing, dict):
        routing["current_document_anchors"] = anchors
        routing["filing_document_roles"] = roles


def _attach_spine_cross_company_pack(
    payload: dict[str, Any],
    *,
    question: str,
    requested_tickers: Sequence[str] | None,
    available_tickers: Sequence[str],
    documents: Sequence[Mapping[str, Any]],
) -> None:
    requested = [ticker for ticker in (requested_tickers or []) if ticker]
    if len(requested) < 2:
        return
    research_pack = payload.setdefault("research_pack", {})
    if not isinstance(research_pack, dict):
        return
    existing = research_pack.get("cross_company_signal_pack")
    if isinstance(existing, Mapping) and existing:
        return
    available = [ticker for ticker in available_tickers if ticker]
    roles_by_ticker = filing_document_roles_from_documents(documents, tickers=available)
    document_by_ticker = latest_document_anchors_from_documents(documents, tickers=available)
    evidence_rows: list[dict[str, Any]] = []
    for ticker in available:
        document = document_by_ticker.get(ticker, {})
        roles = roles_by_ticker.get(ticker, {})
        annual_baseline = roles.get("annual_baseline") if isinstance(roles, Mapping) else None
        evidence_rows.append(
            {
                "ticker": ticker,
                "period": document.get("period"),
                "document_type": document.get("document_type"),
                "current_driver": document,
                "annual_baseline": annual_baseline
                if isinstance(annual_baseline, Mapping)
                else None,
                "signal": "filing_commentary",
                "evidence_strength": "medium",
                "commentary_summary": (
                    f"{ticker} current release contains filing evidence related to "
                    f"{_compact_text(question, max_chars=160)}"
                ),
            }
        )
    research_pack["cross_company_signal_pack"] = {
        "mode": "cross_company_signal_synthesis",
        "answer_policy": "Use this as compact cross-company evidence; cite shard evidence before strong claims.",
        "latest_period_anchor": ", ".join(
            str(anchor.get("source_label") or "")
            for anchor in document_by_ticker.values()
            if isinstance(anchor, Mapping)
        ),
        "current_document_anchors": document_by_ticker,
        "filing_document_roles": roles_by_ticker,
        "ticker_basket": requested,
        "available_tickers": available,
        "missing_tickers": [ticker for ticker in requested if ticker not in available],
        "company_evidence_rows": evidence_rows,
        "signals": [
            {
                "signal": "filing_evidence_available",
                "companies_supporting": available,
                "strength": "medium" if evidence_rows else "weak",
            }
        ],
        "quality": {
            "missing_parts": [],
            "fallback_used": False,
        },
    }


def _attach_chart_series_pack(
    payload: dict[str, Any],
    *,
    question: str,
    requested_tickers: Sequence[str] | None,
    chart_series_path: Path,
    chart_series_status: Mapping[str, Any],
) -> None:
    if not _chart_series_runtime_enabled():
        return
    if not _should_attach_chart_series(question):
        return
    research_pack = payload.setdefault("research_pack", {})
    if not isinstance(research_pack, dict):
        return
    diagnostics = payload.setdefault("search_diagnostics", {})
    if not isinstance(diagnostics, dict):
        diagnostics = {}
        payload["search_diagnostics"] = diagnostics
    diagnostics["chart_series"] = {
        "available": bool(chart_series_status.get("available")),
        "path": str(chart_series_path),
        "source": "chart_series_sidecar",
    }
    if not chart_series_status.get("available"):
        diagnostics["chart_series"]["disabled_reason"] = chart_series_status.get("reason")
        return
    pack = query_chart_series_pack(
        chart_series_path,
        question=question,
        tickers=[ticker for ticker in (requested_tickers or []) if ticker],
    )
    if not pack:
        diagnostics["chart_series"]["matched"] = False
        return
    diagnostics["chart_series"]["matched"] = True
    diagnostics["chart_series"]["series_count"] = len(pack.get("series") or [])
    existing = research_pack.get("metric_series_pack")
    if isinstance(existing, Mapping) and existing.get("series"):
        research_pack["dynamic_metric_series_pack"] = existing
    research_pack["chart_series_pack"] = pack
    research_pack["metric_series_pack"] = pack


def _chart_series_runtime_enabled() -> bool:
    raw = os.getenv(_CHART_SERIES_ENABLED_ENV)
    return str(raw or "").strip().lower() in _TRUE_ENV_VALUES


def _should_attach_chart_series(question: str) -> bool:
    text = str(question or "").lower()
    # The presentation runtime, not the query router, decides whether a visual
    # is warranted.  Attach a bounded verified metric pack for any question
    # that names a supported metric so the agent can later choose prose, a
    # table, or a visualization without a second retrieval pass.  Explicit
    # trend/chart language still helps discovery, but is no longer required.
    metric_terms = (
        "revenue",
        "sales",
        "income",
        "margin",
        "cash flow",
        "fcf",
        "capex",
        "debt",
        "eps",
        "repurchase",
        "buyback",
        "sbc",
        "r&d",
        "m&a",
        "매출",
        "영업이익",
        "순이익",
        "마진",
        "비중",
        "비용",
        "판관비",
        "영업비용",
        "현금흐름",
        "잉여현금",
        "설비투자",
        "자본지출",
        "부채",
        "서비스",
        "아이폰",
        "제품별",
        "지역별",
        "자사주",
        "주식보상",
        "연구개발",
        "인수",
    )
    return any(term in text for term in metric_terms)


def _merge_discovery_payloads(
    payloads: Sequence[Mapping[str, Any]],
    *,
    question: str | None,
    candidate_tickers: Sequence[str],
    routing: Mapping[str, Any],
    limit: int,
) -> dict[str, Any]:
    ticker_candidates: list[dict[str, Any]] = []
    results_by_ticker: dict[str, Any] = {}
    query_frame: dict[str, Any] = {}
    diagnostics_by_ticker: dict[str, Any] = {}
    for payload in payloads:
        if not query_frame and isinstance(payload.get("query_frame"), Mapping):
            query_frame = dict(payload["query_frame"])
        if isinstance(payload.get("results_by_ticker"), Mapping):
            results_by_ticker.update(dict(payload["results_by_ticker"]))
        for candidate in payload.get("ticker_candidates") or []:
            if isinstance(candidate, Mapping):
                ticker_candidates.append(dict(candidate))
        diagnostics = payload.get("search_diagnostics")
        if isinstance(diagnostics, Mapping):
            for ticker in candidate_tickers:
                if ticker not in diagnostics_by_ticker:
                    diagnostics_by_ticker[ticker] = diagnostics
                    break
    return {
        "question": question,
        "query_frame": query_frame,
        "ticker_candidates": ticker_candidates[:limit],
        "results_by_ticker": results_by_ticker,
        "search_diagnostics": {
            "routing": dict(routing),
            "candidate_tickers": list(candidate_tickers),
            "shard_diagnostics": diagnostics_by_ticker,
            "fallback_used": False,
        },
    }


def _normalize_global_chain_direction(direction: str) -> str:
    value = str(direction or "both").strip().lower()
    if value in {"incoming", "in", "upstream", "up"}:
        return "incoming"
    if value in {"outgoing", "out", "downstream", "down"}:
        return "outgoing"
    return "both"


def _locator_from_global_chain_row(row: Mapping[str, Any]) -> dict[str, Any]:
    return {
        "object_id": row["neighbor_object_id"],
        "ticker": row["neighbor_ticker"],
        "document_id": row["neighbor_document_id"],
        "document_type": row["neighbor_document_type"],
        "period": row["neighbor_period"],
        "filing_date": row["neighbor_filing_date"],
        "object_type": row["neighbor_object_type"],
        "compact_label": row["neighbor_label"],
        "compact_summary": row["neighbor_summary"],
        "quality_status": row["neighbor_quality_status"],
    }


def _clean_replica_location(row: Mapping[str, Any]) -> dict[str, Any]:
    """Return bounded public provenance for one semantic-object occurrence."""
    return {str(key): (None if value == "" else value) for key, value in row.items()}


def _global_temporal_alignment(
    left: Mapping[str, Any],
    right: Mapping[str, Any],
) -> dict[str, Any]:
    left_period = str(left.get("period") or "").strip()
    right_period = str(right.get("period") or "").strip()
    if left_period and left_period == right_period:
        return {
            "status": "same_period",
            "score": 1.0,
            "from_period": left_period,
            "to_period": right_period,
        }
    left_year = _period_year(left_period)
    right_year = _period_year(right_period)
    if left_year is None or right_year is None:
        status, score = "unknown", 0.8
    else:
        year_gap = abs(left_year - right_year)
        if year_gap <= 1:
            status, score = "near_period", 0.9
        elif year_gap <= 3:
            status, score = "distant_period", 0.72
        else:
            status, score = "stale_period_gap", 0.55
    return {
        "status": status,
        "score": score,
        "from_period": left_period or None,
        "to_period": right_period or None,
    }


def _period_year(period: str) -> int | None:
    match = re.search(r"(?:19|20)\d{2}", str(period or ""))
    return int(match.group(0)) if match else None


def _global_edge_step_score(row: Mapping[str, Any], *, temporal_score: float) -> float:
    confidence = _bounded_score(row.get("confidence"), default=0.6)
    evidence = _evidence_grade_score(row.get("evidence_grade"))
    materiality = _bounded_score(row.get("materiality"), default=0.5)
    recency = _bounded_score(row.get("recency_score"), default=temporal_score)
    score = (0.42 * confidence + 0.28 * evidence + 0.15 * materiality + 0.15 * recency) * (
        0.9 + 0.1 * temporal_score
    )
    return round(max(0.05, min(score, 1.0)), 6)


def _global_association_step_score(
    row: Mapping[str, Any],
    *,
    temporal_score: float,
) -> float:
    weight = min(_bounded_score(row.get("weight"), default=0.0) / 0.75, 1.0)
    confidence = _bounded_score(row.get("confidence"), default=0.55)
    evidence = _evidence_grade_score(row.get("evidence_grade"))
    generic_penalty = _bounded_score(row.get("generic_penalty"), default=0.0)
    score = 0.55 * weight + 0.25 * confidence + 0.20 * evidence
    score *= 1.0 - (0.5 * generic_penalty)
    score *= temporal_score
    score *= 0.88  # Associations rank below equally strong ontology edges.
    return round(max(0.03, min(score, 0.88)), 6)


def _bounded_score(value: Any, *, default: float) -> float:
    try:
        score = float(value)
    except (TypeError, ValueError):
        return default
    return max(0.0, min(score, 1.0))


def _evidence_grade_score(value: Any) -> float:
    grade = str(value or "").strip().lower()
    return {
        "direct": 1.0,
        "primary": 1.0,
        "verified": 0.95,
        "explicit": 0.95,
        "high": 0.9,
        "medium": 0.72,
        "inferred": 0.62,
        "derived": 0.55,
        "low": 0.4,
        "unsupported": 0.15,
        # Old v2 fragments accidentally projected review status here.  Treat
        # it as neutral, never as strong evidence.
        "accepted": 0.6,
    }.get(grade, 0.58)


def _global_chain_path_payload(
    root_object_id: str,
    *,
    root_ticker: str,
    steps: Sequence[Mapping[str, Any]],
    score: float,
    tickers: Sequence[str],
) -> dict[str, Any]:
    connection_ids = [
        "|".join(
            (
                str(step.get("connection_id") or ""),
                str(step.get("from_ticker") or ""),
                str(step.get("from_object_id") or ""),
                str(step.get("to_ticker") or ""),
                str(step.get("to_object_id") or ""),
            )
        )
        for step in steps
    ]
    terminal = dict(steps[-1].get("object") or {}) if steps else {}
    encoded = "|".join([root_ticker, root_object_id, *connection_ids]).encode("utf-8")
    cross_company_hops = sum(1 for step in steps if step.get("kind") == "cross_company_association")
    return {
        "path_id": f"chain:{hashlib.sha256(encoded).hexdigest()[:24]}",
        "root_occurrence": {"ticker": root_ticker, "object_id": root_object_id},
        "score": round(float(score), 6),
        "depth": len(steps),
        "cross_company_hops": cross_company_hops,
        "contains_discovery_association": cross_company_hops > 0,
        "causal_inference_allowed": False,
        "tickers": list(dict.fromkeys(str(ticker) for ticker in tickers if ticker)),
        "terminal_object": terminal,
        "steps": [dict(step) for step in steps],
    }


def _normalize_tickers(tickers: Iterable[str] | None) -> list[str] | None:
    if tickers is None:
        return None
    normalized = [str(ticker).upper() for ticker in tickers if str(ticker or "").strip()]
    return normalized or None


def _read_json(path: Path) -> dict[str, Any]:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (FileNotFoundError, json.JSONDecodeError, OSError):
        return {}
    return payload if isinstance(payload, dict) else {}


def _count_table(conn: sqlite3.Connection, table_name: str) -> int:
    return int(conn.execute(f"SELECT COUNT(*) FROM {table_name}").fetchone()[0])


def _read_int_env(name: str, default: int, *, min_value: int) -> int:
    raw = os.getenv(name)
    if raw is None or raw.strip() == "":
        return default
    try:
        value = int(raw)
    except ValueError:
        return default
    return value if value >= min_value else default


def _score_summary(candidates: Sequence[Mapping[str, Any]]) -> dict[str, Any]:
    scores = sorted(
        (max(0.0, float(candidate.get("score") or 0.0)) for candidate in candidates),
        reverse=True,
    )
    total = sum(scores)
    entropy = 0.0
    if total > 0 and len(scores) > 1:
        probabilities = [score / total for score in scores if score > 0]
        if len(probabilities) > 1:
            entropy = -sum(
                probability * math.log(probability) for probability in probabilities
            ) / math.log(len(probabilities))
    top_score = scores[0] if scores else 0.0
    return {
        "candidate_count": len(scores),
        "top_score": top_score,
        "top_score_gap": top_score - scores[1] if len(scores) > 1 else top_score,
        "normalized_entropy": round(entropy, 6),
    }


def _tickerless_query_context_limit(requested_limit: int) -> int:
    cap = _read_int_env(
        _TICKERLESS_QUERY_CONTEXT_MAX_TICKERS_ENV,
        _DEFAULT_TICKERLESS_QUERY_CONTEXT_MAX_TICKERS,
        min_value=1,
    )
    return max(1, min(max(1, int(requested_limit)), cap))


def _candidate_budget_payload(
    *,
    scoped: bool,
    requested_limit: int,
    effective_limit: int,
) -> dict[str, Any]:
    payload: dict[str, Any] = {
        "candidate_stage": "global_spine",
        "requested_limit_tickers": max(1, int(requested_limit)),
        "effective_limit_tickers": max(1, int(effective_limit)),
    }
    if not scoped:
        payload["tickerless_candidate_cap"] = _read_int_env(
            _TICKERLESS_QUERY_CONTEXT_MAX_TICKERS_ENV,
            _DEFAULT_TICKERLESS_QUERY_CONTEXT_MAX_TICKERS,
            min_value=1,
        )
    return payload


def _fanout_error(exc: Exception) -> str:
    message = str(exc)
    return f"{type(exc).__name__}:{message}" if message else type(exc).__name__


def _like_terms(text: str) -> list[str]:
    terms: list[str] = []
    for raw in str(text or "").replace("-", " ").replace("_", " ").split():
        cleaned = "".join(ch for ch in raw.lower() if ch.isalnum())
        if len(cleaned) >= 3 and cleaned not in terms:
            terms.append(cleaned)
    return terms


def _compact_text(text: str, *, max_chars: int) -> str:
    compact = " ".join(str(text or "").split())
    if len(compact) <= max_chars:
        return compact
    return compact[: max(0, max_chars - 3)].rstrip() + "..."
