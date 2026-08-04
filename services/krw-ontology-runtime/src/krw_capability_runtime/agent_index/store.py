"""Python SDK for agent-friendly ontology retrieval."""

from __future__ import annotations

from collections import OrderedDict, deque
from concurrent.futures import ThreadPoolExecutor, as_completed
import copy
import json
import os
import re
import sqlite3
import time
from threading import Lock
from pathlib import Path
from typing import Any, Iterable, Mapping, Sequence

from krw_capability_runtime.agent_index.retrieval_text import format_metric_compact
from krw_capability_runtime.agent_index.metric_dictionary import (
    canonical_metric_name,
    normalize_dimension_key,
)
from krw_capability_runtime.agent_index.research_kernel import ResearchKernel, research_request_from_plan_fields
from krw_capability_runtime.agent_index.research_contexts import (
    CompanyOverviewContext,
    DirectExposureContext,
    MetricContext,
    RiskContext,
)
from krw_capability_runtime.agent_index.research_router import (
    context_policy_for_route,
    intent_profile_from_route,
    period_display_policy_for_question,
    preferred_answer_order_for_question,
    route_from_intent,
    route_research,
)
from krw_capability_runtime.agent_index.discovery import (
    build_evidence_frame,
    build_query_frame,
    classify_topic_match,
    tier_rank,
    topic_from_row,
)

DEFAULT_QUERY_TYPES = (
    "EvidenceQuote",
    "ResearchClaim",
    "MetricObservation",
    "Calculation",
    "BusinessFactor",
    "AgreementTerm",
    "BusinessEvent",
    "BusinessActivity",
    "ExternalFactorExposure",
    "CompanyBusinessProfile",
    "TemporalLink",
    "TrendObservation",
    "ChangeEvent",
    "AssumptionCandidate",
)

_TERM_RE = re.compile(r"[A-Za-z0-9_]+")
_PLANNED_TERM_RE = re.compile(r"[0-9A-Za-z\u3131-\u318e\uac00-\ud7a3_]+")
_STRICT_TOPIC_WARNING_TERM_COUNT = 5
_CHAIN_MAX_DEPTH = 5
_CHAIN_MAX_PATHS = 40
_CHAIN_MAX_TEMPORAL_CONTEXT = 12
_DEFAULT_READ_CACHE_MIB = 128
_DEFAULT_READ_MMAP_MIB = 512
_QUERY_EXPANSION_RULES = (
    ("유럽", "europe european"),
    ("가스", "natural_gas_price natural gas feed gas lng"),
    ("천연가스", "natural_gas_price natural gas feed gas lng"),
    ("비축", "storage inventory"),
    ("저장", "storage inventory"),
    ("부족", "shortage deficit supply_disruption demand"),
    ("수요", "demand customer_demand lng_demand"),
    ("가격", "price commodity_price natural_gas_price"),
    ("마진", "operating_margin cost_of_revenue margin"),
    ("계약", "contract agreement spa customer"),
    ("조건", "threshold covenant default termination"),
    ("종료", "termination terminate terminated cancellation cancel"),
    ("해지", "termination terminate terminated cancellation cancel"),
    ("가속화", "debt_acceleration accelerate accelerated acceleration"),
    ("담보", "collateral collateral_enforcement security lien"),
    ("큰일", "risk covenant default impairment liquidity threshold"),
    ("위험", "risk"),
    ("중국", "china"),
    ("수출", "export exports export_controls restrictions"),
    ("규제", "regulatory regulation controls restrictions"),
    ("제한", "restriction restrictions controls"),
    ("허가", "license licensing approval"),
    ("라이선스", "license licensing"),
    ("고객", "customer customers"),
    ("집중", "concentration concentrated"),
    ("주요", "key primary major"),
    ("이벤트", "event business_event change_event"),
    ("일정", "event milestone date"),
    ("변화", "change trend temporal"),
    ("바뀐", "change changed trend"),
    ("부채", "debt maturity obligation"),
    ("만기", "maturity"),
    ("ttf", "natural_gas_price international_lng_price global_lng_price europe"),
    ("jkm", "international_lng_price global_lng_price lng"),
    ("henry hub", "natural_gas_price feed_gas_cost"),
    ("storage", "storage inventory supply_disruption"),
    ("shortage", "shortage deficit supply_disruption demand"),
    ("lng", "lng_sales lng_demand international_lng_price"),
)
_SPLIT_TOPIC_STOP_TERMS = {
    "a",
    "about",
    "an",
    "and",
    "are",
    "as",
    "affected",
    "be",
    "been",
    "being",
    "check",
    "companies",
    "company",
    "does",
    "direct",
    "directly",
    "do",
    "exposed",
    "exposure",
    "find",
    "give",
    "has",
    "have",
    "impact",
    "impacted",
    "is",
    "of",
    "or",
    "prices",
    "price",
    "for",
    "from",
    "how",
    "if",
    "in",
    "into",
    "on",
    "risk",
    "risks",
    "the",
    "this",
    "tell",
    "to",
    "trends",
    "what",
    "when",
    "whether",
    "which",
    "with",
}
_SEMANTIC_NEIGHBOR_TYPES = {
    "BusinessFactor",
    "AgreementTerm",
    "BusinessEvent",
    "BusinessActivity",
    "ExternalFactorExposure",
    "AssumptionCandidate",
}
_CURRENT_FILING_INTENT_TERMS = {
    "current",
    "currently",
    "latest",
    "newest",
    "recent",
    "recently",
}
_CURRENT_FILING_INTENT_MARKERS = (
    "공시상",
    "공시 기준",
    "공시자료",
    "최신",
    "최근",
    "현재",
    "이번 분기",
    "latest filing",
    "current filing",
    "most recent",
)
_DOCUMENT_TYPE_RECENCY_PRIORITY = {
    "10-Q": 50,
    "10-K": 45,
}
_FILING_ROLE_DOCUMENT_TYPES = {"10-Q", "10-K"}

_COMPARE_TICKER_CACHE_MAX = 256
_COMPARE_TICKER_CACHE: OrderedDict[
    tuple[Any, ...], tuple[list[dict[str, Any]], dict[str, Any], dict[str, Any]]
] = OrderedDict()
_COMPARE_TICKER_CACHE_LOCK = Lock()
_DISCOVERY_CACHE_MAX = 128
_DISCOVERY_CACHE: OrderedDict[tuple[Any, ...], dict[str, Any]] = OrderedDict()
_DISCOVERY_CACHE_LOCK = Lock()
_QUERY_COMPACT_CACHE_MAX = 512
_QUERY_COMPACT_CACHE: OrderedDict[tuple[Any, ...], tuple[list[dict[str, Any]], dict[str, Any]]] = (
    OrderedDict()
)
_QUERY_COMPACT_CACHE_LOCK = Lock()
_QUERY_CONTEXT_CACHE_MAX = 512
_QUERY_CONTEXT_CACHE: OrderedDict[tuple[Any, ...], dict[str, Any]] = OrderedDict()
_QUERY_CONTEXT_CACHE_LOCK = Lock()
_METRIC_FAST_PATH_TYPES = frozenset({"MetricObservation", "Calculation", "XBRLFact"})
_METRIC_FAST_PATH_GENERIC_TERMS = frozenset(
    {
        "annual",
        "annually",
        "calendar",
        "compare",
        "comparison",
        "fiscal",
        "metric",
        "metrics",
        "number",
        "numbers",
        "period",
        "periods",
        "year",
        "years",
    }
)
_METRIC_LOOKUP_METRIC_TERMS = frozenset(
    {
        "assets",
        "capex",
        "cash",
        "cagr",
        "cost",
        "costs",
        "debt",
        "earnings",
        "ebit",
        "ebitda",
        "expense",
        "expenses",
        "flow",
        "gross",
        "growth",
        "income",
        "liabilities",
        "margin",
        "margins",
        "net",
        "nii",
        "nim",
        "operating",
        "opex",
        "percent",
        "percentage",
        "profit",
        "rate",
        "rates",
        "ratio",
        "revenue",
        "revenues",
        "sales",
        "share",
        "shares",
        "total",
        "yoy",
    }
)
_METRIC_LOOKUP_CALCULATION_TERMS = frozenset(
    {
        "cagr",
        "growth",
        "percent",
        "percentage",
        "rate",
        "rates",
        "ratio",
        "share",
        "shares",
        "total",
        "yoy",
    }
)
_TYPED_PROJECTION_SPECS: dict[str, dict[str, Any]] = {
    "exposure_lookup": {
        "object_types": {"ExternalFactorExposure"},
        "terms": {
            "exposure",
            "exposed",
            "commodity",
            "price",
            "oil",
            "crude",
            "lng",
            "henry",
            "hub",
            "hbm",
            "export",
            "control",
            "restriction",
            "china",
            "rate",
            "fx",
            "currency",
            "scenario",
        },
        "text_columns": (
            "factor",
            "benchmark",
            "impact_channel",
            "mechanism",
            "scenario_terms",
            "lookup_text",
        ),
    },
    "agreement_lookup": {
        "object_types": {"AgreementTerm"},
        "terms": {
            "agreement",
            "contract",
            "spa",
            "termination",
            "terminate",
            "debt",
            "acceleration",
            "covenant",
            "lease",
            "maturity",
            "commitment",
            "collateral",
            "facility",
        },
        "text_columns": (
            "agreement_type",
            "agreement_subtype",
            "counterparty",
            "maturity_date",
            "termination_terms",
            "covenant_terms",
            "collateral_terms",
            "affected_channels",
            "lookup_text",
        ),
    },
    "event_lookup": {
        "object_types": {"BusinessEvent", "ChangeEvent", "TrendObservation", "TemporalLink"},
        "terms": {
            "event",
            "timeline",
            "approval",
            "delay",
            "delayed",
            "milestone",
            "launch",
            "completion",
            "completed",
            "regulatory",
            "date",
            "when",
            "schedule",
        },
        "text_columns": (
            "event_type",
            "event_status",
            "event_date",
            "date_expression",
            "project_or_product",
            "regulatory_body",
            "affected_channels",
            "lookup_text",
        ),
    },
    "factor_lookup": {
        "object_types": {"BusinessFactor"},
        "terms": {
            "risk",
            "driver",
            "margin",
            "cost",
            "pressure",
            "consumer",
            "slowdown",
            "supply",
            "chain",
            "pricing",
            "demand",
            "fuel",
            "shipping",
            "capex",
            "ai",
            "raw",
            "material",
            "fx",
            "capital",
            "allocation",
        },
        "text_columns": (
            "factor_type",
            "topic_family",
            "impact_channel",
            "business_area",
            "risk_or_driver",
            "topic_label",
            "topic_summary",
            "lookup_text",
        ),
    },
}


def _metric_lookup_selected_types(
    object_types: Iterable[str], *, include_xbrl: bool = False
) -> list[str]:
    requested = list(object_types)
    selected = [object_type for object_type in requested if object_type in _METRIC_FAST_PATH_TYPES]
    if include_xbrl and "MetricObservation" in set(requested) and "XBRLFact" not in selected:
        selected.append("XBRLFact")
    return selected


def _read_int_env(name: str, default: int, *, min_value: int) -> int:
    raw = os.getenv(name)
    if raw is None or raw.strip() == "":
        return default
    try:
        value = int(raw)
    except ValueError:
        return default
    if value < min_value:
        return default
    return value


class OntologyStore:
    """Read-only SDK over an agent index SQLite database."""

    def __init__(
        self,
        index_path: Path | str,
        *,
        check_same_thread: bool = True,
        immutable: bool = False,
    ):
        self.index_path = Path(index_path).expanduser()
        self.immutable = bool(immutable)
        connect_target: Path | str = self.index_path
        connect_kwargs: dict[str, Any] = {}
        if self.immutable:
            connect_target = self.index_path.resolve().as_uri() + "?mode=ro&immutable=1"
            connect_kwargs["uri"] = True
        self.conn = sqlite3.connect(
            connect_target,
            cached_statements=512,
            check_same_thread=check_same_thread,
            **connect_kwargs,
        )
        self.conn.row_factory = sqlite3.Row
        self._configure_read_connection()

    def _configure_read_connection(self) -> None:
        """Apply per-connection read-heavy SQLite settings."""
        cache_mib = _read_int_env(
            "KRW_SQLITE_READ_CACHE_MIB",
            _DEFAULT_READ_CACHE_MIB,
            min_value=1,
        )
        mmap_mib = _read_int_env(
            "KRW_SQLITE_READ_MMAP_MIB",
            _DEFAULT_READ_MMAP_MIB,
            min_value=0,
        )
        pragmas = (
            "PRAGMA query_only = ON",
            "PRAGMA temp_store = MEMORY",
            f"PRAGMA cache_size = -{cache_mib * 1024}",
            f"PRAGMA mmap_size = {mmap_mib * 1024 * 1024}",
            "PRAGMA busy_timeout = 5000",
        )
        for statement in pragmas:
            self.conn.execute(statement)

    def close(self) -> None:
        self.conn.close()

    def __enter__(self) -> "OntologyStore":
        return self

    def __exit__(self, *_exc: object) -> None:
        self.close()

    def list_companies(self) -> list[str]:
        rows = self.conn.execute("SELECT DISTINCT ticker FROM documents ORDER BY ticker").fetchall()
        return [row["ticker"] for row in rows]

    def _available_tickers(self, tickers: Sequence[str]) -> set[str]:
        normalized = [ticker.upper() for ticker in tickers if ticker]
        if not normalized:
            return set()
        placeholders = ", ".join("?" for _ in normalized)
        rows = self.conn.execute(
            f"""
            SELECT DISTINCT ticker FROM documents WHERE ticker IN ({placeholders})
            UNION
            SELECT DISTINCT ticker FROM objects WHERE ticker IN ({placeholders})
            """,
            [*normalized, *normalized],
        ).fetchall()
        return {str(row["ticker"]).upper() for row in rows}

    def list_documents(
        self,
        *,
        ticker: str | None = None,
        tickers: Iterable[str] | None = None,
        document_types: Iterable[str] | None = None,
    ) -> list[dict[str, Any]]:
        where, params = _scope_where(
            ticker=ticker,
            tickers=tickers,
            document_types=document_types,
        )
        rows = self.conn.execute(
            f"""
            SELECT ticker, document_type, doc_type_key, period, artifact_index_path,
                   ontology_dir, counts_json, section_quality_status, section_quality_json
            FROM documents
            {where}
            ORDER BY ticker, doc_type_key, period
            """,
            params,
        ).fetchall()
        return [_document_from_row(row) for row in rows]

    def index_context(
        self,
        *,
        include_counts: bool = True,
        include_capabilities: bool = True,
        include_quality_summary: bool = True,
    ) -> dict[str, Any]:
        """Return a compact capability card for the current serving index."""
        build_metadata = _metadata_json(self.conn, "build")
        documents = self.list_documents()
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
            "index_path": str(self.index_path),
            "schema_version": build_metadata.get("schema_version"),
            "agent_index_schema_version": build_metadata.get("agent_index_schema_version")
            or build_metadata.get("schema_version"),
            "ontology_schema_version": build_metadata.get("ontology_schema_version"),
            "ontology_registry_version": build_metadata.get("ontology_registry_version"),
            "retrieval_text_builder_version": build_metadata.get("retrieval_text_builder_version"),
            "company_topic_builder_version": build_metadata.get("company_topic_builder_version"),
            "company_topic_profile_mode": build_metadata.get("company_topic_profile_mode"),
            "available_tickers": self.list_companies(),
            "available_document_types": sorted(document_types),
            "available_periods_by_ticker": {
                ticker: sorted(periods) for ticker, periods in periods_by_ticker.items()
            },
            "ticker_coverage": _ticker_coverage(self.conn),
            "answer_candidate_object_types": list(DEFAULT_QUERY_TYPES),
            "trace_only_object_types": [
                "SupportLink",
                "Edge",
                "SourceDocument",
                "SourceLocation",
                "SourceSpan",
                "SourceTable",
                "SourceTableCell",
                "XBRLFact",
                "CanonicalEntity",
                "EntityMention",
            ],
            "answerability_policy": {
                "strong_claim_requires_traceable_direct": True,
                "metric_claim_requires_metric_lineage": True,
                "broad_related_must_not_be_presented_as_direct": True,
                "internal_ids_hidden_in_final_answer": True,
            },
        }
        if include_counts:
            payload["object_counts"] = _count_by(self.conn, "objects", "type")
            payload["serving_counts"] = {
                "documents": _table_count(self.conn, "documents"),
                "objects": _table_count(self.conn, "objects"),
                "object_search_text": _table_count(self.conn, "object_search_text"),
                "metric_lookup": _table_count(self.conn, "metric_lookup"),
                "metric_dimension_lookup": _table_count(self.conn, "metric_dimension_lookup"),
                "company_dimension_catalog": _table_count(self.conn, "company_dimension_catalog"),
                "exposure_lookup": _table_count(self.conn, "exposure_lookup"),
                "agreement_lookup": _table_count(self.conn, "agreement_lookup"),
                "event_lookup": _table_count(self.conn, "event_lookup"),
                "factor_lookup": _table_count(self.conn, "factor_lookup"),
                "object_fts": _table_count(self.conn, "object_fts"),
                "object_traceability": _table_count(self.conn, "object_traceability"),
                "company_topic_index": _table_count(self.conn, "company_topic_index"),
                "company_topic_fts": _table_count(self.conn, "company_topic_fts"),
                "company_topic_source_objects": _table_count(
                    self.conn, "company_topic_source_objects"
                ),
            }
        if include_capabilities:
            payload["capabilities"] = {
                "query": True,
                "retrieve": True,
                "trace": True,
                "chain": True,
                "compare": True,
                "quality": True,
                "company_topic_index": _table_exists(self.conn, "company_topic_index"),
                "object_search_text": _table_exists(self.conn, "object_search_text"),
                "metric_lookup": _table_exists(self.conn, "metric_lookup"),
                "metric_dimension_lookup": _table_exists(self.conn, "metric_dimension_lookup"),
                "company_dimension_catalog": _table_exists(self.conn, "company_dimension_catalog"),
                "exposure_lookup": _table_exists(self.conn, "exposure_lookup"),
                "agreement_lookup": _table_exists(self.conn, "agreement_lookup"),
                "event_lookup": _table_exists(self.conn, "event_lookup"),
                "factor_lookup": _table_exists(self.conn, "factor_lookup"),
                "typed_projection_lookup": all(
                    _table_exists(self.conn, table_name) for table_name in _TYPED_PROJECTION_SPECS
                ),
                "company_context": _table_exists(self.conn, "company_topic_index"),
                "query_context": _table_exists(self.conn, "company_topic_index"),
                "answerability_tiers": True,
                "metric_lineage_trace": True,
                "traceability_metadata": _table_exists(self.conn, "object_traceability"),
            }
        if include_quality_summary:
            severity_counts = _count_by(self.conn, "quality_events", "severity")
            payload["quality_summary"] = {
                "severity_counts": severity_counts,
                "critical_errors": int(
                    severity_counts.get("critical") or severity_counts.get("error") or 0
                ),
                "batch_failures": self.conn.execute(
                    "SELECT COUNT(*) FROM quality_events WHERE category = 'batch_failure'"
                ).fetchone()[0],
                "orphan_answer_object_count": self.conn.execute(
                    """
                    SELECT COUNT(*)
                    FROM object_traceability
                    WHERE answer_candidate = 1
                      AND trace_status IN ('orphan', 'untraced', 'unknown')
                    """
                ).fetchone()[0],
            }
        return payload

    def company_context(
        self,
        *,
        ticker: str,
        document_types: Iterable[str] | None = None,
        periods: Iterable[str] | None = None,
        limit_topics: int = 12,
        include_internal_ids: bool = True,
    ) -> dict[str, Any]:
        """Return a compressed evidence-derived company topic profile."""
        ticker = ticker.upper()
        where, params = _company_topic_filters(
            tickers=[ticker],
            document_types=document_types,
            periods=periods,
        )
        where = f"{where} AND COALESCE(company_topic_index.boilerplate_score, 0) < 1"
        rows = self.conn.execute(
            f"""
            SELECT *
            FROM company_topic_index
            {where}
            ORDER BY
                CASE trace_status
                    WHEN 'traceable' THEN 3
                    WHEN 'traceable_metric_lineage' THEN 3
                    WHEN 'related' THEN 2
                    ELSE 1
                END DESC,
                COALESCE(specificity_score, 0) DESC,
                COALESCE(generic_score, 0) ASC,
                COALESCE(boilerplate_score, 0) ASC,
                evidence_chain_count DESC,
                support_quote_count + support_claim_count + support_metric_count DESC
            LIMIT ?
            """,
            [*params, max(1, min(int(limit_topics), 50))],
        ).fetchall()
        topics = [
            _company_topic_payload(
                topic_from_row(dict(row)), include_internal_ids=include_internal_ids
            )
            for row in rows
        ]
        payload = {
            "ticker": ticker,
            "document_types": list(document_types or []),
            "periods": list(periods or []),
            "company_topics": topics,
            "internal_only_fields": [
                "topic_id",
                "source_object_ids",
                "top_traceable_object_ids",
                "untraced_object_ids",
            ],
        }
        if not topics:
            payload["warnings"] = ["no_company_topics_found"]
        return payload

    def query_context(
        self,
        *,
        question: str,
        ticker: str | None = None,
        tickers: Iterable[str] | None = None,
        document_types: Iterable[str] | None = None,
        periods: Iterable[str] | None = None,
        universe: str | None = None,
        limit_results: int = 10,
        limit_tickers: int = 20,
        include_internal_ids: bool = True,
    ) -> dict[str, Any]:
        """Return a deterministic answer-planning pack for one user question."""
        query_context_started_at = time.perf_counter()
        document_types = list(document_types) if document_types is not None else None
        periods = list(periods) if periods is not None else None
        requested_tickers: list[str] | None
        if ticker:
            requested_tickers = [ticker.upper()]
        elif tickers:
            requested_tickers = [str(value).upper() for value in tickers]
        elif universe and universe != "all":
            requested_tickers = [str(universe).upper()]
        else:
            requested_tickers = None
        kernel = ResearchKernel(self)
        kernel_request = research_request_from_plan_fields(
            question=question,
            tickers=requested_tickers,
            document_types=document_types,
            periods=periods,
            universe=universe,
            limit_results=limit_results,
            limit_tickers=limit_tickers,
            mode="context",
        )
        kernel_route = kernel.route(kernel_request)
        cache_key = _query_context_cache_key(
            self.index_path,
            question=question,
            requested_tickers=requested_tickers,
            document_types=document_types,
            periods=periods,
            universe=universe,
            limit_results=limit_results,
            limit_tickers=limit_tickers,
            include_internal_ids=include_internal_ids,
        )
        cached = _query_context_cache_get(cache_key)
        if cached is not None:
            cached.setdefault("search_diagnostics", {})["cache_hit"] = True
            return cached

        valuation_guard = _research_context_valuation_guard(question)
        if valuation_guard is not None:
            payload = {
                "question": question,
                "query_frame": {
                    "raw_query": question,
                    "query_type": "valuation_or_price_target",
                },
                "answerability": {
                    "direct_answerable": False,
                    "related_context_available": False,
                    "negative_answer_supported": False,
                    "needs_user_clarification": False,
                    "recommended_answer_mode": "out_of_scope_for_filing_ontology",
                },
                "ticker_candidates": [],
                "recommended_tools": [],
                "search_diagnostics": {
                    "mode": "research_context_stop_guard",
                    "timing_ms": {
                        "query_context_total": int(
                            (time.perf_counter() - query_context_started_at) * 1000
                        ),
                    },
                },
                "final_answer_guidance": {
                    "safe_answer_pattern": valuation_guard["allowed_answer"],
                    "cannot_answer_reason": valuation_guard["cannot_answer_reason"],
                    "do_not_expose_internal_ids": True,
                },
                "research_context_version": "v1",
                "research_status": "out_of_scope_for_filing_ontology",
                "stop_guard": valuation_guard,
                "research_pack": {
                    "stop_guard": valuation_guard,
                    "valuation_guard": valuation_guard,
                    "scope_guard_pack": {
                        "mode": "scope_guard",
                        "out_of_scope": True,
                        "reason": valuation_guard["cannot_answer_reason"],
                        "allowed_answer": valuation_guard["allowed_answer"],
                        "allowed_filing_support": valuation_guard.get(
                            "allowed_filing_based_support"
                        )
                        or [],
                    },
                    "metric_series_pack": None,
                    "projection_pack": None,
                    "company_topic_pack": {"top_candidates": []},
                    "chain_pack": {
                        "primary_chains": [],
                        "needs_additional_chain": False,
                    },
                    "trace_candidates": [],
                },
                "missing_parts": [],
                "agent_autonomy": _research_agent_autonomy(
                    "out_of_scope_for_filing_ontology",
                    needs_trace=False,
                ),
                "do_not_call": [
                    "krw_ontology_query",
                    "krw_ontology_retrieve",
                    "krw_ontology_compare",
                    "krw_ontology_trace",
                    "krw_ontology_chain",
                ],
                "internal_only_fields": [
                    "topic_id",
                    "primary_object_id",
                    "source_object_ids",
                    "top_traceable_object_ids",
                    "recommended_tools.object_id",
                ],
            }
            payload["kernel"] = kernel.build_envelope(
                kernel_request,
                route=kernel_route,
                research_status=payload["research_status"],
                research_pack=payload["research_pack"],
                answerability=payload["answerability"],
                missing_parts=payload["missing_parts"],
                recommended_tools=payload["recommended_tools"],
                agent_autonomy=payload["agent_autonomy"],
                do_not_call=payload["do_not_call"],
                started_at=query_context_started_at,
                timing_ms=(payload.get("search_diagnostics") or {}).get("timing_ms") or {},
            )
            _query_context_cache_set(cache_key, payload)
            return payload

        planning_started_at = time.perf_counter()
        early_query_frame: dict[str, Any] = {"raw_query": question}
        if _question_requires_direct_match(question):
            early_query_frame["question_requires_direct_match"] = True
        early_answerability = _answerability_from_candidates(early_query_frame, [])
        if requested_tickers:
            early_research_pack = self._research_context_pack(
                question=question,
                search_topic=None,
                query_frame=early_query_frame,
                answerability=early_answerability,
                requested_tickers=requested_tickers,
                document_types=document_types,
                periods=periods,
                selected_candidates=[],
                recommended_tools=[],
                limit_results=limit_results,
                include_internal_ids=include_internal_ids,
            )
            early_research_status = _research_status_from_pack(
                answerability=early_answerability,
                candidates=[],
                research_pack=early_research_pack,
            )
            early_missing_parts = _research_missing_parts(early_research_pack)
            if _research_pack_can_skip_discovery(
                early_research_status, early_research_pack, question=question
            ):
                payload = {
                    "question": question,
                    "query_frame": early_query_frame,
                    "answerability": early_answerability,
                    "ticker_candidates": [],
                    "recommended_tools": [],
                    "search_diagnostics": _research_pack_search_diagnostics(
                        early_research_pack,
                        mode="research_context_fast_path",
                        discovery_skipped=True,
                        query_context_started_at=query_context_started_at,
                        planning_started_at=planning_started_at,
                    ),
                    "final_answer_guidance": _final_answer_guidance(early_answerability, []),
                    "research_context_version": "v1",
                    "research_status": early_research_status,
                    "research_pack": early_research_pack,
                    "missing_parts": early_missing_parts,
                    "agent_autonomy": _research_agent_autonomy(
                        early_research_status,
                        needs_trace=False,
                        missing_parts=early_missing_parts,
                    ),
                    "do_not_call": _research_do_not_call(early_research_status),
                    "internal_only_fields": [
                        "topic_id",
                        "primary_object_id",
                        "source_object_ids",
                        "top_traceable_object_ids",
                        "recommended_tools.object_id",
                    ],
                }
                if include_internal_ids:
                    payload["results_by_ticker"] = {}
                payload["kernel"] = kernel.build_envelope(
                    kernel_request,
                    route=kernel_route,
                    research_status=payload["research_status"],
                    research_pack=payload["research_pack"],
                    answerability=payload["answerability"],
                    missing_parts=payload["missing_parts"],
                    recommended_tools=payload["recommended_tools"],
                    agent_autonomy=payload["agent_autonomy"],
                    do_not_call=payload["do_not_call"],
                    started_at=query_context_started_at,
                    timing_ms=(payload.get("search_diagnostics") or {}).get("timing_ms") or {},
                )
                _query_context_cache_set(cache_key, payload)
                return payload

        discovery_started_at = time.perf_counter()
        discovery = self.discover_company_topics(
            question=question,
            tickers=requested_tickers,
            document_types=document_types,
            periods=periods,
            limit_groups=max(1, min(int(limit_tickers), 50)),
            limit_per_group=3,
            limit=_query_context_discovery_limit(
                limit_results=limit_results,
                limit_tickers=limit_tickers,
                requested_tickers=requested_tickers,
            ),
        )
        discovery_elapsed_ms = int((time.perf_counter() - discovery_started_at) * 1000)
        planning_started_at = time.perf_counter()
        candidates = list(discovery.get("ticker_candidates") or [])
        selected_candidates = candidates[: max(1, min(int(limit_tickers), 50))]
        query_frame = dict(discovery.get("query_frame") or {})
        if _question_requires_direct_match(question):
            query_frame["question_requires_direct_match"] = True
        answerability = _answerability_from_candidates(query_frame, selected_candidates)
        recommended_tools = _recommended_trace_tools(
            selected_candidates, limit=max(1, min(int(limit_results), 10))
        )
        research_pack = self._research_context_pack(
            question=question,
            search_topic=(discovery.get("search_diagnostics") or {}).get("search_topic"),
            query_frame=query_frame,
            answerability=answerability,
            requested_tickers=requested_tickers,
            document_types=document_types,
            periods=periods,
            selected_candidates=selected_candidates,
            recommended_tools=recommended_tools,
            limit_results=limit_results,
            include_internal_ids=include_internal_ids,
        )
        research_status = _research_status_from_pack(
            answerability=answerability,
            candidates=selected_candidates,
            research_pack=research_pack,
        )
        missing_parts = _research_missing_parts(research_pack)
        needs_trace = research_status == "sufficient_but_trace_recommended" or bool(
            recommended_tools
        )
        payload = {
            "question": question,
            "query_frame": query_frame,
            "answerability": answerability,
            "ticker_candidates": selected_candidates,
            "recommended_tools": recommended_tools,
            "search_diagnostics": discovery.get("search_diagnostics"),
            "final_answer_guidance": _final_answer_guidance(answerability, selected_candidates),
            "research_context_version": "v1",
            "research_status": research_status,
            "research_pack": research_pack,
            "missing_parts": missing_parts,
            "agent_autonomy": _research_agent_autonomy(
                research_status,
                needs_trace=needs_trace,
                missing_parts=missing_parts,
            ),
            "do_not_call": _research_do_not_call(research_status),
            "internal_only_fields": [
                "topic_id",
                "primary_object_id",
                "source_object_ids",
                "top_traceable_object_ids",
                "recommended_tools.object_id",
            ],
        }
        if include_internal_ids:
            payload["results_by_ticker"] = discovery.get("results_by_ticker") or {}
        search_diagnostics = payload.get("search_diagnostics")
        if isinstance(search_diagnostics, dict):
            timing = dict(search_diagnostics.get("timing_ms") or {})
            timing["query_context_discovery"] = discovery_elapsed_ms
            timing["query_context_planning"] = int(
                (time.perf_counter() - planning_started_at) * 1000
            )
            timing["query_context_total"] = int(
                (time.perf_counter() - query_context_started_at) * 1000
            )
            search_diagnostics["timing_ms"] = timing
            pack_diagnostics = _research_pack_search_diagnostics(
                research_pack,
                mode="research_context_with_discovery",
                discovery_skipped=False,
                query_context_started_at=query_context_started_at,
                planning_started_at=planning_started_at,
            )
            if pack_diagnostics.get("typed_projection"):
                search_diagnostics.setdefault(
                    "typed_projection", pack_diagnostics["typed_projection"]
                )
            if pack_diagnostics.get("metric_series"):
                search_diagnostics.setdefault("metric_series", pack_diagnostics["metric_series"])
        payload["kernel"] = kernel.build_envelope(
            kernel_request,
            route=kernel_route,
            research_status=payload["research_status"],
            research_pack=payload["research_pack"],
            answerability=payload["answerability"],
            missing_parts=payload["missing_parts"],
            recommended_tools=payload["recommended_tools"],
            agent_autonomy=payload["agent_autonomy"],
            do_not_call=payload["do_not_call"],
            started_at=query_context_started_at,
            timing_ms=(payload.get("search_diagnostics") or {}).get("timing_ms") or {},
        )
        _query_context_cache_set(cache_key, payload)
        return payload

    def _research_context_pack(
        self,
        *,
        question: str,
        search_topic: str | None,
        query_frame: Mapping[str, Any],
        answerability: Mapping[str, Any],
        requested_tickers: Sequence[str] | None,
        document_types: Sequence[str] | None,
        periods: Sequence[str] | None,
        selected_candidates: Sequence[Mapping[str, Any]],
        recommended_tools: Sequence[Mapping[str, Any]],
        limit_results: int,
        include_internal_ids: bool,
    ) -> dict[str, Any]:
        """Compile a bounded research workbench for agent synthesis."""
        research_route = route_research(question, tickers=requested_tickers or [])
        intent_router = intent_profile_from_route(research_route)
        context_policy = dict(context_policy_for_route(research_route))
        period_display_policy = period_display_policy_for_question(question, research_route)
        preferred_answer_order = preferred_answer_order_for_question(question, research_route)
        context_policy["period_display_policy"] = period_display_policy
        context_policy["preferred_answer_order"] = preferred_answer_order
        metric_series_pack: dict[str, Any] | None = None
        projection_pack: dict[str, Any] | None = None
        request = research_request_from_plan_fields(
            question=question,
            tickers=requested_tickers,
            document_types=document_types,
            periods=periods,
            universe=None,
            limit_results=limit_results,
            limit_tickers=len(requested_tickers or []),
            mode="context",
        )
        if context_policy.get("run_metric_series"):
            metric_context_result = MetricContext(self).run(
                request,
                research_route,
                search_topic=search_topic,
                requested_tickers=requested_tickers,
                document_types=document_types,
                periods=periods,
                limit_results=limit_results,
                metric_topic_builder=_research_metric_topic,
                metric_needs_denominator=_metric_lookup_needs_denominator,
                metric_period_filters=_metric_lookup_research_period_filters,
                annual_period_filter_check=_metric_lookup_period_filters_are_annual,
                metric_pack_builder=_metric_series_research_pack,
            )
            metric_series_pack = metric_context_result.get("metric_series_pack")

        if context_policy.get("run_typed_projection"):
            context_executor = _research_context_executor(research_route.primary_context, self)
            projection_context_result = context_executor.run(
                request,
                research_route,
                document_types=document_types,
                periods=periods,
                requested_tickers=requested_tickers,
                limit_results=limit_results,
                query_frame=query_frame,
                answerability=answerability,
                default_object_types=DEFAULT_QUERY_TYPES,
                projection_pack_builder=_projection_research_pack,
            )
            projection_pack = projection_context_result.get("projection_pack")

        chain_roots = _research_chain_roots(
            selected_candidates=selected_candidates,
            recommended_tools=recommended_tools,
            metric_series_pack=metric_series_pack,
            projection_pack=projection_pack,
            limit=2,
        )
        chain_pack = self._research_chain_pack(
            chain_roots,
            include_internal_ids=include_internal_ids,
        )
        company_topic_pack = _company_topic_research_pack(selected_candidates)
        directness_guard = _research_directness_guard(query_frame, answerability)
        business_profile_pack = _business_profile_research_pack(
            route=research_route,
            company_topic_pack=company_topic_pack,
            metric_series_pack=metric_series_pack,
            projection_pack=projection_pack,
            selected_candidates=selected_candidates,
            preferred_answer_order=preferred_answer_order,
            period_display_policy=period_display_policy,
        )
        risk_mechanism_pack = _risk_mechanism_research_pack(
            route=research_route,
            projection_pack=projection_pack,
            company_topic_pack=company_topic_pack,
            chain_pack=chain_pack,
        )
        comparison_view = _comparison_research_view(
            route=research_route,
            metric_series_pack=metric_series_pack,
            projection_pack=projection_pack,
            company_topic_pack=company_topic_pack,
        )
        direct_exposure_pack = _direct_exposure_research_pack(
            route=research_route,
            directness_guard=directness_guard,
            projection_pack=projection_pack,
            company_topic_pack=company_topic_pack,
        )
        cross_company_signal_pack = _cross_company_signal_research_pack(
            question=question,
            route=research_route,
            requested_tickers=requested_tickers,
            company_topic_pack=company_topic_pack,
            projection_pack=projection_pack,
            chain_pack=chain_pack,
            recommended_tools=recommended_tools,
            period_display_policy=period_display_policy,
        )
        scope_guard_pack = _scope_guard_research_pack(research_route)
        evidence_index = _research_evidence_index(
            trace_candidates=recommended_tools,
            chain_pack=chain_pack,
            metric_series_pack=metric_series_pack,
            projection_pack=projection_pack,
        )

        return {
            "intent_router": intent_router,
            "context_policy": context_policy,
            "period_display_policy": period_display_policy,
            "preferred_answer_order": preferred_answer_order,
            "metric_series_pack": metric_series_pack,
            "projection_pack": projection_pack,
            "business_profile_pack": business_profile_pack,
            "risk_mechanism_pack": risk_mechanism_pack,
            "comparison_view": comparison_view,
            "direct_exposure_pack": direct_exposure_pack,
            "cross_company_signal_pack": cross_company_signal_pack,
            "scope_guard_pack": scope_guard_pack,
            "company_topic_pack": company_topic_pack,
            "chain_pack": chain_pack,
            "evidence_index": evidence_index,
            "trace_candidates": [dict(tool) for tool in recommended_tools[:3]],
            "directness_guard": directness_guard,
        }

    def _research_chain_pack(
        self,
        object_ids: Sequence[str],
        *,
        include_internal_ids: bool,
        expand_chains: bool = False,
    ) -> dict[str, Any]:
        root_candidates: list[dict[str, Any]] = []
        for index, object_id in enumerate(object_ids[:2]):
            candidate: dict[str, Any] = {"root_index": index}
            if include_internal_ids:
                candidate["root_object_id"] = object_id
            root_candidates.append(candidate)
        if not expand_chains:
            return {
                "mode": "lazy_root_candidates",
                "chain_depth": 0,
                "max_roots": 2,
                "root_candidates": root_candidates,
                "primary_chains": [],
                "needs_additional_chain": bool(root_candidates),
                "allowed_additional_chain_depth": 1,
            }
        chains: list[dict[str, Any]] = []
        for object_id in object_ids[:2]:
            chain = self.chain(object_id, max_depth=1, direction="both", include_quote_text=False)
            if not chain:
                continue
            root = chain.get("object") or {}
            chain_payload: dict[str, Any] = {
                "root_label": root.get("label") or root.get("name") or root.get("id"),
                "root_type": root.get("type"),
                "chain_depth": (chain.get("chain") or {}).get("max_depth"),
                "semantic_neighbor_count": len(
                    (chain.get("chain") or {}).get("semantic_neighbors") or []
                ),
                "temporal_context_count": len(
                    (chain.get("chain") or {}).get("temporal_context") or []
                ),
                "evidence_claim_count": len(
                    ((chain.get("chain") or {}).get("evidence_chain") or {}).get("claims") or []
                ),
                "evidence_quote_count": len(
                    ((chain.get("chain") or {}).get("evidence_chain") or {}).get("quotes") or []
                ),
                "quality_warnings": (chain.get("quality") or {}).get("warnings") or [],
            }
            if include_internal_ids:
                chain_payload["root_object_id"] = object_id
            chains.append(chain_payload)
        return {
            "chain_depth": 1,
            "max_roots": 2,
            "primary_chains": chains,
            "needs_additional_chain": False,
            "allowed_additional_chain_depth": 1,
        }

    def get_object(self, object_id: str) -> dict[str, Any] | None:
        row = self.conn.execute(
            "SELECT * FROM objects WHERE id = ?",
            (object_id,),
        ).fetchone()
        return _object_from_row(row) if row else None

    def find_object_ids(self, prefix: str, *, limit: int = 20) -> list[dict[str, Any]]:
        """Return object id candidates whose ids start with a caller-provided prefix."""
        rows = self.conn.execute(
            """
            SELECT id, type, ticker, document_type, period, review_status
            FROM objects
            WHERE id LIKE ? || '%'
            ORDER BY id
            LIMIT ?
            """,
            (prefix, max(1, int(limit))),
        ).fetchall()
        return [dict(row) for row in rows]

    def query(
        self,
        *,
        topic: str | None = None,
        tickers: Iterable[str] | None = None,
        document_types: Iterable[str] | None = None,
        periods: Iterable[str] | None = None,
        object_types: Iterable[str] | None = None,
        include_rejected: bool = False,
        limit: int = 20,
    ) -> list[dict[str, Any]]:
        """Return evidence bundles matching structured filters and optional FTS text."""
        bundles, _diagnostics = self.query_with_diagnostics(
            topic=topic,
            tickers=tickers,
            document_types=document_types,
            periods=periods,
            object_types=object_types,
            include_rejected=include_rejected,
            limit=limit,
        )
        return bundles

    def query_with_diagnostics(
        self,
        *,
        topic: str | None = None,
        tickers: Iterable[str] | None = None,
        document_types: Iterable[str] | None = None,
        periods: Iterable[str] | None = None,
        object_types: Iterable[str] | None = None,
        include_rejected: bool = False,
        limit: int = 20,
    ) -> tuple[list[dict[str, Any]], dict[str, Any]]:
        """Return evidence bundles plus deterministic search diagnostics."""
        result_limit = max(1, int(limit))
        original_tickers = list(tickers) if tickers is not None else None
        tickers, unavailable_tickers = self._query_available_tickers(original_tickers)
        if original_tickers is not None and not tickers:
            return [], _ticker_guard_query_diagnostics(topic, unavailable_tickers)
        current_prior = self._current_document_prior_context(
            topic=topic,
            tickers=tickers,
            document_types=document_types,
            periods=periods,
        )
        query_limit = _current_document_prior_candidate_limit(result_limit, current_prior)
        explicit_object_types = object_types is not None
        selected_types = tuple(object_types or DEFAULT_QUERY_TYPES)
        search_strategy: dict[str, Any] | None = None
        metric_profile = self._metric_query_profile(
            topic=topic,
            tickers=tickers,
            periods=periods,
            object_types=selected_types,
            explicit_object_types=explicit_object_types,
        )
        metric_periods = metric_profile.get("periods") or periods
        typed_profile = self._typed_projection_profile(
            topic=topic,
            object_types=selected_types,
            explicit_object_types=explicit_object_types,
        )
        if metric_profile["enabled"]:
            rows, search_strategy = self._query_metric_lookup_with_strategy(
                metric_profile["topic"],
                tickers=tickers,
                document_types=document_types,
                periods=metric_periods,
                object_types=selected_types,
                include_rejected=include_rejected,
                limit=query_limit,
                normalization=metric_profile["normalization"],
            )
            if not rows and topic:
                fallback_types = _metric_lookup_fallback_object_types(
                    selected_types,
                    search_strategy=search_strategy,
                )
                if fallback_types:
                    rows, fallback_strategy = self._query_fts_with_strategy(
                        metric_profile["topic"],
                        tickers=tickers,
                        document_types=document_types,
                        periods=metric_periods,
                        object_types=fallback_types,
                        include_rejected=include_rejected,
                        limit=min(query_limit, 20),
                    )
                    search_strategy["fallback"] = fallback_strategy
                    search_strategy["fallback_used"] = True
                else:
                    search_strategy["fallback_used"] = False
                    search_strategy["fallback_skipped"] = "dimension_metric_not_found"
        elif typed_profile["enabled"]:
            rows, search_strategy = self._query_typed_projection_with_strategy(
                typed_profile,
                tickers=tickers,
                document_types=document_types,
                periods=periods,
                object_types=selected_types,
                include_rejected=include_rejected,
                limit=query_limit,
            )
            if not rows and topic:
                rows, fallback_strategy = self._query_fts_with_strategy(
                    topic,
                    tickers=tickers,
                    document_types=document_types,
                    periods=periods,
                    object_types=selected_types,
                    include_rejected=include_rejected,
                    limit=min(query_limit, 20),
                )
                search_strategy["fallback"] = fallback_strategy
                search_strategy["fallback_used"] = True
        elif topic:
            rows, search_strategy = self._query_fts_with_strategy(
                topic,
                tickers=tickers,
                document_types=document_types,
                periods=periods,
                object_types=selected_types,
                include_rejected=include_rejected,
                limit=query_limit,
            )
        else:
            rows = self._query_objects(
                tickers=tickers,
                document_types=document_types,
                periods=periods,
                object_types=selected_types,
                include_rejected=include_rejected,
                limit=query_limit,
            )
        rows = self._apply_current_document_prior(rows, current_prior, limit=result_limit)
        bundles = [self.bundle(row["id"]) for row in rows if row["id"]]
        diagnostics = _search_diagnostics(
            topic,
            result_count=len(bundles),
            search_strategy=search_strategy,
        )
        if metric_profile["enabled"]:
            diagnostics["metric_fast_path"] = True
            diagnostics["topic_normalization"] = metric_profile["normalization"]
        if typed_profile["enabled"]:
            diagnostics["typed_projection_fast_path"] = True
            diagnostics["projection"] = search_strategy
        if unavailable_tickers:
            diagnostics.setdefault("warnings", [])
            diagnostics["warnings"].append("ticker_not_available")
            diagnostics["unavailable_tickers"] = unavailable_tickers
            diagnostics["ticker_guard"] = True
        if current_prior.get("enabled"):
            diagnostics["current_document_prior"] = current_prior
        return bundles, diagnostics

    def query_compact_with_diagnostics(
        self,
        *,
        topic: str | None = None,
        tickers: Iterable[str] | None = None,
        document_types: Iterable[str] | None = None,
        periods: Iterable[str] | None = None,
        object_types: Iterable[str] | None = None,
        include_rejected: bool = False,
        limit: int = 20,
    ) -> tuple[list[dict[str, Any]], dict[str, Any]]:
        """Return row-level compact query results without eager evidence expansion."""
        result_limit = max(1, int(limit))
        original_tickers = list(tickers) if tickers is not None else None
        tickers, unavailable_tickers = self._query_available_tickers(original_tickers)
        if original_tickers is not None and not tickers:
            return [], _ticker_guard_query_diagnostics(topic, unavailable_tickers, compact=True)
        document_types = list(document_types) if document_types is not None else None
        periods = list(periods) if periods is not None else None
        current_prior = self._current_document_prior_context(
            topic=topic,
            tickers=tickers,
            document_types=document_types,
            periods=periods,
        )
        query_limit = _current_document_prior_candidate_limit(result_limit, current_prior)
        explicit_object_types = object_types is not None
        object_types = list(object_types) if object_types is not None else None
        selected_types = tuple(object_types or DEFAULT_QUERY_TYPES)
        metric_profile = self._metric_query_profile(
            topic=topic,
            tickers=tickers,
            periods=periods,
            object_types=selected_types,
            explicit_object_types=explicit_object_types,
        )
        metric_periods = metric_profile.get("periods") or periods
        scope_guard_types = selected_types
        if (
            _metric_lookup_topic_is_metric_like(topic or "")
            and _metric_lookup_dimension_anchors(topic or "")
            and set(selected_types).intersection(_METRIC_FAST_PATH_TYPES)
        ):
            scope_guard_types = tuple(dict.fromkeys([*selected_types, "XBRLFact"]))
        if (
            tickers
            and periods
            and not self._has_query_scope_objects(
                tickers=tickers,
                document_types=document_types,
                periods=periods,
                object_types=scope_guard_types,
                include_rejected=include_rejected,
            )
            and not metric_profile["enabled"]
        ):
            diagnostics = _search_diagnostics(topic, result_count=0)
            diagnostics["compact_fast_path"] = True
            diagnostics["scope_guard"] = "no_objects_for_ticker_period_scope"
            return [], diagnostics
        cache_key = _query_compact_cache_key(
            self.index_path,
            topic=topic,
            tickers=original_tickers,
            document_types=document_types,
            periods=periods,
            object_types=selected_types,
            include_rejected=include_rejected,
            limit=limit,
        )
        cached = _query_compact_cache_get(cache_key)
        if cached is not None:
            bundles, diagnostics = cached
            diagnostics = {**diagnostics, "cache_hit": True}
            return bundles, diagnostics
        search_strategy: dict[str, Any] | None = None
        typed_profile = self._typed_projection_profile(
            topic=topic,
            object_types=selected_types,
            explicit_object_types=explicit_object_types,
        )
        if metric_profile["enabled"]:
            rows, search_strategy = self._query_metric_lookup_with_strategy(
                metric_profile["topic"],
                tickers=tickers,
                document_types=document_types,
                periods=metric_periods,
                object_types=selected_types,
                include_rejected=include_rejected,
                limit=query_limit,
                normalization=metric_profile["normalization"],
            )
            if not rows and topic:
                fallback_types = _metric_lookup_fallback_object_types(
                    selected_types,
                    search_strategy=search_strategy,
                )
                if fallback_types:
                    rows, fallback_strategy = self._query_fts_with_strategy(
                        metric_profile["topic"],
                        tickers=tickers,
                        document_types=document_types,
                        periods=metric_periods,
                        object_types=fallback_types,
                        include_rejected=include_rejected,
                        limit=min(query_limit, 20),
                    )
                    search_strategy["fallback"] = fallback_strategy
                    search_strategy["fallback_used"] = True
                else:
                    search_strategy["fallback_used"] = False
                    search_strategy["fallback_skipped"] = "dimension_metric_not_found"
        elif typed_profile["enabled"]:
            rows, search_strategy = self._query_typed_projection_with_strategy(
                typed_profile,
                tickers=tickers,
                document_types=document_types,
                periods=periods,
                object_types=selected_types,
                include_rejected=include_rejected,
                limit=query_limit,
            )
            if not rows and topic:
                rows, fallback_strategy = self._query_fts_with_strategy(
                    topic,
                    tickers=tickers,
                    document_types=document_types,
                    periods=periods,
                    object_types=selected_types,
                    include_rejected=include_rejected,
                    limit=min(query_limit, 20),
                )
                search_strategy["fallback"] = fallback_strategy
                search_strategy["fallback_used"] = True
        elif topic:
            rows, search_strategy = self._query_fts_with_strategy(
                topic,
                tickers=tickers,
                document_types=document_types,
                periods=periods,
                object_types=selected_types,
                include_rejected=include_rejected,
                limit=query_limit,
            )
        else:
            rows = self._query_objects(
                tickers=tickers,
                document_types=document_types,
                periods=periods,
                object_types=selected_types,
                include_rejected=include_rejected,
                limit=query_limit,
            )
        rows = self._apply_current_document_prior(rows, current_prior, limit=result_limit)
        bundles = self._compact_bundles_from_rows(rows)
        diagnostics = _search_diagnostics(
            topic,
            result_count=len(bundles),
            search_strategy=search_strategy,
        )
        diagnostics["compact_fast_path"] = True
        if metric_profile["enabled"]:
            diagnostics["metric_fast_path"] = True
            diagnostics["topic_normalization"] = metric_profile["normalization"]
        if typed_profile["enabled"]:
            diagnostics["typed_projection_fast_path"] = True
            diagnostics["projection"] = search_strategy
        if unavailable_tickers:
            diagnostics.setdefault("warnings", [])
            diagnostics["warnings"].append("ticker_not_available")
            diagnostics["unavailable_tickers"] = unavailable_tickers
            diagnostics["ticker_guard"] = True
        if current_prior.get("enabled"):
            diagnostics["current_document_prior"] = current_prior
        result = (bundles, diagnostics)
        _query_compact_cache_set(cache_key, result)
        return result

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
        tickers: Iterable[str] | None = None,
        document_types: Iterable[str] | None = None,
        periods: Iterable[str] | None = None,
        object_types: Iterable[str] | None = None,
        include_rejected: bool = False,
        allow_relaxed: bool = False,
        limit: int = 20,
    ) -> tuple[list[dict[str, Any]], dict[str, Any]]:
        """Execute one agent-authored clause without keyword routing or expansion.

        This is the MCP v2 execution primitive.  The caller has already planned the
        intent and scope, so this method only performs deterministic field-scoped
        FTS.  Strict results require every supplied lexical token.  An optional OR
        pass is kept separate and labelled ``relaxed`` so it can never be promoted
        to direct evidence by the response compiler.
        """
        started_at = time.perf_counter()
        result_limit = max(1, int(limit))
        original_tickers = list(tickers) if tickers is not None else None
        available_tickers, unavailable_tickers = self._query_available_tickers(original_tickers)
        if original_tickers is not None and not available_tickers:
            diagnostics = _ticker_guard_query_diagnostics(
                retrieval_query,
                unavailable_tickers,
                compact=True,
            )
            diagnostics.update(
                {
                    "execution_mode": "planned_fts",
                    "strict_result_count": 0,
                    "relaxed_result_count": 0,
                    "timing_ms": {"total": int((time.perf_counter() - started_at) * 1000)},
                }
            )
            return [], diagnostics

        selected_types = tuple(object_types or DEFAULT_QUERY_TYPES)
        requested_metrics = _unique(
            str(metric).strip() for metric in metrics or [] if str(metric).strip()
        )
        requested_comparison_axes = _unique(
            str(axis).strip().casefold() for axis in comparison_axes or [] if str(axis).strip()
        )
        lexical_terms = _planned_retrieval_terms(
            retrieval_query,
            retrieval_terms=retrieval_terms,
        )
        evidence_terms = _planned_evidence_terms(
            retrieval_query,
            retrieval_terms=retrieval_terms,
        )
        predicate_lexical_terms = _unique(
            term for value in predicate_terms or [] for term in _planned_query_terms(str(value))
        )[:32]
        metric_started_at = time.perf_counter()
        queried_metric_rows = self._query_metrics(
            requested_metrics,
            tickers=available_tickers,
            document_types=document_types,
            periods=periods,
            metric_dimensions=metric_dimensions,
            metric_scope=metric_scope,
            calculation_window=calculation_window,
            comparison_axes=requested_comparison_axes,
            limit_per_metric=result_limit,
        )
        metric_elapsed_ms = int((time.perf_counter() - metric_started_at) * 1000)
        metric_groups_by_name: dict[str, list[sqlite3.Row]] = {}
        for row in queried_metric_rows:
            metric_groups_by_name.setdefault(str(row["metric_name"] or ""), []).append(row)
        metric_groups = list(metric_groups_by_name.values())
        metric_rows: list[sqlite3.Row] = []
        metric_row_index = 0
        while len(metric_rows) < result_limit:
            added = False
            for group in metric_groups:
                if metric_row_index >= len(group):
                    continue
                metric_rows.append(group[metric_row_index])
                added = True
                if len(metric_rows) >= result_limit:
                    break
            if not added:
                break
            metric_row_index += 1

        strict_query = " ".join(f"{term}*" for term in lexical_terms)
        fts_started_at = time.perf_counter()
        predicate_query = " ".join(
            [
                strict_query,
                *[f"{term}*" for term in predicate_lexical_terms],
            ]
        ).strip()
        fts_predicate_rows = (
            self._execute_fts(
                predicate_query,
                tickers=available_tickers,
                document_types=document_types,
                periods=periods,
                object_types=selected_types,
                include_rejected=include_rejected,
                limit=result_limit,
            )
            if strict_query and predicate_lexical_terms
            else []
        )
        fts_strict_rows = (
            self._execute_fts(
                strict_query,
                tickers=available_tickers,
                document_types=document_types,
                periods=periods,
                object_types=selected_types,
                include_rejected=include_rejected,
                limit=result_limit,
            )
            if strict_query
            else []
        )
        fts_elapsed_ms = int((time.perf_counter() - fts_started_at) * 1000)

        balanced_strict_rows: list[sqlite3.Row] = []
        balanced_strict_ids: set[str] = set()

        def append_strict_row(row: sqlite3.Row) -> None:
            row_id = str(row["id"] or "")
            if not row_id or row_id in balanced_strict_ids:
                return
            balanced_strict_ids.add(row_id)
            balanced_strict_rows.append(row)

        # QueryClause validation keeps metric propositions independent from
        # qualitative propositions. Interleave metric rows with FTS rows so
        # that text evidence (EvidenceQuote, ResearchClaim) is not excluded
        # when metric results fill the limit. Reserve at least half the limit
        # for FTS evidence so causal/qualitative context is always present.
        metric_reserve = min(len(metric_rows), result_limit // 2) if requested_metrics else 0
        if metric_reserve:
            for row in metric_rows[:metric_reserve]:
                append_strict_row(row)

        strict_row_index = 0
        strict_groups = [list(fts_predicate_rows), list(fts_strict_rows), list(metric_rows[metric_reserve:]) if metric_reserve else []]
        while len(balanced_strict_rows) < result_limit:
            progressed = False
            for group in strict_groups:
                if strict_row_index >= len(group):
                    continue
                progressed = True
                append_strict_row(group[strict_row_index])
                if len(balanced_strict_rows) >= result_limit:
                    break
            if not progressed:
                break
            strict_row_index += 1
        ordered_rows = balanced_strict_rows
        metric_ids = {str(row["id"]) for row in metric_rows if row["id"]}
        strict_ids = {
            str(row["id"])
            for row in [*metric_rows, *fts_predicate_rows, *fts_strict_rows]
            if row["id"]
        }
        predicate_ids = {str(row["id"]) for row in fts_predicate_rows if row["id"]}
        relaxed_ids: set[str] = set()
        relaxed_elapsed_ms = 0
        if lexical_terms and len(ordered_rows) < result_limit:
            relaxed_started_at = time.perf_counter()
            relaxed_query = " OR ".join(f"{term}*" for term in lexical_terms)
            relaxed_rows = self._execute_fts(
                relaxed_query,
                tickers=available_tickers,
                document_types=document_types,
                periods=periods,
                object_types=selected_types,
                include_rejected=include_rejected,
                limit=result_limit,
            )
            for row in relaxed_rows:
                row_id = str(row["id"] or "")
                if not row_id or row_id in strict_ids or row_id in relaxed_ids:
                    continue
                relaxed_ids.add(row_id)
                ordered_rows.append(row)
                if len(ordered_rows) >= result_limit:
                    break
            relaxed_elapsed_ms = int((time.perf_counter() - relaxed_started_at) * 1000)

        bundles = self._compact_bundles_from_rows(ordered_rows[:result_limit])
        metric_metadata_by_id = {
            str(row["id"]): row
            for row in ordered_rows
            if str(row["id"] or "") in metric_ids
            and "planned_metric_is_company_total" in row.keys()
        }
        for bundle in bundles:
            row_id = str(bundle.get("id") or "")
            metadata = metric_metadata_by_id.get(row_id)
            if metadata is not None:
                obj = dict(bundle.get("object") or {})
                obj["is_company_total"] = bool(metadata["planned_metric_is_company_total"])
                try:
                    dimensions = json.loads(str(metadata["planned_metric_dimensions_json"] or "{}"))
                except (TypeError, ValueError):
                    dimensions = {}
                if isinstance(dimensions, Mapping):
                    obj["dimensions"] = dict(dimensions)
                if metadata["planned_metric_canonical_metric"]:
                    obj["canonical_metric"] = metadata["planned_metric_canonical_metric"]
                if metadata["planned_metric_unit"] and not obj.get("unit"):
                    obj["unit"] = metadata["planned_metric_unit"]
                observation_period = str(
                    metadata["planned_metric_observation_period"] or ""
                ).strip()
                filing_period = str(metadata["planned_metric_filing_period"] or "").strip()
                if observation_period:
                    bundle["period"] = observation_period
                    obj["observation_period"] = observation_period
                if filing_period:
                    bundle["filing_period"] = filing_period
                    obj["filing_period"] = filing_period
                period_type = str(metadata["planned_metric_observation_period_type"] or "").strip()
                if period_type:
                    obj["period_type"] = period_type
                if metadata["planned_metric_observation_start_date"]:
                    obj["period_start"] = metadata["planned_metric_observation_start_date"]
                if metadata["planned_metric_observation_end_date"]:
                    obj["period_end"] = metadata["planned_metric_observation_end_date"]
                obj["observation_context_key"] = metadata["planned_metric_observation_context_key"]
                conflict_count = int(metadata["planned_metric_conflict_value_count"] or 0)
                if conflict_count > 1:
                    obj["metric_conflict"] = True
                    obj["metric_conflict_value_count"] = conflict_count
                bundle["object"] = obj
            bundle["planned_match_mode"] = "strict" if row_id in strict_ids else "relaxed"
            bundle["planned_lexical_terms"] = lexical_terms
            bundle["planned_evidence_terms"] = evidence_terms

        diagnostics: dict[str, Any] = {
            "execution_mode": "planned_fts",
            "retrieval_query": retrieval_query,
            "lexical_terms": lexical_terms,
            "evidence_terms": evidence_terms,
            "predicate_terms": predicate_lexical_terms,
            "predicate_boost_result_count": len(predicate_ids),
            "strict_operator": "AND",
            "strict_result_count": len(strict_ids),
            "fts_strict_result_count": len(fts_strict_rows),
            "metric_lookup_used": bool(requested_metrics),
            "requested_metrics": requested_metrics,
            "metric_result_count": len(metric_ids),
            "calculation_window": calculation_window,
            "comparison_axes": sorted(requested_comparison_axes),
            "metric_observation_period_v2": bool(requested_metrics),
            "metric_conflict_result_count": sum(
                1 for bundle in bundles if bool((bundle.get("object") or {}).get("metric_conflict"))
            ),
            "relaxed_enabled": bool(allow_relaxed),
            "relaxed_result_count": len(relaxed_ids),
            "result_count": len(bundles),
            "compact_fast_path": True,
            "keyword_expansion_used": False,
            "intent_reclassification_used": False,
            "warnings": [],
            "timing_ms": {
                "metric_lookup": metric_elapsed_ms,
                "strict_fts": fts_elapsed_ms,
                "relaxed_fts": relaxed_elapsed_ms,
                "total": int((time.perf_counter() - started_at) * 1000),
            },
        }
        if unavailable_tickers:
            diagnostics["warnings"].append("ticker_not_available")
            diagnostics["unavailable_tickers"] = unavailable_tickers
            diagnostics["ticker_guard"] = True
        return bundles, diagnostics

    def _current_document_prior_context(
        self,
        *,
        topic: str | None,
        tickers: Sequence[str] | None,
        document_types: Iterable[str] | None,
        periods: Iterable[str] | None,
    ) -> dict[str, Any]:
        if not _query_wants_current_document_prior(topic):
            return {"enabled": False, "reason": "no_current_filing_intent"}
        if _query_has_explicit_period(topic, periods):
            return {"enabled": False, "reason": "explicit_period_scope"}
        roles = self._filing_document_roles(
            tickers=tickers,
            document_types=document_types,
        )
        anchors = _current_driver_anchors_from_roles(roles)
        if not anchors:
            return {"enabled": False, "reason": "no_current_document_anchor"}
        return {
            "enabled": True,
            "reason": "current_filing_intent",
            "latest_documents": anchors,
            "filing_document_roles": roles,
            "policy": (
                "Prefer current_driver for current/latest questions. Use annual_baseline for "
                "business mix, long-term structure, and historical baseline unless the user asked "
                "for a past period."
            ),
        }

    def _filing_document_roles(
        self,
        *,
        tickers: Sequence[str] | None,
        document_types: Iterable[str] | None,
    ) -> dict[str, dict[str, Any]]:
        documents: list[dict[str, Any]] = []
        ticker_values = [
            str(ticker).upper() for ticker in tickers or [] if str(ticker or "").strip()
        ]
        if ticker_values:
            for ticker in ticker_values:
                documents.extend(self.list_documents(ticker=ticker, document_types=document_types))
        else:
            documents = self.list_documents(document_types=document_types)
        return filing_document_roles_from_documents(documents, tickers=ticker_values or None)

    def _apply_current_document_prior(
        self,
        rows: Sequence[sqlite3.Row],
        prior: Mapping[str, Any],
        *,
        limit: int,
    ) -> list[sqlite3.Row]:
        if not rows:
            return []
        if not prior.get("enabled"):
            return list(rows)[: max(1, int(limit))]
        anchors = prior.get("latest_documents")
        if not isinstance(anchors, Mapping) or not anchors:
            return list(rows)[: max(1, int(limit))]

        def row_key(
            index_and_row: tuple[int, sqlite3.Row],
        ) -> tuple[int, tuple[int, int], int, int]:
            index, row = index_and_row
            ticker = str(row["ticker"] or "").upper()
            period = str(row["period"] or "")
            document_type = str(row["document_type"] or "")
            anchor = anchors.get(ticker)
            latest_doc_hit = 0
            if isinstance(anchor, Mapping):
                latest_doc_hit = int(
                    str(anchor.get("period") or "") == period
                    and str(anchor.get("document_type") or "") == document_type
                )
            return (
                latest_doc_hit,
                _period_recency_key(period),
                _document_type_recency_priority(document_type),
                -index,
            )

        ranked = sorted(enumerate(rows), key=row_key, reverse=True)
        return [row for _index, row in ranked[: max(1, int(limit))]]

    def _query_available_tickers(
        self,
        tickers: Sequence[str] | None,
    ) -> tuple[list[str] | None, list[str]]:
        if tickers is None:
            return None, []
        normalized = [str(ticker).upper() for ticker in tickers if ticker]
        if not normalized:
            return None, []
        available = self._available_tickers(normalized)
        filtered = [ticker for ticker in normalized if ticker in available]
        unavailable = [ticker for ticker in normalized if ticker not in available]
        return filtered, unavailable

    def search_diagnostics(
        self,
        topic: str | None,
        *,
        result_count: int | None = None,
    ) -> dict[str, Any]:
        """Return deterministic diagnostics for the FTS topic query."""
        return _search_diagnostics(topic, result_count=result_count)

    def discover_company_topics(
        self,
        *,
        question: str,
        tickers: Iterable[str] | None = None,
        document_types: Iterable[str] | None = None,
        periods: Iterable[str] | None = None,
        limit_groups: int = 20,
        limit_per_group: int = 3,
        limit: int = 200,
    ) -> dict[str, Any]:
        """Discover ticker candidates by matching QueryFrame to evidence-derived topics."""
        discovery_started_at = time.perf_counter()
        cache_key = _discovery_cache_key(
            self.index_path,
            question=question,
            tickers=tickers,
            document_types=document_types,
            periods=periods,
            limit_groups=limit_groups,
            limit_per_group=limit_per_group,
            limit=limit,
        )
        cached = _discovery_cache_get(cache_key)
        if cached is not None:
            cached.setdefault("search_diagnostics", {})["cache_hit"] = True
            return cached
        query_frame_started_at = time.perf_counter()
        query_frame = build_query_frame(question)
        search_topic = _discovery_search_topic(question)
        expanded_search_topic = _expanded_topic(search_topic)
        fts_query = _fts_query(expanded_search_topic, operator="OR")
        fts_strategy = "or"
        query_frame_elapsed_ms = int((time.perf_counter() - query_frame_started_at) * 1000)
        try:
            topic_query_started_at = time.perf_counter()
            rows: list[sqlite3.Row] = []
            ticker_scope = [str(ticker).upper() for ticker in tickers] if tickers else []
            topic_query_attempts: list[dict[str, Any]] = []

            def query_company_topics_attempt(
                label: str,
                query: str,
                *,
                attempt_limit: int,
            ) -> list[sqlite3.Row]:
                attempt_started_at = time.perf_counter()
                attempt_rows = self._query_company_topics(
                    query,
                    tickers=tickers,
                    document_types=document_types,
                    periods=periods,
                    limit=attempt_limit,
                )
                topic_query_attempts.append(
                    {
                        "label": label,
                        "result_count": len(attempt_rows),
                        "limit": attempt_limit,
                        "elapsed_ms": int((time.perf_counter() - attempt_started_at) * 1000),
                    }
                )
                return attempt_rows

            if (
                fts_query
                and len(ticker_scope) == 1
                and len(_query_terms(expanded_search_topic)) >= 4
            ):
                and_fts_query = _fts_query(expanded_search_topic, operator="AND")
                and_rows = (
                    query_company_topics_attempt(
                        "ticker_full_and",
                        and_fts_query,
                        attempt_limit=limit,
                    )
                    if and_fts_query
                    else []
                )
                if len(and_rows) >= max(1, int(limit_per_group)):
                    rows = and_rows
                    fts_query = and_fts_query
                    fts_strategy = "and_first"
                else:
                    pair_rows: list[sqlite3.Row] = []
                    seen_topic_ids: set[str] = set()
                    pair_queries = [
                        _fts_query(chunk, operator="AND")
                        for chunk in _split_topic_queries(search_topic, limit=8)
                        if len(_query_terms(chunk)) >= 2
                    ]
                    pair_limit = max(1, min(max(int(limit_per_group) * 2, 6), int(limit)))
                    pair_stop = max(1, min(max(int(limit_per_group) * 2, 6), int(limit)))
                    for pair_query in pair_queries:
                        if not pair_query:
                            continue
                        for row in query_company_topics_attempt(
                            "ticker_pair_and",
                            pair_query,
                            attempt_limit=pair_limit,
                        ):
                            topic_id = row["topic_id"]
                            if topic_id in seen_topic_ids:
                                continue
                            seen_topic_ids.add(topic_id)
                            pair_rows.append(row)
                            if len(pair_rows) >= pair_stop:
                                break
                        if len(pair_rows) >= pair_stop:
                            break
                    if len(pair_rows) >= max(1, int(limit_per_group)):
                        rows = pair_rows[:limit]
                        fts_query = " OR ".join(pair_queries[:4])
                        fts_strategy = "and_pair_fallback"
                    else:
                        rows = query_company_topics_attempt(
                            "ticker_relaxed_or",
                            fts_query,
                            attempt_limit=limit,
                        )
                        fts_strategy = "and_fallback_or"
            elif fts_query and not ticker_scope and len(_query_terms(expanded_search_topic)) >= 4:
                and_fts_query = _fts_query(expanded_search_topic, operator="AND")
                and_rows = (
                    query_company_topics_attempt(
                        "global_full_and",
                        and_fts_query,
                        attempt_limit=limit,
                    )
                    if and_fts_query
                    else []
                )
                and_ticker_count = len({row["ticker"] for row in and_rows if row["ticker"]})
                required_tickers = min(max(2, int(limit_groups) // 2), 5)
                if (
                    len(and_rows) >= max(1, int(limit_per_group))
                    and and_ticker_count >= required_tickers
                ):
                    rows = and_rows
                    fts_query = and_fts_query
                    fts_strategy = "global_and_first"
                else:
                    pair_rows = []
                    seen_topic_ids: set[str] = set()
                    pair_queries = [
                        _fts_query(chunk, operator="AND")
                        for chunk in _split_topic_queries(search_topic, limit=10)
                        if len(_query_terms(chunk)) >= 2
                    ]
                    pair_limit = max(1, min(max(int(limit_per_group) * 2, 6), int(limit)))
                    pair_stop = max(
                        1, min(max(int(limit_groups) * int(limit_per_group), 16), int(limit))
                    )
                    for pair_query in pair_queries:
                        if not pair_query:
                            continue
                        for row in query_company_topics_attempt(
                            "global_pair_and",
                            pair_query,
                            attempt_limit=pair_limit,
                        ):
                            topic_id = row["topic_id"]
                            if topic_id in seen_topic_ids:
                                continue
                            seen_topic_ids.add(topic_id)
                            pair_rows.append(row)
                            if (
                                len(pair_rows) >= pair_stop
                                and len(
                                    {
                                        candidate["ticker"]
                                        for candidate in pair_rows
                                        if candidate["ticker"]
                                    }
                                )
                                >= required_tickers
                            ):
                                break
                        if (
                            len(pair_rows) >= pair_stop
                            and len(
                                {
                                    candidate["ticker"]
                                    for candidate in pair_rows
                                    if candidate["ticker"]
                                }
                            )
                            >= required_tickers
                        ):
                            break
                    pair_ticker_count = len({row["ticker"] for row in pair_rows if row["ticker"]})
                    if (
                        len(pair_rows) >= max(1, int(limit_per_group))
                        and pair_ticker_count >= required_tickers
                    ):
                        rows = pair_rows[:limit]
                        fts_query = " OR ".join(pair_queries[:4])
                        fts_strategy = "global_and_pair_fallback"
                    else:
                        rows = query_company_topics_attempt(
                            "global_relaxed_or",
                            fts_query,
                            attempt_limit=limit,
                        )
                        fts_strategy = "global_and_fallback_or"
            elif fts_query:
                rows = query_company_topics_attempt(
                    "default_or",
                    fts_query,
                    attempt_limit=limit,
                )
            topic_query_elapsed_ms = int((time.perf_counter() - topic_query_started_at) * 1000)
        except sqlite3.OperationalError as exc:
            return {
                "query_frame": query_frame.as_dict(),
                "ticker_candidates": [],
                "results_by_ticker": {},
                "search_diagnostics": {
                    "topic": question,
                    "fts_query": fts_query,
                    "searched_company_topics": 0,
                    "matched_tickers": 0,
                    "warnings": ["company_topic_index_unavailable"],
                    "error": str(exc),
                },
            }

        grouped: dict[str, list[dict[str, Any]]] = {}
        classify_started_at = time.perf_counter()
        for row in rows:
            topic = topic_from_row(dict(row))
            evidence_frame = build_evidence_frame(topic)
            match = classify_topic_match(query_frame, evidence_frame)
            if match["tier"] == "insufficient":
                continue
            semantic_relevance = str(match.get("tier") or "insufficient")
            trace_status = str(topic.get("trace_status") or "unknown")
            final_tier = _combined_discovery_tier(semantic_relevance, trace_status)
            match = {
                **match,
                "semantic_relevance": semantic_relevance,
                "trace_status": trace_status,
                "tier": final_tier,
                "why_tier": _why_discovery_tier(semantic_relevance, trace_status, final_tier),
            }
            topic_payload = {
                "topic_id": topic.get("topic_id"),
                "ticker": topic.get("ticker"),
                "period": topic.get("period"),
                "document_type": topic.get("document_type"),
                "topic_label": topic.get("topic_label"),
                "topic_summary": topic.get("topic_summary"),
                "primary_object_id": topic.get("primary_object_id"),
                "primary_object_type": topic.get("primary_object_type"),
                "source_object_ids": topic.get("source_object_ids") or [],
                "dominant_object_types": topic.get("dominant_object_types") or [],
                "impact_channels": topic.get("impact_channels") or [],
                "evidence_strength": topic.get("evidence_strength"),
                "support_quote_count": topic.get("support_quote_count") or 0,
                "support_claim_count": topic.get("support_claim_count") or 0,
                "trace_status": trace_status,
                "evidence_chain_count": topic.get("evidence_chain_count") or 0,
                "support_depth": topic.get("support_depth"),
                "support_link_count": topic.get("support_link_count") or 0,
                "trace_method": topic.get("trace_method"),
                "metric_lineage_status": topic.get("metric_lineage_status"),
                "answer_candidate": bool(topic.get("answer_candidate")),
                "specificity_score": topic.get("specificity_score"),
                "generic_score": topic.get("generic_score"),
                "boilerplate_score": topic.get("boilerplate_score"),
                "materiality_hint": topic.get("materiality_hint"),
                "materiality_score": topic.get("materiality_score"),
                "topic_type": topic.get("topic_type"),
                "topic_family": topic.get("topic_family"),
                "factor_terms": topic.get("factor_terms") or [],
                "metric_terms": topic.get("metric_terms") or [],
                "entity_terms": topic.get("entity_terms") or [],
                "mechanism_terms": topic.get("mechanism_terms") or [],
                "scenario_terms": topic.get("scenario_terms") or [],
                "top_traceable_object_ids": topic.get("top_traceable_object_ids") or [],
                "untraced_object_ids": topic.get("untraced_object_ids") or [],
                "match": match,
            }
            grouped.setdefault(str(topic.get("ticker") or "UNKNOWN"), []).append(topic_payload)
        projection_diagnostics: dict[str, Any] | None = None
        projection_elapsed_ms = 0
        projection_profile = self._typed_projection_profile(
            topic=question,
            object_types=DEFAULT_QUERY_TYPES,
            explicit_object_types=False,
        )
        if projection_profile["enabled"]:
            projection_started_at = time.perf_counter()
            projection_rows, projection_strategy = self._query_typed_projection_with_strategy(
                projection_profile,
                tickers=[str(ticker).upper() for ticker in tickers] if tickers else None,
                document_types=document_types,
                periods=periods,
                object_types=DEFAULT_QUERY_TYPES,
                include_rejected=False,
                limit=min(max(int(limit_groups) * int(limit_per_group), 10), int(limit)),
            )
            projection_bundles = self._compact_bundles_from_rows(projection_rows)
            for bundle in projection_bundles:
                topic_payload = _topic_payload_from_compact_bundle(
                    bundle,
                    query_frame=query_frame,
                    source="typed_projection",
                )
                if topic_payload is None:
                    continue
                grouped.setdefault(str(topic_payload.get("ticker") or "UNKNOWN"), []).append(
                    topic_payload
                )
            projection_elapsed_ms = int((time.perf_counter() - projection_started_at) * 1000)
            projection_diagnostics = projection_strategy
        classify_elapsed_ms = int((time.perf_counter() - classify_started_at) * 1000)

        candidates: list[dict[str, Any]] = []
        results_by_ticker: dict[str, list[dict[str, Any]]] = {}
        grouping_started_at = time.perf_counter()
        for ticker, topics in grouped.items():
            topics.sort(
                key=lambda item: (
                    tier_rank(str((item.get("match") or {}).get("tier") or "")),
                    float((item.get("match") or {}).get("score") or 0.0),
                    float(item.get("specificity_score") or 0.0),
                    -float(item.get("generic_score") or 0.0),
                    -float(item.get("boilerplate_score") or 0.0),
                    int(item.get("support_quote_count") or 0)
                    + int(item.get("support_claim_count") or 0),
                ),
                reverse=True,
            )
            selected = topics[:limit_per_group]
            if not selected:
                continue
            results_by_ticker[ticker] = selected
            best_tier = str((selected[0].get("match") or {}).get("tier") or "insufficient")
            top_object_ids: list[str] = []
            for topic in selected:
                for object_id in topic.get("source_object_ids") or []:
                    if object_id not in top_object_ids:
                        top_object_ids.append(object_id)
            candidates.append(
                {
                    "ticker": ticker,
                    "score": round(
                        sum(
                            float((topic.get("match") or {}).get("score") or 0.0)
                            for topic in selected
                        ),
                        4,
                    ),
                    "tier": best_tier,
                    "matched_topic_count": len(topics),
                    "matched_object_counts": _topic_object_counts(topics),
                    "evidence_counts": _topic_evidence_counts(topics),
                    "trace_status": _ticker_trace_status(selected),
                    "trace_counts": _topic_trace_counts(selected),
                    "top_traceable_object_ids": _traceable_object_ids(selected)[:10],
                    "untraced_object_ids": _untraced_object_ids(selected)[:10],
                    "top_reasons": [_topic_reason(topic) for topic in selected],
                    "top_object_ids": top_object_ids[:10],
                    "matched_topics": selected,
                }
            )

        candidates.sort(
            key=lambda item: (
                tier_rank(str(item.get("tier") or "")),
                float(item.get("score") or 0.0),
                int(item.get("matched_topic_count") or 0),
            ),
            reverse=True,
        )
        candidates = candidates[:limit_groups]
        grouping_elapsed_ms = int((time.perf_counter() - grouping_started_at) * 1000)
        allowed = {str(candidate.get("ticker")) for candidate in candidates}
        fallback_diagnostics: dict[str, Any] | None = None
        fallback_elapsed_ms = 0
        if not candidates and question:
            fallback_started_at = time.perf_counter()
            fallback_limit = min(max(limit, limit_groups * limit_per_group), 80)
            if tickers:
                fallback_limit = min(fallback_limit, 24)
            fallback = self._discovery_object_fallback(
                question=search_topic,
                tickers=tickers,
                document_types=document_types,
                periods=periods,
                limit_groups=limit_groups,
                limit_per_group=limit_per_group,
                limit=fallback_limit,
            )
            candidates = fallback["ticker_candidates"]
            results_by_ticker = fallback["results_by_ticker"]
            allowed = {str(candidate.get("ticker")) for candidate in candidates}
            fallback_diagnostics = fallback["search_diagnostics"]
            fallback_elapsed_ms = int((time.perf_counter() - fallback_started_at) * 1000)
        result = {
            "query_frame": query_frame.as_dict(),
            "ticker_candidates": candidates,
            "results_by_ticker": {
                ticker: results_by_ticker[ticker]
                for ticker in sorted(allowed)
                if ticker in results_by_ticker
            },
            "search_diagnostics": {
                "topic": question,
                "search_topic": search_topic,
                "fts_query": fts_query,
                "fts_strategy": fts_strategy,
                "company_topic_attempts": topic_query_attempts,
                "searched_company_topics": len(rows),
                "matched_tickers": len(candidates),
                "timing_ms": {
                    "query_frame": query_frame_elapsed_ms,
                    "company_topic_query": topic_query_elapsed_ms,
                    "typed_projection_query": projection_elapsed_ms,
                    "classify_topics": classify_elapsed_ms,
                    "group_rank_candidates": grouping_elapsed_ms,
                    "object_fallback": fallback_elapsed_ms,
                    "discover_total": int((time.perf_counter() - discovery_started_at) * 1000),
                },
                **({"typed_projection": projection_diagnostics} if projection_diagnostics else {}),
                **({"object_fallback": fallback_diagnostics} if fallback_diagnostics else {}),
            },
        }
        _discovery_cache_set(cache_key, result)
        return result

    def _discovery_object_fallback(
        self,
        *,
        question: str,
        tickers: Iterable[str] | None,
        document_types: Iterable[str] | None,
        periods: Iterable[str] | None,
        limit_groups: int,
        limit_per_group: int,
        limit: int,
    ) -> dict[str, Any]:
        """Fallback discovery from compact object hits when rich topic matching yields no candidates."""
        bundles, diagnostics = self.query_compact_with_diagnostics(
            topic=question,
            tickers=tickers,
            document_types=document_types,
            periods=periods,
            object_types=DEFAULT_QUERY_TYPES,
            include_rejected=False,
            limit=limit,
        )
        grouped: dict[str, list[dict[str, Any]]] = {}
        for rank, bundle in enumerate(bundles, start=1):
            ticker = str(bundle.get("ticker") or "UNKNOWN")
            if ticker == "UNKNOWN":
                continue
            trace_status = str(bundle.get("trace_status") or "unknown")
            traceable = _is_traceable_status(trace_status)
            score = round((1.0 / rank) + (0.5 if traceable else 0.0), 4)
            topic_payload = {
                "topic_id": None,
                "ticker": ticker,
                "period": bundle.get("period"),
                "document_type": bundle.get("document_type"),
                "topic_label": str(bundle.get("type") or "Object evidence"),
                "topic_summary": str(bundle.get("text") or "")[:500],
                "primary_object_id": bundle.get("id"),
                "primary_object_type": bundle.get("type"),
                "source_object_ids": [bundle.get("id")] if bundle.get("id") else [],
                "dominant_object_types": [bundle.get("type")] if bundle.get("type") else [],
                "impact_channels": [],
                "evidence_strength": "object_fallback",
                "support_quote_count": bundle.get("support_quote_count") or 0,
                "support_claim_count": bundle.get("support_claim_count") or 0,
                "trace_status": trace_status,
                "evidence_chain_count": bundle.get("evidence_chain_count") or 0,
                "support_depth": bundle.get("support_depth"),
                "support_link_count": bundle.get("support_link_count") or 0,
                "trace_method": "object_compact_fallback",
                "metric_lineage_status": bundle.get("metric_lineage_status"),
                "answer_candidate": bool(bundle.get("answer_candidate")),
                "specificity_score": None,
                "generic_score": None,
                "boilerplate_score": None,
                "materiality_hint": None,
                "materiality_score": None,
                "topic_type": "object_fallback",
                "topic_family": "object_fallback",
                "factor_terms": [],
                "metric_terms": [],
                "entity_terms": [],
                "mechanism_terms": [],
                "scenario_terms": [],
                "top_traceable_object_ids": [bundle.get("id")]
                if traceable and bundle.get("id")
                else [],
                "untraced_object_ids": []
                if traceable or not bundle.get("id")
                else [bundle.get("id")],
                "match": {
                    "tier": "traceable_related" if traceable else "untraced_related",
                    "semantic_relevance": "object_fallback",
                    "trace_status": trace_status,
                    "score": score,
                    "why_tier": "Rich company-topic matching returned no candidates; compact object search found related answer-candidate evidence.",
                },
            }
            grouped.setdefault(ticker, []).append(topic_payload)

        candidates: list[dict[str, Any]] = []
        results_by_ticker: dict[str, list[dict[str, Any]]] = {}
        for ticker, topics in grouped.items():
            selected = topics[:limit_per_group]
            results_by_ticker[ticker] = selected
            candidates.append(
                {
                    "ticker": ticker,
                    "score": round(
                        sum(
                            float((topic.get("match") or {}).get("score") or 0.0)
                            for topic in selected
                        ),
                        4,
                    ),
                    "tier": str((selected[0].get("match") or {}).get("tier") or "untraced_related"),
                    "matched_topic_count": len(topics),
                    "matched_object_counts": _topic_object_counts(topics),
                    "evidence_counts": _topic_evidence_counts(topics),
                    "trace_status": _ticker_trace_status(selected),
                    "trace_counts": _topic_trace_counts(selected),
                    "top_traceable_object_ids": _traceable_object_ids(selected)[:10],
                    "untraced_object_ids": _untraced_object_ids(selected)[:10],
                    "top_reasons": [_topic_reason(topic) for topic in selected],
                    "top_object_ids": [
                        str(object_id)
                        for topic in selected
                        for object_id in (topic.get("source_object_ids") or [])
                    ][:10],
                    "matched_topics": selected,
                }
            )
        candidates.sort(
            key=lambda item: (
                tier_rank(str(item.get("tier") or "")),
                float(item.get("score") or 0.0),
                int(item.get("matched_topic_count") or 0),
            ),
            reverse=True,
        )
        candidates = candidates[:limit_groups]
        allowed = {str(candidate.get("ticker")) for candidate in candidates}
        return {
            "ticker_candidates": candidates,
            "results_by_ticker": {
                ticker: results_by_ticker[ticker]
                for ticker in sorted(allowed)
                if ticker in results_by_ticker
            },
            "search_diagnostics": {
                "enabled": True,
                "searched_objects": len(bundles),
                "matched_tickers": len(candidates),
                "query_diagnostics": diagnostics,
            },
        }

    def topic_map(
        self,
        *,
        ticker: str,
        document_types: Iterable[str] | None = None,
        periods: Iterable[str] | None = None,
        limit: int = 20,
    ) -> dict[str, Any]:
        """Return a search-vocabulary map for a company from accepted objects.

        This is a retrieval helper, not a new ontology artifact. It repackages
        CompanyBusinessProfile and falls back to BusinessActivity and
        ExternalFactorExposure when a profile is unavailable.
        """
        ticker = ticker.upper()
        limit = max(1, int(limit))
        profiles = self._topic_map_objects(
            ticker=ticker,
            object_types=("CompanyBusinessProfile",),
            document_types=None,
            periods=None,
            limit=3,
        )
        activities = self._topic_map_objects(
            ticker=ticker,
            object_types=("BusinessActivity",),
            document_types=document_types,
            periods=periods,
            limit=limit,
        )
        exposures = self._topic_map_objects(
            ticker=ticker,
            object_types=("ExternalFactorExposure",),
            document_types=document_types,
            periods=periods,
            limit=limit,
        )
        metric_objects = self._topic_map_objects(
            ticker=ticker,
            object_types=("MetricObservation",),
            document_types=document_types,
            periods=periods,
            limit=limit * 2,
        )

        external_factors: dict[str, dict[str, Any]] = {}
        business_activities: dict[str, dict[str, Any]] = {}
        metrics: dict[str, dict[str, Any]] = {}
        projects_assets: dict[str, dict[str, Any]] = {}

        for profile in profiles:
            source_id = profile.get("id")
            for factor in _string_values(profile.get("key_external_factors")):
                _merge_topic_entry(external_factors, factor, source_id=source_id)
            for activity in _string_values(profile.get("primary_business_activities")):
                _merge_topic_entry(business_activities, activity, source_id=source_id)
            for metric in _string_values(profile.get("key_metrics")):
                _merge_topic_entry(metrics, metric, source_id=source_id)
            for value in _explicit_project_asset_values(profile):
                _merge_topic_entry(projects_assets, value, source_id=source_id)

        for activity in activities:
            source_id = activity.get("id")
            term = activity.get("activity_type") or activity.get("name")
            _merge_topic_entry(
                business_activities,
                term,
                source_id=source_id,
                related_metrics=_object_metrics(activity),
                search_terms=[activity.get("name"), activity.get("activity_type")],
            )
            for metric in _object_metrics(activity):
                _merge_topic_entry(metrics, metric, source_id=source_id)

        for exposure in exposures:
            source_id = exposure.get("id")
            term = exposure.get("factor")
            related_channels = _string_values(
                exposure.get("impact_channel")
                or exposure.get("impact_channels")
                or exposure.get("affects")
            )
            _merge_topic_entry(
                external_factors,
                term,
                source_id=source_id,
                related_channels=related_channels,
                search_terms=[exposure.get("factor"), exposure.get("benchmark")],
                evidence_grade=exposure.get("evidence_grade"),
            )
            for channel in related_channels:
                _merge_topic_entry(metrics, channel, source_id=source_id)

        for metric_obj in metric_objects:
            metric = metric_obj.get("metric_name")
            _merge_topic_entry(metrics, metric, source_id=metric_obj.get("id"))

        suggested_first_queries = _suggest_topic_queries(
            external_factors=external_factors,
            business_activities=business_activities,
        )

        return {
            "ticker": ticker,
            "query": {
                "document_types": list(document_types or []),
                "periods": list(periods or []),
                "limit": limit,
            },
            "source": {
                "company_business_profile_ids": [profile.get("id") for profile in profiles],
                "fallback_used": not bool(profiles),
                "business_activity_count": len(activities),
                "external_factor_exposure_count": len(exposures),
            },
            "topics": {
                "external_factors": _topic_entries(external_factors, limit=limit),
                "business_activities": _topic_entries(business_activities, limit=limit),
                "metrics": _topic_entries(metrics, limit=limit),
                "projects_assets": _topic_entries(projects_assets, limit=limit),
            },
            "suggested_first_queries": suggested_first_queries[: min(limit, 10)],
        }

    def trace(self, object_id: str) -> dict[str, Any] | None:
        """Trace an object back to supporting evidence and document metadata."""
        row = self.conn.execute("SELECT * FROM objects WHERE id = ?", (object_id,)).fetchone()
        if not row:
            return None
        obj = _object_from_row(row)
        evidence = self._expand_evidence(obj)
        return {
            "object": obj,
            "document": self._document_for(obj),
            "evidence": evidence,
            "quality": self._quality_for_object(obj),
        }

    def chain(
        self,
        object_id: str,
        *,
        max_depth: int = 2,
        direction: str = "both",
        include_quote_text: bool = False,
    ) -> dict[str, Any] | None:
        """Return a compact evidence and graph chain around one ontology object."""
        obj = self.get_object(object_id)
        if not obj:
            return None

        max_depth = max(0, min(int(max_depth), _CHAIN_MAX_DEPTH))
        direction = _normalize_chain_direction(direction)
        evidence = self._expand_evidence(obj)
        graph_paths = self._edge_paths(
            obj["id"],
            direction=direction,
            max_depth=max_depth,
            include_quote_text=include_quote_text,
        )
        temporal_context = self._temporal_context_for_object(
            obj,
            evidence=evidence,
            graph_paths=graph_paths,
            include_quote_text=include_quote_text,
        )
        quality = self._quality_for_object(obj)
        quality["evidence_grade"] = obj.get("evidence_grade")
        quality["warnings"] = _chain_warnings(obj, evidence, quality)

        return {
            "object": _chain_node(obj, include_quote_text=include_quote_text),
            "document": self._document_for(obj),
            "chain": {
                "max_depth": max_depth,
                "direction": direction,
                "include_quote_text": include_quote_text,
                "evidence_chain": {
                    "claims": [
                        _chain_node(claim, include_quote_text=include_quote_text)
                        for claim in evidence.get("claims", [])
                    ],
                    "quotes": [
                        _chain_node(quote, include_quote_text=include_quote_text)
                        for quote in evidence.get("quotes", [])
                    ],
                    "spans": [
                        _chain_node(span, include_quote_text=include_quote_text)
                        for span in evidence.get("spans", [])
                    ],
                },
                "semantic_neighbors": self._semantic_neighbors_for_chain(
                    obj,
                    evidence=evidence,
                    graph_paths=graph_paths,
                    include_quote_text=include_quote_text,
                ),
                "temporal_context": temporal_context,
                "edge_paths": graph_paths,
            },
            "quality": quality,
        }

    def bundle(self, object_id: str) -> dict[str, Any]:
        """Return a compact evidence bundle for query results."""
        trace = self.trace(object_id)
        if trace is None:
            return {"id": object_id, "missing": True}
        obj = trace["object"]
        return {
            "id": obj["id"],
            "type": obj["type"],
            "ticker": obj.get("ticker"),
            "document_type": obj.get("document_type"),
            "period": obj.get("period"),
            "section": obj.get("section_name") or obj.get("section_key"),
            "text": _display_text(obj),
            "object": obj,
            "evidence": trace["evidence"],
            "quality": trace["quality"],
            "document": trace["document"],
        }

    def _comparison_kernel_envelope(
        self,
        *,
        topic: str | None,
        metric: str | None,
        ticker_list: Sequence[str],
        document_types: Iterable[str] | None,
        periods: Iterable[str] | None,
        limit_per_ticker: int,
        results: Mapping[str, Sequence[Mapping[str, Any]]],
        comparison_contexts: Mapping[str, Mapping[str, Any]],
    ) -> dict[str, Any]:
        request = research_request_from_plan_fields(
            question=topic or metric or "comparison",
            tickers=ticker_list,
            document_types=list(document_types or []),
            periods=list(periods or []),
            universe=None,
            limit_results=limit_per_ticker,
            limit_tickers=len(ticker_list),
            mode="compare",
        )
        has_results = any(bool(items) for items in results.values()) or bool(comparison_contexts)
        kernel = ResearchKernel(self)
        return kernel.build_envelope(
            request,
            research_status="sufficient_for_default_answer"
            if has_results
            else "needs_targeted_followup",
            answer_mode="comparison_research_state",
            research_pack={"comparison_contexts": comparison_contexts},
            answerability={
                "direct_answerable": any(
                    bool((context.get("answerability") or {}).get("direct_answerable"))
                    for context in comparison_contexts.values()
                    if isinstance(context, Mapping)
                ),
                "related_context_available": has_results,
                "negative_answer_supported": False,
                "needs_user_clarification": False,
                "recommended_answer_mode": "comparison_research_state",
            },
            missing_parts=[] if has_results else ["comparison_candidates_not_found"],
            recommended_tools=[],
            agent_autonomy={
                "allowed_next_tools": ["krw_ontology_trace", "krw_ontology_chain"]
                if has_results
                else ["krw_ontology_query"],
                "max_additional_tool_calls": 2 if has_results else 1,
            },
            do_not_call=["raw_fts_winner_by_hit_count", "broad_retrieve"],
        )

    def compare(
        self,
        *,
        tickers: Iterable[str],
        topic: str | None = None,
        metric: str | None = None,
        document_types: Iterable[str] | None = None,
        periods: Iterable[str] | None = None,
        limit_per_ticker: int = 5,
    ) -> dict[str, Any]:
        """Compare companies by topic or canonical metric at query time."""
        ticker_list = [ticker.upper() for ticker in tickers]
        results: dict[str, list[dict[str, Any]]] = {}
        comparison_evaluations: dict[str, dict[str, Any]] = {}
        comparison_contexts: dict[str, dict[str, Any]] = {}
        for ticker in ticker_list:
            if metric:
                rows = self._query_metric(
                    metric,
                    ticker=ticker,
                    document_types=document_types,
                    periods=periods,
                    limit=limit_per_ticker,
                )
                results[ticker] = [self.bundle(row["id"]) for row in rows]
                comparison_evaluations[ticker] = _comparison_evaluation_from_items(
                    results[ticker],
                    metric=metric,
                )
            else:
                context = self.query_context(
                    question=topic or "",
                    ticker=ticker,
                    document_types=document_types,
                    periods=periods,
                    limit_results=limit_per_ticker,
                    limit_tickers=1,
                    include_internal_ids=True,
                )
                comparison_contexts[ticker] = _compact_comparison_context(context)
                comparison_evaluations[ticker] = _comparison_evaluation_from_query_context(context)
                context_bundles = self._bundles_from_object_ids(
                    _object_ids_from_query_context(context),
                    limit=limit_per_ticker,
                )
                query_bundles = self.query(
                    topic=topic,
                    tickers=[ticker],
                    document_types=document_types,
                    periods=periods,
                    limit=limit_per_ticker,
                )
                results[ticker] = _merge_bundle_lists(
                    context_bundles,
                    query_bundles,
                    limit=max(limit_per_ticker * 2, limit_per_ticker + 2),
                )
                _apply_comparison_evaluation(results[ticker], comparison_evaluations[ticker])
        payload = {
            "mode": "metric" if metric else "topic",
            "topic": topic,
            "metric": metric,
            "tickers": ticker_list,
            "results": results,
            "comparison_evaluations": comparison_evaluations,
            "comparison_contexts": comparison_contexts,
        }
        payload["kernel"] = self._comparison_kernel_envelope(
            topic=topic,
            metric=metric,
            ticker_list=ticker_list,
            document_types=document_types,
            periods=periods,
            limit_per_ticker=limit_per_ticker,
            results=results,
            comparison_contexts=comparison_contexts,
        )
        return payload

    def compare_compact(
        self,
        *,
        tickers: Iterable[str],
        topic: str | None = None,
        metric: str | None = None,
        document_types: Iterable[str] | None = None,
        periods: Iterable[str] | None = None,
        limit_per_ticker: int = 5,
    ) -> dict[str, Any]:
        """Compare companies using compact row-level candidates without eager traces."""
        ticker_list = [ticker.upper() for ticker in tickers]
        results: dict[str, list[dict[str, Any]]] = {}
        comparison_evaluations: dict[str, dict[str, Any]] = {}
        comparison_contexts: dict[str, dict[str, Any]] = {}
        available_tickers = self._available_tickers(ticker_list)
        for ticker in ticker_list:
            if ticker not in available_tickers:
                results[ticker] = []
                comparison_evaluations[ticker] = {
                    "direct_answerable": False,
                    "related_context_available": False,
                    "negative_answer_supported": False,
                    "recommended_answer_mode": "not_answerable",
                    "tier": "not_answerable",
                    "trace_status": "untraced",
                    "why_tier": "Ticker is not available in the current ontology index.",
                }
                comparison_contexts[ticker] = {
                    "ticker": ticker,
                    "available": False,
                    "why_tier": "Ticker is not available in the current ontology index.",
                }
        searchable_tickers = [ticker for ticker in ticker_list if ticker in available_tickers]
        if not metric and len(ticker_list) > 1:
            index_signature = (str(self.index_path), self.index_path.stat().st_mtime_ns)
            document_type_key = tuple(document_types or ())
            period_key = tuple(periods or ())

            def compare_one(
                ticker: str,
            ) -> tuple[str, list[dict[str, Any]], dict[str, Any], dict[str, Any]]:
                cache_key = (
                    *index_signature,
                    ticker,
                    topic,
                    document_type_key,
                    period_key,
                    int(limit_per_ticker),
                )
                cached = _compare_ticker_cache_get(cache_key)
                if cached is not None:
                    ticker_results, evaluation, context = cached
                    return ticker, ticker_results, evaluation, context
                worker_started_at = time.perf_counter()
                with OntologyStore(self.index_path) as store:
                    context_started_at = time.perf_counter()
                    context = store.query_context(
                        question=topic or "",
                        ticker=ticker,
                        document_types=document_types,
                        periods=periods,
                        limit_results=limit_per_ticker,
                        limit_tickers=1,
                        include_internal_ids=True,
                    )
                    context_elapsed_ms = int((time.perf_counter() - context_started_at) * 1000)
                    evaluation = _comparison_evaluation_from_query_context(context)
                    bundle_started_at = time.perf_counter()
                    context_bundles = store._compact_bundles_from_object_ids(
                        _object_ids_from_query_context(context),
                        limit=limit_per_ticker,
                    )
                    bundle_elapsed_ms = int((time.perf_counter() - bundle_started_at) * 1000)
                    skipped_query_compact = bool(
                        context_bundles and evaluation.get("direct_answerable")
                    )
                    skip_reason = (
                        "direct_answerable"
                        if context_bundles and evaluation.get("direct_answerable")
                        else None
                    )
                    query_compact_elapsed_ms = 0
                    query_compact_diagnostics: dict[str, Any] = {}
                    if skipped_query_compact:
                        query_bundles = []
                    else:
                        query_compact_started_at = time.perf_counter()
                        query_bundles, query_compact_diagnostics = (
                            store.query_compact_with_diagnostics(
                                topic=topic,
                                tickers=[ticker],
                                document_types=document_types,
                                periods=periods,
                                limit=limit_per_ticker,
                            )
                        )
                        query_compact_elapsed_ms = int(
                            (time.perf_counter() - query_compact_started_at) * 1000
                        )
                    ticker_results = _merge_bundle_lists(
                        context_bundles,
                        query_bundles,
                        limit=max(limit_per_ticker * 2, limit_per_ticker + 2),
                    )
                    _apply_comparison_evaluation(ticker_results, evaluation)
                    timing_payload = {
                        "query_context": context_elapsed_ms,
                        "context_bundle": bundle_elapsed_ms,
                        "query_compact": query_compact_elapsed_ms,
                        "total_worker": int((time.perf_counter() - worker_started_at) * 1000),
                    }
                    if query_compact_diagnostics:
                        timing_payload["query_compact_strategy"] = (
                            query_compact_diagnostics.get("search_strategy")
                            or query_compact_diagnostics.get("fts_strategy")
                            or {}
                        )
                    context_stage_timing = (
                        (context.get("search_diagnostics") or {}).get("timing_ms")
                        if isinstance(context.get("search_diagnostics"), Mapping)
                        else None
                    )
                    if isinstance(context.get("search_diagnostics"), Mapping):
                        timing_payload["query_context_fts_strategy"] = (
                            context.get("search_diagnostics") or {}
                        ).get("fts_strategy")
                        timing_payload["query_context_company_topic_attempts"] = (
                            context.get("search_diagnostics") or {}
                        ).get("company_topic_attempts") or []
                    if isinstance(context_stage_timing, Mapping):
                        timing_payload["query_context_stages"] = dict(context_stage_timing)
                    evaluation["timing_ms"] = timing_payload
                    context_payload = _compact_comparison_context(context)
                    context_payload["timing_ms"] = timing_payload
                    if skipped_query_compact:
                        context_payload["query_compact_skipped"] = True
                        context_payload["query_compact_skip_reason"] = skip_reason
                    _compare_ticker_cache_set(
                        cache_key, (ticker_results, evaluation, context_payload)
                    )
                    return ticker, ticker_results, evaluation, context_payload

            max_workers = min(len(ticker_list), 4)
            with ThreadPoolExecutor(max_workers=max_workers) as executor:
                future_by_ticker = {
                    executor.submit(compare_one, ticker): ticker for ticker in searchable_tickers
                }
                for future in as_completed(future_by_ticker):
                    ticker, ticker_results, evaluation, context = future.result()
                    results[ticker] = ticker_results
                    comparison_evaluations[ticker] = evaluation
                    comparison_contexts[ticker] = context
            results = {ticker: results.get(ticker, []) for ticker in ticker_list}
            comparison_evaluations = {
                ticker: comparison_evaluations.get(ticker, {}) for ticker in ticker_list
            }
            comparison_contexts = {
                ticker: comparison_contexts.get(ticker, {}) for ticker in ticker_list
            }
            payload = {
                "mode": "topic",
                "topic": topic,
                "metric": metric,
                "tickers": ticker_list,
                "results": results,
                "comparison_evaluations": comparison_evaluations,
                "comparison_contexts": comparison_contexts,
                "compact_fast_path": True,
                "parallel_fast_path": True,
            }
            payload["kernel"] = self._comparison_kernel_envelope(
                topic=topic,
                metric=metric,
                ticker_list=ticker_list,
                document_types=document_types,
                periods=periods,
                limit_per_ticker=limit_per_ticker,
                results=results,
                comparison_contexts=comparison_contexts,
            )
            return payload
        for ticker in ticker_list:
            if ticker not in searchable_tickers:
                continue
            if metric:
                rows = self._query_metric(
                    metric,
                    ticker=ticker,
                    document_types=document_types,
                    periods=periods,
                    limit=limit_per_ticker,
                )
                results[ticker] = self._compact_bundles_from_rows(rows)
                comparison_evaluations[ticker] = _comparison_evaluation_from_items(
                    results[ticker],
                    metric=metric,
                )
            else:
                context = self.query_context(
                    question=topic or "",
                    ticker=ticker,
                    document_types=document_types,
                    periods=periods,
                    limit_results=limit_per_ticker,
                    limit_tickers=1,
                    include_internal_ids=True,
                )
                comparison_contexts[ticker] = _compact_comparison_context(context)
                comparison_evaluations[ticker] = _comparison_evaluation_from_query_context(context)
                context_bundles = self._compact_bundles_from_object_ids(
                    _object_ids_from_query_context(context),
                    limit=limit_per_ticker,
                )
                if context_bundles and comparison_evaluations[ticker].get("direct_answerable"):
                    query_bundles = []
                    comparison_contexts[ticker]["query_compact_skipped"] = True
                else:
                    query_bundles, _diagnostics = self.query_compact_with_diagnostics(
                        topic=topic,
                        tickers=[ticker],
                        document_types=document_types,
                        periods=periods,
                        limit=limit_per_ticker,
                    )
                results[ticker] = _merge_bundle_lists(
                    context_bundles,
                    query_bundles,
                    limit=max(limit_per_ticker * 2, limit_per_ticker + 2),
                )
                _apply_comparison_evaluation(results[ticker], comparison_evaluations[ticker])
        payload = {
            "mode": "metric" if metric else "topic",
            "topic": topic,
            "metric": metric,
            "tickers": ticker_list,
            "results": results,
            "comparison_evaluations": comparison_evaluations,
            "comparison_contexts": comparison_contexts,
            "compact_fast_path": True,
        }
        payload["kernel"] = self._comparison_kernel_envelope(
            topic=topic,
            metric=metric,
            ticker_list=ticker_list,
            document_types=document_types,
            periods=periods,
            limit_per_ticker=limit_per_ticker,
            results=results,
            comparison_contexts=comparison_contexts,
        )
        return payload

    def quality(
        self,
        *,
        ticker: str | None = None,
        document_type: str | None = None,
        period: str | None = None,
    ) -> dict[str, Any]:
        where_parts: list[str] = []
        params: list[Any] = []
        if ticker:
            where_parts.append("ticker = ?")
            params.append(ticker.upper())
        if document_type:
            where_parts.append("document_type = ?")
            params.append(document_type)
        if period:
            where_parts.append("period = ?")
            params.append(period)
        where = f"WHERE {' AND '.join(where_parts)}" if where_parts else ""

        documents = [
            _document_from_row(row)
            for row in self.conn.execute(
                f"SELECT * FROM documents {where} ORDER BY ticker, doc_type_key, period",
                params,
            ).fetchall()
        ]
        events = [
            _quality_from_row(row)
            for row in self.conn.execute(
                f"SELECT * FROM quality_events {where} ORDER BY ticker, doc_type_key, period, category",
                params,
            ).fetchall()
        ]
        return {
            "documents": documents,
            "events": events,
            "summary": {
                "documents": len(documents),
                "events": len(events),
                "rejected_objects": sum(
                    1 for event in events if event["category"] == "rejected_object"
                ),
                "batch_failures": sum(
                    1 for event in events if event["category"] == "batch_failure"
                ),
                "section_warnings": sum(
                    1 for event in events if event["category"] == "section_quality"
                ),
            },
        }

    def _query_fts(
        self,
        topic: str,
        *,
        tickers: Iterable[str] | None,
        document_types: Iterable[str] | None,
        periods: Iterable[str] | None,
        object_types: Iterable[str],
        include_rejected: bool,
        limit: int,
    ) -> list[sqlite3.Row]:
        rows, _strategy = self._query_fts_with_strategy(
            topic,
            tickers=tickers,
            document_types=document_types,
            periods=periods,
            object_types=object_types,
            include_rejected=include_rejected,
            limit=limit,
        )
        return rows

    def _query_fts_with_strategy(
        self,
        topic: str,
        *,
        tickers: Iterable[str] | None,
        document_types: Iterable[str] | None,
        periods: Iterable[str] | None,
        object_types: Iterable[str],
        include_rejected: bool,
        limit: int,
    ) -> tuple[list[sqlite3.Row], dict[str, Any]]:
        tickers = list(tickers) if tickers is not None else None
        document_types = list(document_types) if document_types is not None else None
        periods = list(periods) if periods is not None else None
        object_types = tuple(object_types)
        attempts: list[dict[str, Any]] = []
        selected_rows: dict[str, sqlite3.Row] = {}
        expanded_topic = _expanded_topic(topic)
        original_terms = _query_terms(topic)
        expanded_terms = _query_terms(expanded_topic)
        should_relax = expanded_topic != topic or len(original_terms) >= 3

        def run_attempt(mode: str, query_topic: str, *, operator: str) -> None:
            if len(selected_rows) >= limit:
                return
            attempt_started_at = time.perf_counter()
            fts_query = _fts_query(query_topic, operator=operator)
            if not fts_query:
                attempts.append(
                    {
                        "mode": mode,
                        "topic": query_topic,
                        "operator": operator,
                        "fts_query": "",
                        "result_count": 0,
                        "added_count": 0,
                        "elapsed_ms": int((time.perf_counter() - attempt_started_at) * 1000),
                    }
                )
                return
            rows = self._execute_fts(
                fts_query,
                tickers=tickers,
                document_types=document_types,
                periods=periods,
                object_types=object_types,
                include_rejected=include_rejected,
                limit=limit,
            )
            added_count = 0
            for row in rows:
                row_id = row["id"]
                if row_id in selected_rows:
                    continue
                selected_rows[row_id] = row
                added_count += 1
                if len(selected_rows) >= limit:
                    break
            attempts.append(
                {
                    "mode": mode,
                    "topic": query_topic,
                    "operator": operator,
                    "fts_query": fts_query,
                    "result_count": len(rows),
                    "added_count": added_count,
                    "elapsed_ms": int((time.perf_counter() - attempt_started_at) * 1000),
                }
            )

        run_attempt("strict_and", topic, operator="AND")
        if len(selected_rows) < limit and expanded_topic != topic:
            run_attempt("expanded_and", expanded_topic, operator="AND")
        if should_relax and len(selected_rows) < limit:
            split_limit = 12
            if tickers and len(tickers) == 1 and not periods and limit <= 2:
                split_limit = 5
            split_topics = _split_topic_queries(expanded_topic, limit=split_limit)
            should_prescan_split_terms = bool(tickers and periods)
            if should_prescan_split_terms:
                present_terms = self._scoped_present_terms(
                    tickers=tickers,
                    document_types=document_types,
                    periods=periods,
                    object_types=object_types,
                    include_rejected=include_rejected,
                    terms=expanded_terms,
                )
                if present_terms:
                    filtered_split_topics = [
                        split_topic
                        for split_topic in split_topics
                        if all(term in present_terms for term in _query_terms(split_topic))
                    ]
                    if filtered_split_topics:
                        split_topics = filtered_split_topics
            for split_topic in split_topics:
                run_attempt("split_and", split_topic, operator="AND")
                if len(selected_rows) >= limit:
                    break
        if should_relax and len(selected_rows) < limit:
            max_or_terms = 8
            if tickers and periods:
                max_or_terms = 4
            elif tickers and len(tickers) == 1 and limit <= 2:
                max_or_terms = 3
            elif tickers:
                max_or_terms = 6
            relaxed_topic = _limited_or_topic(expanded_topic, max_terms=max_or_terms)
            run_attempt("relaxed_or", relaxed_topic, operator="OR")

        selected_mode = next(
            (attempt["mode"] for attempt in attempts if attempt.get("added_count")),
            None,
        )
        strategy = {
            "original_topic": topic,
            "expanded_topic": expanded_topic,
            "expanded_terms": expanded_terms,
            "selected_mode": selected_mode,
            "attempts": attempts,
        }
        return list(selected_rows.values())[:limit], strategy

    def _execute_fts(
        self,
        fts_query: str,
        *,
        tickers: Iterable[str] | None,
        document_types: Iterable[str] | None,
        periods: Iterable[str] | None,
        object_types: Iterable[str],
        include_rejected: bool,
        limit: int,
    ) -> list[sqlite3.Row]:
        where, params = _object_filters(
            tickers=tickers,
            document_types=document_types,
            periods=periods,
            object_types=object_types,
            include_rejected=include_rejected,
        )
        scoped_query = _scoped_fts_query(
            fts_query,
            tickers=tickers,
            document_types=document_types,
            periods=periods,
            object_types=object_types,
        )

        def run(query: str) -> list[sqlite3.Row]:
            return self.conn.execute(
                f"""
                SELECT objects.*
                FROM object_fts
                JOIN objects ON objects.id = object_fts.object_id
                {where} AND object_fts MATCH ?
                ORDER BY rank
                LIMIT ?
                """,
                [*params, query, limit],
            ).fetchall()

        if scoped_query != fts_query:
            rows = run(scoped_query)
            if rows:
                return rows
        return run(fts_query)

    def _query_company_topics(
        self,
        fts_query: str,
        *,
        tickers: Iterable[str] | None,
        document_types: Iterable[str] | None,
        periods: Iterable[str] | None,
        limit: int,
    ) -> list[sqlite3.Row]:
        where, params = _company_topic_filters(
            tickers=tickers,
            document_types=document_types,
            periods=periods,
        )
        scoped_query = _scoped_fts_query(
            fts_query,
            tickers=tickers,
            document_types=document_types,
            periods=periods,
        )

        def run(query: str) -> list[sqlite3.Row]:
            return self.conn.execute(
                f"""
                SELECT company_topic_index.*
                FROM company_topic_fts
                JOIN company_topic_index
                  ON company_topic_index.topic_id = company_topic_fts.topic_id
                {where} AND company_topic_fts MATCH ?
                ORDER BY
                    rank,
                    CASE company_topic_index.trace_status
                        WHEN 'traceable' THEN 3
                        WHEN 'traceable_metric_lineage' THEN 3
                        WHEN 'related' THEN 2
                        ELSE 1
                    END DESC,
                    COALESCE(company_topic_index.specificity_score, 0) DESC,
                    COALESCE(company_topic_index.generic_score, 0) ASC,
                    COALESCE(company_topic_index.boilerplate_score, 0) ASC,
                    company_topic_index.evidence_chain_count DESC,
                    company_topic_index.support_quote_count + company_topic_index.support_claim_count + company_topic_index.support_metric_count DESC
                LIMIT ?
                """,
                [*params, query, limit],
            ).fetchall()

        if scoped_query != fts_query:
            rows = run(scoped_query)
            if rows:
                return rows
        return run(fts_query)

    def _bundles_from_object_ids(
        self, object_ids: Iterable[str], *, limit: int
    ) -> list[dict[str, Any]]:
        bundles: list[dict[str, Any]] = []
        seen: set[str] = set()
        for object_id in object_ids:
            if not object_id or object_id in seen:
                continue
            seen.add(object_id)
            bundle = self.bundle(object_id)
            if bundle.get("missing"):
                continue
            bundles.append(bundle)
            if len(bundles) >= max(1, int(limit)):
                break
        return bundles

    def _compact_bundles_from_object_ids(
        self, object_ids: Iterable[str], *, limit: int
    ) -> list[dict[str, Any]]:
        ids: list[str] = []
        seen: set[str] = set()
        for object_id in object_ids:
            if not object_id or object_id in seen:
                continue
            seen.add(object_id)
            ids.append(object_id)
            if len(ids) >= max(1, int(limit)):
                break
        if not ids:
            return []
        placeholders = ",".join("?" for _ in ids)
        rows = self.conn.execute(
            f"SELECT * FROM objects WHERE id IN ({placeholders})",
            ids,
        ).fetchall()
        by_id = {row["id"]: row for row in rows}
        ordered_rows = [by_id[object_id] for object_id in ids if object_id in by_id]
        return self._compact_bundles_from_rows(ordered_rows)

    def _query_objects(
        self,
        *,
        tickers: Iterable[str] | None,
        document_types: Iterable[str] | None,
        periods: Iterable[str] | None,
        object_types: Iterable[str],
        include_rejected: bool,
        limit: int,
    ) -> list[sqlite3.Row]:
        where, params = _object_filters(
            tickers=tickers,
            document_types=document_types,
            periods=periods,
            object_types=object_types,
            include_rejected=include_rejected,
        )
        return self.conn.execute(
            f"SELECT * FROM objects {where} ORDER BY ticker, doc_type_key, period, type LIMIT ?",
            [*params, limit],
        ).fetchall()

    def _has_query_scope_objects(
        self,
        *,
        tickers: Iterable[str] | None,
        document_types: Iterable[str] | None,
        periods: Iterable[str] | None,
        object_types: Iterable[str],
        include_rejected: bool,
    ) -> bool:
        where, params = _object_filters(
            tickers=tickers,
            document_types=document_types,
            periods=periods,
            object_types=object_types,
            include_rejected=include_rejected,
        )
        row = self.conn.execute(
            f"SELECT 1 FROM objects {where} LIMIT 1",
            params,
        ).fetchone()
        return row is not None

    def _scoped_present_terms(
        self,
        *,
        tickers: Iterable[str] | None,
        document_types: Iterable[str] | None,
        periods: Iterable[str] | None,
        object_types: Iterable[str],
        include_rejected: bool,
        terms: Sequence[str],
    ) -> set[str]:
        candidate_terms = _unique(
            term for term in terms if term not in _SPLIT_TOPIC_STOP_TERMS and len(term) > 2
        )
        if not candidate_terms:
            return set()
        where, params = _object_filters(
            tickers=tickers,
            document_types=document_types,
            periods=periods,
            object_types=object_types,
            include_rejected=include_rejected,
        )
        present: set[str] = set()
        text_expr = "lower(COALESCE(object_search_text.compact_text, objects.text, ''))"
        for term in candidate_terms:
            row = self.conn.execute(
                f"""
                SELECT 1
                FROM objects
                LEFT JOIN object_search_text
                  ON object_search_text.object_id = objects.id
                {where} AND {text_expr} LIKE ?
                LIMIT 1
                """,
                [*params, f"%{term}%"],
            ).fetchone()
            if row is not None:
                present.add(term)
        return present

    def _compact_bundles_from_rows(self, rows: Sequence[sqlite3.Row]) -> list[dict[str, Any]]:
        if not rows:
            return []
        object_ids = [row["id"] for row in rows if row["id"]]
        trace_by_id = self._traceability_by_id(object_ids)
        bundles: list[dict[str, Any]] = []
        for row in rows:
            obj = _object_from_row(row)
            trace = trace_by_id.get(row["id"], {})
            bundles.append(
                {
                    "id": row["id"],
                    "type": row["type"],
                    "ticker": row["ticker"],
                    "document_type": row["document_type"],
                    "period": row["period"],
                    "section": row["section_name"],
                    "text": row["text"] or _display_text(obj),
                    "object": obj,
                    "evidence": {
                        "claims": [],
                        "quotes": [],
                        "spans": [],
                        "related_objects": [],
                        "metric_lineage": None,
                    },
                    "quality": {
                        "object_status": obj.get("review_status"),
                        "section_quality": None,
                        "evidence_grade": obj.get("evidence_grade"),
                        "events": [],
                    },
                    "document": {},
                    "trace_status": trace.get("trace_status"),
                    "evidence_chain_count": trace.get("evidence_chain_count"),
                    "support_depth": trace.get("support_depth"),
                    "support_quote_count": trace.get("support_quote_count"),
                    "support_claim_count": trace.get("support_claim_count"),
                    "support_link_count": trace.get("support_link_count"),
                    "metric_lineage_status": trace.get("metric_lineage_status"),
                    "answer_candidate": bool(trace.get("answer_candidate")),
                    "compact_only": True,
                    "trace_id": row["id"],
                    "trace_required": False,
                    "trace_policy": {
                        "mode": "lazy",
                        "reason": "query_compact_fast_path_skips_evidence_bundle_expansion",
                    },
                }
            )
        return bundles

    def _traceability_by_id(self, object_ids: Sequence[str]) -> dict[str, dict[str, Any]]:
        ids = [object_id for object_id in object_ids if object_id]
        if not ids:
            return {}
        placeholders = ",".join("?" for _ in ids)
        rows = self.conn.execute(
            f"SELECT * FROM object_traceability WHERE object_id IN ({placeholders})",
            ids,
        ).fetchall()
        return {
            row["object_id"]: {
                "trace_status": row["trace_status"],
                "evidence_chain_count": row["evidence_chain_count"],
                "support_depth": row["support_depth"],
                "support_quote_count": row["support_quote_count"],
                "support_claim_count": row["support_claim_count"],
                "support_link_count": row["support_link_count"],
                "metric_lineage_status": row["metric_lineage_status"],
                "answer_candidate": row["answer_candidate"],
            }
            for row in rows
        }

    def _metric_query_profile(
        self,
        *,
        topic: str | None,
        tickers: Sequence[str] | None,
        periods: Iterable[str] | None,
        object_types: Sequence[str],
        explicit_object_types: bool,
    ) -> dict[str, Any]:
        metric_periods = _metric_lookup_period_filters(topic, periods)
        if not topic or not tickers or not metric_periods:
            return {"enabled": False, "topic": topic, "normalization": {}}
        if not explicit_object_types and not _metric_lookup_topic_is_metric_like(topic):
            return {"enabled": False, "topic": topic, "normalization": {}}
        if not set(object_types).intersection(_METRIC_FAST_PATH_TYPES):
            return {"enabled": False, "topic": topic, "normalization": {}}
        if not _table_exists(self.conn, "metric_lookup"):
            return {
                "enabled": False,
                "topic": topic,
                "normalization": {"warning": "metric_lookup_unavailable"},
            }
        normalized_topic, normalization = _normalize_metric_lookup_topic(topic, metric_periods)
        if not periods and metric_periods:
            normalization["inferred_period_filters"] = list(metric_periods)
        return {
            "enabled": True,
            "topic": normalized_topic,
            "periods": list(metric_periods),
            "normalization": normalization,
        }

    def _query_metric_lookup_with_strategy(
        self,
        topic: str | None,
        *,
        tickers: Sequence[str] | None,
        document_types: Iterable[str] | None,
        periods: Iterable[str] | None,
        object_types: Iterable[str],
        include_rejected: bool,
        limit: int,
        normalization: Mapping[str, Any],
    ) -> tuple[list[sqlite3.Row], dict[str, Any]]:
        terms = _metric_lookup_search_terms(topic)
        ticker_terms = {str(ticker).lower() for ticker in tickers or []}
        dimension_anchors = [
            term for term in _metric_lookup_dimension_anchors(topic) if term not in ticker_terms
        ]
        metric_terms = [term for term in terms if term not in set(dimension_anchors)] or terms
        metric_terms = _metric_lookup_base_metric_terms(metric_terms)
        period_values = _metric_lookup_period_values(periods)
        years = _metric_lookup_years(periods)
        selected_types = _metric_lookup_selected_types(
            object_types, include_xbrl=bool(dimension_anchors)
        )
        dimension_matches = self._resolve_metric_dimension_anchors(
            topic=topic,
            tickers=tickers,
            dimension_anchors=dimension_anchors,
        )
        if (
            dimension_anchors
            and dimension_matches
            and _table_exists(self.conn, "metric_dimension_lookup")
        ):
            return self._query_metric_dimension_lookup_with_strategy(
                topic,
                tickers=tickers,
                document_types=document_types,
                periods=periods,
                object_types=object_types,
                include_rejected=include_rejected,
                limit=limit,
                normalization=normalization,
                metric_terms=metric_terms,
                dimension_anchors=dimension_anchors,
                dimension_matches=dimension_matches,
            )
        where_parts = ["1 = 1"]
        where_params: list[Any] = []
        _add_in_filter(
            where_parts,
            where_params,
            "metric_lookup.ticker",
            [ticker.upper() for ticker in tickers or []],
        )
        _add_in_filter(
            where_parts, where_params, "metric_lookup.document_type", list(document_types or [])
        )
        _add_in_filter(where_parts, where_params, "metric_lookup.object_type", selected_types)
        period_clause_parts: list[str] = []
        period_params: list[Any] = []
        if years:
            placeholders = ",".join("?" for _ in years)
            period_clause_parts.append(f"metric_lookup.fiscal_year IN ({placeholders})")
            period_params.extend(years)
            if period_values:
                placeholders = ",".join("?" for _ in period_values)
                period_clause_parts.append(
                    f"(metric_lookup.fiscal_year IS NULL AND metric_lookup.observation_period IN ({placeholders}))"
                )
                period_params.extend(period_values)
        elif period_values:
            placeholders = ",".join("?" for _ in period_values)
            period_clause_parts.append(f"metric_lookup.observation_period IN ({placeholders})")
            period_params.extend(period_values)
        if period_clause_parts:
            where_parts.append("(" + " OR ".join(period_clause_parts) + ")")
            where_params.extend(period_params)
        if not include_rejected:
            where_parts.append(
                "(objects.review_status IS NULL OR objects.review_status != 'rejected')"
            )
        score_parts: list[str] = []
        score_params: list[Any] = []
        wants_total = _metric_lookup_wants_total(topic)
        if wants_total and not dimension_anchors:
            score_parts.append("CASE WHEN metric_lookup.is_company_total = 1 THEN 120 ELSE 0 END")
        if {"net", "sales"}.issubset(set(metric_terms)):
            for column, weight in (
                ("metric_lookup.metric_alias_text", 45),
                ("metric_lookup.text", 45),
                ("metric_lookup.metric_name", 30),
            ):
                score_parts.append(
                    f"CASE WHEN lower(COALESCE({column}, '')) LIKE ? THEN {weight} ELSE 0 END"
                )
                score_params.append("%net sales%")
        for term in metric_terms[:8]:
            like = f"%{term}%"
            score_parts.append(
                "CASE WHEN lower(COALESCE(metric_lookup.metric_name, '')) LIKE ? THEN 18 ELSE 0 END"
            )
            score_params.append(like)
            score_parts.append(
                "CASE WHEN lower(COALESCE(metric_lookup.metric_alias_text, '')) LIKE ? THEN 8 ELSE 0 END"
            )
            score_params.append(like)
        canonical_candidates = _unique(
            _canonical_metric_name(term) for term in metric_terms if term
        )
        if "sales" in metric_terms or "revenue" in metric_terms:
            canonical_candidates = _unique([*canonical_candidates, "revenue", "net_sales"])
        if canonical_candidates:
            placeholders = ",".join("?" for _ in canonical_candidates)
            score_parts.append(
                f"CASE WHEN metric_lookup.canonical_metric IN ({placeholders}) THEN 40 ELSE 0 END"
            )
            score_params.extend(canonical_candidates)
            lookup_clauses = [f"metric_lookup.canonical_metric IN ({placeholders})"]
            lookup_params: list[Any] = list(canonical_candidates)
            for term in metric_terms[:8]:
                lookup_clauses.append("lower(COALESCE(metric_lookup.metric_alias_text, '')) LIKE ?")
                lookup_params.append(f"%{term}%")
            if lookup_clauses:
                where_parts.append("(" + " OR ".join(lookup_clauses) + ")")
                where_params.extend(lookup_params)
        score_expr = " + ".join(score_parts) if score_parts else "0"
        dimension_score_parts: list[str] = []
        dimension_score_params: list[Any] = []
        for anchor in dimension_anchors[:8]:
            like = f"%{anchor}%"
            for column, weight in (
                ("metric_lookup.product_name", 80),
                ("metric_lookup.segment_name", 80),
                ("metric_lookup.geography_name", 80),
                ("metric_lookup.dimensions_json", 60),
                ("metric_lookup.metric_alias_text", 35),
                ("metric_lookup.metric_name", 25),
                ("metric_lookup.text", 25),
            ):
                dimension_score_parts.append(
                    f"CASE WHEN lower(COALESCE({column}, '')) LIKE ? THEN {weight} ELSE 0 END"
                )
                dimension_score_params.append(like)
        dimension_score_expr = " + ".join(dimension_score_parts) if dimension_score_parts else "0"
        where = "WHERE " + " AND ".join(where_parts)
        rows = self.conn.execute(
            f"""
            SELECT
                objects.*,
                metric_lookup.fiscal_year AS metric_lookup_fiscal_year,
                metric_lookup.canonical_metric AS metric_lookup_canonical_metric,
                metric_lookup.value_text AS metric_lookup_value_text,
                metric_lookup.unit AS metric_lookup_unit,
                metric_lookup.segment_name AS metric_lookup_segment_name,
                metric_lookup.product_name AS metric_lookup_product_name,
                metric_lookup.geography_name AS metric_lookup_geography_name,
                metric_lookup.is_company_total AS metric_lookup_is_company_total,
                ({score_expr}) AS metric_match_score,
                ({dimension_score_expr}) AS dimension_match_score
            FROM metric_lookup
            JOIN objects ON objects.id = metric_lookup.object_id
            {where}
            ORDER BY
                dimension_match_score DESC,
                metric_match_score DESC,
                metric_lookup.is_company_total {"ASC" if dimension_anchors else "DESC"},
                metric_lookup.fiscal_year ASC,
                metric_lookup.observation_period ASC,
                metric_lookup.object_type ASC,
                metric_lookup.object_id ASC
            LIMIT ?
            """,
            [*score_params, *dimension_score_params, *where_params, max(1, min(limit * 8, 200))],
        ).fetchall()
        dimension_metric_not_found = False
        if dimension_anchors:
            dimension_rows = [
                row
                for row in rows
                if row["dimension_match_score"] and row["dimension_match_score"] > 0
            ]
            dimension_metric_not_found = not dimension_rows
            rows = dimension_rows
        rows = _dedupe_metric_lookup_rows(rows, limit=limit)
        strategy = {
            "mode": "metric_lookup",
            "normalized_topic": topic,
            "terms": metric_terms,
            "dimension_anchors": dimension_anchors,
            "dimension_anchor_filter": bool(dimension_anchors),
            "dimension_metric_not_found": dimension_metric_not_found,
            "company_total_role": "denominator_or_support"
            if dimension_anchors
            else ("primary" if wants_total else None),
            "period_values": period_values,
            "period_years": years,
            "object_types": selected_types,
            "topic_normalization": dict(normalization),
            "fallback_used": False,
        }
        return rows, strategy

    def _resolve_metric_dimension_anchors(
        self,
        *,
        topic: str | None,
        tickers: Sequence[str] | None,
        dimension_anchors: Sequence[str],
    ) -> list[dict[str, Any]]:
        if (
            not topic
            or not tickers
            or not dimension_anchors
            or not _table_exists(self.conn, "company_dimension_catalog")
        ):
            return []
        ticker_values = [ticker.upper() for ticker in tickers if ticker]
        if not ticker_values:
            return []
        placeholders = ",".join("?" for _ in ticker_values)
        rows = self.conn.execute(
            f"""
            SELECT ticker, dimension_key, dimension_label, dimension_kind, aliases_text
            FROM company_dimension_catalog
            WHERE ticker IN ({placeholders})
            """,
            ticker_values,
        ).fetchall()
        topic_space = f" {' '.join(str(topic or '').lower().split())} "
        topic_key = _metric_dimension_key(topic)
        anchor_keys = {_metric_dimension_key(anchor) for anchor in dimension_anchors if anchor}
        matches: list[dict[str, Any]] = []
        seen: set[tuple[str, str]] = set()
        for row in rows:
            dimension_key = str(row["dimension_key"] or "")
            label = str(row["dimension_label"] or dimension_key)
            aliases = " ".join(
                part
                for part in (
                    dimension_key.replace("_", " "),
                    label.lower(),
                    str(row["aliases_text"] or "").lower(),
                )
                if part
            )
            key_parts = set(part for part in dimension_key.split("_") if part)
            matched = False
            if dimension_key and dimension_key in topic_key:
                matched = True
            if f" {dimension_key.replace('_', ' ')} " in topic_space:
                matched = True
            if label and f" {label.lower()} " in topic_space:
                matched = True
            if key_parts and key_parts.issubset(anchor_keys):
                matched = True
            if not matched:
                for anchor_key in anchor_keys:
                    if anchor_key and re.search(
                        rf"\b{re.escape(anchor_key.replace('_', ' '))}\b", aliases
                    ):
                        matched = True
                        break
            dedupe_key = (str(row["ticker"]), dimension_key)
            if matched and dimension_key and dedupe_key not in seen:
                seen.add(dedupe_key)
                matches.append(
                    {
                        "ticker": row["ticker"],
                        "dimension_key": dimension_key,
                        "dimension_label": label,
                        "dimension_kind": row["dimension_kind"],
                    }
                )
        return matches

    def _query_metric_dimension_lookup_with_strategy(
        self,
        topic: str | None,
        *,
        tickers: Sequence[str] | None,
        document_types: Iterable[str] | None,
        periods: Iterable[str] | None,
        object_types: Iterable[str],
        include_rejected: bool,
        limit: int,
        normalization: Mapping[str, Any],
        metric_terms: Sequence[str],
        dimension_anchors: Sequence[str],
        dimension_matches: Sequence[Mapping[str, Any]],
    ) -> tuple[list[sqlite3.Row], dict[str, Any]]:
        period_values = _metric_lookup_period_values(periods)
        years = _metric_lookup_years(periods)
        selected_types = _metric_lookup_selected_types(object_types, include_xbrl=True)
        canonical_candidates = _unique(
            _canonical_metric_name(term) for term in metric_terms if term
        )
        if "sales" in metric_terms or "revenue" in metric_terms:
            canonical_candidates = _unique([*canonical_candidates, "revenue", "net_sales"])
        dimension_keys = _unique(
            str(match["dimension_key"]) for match in dimension_matches if match.get("dimension_key")
        )

        def add_period_filters(parts: list[str], params: list[Any], prefix: str) -> None:
            period_clause_parts: list[str] = []
            period_params: list[Any] = []
            if years:
                placeholders = ",".join("?" for _ in years)
                period_clause_parts.append(f"{prefix}.fiscal_year IN ({placeholders})")
                period_params.extend(years)
                if period_values:
                    placeholders = ",".join("?" for _ in period_values)
                    period_clause_parts.append(
                        f"({prefix}.fiscal_year IS NULL AND {prefix}.observation_period IN ({placeholders}))"
                    )
                    period_params.extend(period_values)
            elif period_values:
                placeholders = ",".join("?" for _ in period_values)
                period_clause_parts.append(f"{prefix}.observation_period IN ({placeholders})")
                period_params.extend(period_values)
            if period_clause_parts:
                parts.append("(" + " OR ".join(period_clause_parts) + ")")
                params.extend(period_params)

        def metric_score(prefix: str) -> tuple[str, list[Any], list[str], list[Any]]:
            score_parts: list[str] = []
            score_params: list[Any] = []
            lookup_clauses: list[str] = []
            lookup_params: list[Any] = []
            if {"net", "sales"}.issubset(set(metric_terms)):
                for column, weight in (
                    (f"{prefix}.metric_alias_text", 45),
                    (f"{prefix}.text", 45),
                    (f"{prefix}.metric_name", 30),
                ):
                    score_parts.append(
                        f"CASE WHEN lower(COALESCE({column}, '')) LIKE ? THEN {weight} ELSE 0 END"
                    )
                    score_params.append("%net sales%")
            for term in metric_terms[:8]:
                like = f"%{term}%"
                score_parts.append(
                    f"CASE WHEN lower(COALESCE({prefix}.metric_name, '')) LIKE ? THEN 18 ELSE 0 END"
                )
                score_params.append(like)
                score_parts.append(
                    f"CASE WHEN lower(COALESCE({prefix}.metric_alias_text, '')) LIKE ? THEN 8 ELSE 0 END"
                )
                score_params.append(like)
            if canonical_candidates:
                placeholders = ",".join("?" for _ in canonical_candidates)
                score_parts.append(
                    f"CASE WHEN {prefix}.canonical_metric IN ({placeholders}) THEN 40 ELSE 0 END"
                )
                score_params.extend(canonical_candidates)
                lookup_clauses.append(f"{prefix}.canonical_metric IN ({placeholders})")
                lookup_params.extend(canonical_candidates)
            for term in metric_terms[:8]:
                lookup_clauses.append(f"lower(COALESCE({prefix}.metric_alias_text, '')) LIKE ?")
                lookup_params.append(f"%{term}%")
            return (
                " + ".join(score_parts) if score_parts else "0",
                score_params,
                lookup_clauses,
                lookup_params,
            )

        target_where = ["1 = 1"]
        target_params: list[Any] = []
        _add_in_filter(
            target_where,
            target_params,
            "metric_dimension_lookup.ticker",
            [ticker.upper() for ticker in tickers or []],
        )
        _add_in_filter(
            target_where, target_params, "metric_lookup.document_type", list(document_types or [])
        )
        _add_in_filter(target_where, target_params, "metric_lookup.object_type", selected_types)
        _add_in_filter(
            target_where, target_params, "metric_dimension_lookup.dimension_key", dimension_keys
        )
        add_period_filters(target_where, target_params, "metric_dimension_lookup")
        if not include_rejected:
            target_where.append(
                "(objects.review_status IS NULL OR objects.review_status != 'rejected')"
            )
        score_expr, score_params, lookup_clauses, lookup_params = metric_score("metric_lookup")
        if lookup_clauses:
            target_where.append("(" + " OR ".join(lookup_clauses) + ")")
            target_params.extend(lookup_params)
        rows = self.conn.execute(
            f"""
            SELECT
                objects.*,
                metric_lookup.fiscal_year AS metric_lookup_fiscal_year,
                metric_lookup.canonical_metric AS metric_lookup_canonical_metric,
                metric_lookup.value_text AS metric_lookup_value_text,
                metric_lookup.unit AS metric_lookup_unit,
                metric_lookup.segment_name AS metric_lookup_segment_name,
                metric_lookup.product_name AS metric_lookup_product_name,
                metric_lookup.geography_name AS metric_lookup_geography_name,
                metric_lookup.is_company_total AS metric_lookup_is_company_total,
                metric_dimension_lookup.dimension_key AS metric_dimension_key,
                metric_dimension_lookup.dimension_label AS metric_dimension_label,
                metric_dimension_lookup.dimension_kind AS metric_dimension_kind,
                ({score_expr}) AS metric_match_score,
                100 AS dimension_match_score
            FROM metric_dimension_lookup
            JOIN metric_lookup ON metric_lookup.object_id = metric_dimension_lookup.object_id
            JOIN objects ON objects.id = metric_lookup.object_id
            WHERE {" AND ".join(target_where)}
            ORDER BY
                metric_dimension_lookup.dimension_key ASC,
                metric_match_score DESC,
                metric_dimension_lookup.fiscal_year ASC,
                metric_dimension_lookup.observation_period ASC,
                metric_lookup.object_id ASC
            LIMIT ?
            """,
            [*score_params, *target_params, max(1, min(limit * 8, 240))],
        ).fetchall()

        found_keys = {
            str(row["metric_dimension_key"]) for row in rows if row["metric_dimension_key"]
        }
        missing_dimension_keys = [key for key in dimension_keys if key not in found_keys]
        denominator_rows: list[sqlite3.Row] = []
        denominator_needed = _metric_lookup_needs_denominator(topic)
        if denominator_needed and rows:
            total_where = ["metric_lookup.is_company_total = 1"]
            total_params: list[Any] = []
            _add_in_filter(
                total_where,
                total_params,
                "metric_lookup.ticker",
                [ticker.upper() for ticker in tickers or []],
            )
            _add_in_filter(
                total_where, total_params, "metric_lookup.document_type", list(document_types or [])
            )
            _add_in_filter(total_where, total_params, "metric_lookup.object_type", selected_types)
            add_period_filters(total_where, total_params, "metric_lookup")
            if not include_rejected:
                total_where.append(
                    "(objects.review_status IS NULL OR objects.review_status != 'rejected')"
                )
            total_score_expr, total_score_params, total_lookup_clauses, total_lookup_params = (
                metric_score("metric_lookup")
            )
            if total_lookup_clauses:
                total_where.append("(" + " OR ".join(total_lookup_clauses) + ")")
                total_params.extend(total_lookup_params)
            denominator_rows = self.conn.execute(
                f"""
                SELECT
                    objects.*,
                    metric_lookup.fiscal_year AS metric_lookup_fiscal_year,
                    metric_lookup.canonical_metric AS metric_lookup_canonical_metric,
                    metric_lookup.value_text AS metric_lookup_value_text,
                    metric_lookup.unit AS metric_lookup_unit,
                    metric_lookup.segment_name AS metric_lookup_segment_name,
                    metric_lookup.product_name AS metric_lookup_product_name,
                    metric_lookup.geography_name AS metric_lookup_geography_name,
                    metric_lookup.is_company_total AS metric_lookup_is_company_total,
                    '__company_total__' AS metric_dimension_key,
                    'Company Total' AS metric_dimension_label,
                    'company_total' AS metric_dimension_kind,
                    ({total_score_expr}) AS metric_match_score,
                    0 AS dimension_match_score
                FROM metric_lookup
                JOIN objects ON objects.id = metric_lookup.object_id
                WHERE {" AND ".join(total_where)}
                ORDER BY
                    metric_match_score DESC,
                    metric_lookup.fiscal_year ASC,
                    metric_lookup.observation_period ASC,
                    metric_lookup.object_id ASC
                LIMIT ?
                """,
                [*total_score_params, *total_params, max(1, min(limit * 4, 120))],
            ).fetchall()
        role_by_object_id = {row["id"]: "target_dimension_metric" for row in rows if row["id"]}
        dimension_by_object_id = {
            row["id"]: {
                "dimension_key": row["metric_dimension_key"],
                "dimension_label": row["metric_dimension_label"],
                "dimension_display_label": _metric_dimension_display_label(
                    row["metric_dimension_label"],
                    row["metric_dimension_key"],
                    dimension_anchors,
                ),
                "dimension_kind": row["metric_dimension_kind"],
            }
            for row in rows
            if row["id"]
        }
        for row in denominator_rows:
            if row["id"]:
                role_by_object_id[row["id"]] = "denominator_metric"
                dimension_by_object_id[row["id"]] = {
                    "dimension_key": row["metric_dimension_key"],
                    "dimension_label": row["metric_dimension_label"],
                    "dimension_display_label": "Company total",
                    "dimension_kind": row["metric_dimension_kind"],
                }
        if denominator_rows:
            denominator_limit = max(1, min(len(denominator_rows), max(1, int(limit) // 3)))
            target_limit = max(1, int(limit) - denominator_limit)
            target_rows = _dedupe_metric_lookup_rows(rows, limit=target_limit)
            denominator_rows = _dedupe_metric_lookup_rows(denominator_rows, limit=denominator_limit)
            rows = _dedupe_metric_lookup_rows([*target_rows, *denominator_rows], limit=limit)
        else:
            rows = _dedupe_metric_lookup_rows(rows, limit=limit)
        dimension_metric_not_found = not found_keys
        strategy = {
            "mode": "metric_dimension_lookup",
            "normalized_topic": topic,
            "terms": list(metric_terms),
            "dimension_anchors": list(dimension_anchors),
            "resolved_dimensions": [dict(match) for match in dimension_matches],
            "dimension_keys": dimension_keys,
            "missing_dimension_keys": missing_dimension_keys,
            "dimension_anchor_filter": True,
            "dimension_metric_not_found": dimension_metric_not_found,
            "company_total_role": "denominator_or_support",
            "denominator_needed": denominator_needed,
            "metric_roles_by_object_id": role_by_object_id,
            "metric_dimensions_by_object_id": dimension_by_object_id,
            "period_values": period_values,
            "period_years": years,
            "object_types": selected_types,
            "topic_normalization": dict(normalization),
            "fallback_used": False,
        }
        return rows, strategy

    def _typed_projection_profile(
        self,
        *,
        topic: str | None,
        object_types: Sequence[str],
        explicit_object_types: bool,
    ) -> dict[str, Any]:
        if not topic:
            return {"enabled": False}
        topic_terms = set(_query_terms(topic))
        selected_types = set(object_types)
        for table_name, spec in _TYPED_PROJECTION_SPECS.items():
            projection_types = set(spec["object_types"])
            if not selected_types.intersection(projection_types):
                continue
            if not _table_exists(self.conn, table_name):
                continue
            term_match = topic_terms.intersection(set(spec["terms"]))
            type_is_narrow = explicit_object_types and selected_types.issubset(projection_types)
            if table_name == "factor_lookup" and not type_is_narrow and len(term_match) < 2:
                continue
            if term_match or type_is_narrow:
                return {
                    "enabled": True,
                    "table": table_name,
                    "topic": topic,
                    "terms": sorted(topic_terms),
                    "matched_terms": sorted(term_match),
                    "object_types": sorted(projection_types.intersection(selected_types)),
                    "reason": "explicit_object_type" if type_is_narrow else "topic_terms",
                }
        return {"enabled": False}

    def _query_typed_projection_with_strategy(
        self,
        profile: Mapping[str, Any],
        *,
        tickers: Sequence[str] | None,
        document_types: Iterable[str] | None,
        periods: Iterable[str] | None,
        object_types: Iterable[str],
        include_rejected: bool,
        limit: int,
    ) -> tuple[list[sqlite3.Row], dict[str, Any]]:
        table_name = str(profile["table"])
        spec = _TYPED_PROJECTION_SPECS[table_name]
        selected_types = [
            object_type for object_type in object_types if object_type in set(spec["object_types"])
        ]
        terms = [
            term
            for term in _query_terms(str(profile.get("topic") or ""))
            if term not in _SPLIT_TOPIC_STOP_TERMS and len(term) > 1
        ][:10]
        where_parts = ["1 = 1"]
        where_params: list[Any] = []
        _add_in_filter(
            where_parts,
            where_params,
            f"{table_name}.ticker",
            [ticker.upper() for ticker in tickers or []],
        )
        _add_in_filter(
            where_parts, where_params, f"{table_name}.document_type", list(document_types or [])
        )
        _add_in_filter(where_parts, where_params, f"{table_name}.period", list(periods or []))
        _add_in_filter(where_parts, where_params, f"{table_name}.object_type", selected_types)
        if not include_rejected:
            where_parts.append(
                "(objects.review_status IS NULL OR objects.review_status != 'rejected')"
            )

        text_columns = tuple(spec["text_columns"])
        term_clauses: list[str] = []
        term_params: list[Any] = []
        score_parts: list[str] = []
        score_params: list[Any] = []
        for term in terms:
            per_term = []
            like = f"%{term}%"
            for column in text_columns:
                per_term.append(f"lower(COALESCE({table_name}.{column}, '')) LIKE ?")
                term_params.append(like)
                score_parts.append(
                    f"CASE WHEN lower(COALESCE({table_name}.{column}, '')) LIKE ? THEN 1 ELSE 0 END"
                )
                score_params.append(like)
            if per_term:
                term_clauses.append("(" + " OR ".join(per_term) + ")")
        if term_clauses:
            where_parts.append("(" + " OR ".join(term_clauses) + ")")
            where_params.extend(term_params)
        score_expr = " + ".join(score_parts) if score_parts else "0"
        where = "WHERE " + " AND ".join(where_parts)
        rows = self.conn.execute(
            f"""
            SELECT
                objects.*,
                ({score_expr}) AS projection_match_score
            FROM {table_name}
            JOIN objects ON objects.id = {table_name}.object_id
            {where}
            ORDER BY
                projection_match_score DESC,
                CASE {table_name}.trace_status
                    WHEN 'traceable' THEN 3
                    WHEN 'traceable_metric_lineage' THEN 3
                    WHEN 'related' THEN 2
                    ELSE 1
                END DESC,
                COALESCE({table_name}.specificity_score, 0) DESC,
                COALESCE({table_name}.generic_score, 0) ASC,
                COALESCE({table_name}.boilerplate_score, 0) ASC,
                COALESCE({table_name}.evidence_chain_count, 0) DESC,
                {table_name}.object_id ASC
            LIMIT ?
            """,
            [*score_params, *where_params, max(1, min(limit * 4, 120))],
        ).fetchall()
        rows = _dedupe_object_rows(rows, limit=limit)
        return rows, {
            "mode": "typed_projection",
            "projection_used": table_name,
            "projection_candidate_count": len(rows),
            "projection_sufficient": bool(rows),
            "projection_fallback_reason": None if rows else "no_projection_candidates",
            "terms": terms,
            "matched_terms": list(profile.get("matched_terms") or []),
            "object_types": selected_types,
            "fallback_used": False,
        }

    def _query_metric(
        self,
        metric: str,
        *,
        ticker: str,
        document_types: Iterable[str] | None,
        periods: Iterable[str] | None,
        limit: int,
    ) -> list[sqlite3.Row]:
        return self._query_metrics(
            [metric],
            tickers=[ticker],
            document_types=document_types,
            periods=periods,
            metric_dimensions=None,
            metric_scope="any",
            calculation_window=None,
            comparison_axes=None,
            limit_per_metric=limit,
        )[:limit]

    def _query_metrics(
        self,
        metrics: Iterable[str],
        *,
        tickers: Iterable[str] | None,
        document_types: Iterable[str] | None,
        periods: Iterable[str] | None,
        metric_dimensions: Iterable[str] | None,
        metric_scope: str,
        limit_per_metric: int,
        calculation_window: str | None = None,
        comparison_axes: Iterable[str] | None = None,
    ) -> list[sqlite3.Row]:
        requested = _unique(str(metric).strip() for metric in metrics if str(metric).strip())
        if not requested:
            return []
        normalized_scope = str(metric_scope or "company_total").strip().lower()
        if normalized_scope not in {"company_total", "dimensioned", "any"}:
            raise ValueError(f"unsupported metric_scope: {metric_scope!r}")
        metric_candidates = _unique(
            candidate
            for metric in requested
            for candidate in (metric, _canonical_metric_name(metric))
            if candidate
        )
        clauses = [
            "(lower(COALESCE(metric_lookup.canonical_metric, '')) IN ({metrics}) "
            "OR lower(COALESCE(metric_lookup.metric_name, '')) IN ({metrics}))"
        ]
        params: list[Any] = [
            *[candidate.casefold() for candidate in metric_candidates],
            *[candidate.casefold() for candidate in metric_candidates],
        ]
        normalized_tickers = _unique(
            str(ticker).strip().upper() for ticker in tickers or [] if str(ticker).strip()
        )
        if normalized_tickers:
            clauses.append(
                "metric_lookup.ticker IN (" + ",".join("?" for _ in normalized_tickers) + ")"
            )
            params.extend(normalized_tickers)
        normalized_document_types = _unique(
            str(value).strip().upper() for value in document_types or [] if str(value).strip()
        )
        if normalized_document_types:
            clauses.append(
                "upper(metric_lookup.document_type) IN ("
                + ",".join("?" for _ in normalized_document_types)
                + ")"
            )
            params.extend(normalized_document_types)
        normalized_periods = _unique(
            str(value).strip() for value in periods or [] if str(value).strip()
        )
        if normalized_periods:
            period_clauses = [
                "metric_lookup.observation_period IN ("
                + ",".join("?" for _ in normalized_periods)
                + ")"
            ]
            params.extend(normalized_periods)
            for year, quarter in _planned_metric_period_coordinates(normalized_periods):
                if quarter is None:
                    period_clauses.append(
                        "(metric_lookup.fiscal_year = ? AND metric_lookup.fiscal_quarter IS NULL)"
                    )
                    params.append(year)
                else:
                    period_clauses.append(
                        "(metric_lookup.fiscal_year = ? AND metric_lookup.fiscal_quarter = ?)"
                    )
                    params.extend([year, quarter])
            clauses.append("(" + " OR ".join(period_clauses) + ")")
        if normalized_scope == "company_total":
            clauses.append("metric_lookup.is_company_total = 1")
        elif normalized_scope == "dimensioned":
            clauses.append("metric_lookup.is_company_total = 0")
        dimension_keys = _unique(
            key for value in metric_dimensions or [] if (key := _metric_dimension_key(value))
        )
        for dimension_key in dimension_keys:
            # Dimension identity is exact.  Substring matching here can turn
            # selectors such as ``US`` into matches for ``business`` or
            # ``Russia`` and may consume the bounded result set before the true
            # member is seen.
            clauses.append(
                "EXISTS ("
                "SELECT 1 FROM metric_dimension_lookup AS planned_dimension "
                "WHERE planned_dimension.object_id = metric_lookup.object_id "
                "AND (planned_dimension.dimension_key = ? "
                "OR planned_dimension.member_key = ?)"
                ")"
            )
            params.extend([dimension_key, dimension_key])
        metric_placeholders = ",".join("?" for _ in metric_candidates)
        clauses[0] = clauses[0].format(metrics=metric_placeholders)
        normalized_axes = {
            str(axis).strip().casefold() for axis in comparison_axes or [] if str(axis).strip()
        }
        temporal = str(calculation_window or "").strip() in {
            "period_over_period",
            "year_over_year",
        } and bool(normalized_axes.intersection({"absolute_change", "growth_rate"}))
        observation_depth = max(
            4 if temporal else 2,
            len(normalized_periods),
        )
        params.append(observation_depth)
        rows = self.conn.execute(
            f"""
            WITH filtered AS (
                SELECT
                    metric_lookup.*,
                    lower(COALESCE(
                        metric_lookup.canonical_metric,
                        metric_lookup.metric_name,
                        ''
                    )) AS metric_key,
                    CASE
                        WHEN metric_lookup.value_numeric IS NOT NULL
                            THEN 'n:' || printf('%.17g', metric_lookup.value_numeric)
                        ELSE 't:' || COALESCE(metric_lookup.value_text, '__NULL__')
                    END AS value_identity
                FROM metric_lookup
                WHERE {" AND ".join(clauses)}
            ), ranked AS (
                SELECT
                    filtered.*,
                    DENSE_RANK() OVER (
                        PARTITION BY
                            filtered.ticker,
                            filtered.metric_key,
                            COALESCE(filtered.unit, ''),
                            COALESCE(filtered.dimensions_json, ''),
                            filtered.is_company_total,
                            filtered.observation_period_type
                        ORDER BY
                            COALESCE(filtered.fiscal_year, 0) DESC,
                            COALESCE(filtered.fiscal_quarter, 0) DESC,
                            filtered.observation_period DESC,
                            filtered.observation_context_key DESC
                    ) AS observation_rank,
                    ROW_NUMBER() OVER (
                        PARTITION BY
                            filtered.ticker,
                            filtered.metric_key,
                            COALESCE(filtered.unit, ''),
                            COALESCE(filtered.dimensions_json, ''),
                            filtered.is_company_total,
                            filtered.observation_period_type,
                            filtered.observation_context_key,
                            filtered.value_identity
                        ORDER BY filtered.filing_period DESC, filtered.object_id
                    ) AS duplicate_value_rank
                FROM filtered
            ), context_variants AS (
                SELECT
                    ticker,
                    metric_key,
                    COALESCE(unit, '') AS unit_key,
                    COALESCE(dimensions_json, '') AS dimensions_key,
                    is_company_total,
                    observation_period_type,
                    observation_context_key,
                    COUNT(DISTINCT value_identity) AS conflict_value_count
                FROM filtered
                GROUP BY
                    ticker,
                    metric_key,
                    COALESCE(unit, ''),
                    COALESCE(dimensions_json, ''),
                    is_company_total,
                    observation_period_type,
                    observation_context_key
            )
            SELECT
                objects.*,
                ranked.is_company_total AS planned_metric_is_company_total,
                ranked.dimensions_json AS planned_metric_dimensions_json,
                ranked.canonical_metric AS planned_metric_canonical_metric,
                ranked.unit AS planned_metric_unit,
                ranked.filing_period AS planned_metric_filing_period,
                ranked.observation_period AS planned_metric_observation_period,
                ranked.observation_period_type AS planned_metric_observation_period_type,
                ranked.observation_start_date AS planned_metric_observation_start_date,
                ranked.observation_end_date AS planned_metric_observation_end_date,
                ranked.observation_context_key AS planned_metric_observation_context_key,
                ranked.fiscal_year AS planned_metric_fiscal_year,
                ranked.fiscal_quarter AS planned_metric_fiscal_quarter,
                context_variants.conflict_value_count AS planned_metric_conflict_value_count
            FROM ranked
            JOIN objects ON objects.id = ranked.object_id
            JOIN context_variants
              ON context_variants.ticker = ranked.ticker
             AND context_variants.metric_key = ranked.metric_key
             AND context_variants.unit_key = COALESCE(ranked.unit, '')
             AND context_variants.dimensions_key = COALESCE(ranked.dimensions_json, '')
             AND context_variants.is_company_total = ranked.is_company_total
             AND context_variants.observation_period_type = ranked.observation_period_type
             AND context_variants.observation_context_key = ranked.observation_context_key
            WHERE ranked.observation_rank <= ?
              AND ranked.duplicate_value_rank = 1
              AND (objects.review_status IS NULL OR objects.review_status != 'rejected')
            ORDER BY
                (context_variants.conflict_value_count > 1) DESC,
                ranked.observation_rank,
                ranked.ticker,
                ranked.metric_key,
                ranked.dimensions_json,
                ranked.observation_period DESC,
                ranked.value_identity,
                ranked.object_id
            """,
            params,
        ).fetchall()
        return _select_planned_metric_rows(
            rows,
            limit_per_group=max(1, int(limit_per_metric)),
            calculation_window=str(calculation_window or "").strip() or None,
            comparison_axes=normalized_axes,
            requested_periods=normalized_periods,
        )

    def _topic_map_objects(
        self,
        *,
        ticker: str,
        object_types: Iterable[str],
        document_types: Iterable[str] | None,
        periods: Iterable[str] | None,
        limit: int,
    ) -> list[dict[str, Any]]:
        where, params = _object_filters(
            tickers=[ticker],
            document_types=document_types,
            periods=periods,
            object_types=object_types,
            include_rejected=False,
        )
        rows = self.conn.execute(
            f"""
            SELECT * FROM objects
            {where}
            ORDER BY period DESC, type, id
            LIMIT ?
            """,
            [*params, max(1, int(limit))],
        ).fetchall()
        return [_object_from_row(row) for row in rows]

    def _edge_paths(
        self,
        object_id: str,
        *,
        direction: str,
        max_depth: int,
        include_quote_text: bool,
    ) -> list[dict[str, Any]]:
        if max_depth <= 0:
            return []

        paths: list[dict[str, Any]] = []
        queue: deque[tuple[str, list[dict[str, Any]], set[str]]] = deque()
        queue.append((object_id, [], {object_id}))
        while queue and len(paths) < _CHAIN_MAX_PATHS:
            current_id, current_steps, seen_ids = queue.popleft()
            if len(current_steps) >= max_depth:
                continue
            for edge, neighbor in self._direct_edge_neighbors(current_id, direction=direction):
                neighbor_id = neighbor.get("id")
                if not neighbor_id or neighbor_id in seen_ids:
                    continue
                step = {
                    "direction": edge.pop("_chain_direction"),
                    "edge": _compact_edge(edge),
                    "object": _chain_node(neighbor, include_quote_text=include_quote_text),
                }
                next_steps = [*current_steps, step]
                paths.append({"depth": len(next_steps), "steps": next_steps})
                queue.append((neighbor_id, next_steps, {*seen_ids, neighbor_id}))
                if len(paths) >= _CHAIN_MAX_PATHS:
                    break
        return paths

    def _direct_edge_neighbors(
        self,
        object_id: str,
        *,
        direction: str,
    ) -> list[tuple[dict[str, Any], dict[str, Any]]]:
        rows: list[tuple[dict[str, Any], dict[str, Any]]] = []
        if direction in {"both", "outgoing"}:
            rows.extend(self._edge_neighbor_rows(object_id, outgoing=True))
        if direction in {"both", "incoming"}:
            rows.extend(self._edge_neighbor_rows(object_id, outgoing=False))
        return rows

    def _edge_neighbor_rows(
        self,
        object_id: str,
        *,
        outgoing: bool,
    ) -> list[tuple[dict[str, Any], dict[str, Any]]]:
        from_column, to_column = ("from_id", "to_id") if outgoing else ("to_id", "from_id")
        rows = self.conn.execute(
            f"""
            SELECT edges.*, objects.json AS object_json, objects.review_status AS object_review_status
            FROM edges
            JOIN objects ON objects.id = edges.{to_column}
            WHERE edges.{from_column} = ?
              AND (objects.review_status IS NULL OR objects.review_status != 'rejected')
            ORDER BY edges.relation_id, objects.type, objects.id
            LIMIT 20
            """,
            (object_id,),
        ).fetchall()
        neighbors: list[tuple[dict[str, Any], dict[str, Any]]] = []
        for row in rows:
            edge = _edge_from_row(row)
            edge["_chain_direction"] = "outgoing" if outgoing else "incoming"
            neighbor = json.loads(row["object_json"])
            if row["object_review_status"]:
                neighbor["review_status"] = row["object_review_status"]
            neighbors.append((edge, neighbor))
        return neighbors

    def _semantic_neighbors_for_chain(
        self,
        obj: dict[str, Any],
        *,
        evidence: dict[str, list[dict[str, Any]]],
        graph_paths: list[dict[str, Any]],
        include_quote_text: bool,
    ) -> list[dict[str, Any]]:
        neighbors: dict[str, dict[str, Any]] = {}

        def add_neighbor(candidate: dict[str, Any], *, via: dict[str, Any]) -> None:
            candidate_id = candidate.get("id")
            if (
                not candidate_id
                or candidate_id == obj.get("id")
                or candidate.get("type") not in _SEMANTIC_NEIGHBOR_TYPES
            ):
                return
            entry = neighbors.setdefault(
                candidate_id,
                {
                    "object": _chain_node(candidate, include_quote_text=include_quote_text),
                    "via": [],
                },
            )
            entry["via"].append(via)

        for related in evidence.get("related_objects", []):
            add_neighbor(related, via={"source": "shared_claim"})

        for path in graph_paths:
            for step in path.get("steps", []):
                candidate = step.get("object") or {}
                add_neighbor(
                    candidate,
                    via={
                        "source": "edge",
                        "depth": path.get("depth"),
                        "direction": step.get("direction"),
                        "relation_id": (step.get("edge") or {}).get("relation_id"),
                    },
                )

        return list(neighbors.values())[:20]

    def _temporal_context_for_object(
        self,
        obj: dict[str, Any],
        *,
        evidence: dict[str, list[dict[str, Any]]],
        graph_paths: list[dict[str, Any]],
        include_quote_text: bool,
    ) -> list[dict[str, Any]]:
        object_ids = {obj.get("id")}
        object_ids.update(
            claim.get("id") for claim in evidence.get("claims", []) if claim.get("id")
        )
        object_ids.update(
            quote.get("id") for quote in evidence.get("quotes", []) if quote.get("id")
        )
        for path in graph_paths:
            for step in path.get("steps", []):
                step_object = step.get("object") or {}
                if step_object.get("id"):
                    object_ids.add(step_object["id"])

        rows = self.conn.execute(
            """
            SELECT *
            FROM objects
            WHERE ticker = ?
              AND type IN ('TemporalLink', 'TrendObservation', 'ChangeEvent')
              AND (review_status IS NULL OR review_status != 'rejected')
            ORDER BY period DESC, type, id
            LIMIT 200
            """,
            (obj.get("ticker"),),
        ).fetchall()
        temporal: list[dict[str, Any]] = []
        for row in rows:
            candidate = _object_from_row(row)
            if candidate.get("id") == obj.get("id") or _temporal_references_object(
                candidate, object_ids
            ):
                temporal.append(_chain_node(candidate, include_quote_text=include_quote_text))
            if len(temporal) >= _CHAIN_MAX_TEMPORAL_CONTEXT:
                break
        return temporal

    def _expand_evidence(self, obj: dict[str, Any]) -> dict[str, list[dict[str, Any]]]:
        claims: list[dict[str, Any]] = []
        quotes: list[dict[str, Any]] = []
        spans: list[dict[str, Any]] = []

        obj_type = obj.get("type")
        if obj_type == "ResearchClaim":
            claims = [obj]
            quotes = _dedupe_objects(
                [
                    *self._objects_by_ids(obj.get("supported_by_quotes") or []),
                    *self._support_objects_for(obj["id"], support_types={"EvidenceQuote"}),
                ]
            )
        elif obj_type == "EvidenceQuote":
            quotes = [obj]
            claims = self._claims_supported_by_quote(obj["id"])
        elif obj_type in {
            "BusinessFactor",
            "AgreementTerm",
            "BusinessEvent",
            "BusinessActivity",
            "ExternalFactorExposure",
        }:
            claims = _dedupe_objects(
                [
                    *self._objects_by_ids(obj.get("supported_by_claims") or []),
                    *self._support_objects_for(obj["id"], support_types={"ResearchClaim"}),
                ]
            )
            quote_ids = _unique(
                quote_id for claim in claims for quote_id in claim.get("supported_by_quotes") or []
            )
            quote_ids = _unique([*quote_ids, *(obj.get("supported_by_quotes") or [])])
            quotes = _dedupe_objects(
                [
                    *self._objects_by_ids(quote_ids),
                    *self._support_objects_for(obj["id"], support_types={"EvidenceQuote"}),
                ]
            )
        elif obj_type == "AssumptionCandidate":
            quotes = _dedupe_objects(
                [
                    *self._objects_by_ids(obj.get("supported_by_quotes") or []),
                    *self._support_objects_for(obj["id"], support_types={"EvidenceQuote"}),
                ]
            )
            claims = _dedupe_objects(
                [
                    *self._objects_by_ids(obj.get("supported_by_claims") or []),
                    *self._support_objects_for(obj["id"], support_types={"ResearchClaim"}),
                ]
            )
        elif obj_type == "ChangeEvent":
            quotes = _dedupe_objects(
                [
                    *self._objects_by_ids(obj.get("supported_by_quotes") or []),
                    *self._support_objects_for(obj["id"], support_types={"EvidenceQuote"}),
                ]
            )
            claims = _dedupe_objects(
                [
                    *self._objects_by_ids(obj.get("supported_by_claims") or []),
                    *self._support_objects_for(obj["id"], support_types={"ResearchClaim"}),
                ]
            )
        elif obj_type == "CompanyBusinessProfile":
            source_objects = self._objects_by_ids(obj.get("source_object_ids") or [])
            claims = self._claims_from_source_objects(source_objects)
            quote_ids = _unique(
                [
                    *[
                        quote_id
                        for claim in claims
                        for quote_id in claim.get("supported_by_quotes") or []
                    ],
                    *[
                        quote_id
                        for source in source_objects
                        for quote_id in source.get("supported_by_quotes") or []
                    ],
                ]
            )
            quotes = self._objects_by_ids(quote_ids)
        elif obj_type == "TrendObservation":
            source_objects = self._objects_by_ids(obj.get("supported_by_objects") or [])
            claims = self._claims_from_source_objects(source_objects)
            quote_ids = _unique(
                quote_id for claim in claims for quote_id in claim.get("supported_by_quotes") or []
            )
            quotes = self._objects_by_ids(quote_ids)

        span_ids = _unique(
            quote.get("source_span_id") for quote in quotes if quote.get("source_span_id")
        )
        spans = self._objects_by_ids(span_ids)
        related_objects = self._objects_sharing_claims(obj, claims)
        evidence: dict[str, Any] = {
            "claims": [_compact_object(claim) for claim in claims],
            "quotes": [_compact_object(quote) for quote in quotes],
            "spans": [_compact_object(span) for span in spans],
            "related_objects": [_compact_object(related) for related in related_objects],
        }
        if obj_type == "MetricObservation":
            evidence["metric_lineage"] = self._metric_lineage_for(obj)
        return evidence

    def _metric_lineage_for(self, metric: dict[str, Any]) -> dict[str, Any]:
        calculation = self._metric_calculation(metric)
        input_metrics: list[dict[str, Any]] = []
        if calculation:
            input_metrics.extend(
                self._objects_by_ids(
                    [
                        *list(calculation.get("input_metric_ids") or []),
                        *list(calculation.get("source_metric_ids") or []),
                    ]
                )
            )
        input_metrics.extend(self._objects_by_ids(metric.get("source_metric_ids") or []))
        input_metrics = _dedupe_objects(input_metrics)

        fact_ids = list(metric.get("source_fact_ids") or [])
        for input_metric in input_metrics:
            fact_ids.extend(input_metric.get("source_fact_ids") or [])
        xbrl_facts = self._objects_by_ids(_unique(fact_ids))
        source_document_ids = _unique(
            fact.get("source_document_id") for fact in xbrl_facts if fact.get("source_document_id")
        )
        return {
            "trace_type": "metric_lineage",
            "formatted_value": format_metric_compact(metric),
            "calculation": _compact_object(calculation) if calculation else None,
            "input_metrics": [_compact_object(input_metric) for input_metric in input_metrics],
            "xbrl_facts": [_compact_object(fact) for fact in xbrl_facts],
            "source_document_ids": source_document_ids,
        }

    def _metric_calculation(self, metric: dict[str, Any]) -> dict[str, Any] | None:
        calculation_id = metric.get("calculation_id")
        if calculation_id:
            calculations = self._objects_by_ids([calculation_id])
            if calculations:
                return calculations[0]
        rows = self.conn.execute(
            """
            SELECT *
            FROM objects
            WHERE type = 'Calculation'
              AND json_extract(json, '$.output_metric_id') = ?
              AND (review_status IS NULL OR review_status != 'rejected')
            ORDER BY id
            LIMIT 1
            """,
            (metric.get("id"),),
        ).fetchall()
        return _object_from_row(rows[0]) if rows else None

    def _objects_by_ids(self, object_ids: Iterable[str]) -> list[dict[str, Any]]:
        ids = [object_id for object_id in object_ids if object_id]
        if not ids:
            return []
        placeholders = ",".join("?" for _ in ids)
        rows = self.conn.execute(
            f"SELECT * FROM objects WHERE id IN ({placeholders})",
            ids,
        ).fetchall()
        by_id = {row["id"]: _object_from_row(row) for row in rows}
        return [by_id[object_id] for object_id in ids if object_id in by_id]

    def _claims_supported_by_quote(self, quote_id: str) -> list[dict[str, Any]]:
        rows = self.conn.execute(
            """
            SELECT objects.*
            FROM edges
            JOIN objects ON objects.id = edges.to_id
            WHERE edges.from_id = ? AND edges.relation_id = 'supports'
            ORDER BY objects.id
            """,
            (quote_id,),
        ).fetchall()
        return _dedupe_objects(
            [
                *[_object_from_row(row) for row in rows],
                *self._support_targets_for(quote_id, target_types={"ResearchClaim"}),
            ]
        )

    def _support_objects_for(
        self,
        object_id: str,
        *,
        support_types: set[str],
    ) -> list[dict[str, Any]]:
        rows = self.conn.execute(
            """
            SELECT support.*
            FROM objects AS links
            JOIN objects AS support
              ON support.id = COALESCE(
                    json_extract(links.json, '$.support_object_id'),
                    json_extract(links.json, '$.from_id')
                 )
            WHERE links.type = 'SupportLink'
              AND (
                    json_extract(links.json, '$.target_object_id') = ?
                 OR json_extract(links.json, '$.to_id') = ?
              )
              AND (support.review_status IS NULL OR support.review_status != 'rejected')
            ORDER BY support.type, support.id
            """,
            (object_id, object_id),
        ).fetchall()
        return [
            obj
            for obj in (_object_from_row(row) for row in rows)
            if obj.get("type") in support_types
        ]

    def _support_targets_for(
        self,
        support_id: str,
        *,
        target_types: set[str],
    ) -> list[dict[str, Any]]:
        rows = self.conn.execute(
            """
            SELECT target.*
            FROM objects AS links
            JOIN objects AS target
              ON target.id = COALESCE(
                    json_extract(links.json, '$.target_object_id'),
                    json_extract(links.json, '$.to_id')
                 )
            WHERE links.type = 'SupportLink'
              AND (
                    json_extract(links.json, '$.support_object_id') = ?
                 OR json_extract(links.json, '$.from_id') = ?
              )
              AND (target.review_status IS NULL OR target.review_status != 'rejected')
            ORDER BY target.type, target.id
            """,
            (support_id, support_id),
        ).fetchall()
        return [
            obj
            for obj in (_object_from_row(row) for row in rows)
            if obj.get("type") in target_types
        ]

    def _claims_from_source_objects(
        self, source_objects: list[dict[str, Any]]
    ) -> list[dict[str, Any]]:
        claim_ids = _unique(
            [
                *[
                    obj["id"]
                    for obj in source_objects
                    if obj.get("type") == "ResearchClaim" and obj.get("id")
                ],
                *[
                    claim_id
                    for obj in source_objects
                    for claim_id in obj.get("supported_by_claims") or []
                ],
            ]
        )
        return self._objects_by_ids(claim_ids)

    def _objects_sharing_claims(
        self,
        obj: dict[str, Any],
        claims: list[dict[str, Any]],
        *,
        limit: int = 12,
    ) -> list[dict[str, Any]]:
        claim_ids = set(obj.get("supported_by_claims") or [])
        if obj.get("type") == "ResearchClaim" and obj.get("id"):
            claim_ids.add(obj["id"])
        claim_ids.update(claim["id"] for claim in claims if claim.get("id"))
        if not claim_ids:
            return []

        rows = self.conn.execute(
            """
            SELECT * FROM objects
            WHERE ticker = ?
              AND document_type = ?
              AND period = ?
              AND type IN (
                'BusinessFactor',
                'AgreementTerm',
                'BusinessEvent',
                'BusinessActivity',
                'ExternalFactorExposure',
                'ChangeEvent'
              )
            ORDER BY type, id
            """,
            (obj.get("ticker"), obj.get("document_type"), obj.get("period")),
        ).fetchall()
        related: list[dict[str, Any]] = []
        for row in rows:
            candidate = _object_from_row(row)
            if candidate.get("id") == obj.get("id"):
                continue
            if claim_ids.intersection(candidate.get("supported_by_claims") or []):
                related.append(candidate)
            if len(related) >= limit:
                break
        return related

    def _document_for(self, obj: dict[str, Any]) -> dict[str, Any] | None:
        row = self.conn.execute(
            """
            SELECT * FROM documents
            WHERE ticker = ? AND document_type = ? AND period = ?
            """,
            (obj.get("ticker"), obj.get("document_type"), obj.get("period")),
        ).fetchone()
        return _document_from_row(row) if row else None

    def _quality_for_object(self, obj: dict[str, Any]) -> dict[str, Any]:
        document = self._document_for(obj) or {}
        rows = self.conn.execute(
            """
            SELECT * FROM quality_events
            WHERE object_id = ?
               OR (
                    ticker = ?
                AND document_type = ?
                AND period = ?
                AND category IN ('section_quality', 'batch_failure')
               )
            ORDER BY category
            """,
            (obj["id"], obj.get("ticker"), obj.get("document_type"), obj.get("period")),
        ).fetchall()
        return {
            "object_status": obj.get("review_status") or "unknown",
            "section_quality": document.get("section_quality_status"),
            "events": [_quality_from_row(row) for row in rows],
        }


def _metadata_json(conn: sqlite3.Connection, key: str) -> dict[str, Any]:
    if not _table_exists(conn, "metadata"):
        return {}
    row = conn.execute("SELECT value FROM metadata WHERE key = ?", (key,)).fetchone()
    if not row:
        return {}
    try:
        value = json.loads(row["value"])
    except (TypeError, json.JSONDecodeError):
        return {}
    return value if isinstance(value, dict) else {}


def _table_exists(conn: sqlite3.Connection, table_name: str) -> bool:
    row = conn.execute(
        """
        SELECT 1
        FROM sqlite_master
        WHERE type IN ('table', 'view')
          AND name = ?
        """,
        (table_name,),
    ).fetchone()
    return row is not None


def _table_count(conn: sqlite3.Connection, table_name: str) -> int:
    if not _table_exists(conn, table_name):
        return 0
    return int(conn.execute(f"SELECT COUNT(*) FROM {table_name}").fetchone()[0])


def _count_by(conn: sqlite3.Connection, table_name: str, column_name: str) -> dict[str, int]:
    if not _table_exists(conn, table_name):
        return {}
    rows = conn.execute(
        f"""
        SELECT {column_name} AS key, COUNT(*) AS value
        FROM {table_name}
        GROUP BY {column_name}
        ORDER BY value DESC, key
        """
    ).fetchall()
    return {str(row["key"] or "unknown"): int(row["value"] or 0) for row in rows}


def _company_topic_payload(
    topic: Mapping[str, Any], *, include_internal_ids: bool
) -> dict[str, Any]:
    payload = {
        "ticker": topic.get("ticker"),
        "period": topic.get("period"),
        "document_type": topic.get("document_type"),
        "topic_label": topic.get("topic_label"),
        "topic_summary": topic.get("topic_summary"),
        "topic_type": topic.get("topic_type"),
        "topic_family": topic.get("topic_family"),
        "evidence_strength": topic.get("evidence_strength"),
        "trace_status": topic.get("trace_status"),
        "evidence_chain_count": topic.get("evidence_chain_count") or 0,
        "support_quote_count": topic.get("support_quote_count") or 0,
        "support_claim_count": topic.get("support_claim_count") or 0,
        "support_metric_count": topic.get("support_metric_count") or 0,
        "impact_channels": topic.get("impact_channels") or [],
        "factor_terms": topic.get("factor_terms") or [],
        "metric_terms": topic.get("metric_terms") or [],
        "entity_terms": topic.get("entity_terms") or [],
        "mechanism_terms": topic.get("mechanism_terms") or [],
        "scenario_terms": topic.get("scenario_terms") or [],
        "specificity_score": topic.get("specificity_score"),
        "generic_score": topic.get("generic_score"),
        "boilerplate_score": topic.get("boilerplate_score"),
        "materiality_hint": topic.get("materiality_hint"),
        "materiality_score": topic.get("materiality_score"),
    }
    if include_internal_ids:
        payload.update(
            {
                "topic_id": topic.get("topic_id"),
                "primary_object_id": topic.get("primary_object_id"),
                "primary_object_type": topic.get("primary_object_type"),
                "source_object_ids": topic.get("source_object_ids") or [],
                "top_traceable_object_ids": topic.get("top_traceable_object_ids") or [],
                "untraced_object_ids": topic.get("untraced_object_ids") or [],
                "internal_only": {
                    "topic_id": True,
                    "primary_object_id": True,
                    "source_object_ids": True,
                    "top_traceable_object_ids": True,
                    "untraced_object_ids": True,
                },
            }
        )
    return payload


def _answerability_from_candidates(
    query_frame: Mapping[str, Any], candidates: Sequence[Mapping[str, Any]]
) -> dict[str, Any]:
    tiers = _collect_values(candidates, "tier")
    has_direct = any(
        str(tier) in {"traceable_direct", "traceable_metric_lineage"} for tier in tiers
    )
    has_related = any(
        str(tier)
        in {
            "traceable_related",
            "untraced_related",
            "broad_related_candidate",
            "untraced_direct_candidate",
            "related",
        }
        for tier in tiers
    )
    requires_direct = bool(
        query_frame.get("question_requires_direct_match")
        or query_frame.get("requires_direct_match")
    )
    needs_clarification = not candidates
    return {
        "direct_answerable": has_direct,
        "related_context_available": has_related,
        "negative_answer_supported": bool(requires_direct and not has_direct and has_related),
        "needs_user_clarification": needs_clarification,
        "recommended_answer_mode": (
            "direct_answer"
            if has_direct
            else "no_direct_evidence_with_related_context"
            if requires_direct and has_related
            else "related_context_only"
            if has_related
            else "not_answerable"
        ),
    }


def _compact_comparison_context(context: Mapping[str, Any]) -> dict[str, Any]:
    candidates = list(context.get("ticker_candidates") or [])
    research_pack = context.get("research_pack") or {}
    metric_pack = (
        research_pack.get("metric_series_pack") if isinstance(research_pack, Mapping) else None
    )
    projection_pack = (
        research_pack.get("projection_pack") if isinstance(research_pack, Mapping) else None
    )
    chain_pack = research_pack.get("chain_pack") if isinstance(research_pack, Mapping) else None
    directness_guard = (
        research_pack.get("directness_guard") if isinstance(research_pack, Mapping) else None
    )
    metric_calculations = (
        metric_pack.get("calculations") if isinstance(metric_pack, Mapping) else {}
    )
    metric_quality = metric_pack.get("quality") if isinstance(metric_pack, Mapping) else {}
    return {
        "answerability": context.get("answerability") or {},
        "query_frame": context.get("query_frame") or {},
        "top_candidate": _comparison_candidate_summary(candidates[0]) if candidates else None,
        "recommended_tools": list(context.get("recommended_tools") or [])[:3],
        "research_status": context.get("research_status"),
        "agent_autonomy": context.get("agent_autonomy") or {},
        "missing_parts": context.get("missing_parts") or [],
        "do_not_call": context.get("do_not_call") or [],
        "directness_guard": directness_guard or {},
        "research_pack_summary": {
            "metric_mode": metric_pack.get("mode") if isinstance(metric_pack, Mapping) else None,
            "metric_result_count": metric_pack.get("result_count")
            if isinstance(metric_pack, Mapping)
            else 0,
            "metric_roles": metric_pack.get("roles") if isinstance(metric_pack, Mapping) else [],
            "metric_series_count": len(metric_pack.get("series") or [])
            if isinstance(metric_pack, Mapping)
            else 0,
            "metric_share_of_total_count": len(metric_calculations.get("share_of_total") or [])
            if isinstance(metric_calculations, Mapping)
            else 0,
            "metric_growth_rate_count": len(metric_calculations.get("growth_rate") or [])
            if isinstance(metric_calculations, Mapping)
            else 0,
            "metric_growth_difference_count": len(
                metric_calculations.get("growth_difference") or []
            )
            if isinstance(metric_calculations, Mapping)
            else 0,
            "metric_period_alignment": metric_quality.get("period_alignment")
            if isinstance(metric_quality, Mapping)
            else None,
            "metric_unit_consistency": metric_quality.get("unit_consistency")
            if isinstance(metric_quality, Mapping)
            else None,
            "projection_mode": projection_pack.get("mode")
            if isinstance(projection_pack, Mapping)
            else None,
            "projection_result_count": projection_pack.get("result_count")
            if isinstance(projection_pack, Mapping)
            else 0,
            "chain_preview_count": len(chain_pack.get("primary_chains") or [])
            if isinstance(chain_pack, Mapping)
            else 0,
        },
    }


def _comparison_candidate_summary(candidate: Mapping[str, Any]) -> dict[str, Any]:
    match = candidate.get("match") or {}
    return {
        "ticker": candidate.get("ticker"),
        "topic_id": candidate.get("topic_id"),
        "topic_label": candidate.get("topic_label"),
        "primary_object_id": candidate.get("primary_object_id"),
        "primary_object_type": candidate.get("primary_object_type"),
        "tier": candidate.get("tier") or match.get("tier"),
        "semantic_relevance": candidate.get("semantic_relevance")
        or match.get("semantic_relevance"),
        "trace_status": candidate.get("trace_status") or match.get("trace_status"),
        "matched_required_facets": candidate.get("matched_required_facets")
        or match.get("matched_required_facets")
        or [],
        "missing_required_facets": candidate.get("missing_required_facets")
        or match.get("missing_required_facets")
        or [],
        "why_tier": candidate.get("why_tier") or match.get("why_tier"),
    }


def _comparison_evaluation_from_query_context(context: Mapping[str, Any]) -> dict[str, Any]:
    answerability = context.get("answerability") or {}
    candidates = list(context.get("ticker_candidates") or [])
    top = candidates[0] if candidates else {}
    match = top.get("match") if isinstance(top, Mapping) else {}
    if not isinstance(match, Mapping):
        match = {}
    tier = top.get("tier") or match.get("tier")
    semantic_relevance = top.get("semantic_relevance") or match.get("semantic_relevance")
    trace_status = top.get("trace_status") or match.get("trace_status")
    matched_required = (
        top.get("matched_required_facets") or match.get("matched_required_facets") or []
    )
    missing_required = (
        top.get("missing_required_facets") or match.get("missing_required_facets") or []
    )
    why_tier = top.get("why_tier") or match.get("why_tier")
    return {
        "direct_answerable": bool(answerability.get("direct_answerable")),
        "related_context_available": bool(answerability.get("related_context_available")),
        "negative_answer_supported": bool(answerability.get("negative_answer_supported")),
        "recommended_answer_mode": answerability.get("recommended_answer_mode"),
        "semantic_relevance": semantic_relevance,
        "trace_status": trace_status,
        "tier": tier,
        "matched_required_facets": list(matched_required),
        "missing_required_facets": list(missing_required),
        "why_tier": why_tier,
        "evidence_chain_count": top.get("evidence_chain_count") or 0,
        "support_depth": top.get("support_depth"),
        "support_quote_count": top.get("support_quote_count") or 0,
        "support_claim_count": top.get("support_claim_count") or 0,
        "source_object_ids": _object_ids_from_query_context(context),
        "top_traceable_object_ids": top.get("top_traceable_object_ids") or [],
    }


def _object_ids_from_query_context(context: Mapping[str, Any]) -> list[str]:
    candidates = list(context.get("ticker_candidates") or [])
    ids: list[str] = []
    for candidate in candidates:
        for key in (
            "top_traceable_object_ids",
            "top_object_ids",
            "source_object_ids",
            "primary_object_id",
        ):
            value = candidate.get(key)
            if isinstance(value, list):
                ids.extend(str(item) for item in value if item)
            elif value:
                ids.append(str(value))
        for topic in candidate.get("matched_topics") or []:
            for key in ("top_traceable_object_ids", "source_object_ids", "primary_object_id"):
                value = topic.get(key)
                if isinstance(value, list):
                    ids.extend(str(item) for item in value if item)
                elif value:
                    ids.append(str(value))
        for reason in candidate.get("top_reasons") or []:
            for key in ("source_object_ids", "object_id"):
                value = reason.get(key)
                if isinstance(value, list):
                    ids.extend(str(item) for item in value if item)
                elif value:
                    ids.append(str(value))
    deduped: list[str] = []
    seen: set[str] = set()
    for object_id in ids:
        if object_id and object_id not in seen:
            seen.add(object_id)
            deduped.append(object_id)
    return deduped


def _research_context_valuation_guard(question: str) -> dict[str, Any] | None:
    text = str(question or "").lower()
    if not any(
        term in text
        for term in (
            "target price",
            "price target",
            "12-month target",
            "12 month target",
            "fair value",
            "valuation",
            "목표가",
            "목표 주가",
            "목표치",
            "12개월 목표",
            "적정가치",
            "밸류에이션",
        )
    ):
        return None
    return {
        "question_type": "valuation_or_price_target",
        "cannot_answer_reason": (
            "12-month target price or valuation output requires market price, "
            "valuation model inputs, or external analyst assumptions that are not "
            "contained in the filing ontology."
        ),
        "allowed_answer": (
            "공시자료만으로 12개월 목표치나 목표주가를 직접 산출하지 말고, "
            "공시자료에서 확인되는 매출 성장 지속성 가정, 마진/수요 리스크, "
            "추적 가능한 사업 근거만 제한적으로 설명합니다."
        ),
        "allowed_filing_based_support": [
            "revenue_growth_durability",
            "margin_risk",
            "demand_or_order_backlog_context",
            "capex_or_cost_pressure",
        ],
    }


def _contains_any(text: str, terms: Sequence[str]) -> bool:
    return any(term in text for term in terms)


def _research_intent_profile(question: str) -> dict[str, Any]:
    return intent_profile_from_route(route_research(question, tickers=[]))


def _research_context_policy(intent: Any) -> dict[str, Any]:
    return context_policy_for_route(route_from_intent(intent))


def _research_context_executor(primary_context: str, store: "OntologyStore") -> Any:
    if primary_context == "direct_exposure_context":
        return DirectExposureContext(store)
    if primary_context == "company_overview_context":
        return CompanyOverviewContext(store)
    return RiskContext(store)


def _research_metric_topic(question: str, search_topic: str | None) -> str | None:
    raw = str(question or "")
    raw_lower = raw.lower()
    base = str(search_topic or raw).strip()
    seed_terms = _unique([term for term in (raw.strip(), base) if term])
    metric_terms: list[str] = []
    if _metric_lookup_topic_is_metric_like(raw) or _metric_lookup_topic_is_metric_like(base):
        metric_terms.extend(seed_terms)
    elif any(term in raw_lower for term in ("매출", "수익", "revenue", "sales")):
        metric_terms.extend(seed_terms)
        metric_terms.append("revenue net sales")
    elif any(
        term in raw_lower
        for term in ("마진", "margin", "영업이익률", "gross margin", "operating margin")
    ):
        metric_terms.extend(seed_terms)
        metric_terms.append("margin gross margin operating margin")
    elif any(term in raw_lower for term in ("비용", "원가", "cost", "expense", "영업비용")):
        metric_terms.extend(seed_terms)
        metric_terms.append("cost expense operating expense")
    elif any(
        term in raw_lower
        for term in ("대손", "신용", "credit", "charge-off", "provision", "allowance")
    ):
        metric_terms.extend(seed_terms)
        metric_terms.append("credit loss provision allowance charge off")
    elif any(term in raw_lower for term in ("nii", "net interest", "순이자")):
        metric_terms.extend(seed_terms)
        metric_terms.append("net interest income")
    if any(term in raw_lower for term in ("비중", "share", "대비")):
        metric_terms.append("share total revenue net sales")
    if any(term in raw_lower for term in ("성장률", "증가율", "성장", "growth")):
        metric_terms.append("growth")
    topic = " ".join(term for term in _unique(metric_terms) if term).strip()
    return topic or None


def _metric_series_research_pack(
    results: Sequence[Mapping[str, Any]], diagnostics: Mapping[str, Any]
) -> dict[str, Any]:
    strategy = diagnostics.get("search_strategy") or diagnostics.get("projection") or {}
    role_by_id = strategy.get("metric_roles_by_object_id") or {}
    dimension_by_id = strategy.get("metric_dimensions_by_object_id") or {}
    observations: list[dict[str, Any]] = []
    for item in results:
        obj = item.get("object") if isinstance(item.get("object"), Mapping) else {}
        object_id = str(item.get("id") or obj.get("id") or "")
        role = role_by_id.get(object_id)
        if not role:
            role = "denominator_metric" if obj.get("is_company_total") else "metric"
        dimension_info = (
            dimension_by_id.get(object_id) if isinstance(dimension_by_id, Mapping) else None
        )
        dimensions = obj.get("dimensions") or obj.get("dimension") or {}
        if dimension_info and role == "target_dimension_metric" and not dimensions:
            dimension_key = str(dimension_info.get("dimension_key") or "")
            dimension_label = str(
                dimension_info.get("dimension_display_label")
                or dimension_info.get("dimension_label")
                or dimension_key
            )
            dimension_kind = str(dimension_info.get("dimension_kind") or "unknown")
            dimensions = {dimension_kind: dimension_label} if dimension_label else {}
        observation = {
            "id": object_id,
            "ticker": item.get("ticker") or obj.get("ticker"),
            "period": item.get("period") or obj.get("period"),
            "document_type": item.get("document_type") or obj.get("document_type"),
            "metric_name": obj.get("metric_name") or obj.get("metric_term_id") or obj.get("name"),
            "canonical_metric": obj.get("canonical_metric")
            or obj.get("metric_term_id")
            or obj.get("metric_name"),
            "value": obj.get("value"),
            "unit": obj.get("unit"),
            "dimensions": dimensions,
            "dimension_key": dimension_info.get("dimension_key")
            if isinstance(dimension_info, Mapping)
            else None,
            "dimension_label": dimension_info.get("dimension_label")
            if isinstance(dimension_info, Mapping)
            else None,
            "dimension_display_label": dimension_info.get("dimension_display_label")
            if isinstance(dimension_info, Mapping)
            else None,
            "dimension_kind": dimension_info.get("dimension_kind")
            if isinstance(dimension_info, Mapping)
            else None,
            "metric_role": role,
            "trace_status": item.get("trace_status"),
            "metric_lineage_status": item.get("metric_lineage_status"),
        }
        if obj:
            observation["formatted_value"] = format_metric_compact(dict(obj))
        observations.append(observation)
    series = _metric_series_from_observations(observations)
    calculations = _metric_series_calculations(
        series,
        denominator_needed=bool(strategy.get("denominator_needed")),
    )
    missing_parts: list[str] = []
    for key in strategy.get("missing_dimension_keys") or []:
        missing_parts.append(f"dimension:{key}")
    if strategy.get("dimension_metric_not_found"):
        missing_parts.append("dimension_metric_not_found")
    if not observations and diagnostics.get("metric_fast_path"):
        missing_parts.append("metric_series_not_found")
    return {
        "mode": strategy.get("mode")
        or ("metric_lookup" if diagnostics.get("metric_fast_path") else None),
        "result_count": len(observations),
        "observations": observations,
        "series": series,
        "calculations": calculations,
        "roles": sorted(
            {str(obs.get("metric_role")) for obs in observations if obs.get("metric_role")}
        ),
        "dimension_anchors": strategy.get("dimension_anchors") or [],
        "resolved_dimensions": strategy.get("resolved_dimensions") or [],
        "denominator_needed": bool(strategy.get("denominator_needed")),
        "company_total_role": strategy.get("company_total_role"),
        "period_years": strategy.get("period_years") or [],
        "period_values": strategy.get("period_values") or [],
        "quality": {
            "missing_parts": sorted(set(missing_parts)),
            "dimension_metric_not_found": bool(strategy.get("dimension_metric_not_found")),
            "fallback_used": bool(strategy.get("fallback_used")),
            "period_alignment": calculations.get("period_alignment"),
            "unit_consistency": calculations.get("unit_consistency"),
        },
        "render_hints": {
            "prefer_table": True,
            "include_share_of_total": bool(calculations.get("share_of_total")),
            "include_yoy_growth": bool(calculations.get("growth_rate")),
            "include_growth_difference": bool(calculations.get("growth_difference")),
            "do_not_requery_per_metric": True,
            "user_period_label_style": "CY",
        },
        "diagnostics": {
            "metric_fast_path": bool(diagnostics.get("metric_fast_path")),
            "compact_fast_path": bool(diagnostics.get("compact_fast_path")),
            "search_mode": strategy.get("mode"),
        },
    }


def _metric_series_from_observations(
    observations: Sequence[Mapping[str, Any]],
) -> list[dict[str, Any]]:
    grouped: dict[str, dict[str, Any]] = {}
    for observation in observations:
        value = _metric_observation_number(observation.get("value"))
        period = str(observation.get("period") or "")
        if value is None or not period:
            continue
        key, label = _metric_observation_series_key(observation)
        series = grouped.setdefault(
            key,
            {
                "series_key": key,
                "label": label,
                "ticker": observation.get("ticker"),
                "metric_role": observation.get("metric_role"),
                "metric_name": observation.get("metric_name"),
                "canonical_metric": observation.get("canonical_metric"),
                "unit": observation.get("unit"),
                "dimensions": observation.get("dimensions") or {},
                "points": [],
            },
        )
        if any(str(point.get("period") or "") == period for point in series["points"]):
            continue
        series["points"].append(
            {
                "period": period,
                "value": value,
                "formatted_value": observation.get("formatted_value"),
                "object_id": observation.get("id"),
            }
        )
    for series in grouped.values():
        series["points"].sort(key=lambda point: _period_sort_key(str(point.get("period") or "")))
        series["periods"] = [point["period"] for point in series["points"]]
        series["point_count"] = len(series["points"])
    return sorted(
        grouped.values(),
        key=lambda item: (
            0 if item.get("metric_role") == "target_dimension_metric" else 1,
            str(item.get("label") or item.get("series_key") or ""),
        ),
    )


def _metric_observation_series_key(observation: Mapping[str, Any]) -> tuple[str, str]:
    role = str(observation.get("metric_role") or "metric")
    metric = str(observation.get("canonical_metric") or observation.get("metric_name") or "metric")
    dimensions = (
        observation.get("dimensions") if isinstance(observation.get("dimensions"), Mapping) else {}
    )
    dimension_bits = [f"{key}:{value}" for key, value in sorted(dimensions.items()) if value]
    if dimension_bits:
        dimension_label = " / ".join(
            str(value) for _key, value in sorted(dimensions.items()) if value
        )
    elif role == "denominator_metric":
        dimension_label = "Company total"
    else:
        object_id = str(observation.get("id") or "")
        dimension_label = object_id.rsplit(":", 1)[-1] if object_id else role
    key = (
        "|".join([role, metric, *dimension_bits])
        if dimension_bits
        else "|".join([role, metric, dimension_label])
    )
    return key, dimension_label


def _metric_dimension_display_label(
    label: Any, dimension_key: Any, dimension_anchors: Sequence[Any] | None = None
) -> str:
    text = str(label or "").strip() or str(dimension_key or "").replace("_", " ").strip()
    if not text:
        return ""
    text = re.sub(r"\s+", " ", text).strip()
    single_letter_match = re.match(r"^([A-Za-z]) ([A-Z][A-Za-z0-9]+)(\\b.*)?$", text)
    if single_letter_match:
        suffix = single_letter_match.group(3) or ""
        text = f"{single_letter_match.group(1).lower()}{single_letter_match.group(2)}{suffix}"
    text_key = _metric_dimension_key(text)
    key = str(dimension_key or "").strip()
    for anchor in dimension_anchors or []:
        anchor_text = str(anchor or "").strip()
        anchor_key = _metric_dimension_key(anchor_text)
        if not anchor_key:
            continue
        if anchor_key == f"{text_key}s" and not text.lower().endswith("s"):
            return _metric_dimension_title(anchor_text)
        if anchor_key == key and "_" in anchor_text:
            return _metric_dimension_title(anchor_text)
    return text


def _metric_dimension_title(value: Any) -> str:
    text = str(value or "").replace("_", " ").strip()
    if not text:
        return ""
    return " ".join(part[:1].upper() + part[1:] for part in text.split())


def _metric_observation_number(value: Any) -> float | None:
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


def _period_sort_key(period: str) -> tuple[int, int, str]:
    match = re.search(r"(?:CY|FY)?(20\d{2}|19\d{2})(?:Q([1-4]))?", period.upper())
    if not match:
        return (0, 0, period)
    return (int(match.group(1)), int(match.group(2) or 0), period)


def _metric_series_calculations(
    series: Sequence[Mapping[str, Any]],
    *,
    denominator_needed: bool,
) -> dict[str, Any]:
    units = {str(item.get("unit") or "").lower() for item in series if item.get("unit")}
    unit_consistency = len(units) <= 1
    denominator_series = [
        item for item in series if item.get("metric_role") == "denominator_metric"
    ]
    target_series = [
        item for item in series if item.get("metric_role") == "target_dimension_metric"
    ]
    denominator_by_period: dict[str, float] = {}
    if denominator_series:
        for point in denominator_series[0].get("points") or []:
            value = _metric_observation_number(point.get("value"))
            period = str(point.get("period") or "")
            if period and value not in (None, 0.0):
                denominator_by_period[period] = float(value)

    share_of_total: list[dict[str, Any]] = []
    if denominator_needed and denominator_by_period:
        for item in target_series:
            for point in item.get("points") or []:
                period = str(point.get("period") or "")
                numerator = _metric_observation_number(point.get("value"))
                denominator = denominator_by_period.get(period)
                if not period or numerator is None or not denominator:
                    continue
                share_of_total.append(
                    {
                        "series_key": item.get("series_key"),
                        "label": item.get("label"),
                        "period": period,
                        "numerator": numerator,
                        "denominator": denominator,
                        "share": numerator / denominator,
                    }
                )

    growth_rate: list[dict[str, Any]] = []
    growth_by_series: dict[str, dict[str, dict[str, Any]]] = {}
    for item in series:
        previous: Mapping[str, Any] | None = None
        for point in item.get("points") or []:
            current_value = _metric_observation_number(point.get("value"))
            previous_value = _metric_observation_number(previous.get("value")) if previous else None
            if current_value is not None and previous_value not in (None, 0.0):
                growth_entry = {
                    "series_key": item.get("series_key"),
                    "label": item.get("label"),
                    "from_period": previous.get("period"),
                    "to_period": point.get("period"),
                    "growth": (current_value - float(previous_value)) / float(previous_value),
                }
                growth_rate.append(growth_entry)
                series_key = str(item.get("series_key") or "")
                to_period = str(point.get("period") or "")
                if series_key and to_period:
                    growth_by_series.setdefault(series_key, {})[to_period] = growth_entry
            previous = point

    growth_difference: list[dict[str, Any]] = []
    for left_index, left in enumerate(target_series):
        left_key = str(left.get("series_key") or "")
        if not left_key:
            continue
        for right in target_series[left_index + 1 :]:
            right_key = str(right.get("series_key") or "")
            if not right_key:
                continue
            common_periods = sorted(
                set(growth_by_series.get(left_key, {})).intersection(
                    growth_by_series.get(right_key, {})
                ),
                key=_period_sort_key,
            )
            for period in common_periods:
                left_growth = growth_by_series[left_key][period]
                right_growth = growth_by_series[right_key][period]
                growth_difference.append(
                    {
                        "left_series_key": left_key,
                        "left_label": left.get("label"),
                        "right_series_key": right_key,
                        "right_label": right.get("label"),
                        "from_period": left_growth.get("from_period"),
                        "to_period": period,
                        "left_growth": left_growth.get("growth"),
                        "right_growth": right_growth.get("growth"),
                        "difference": float(left_growth.get("growth"))
                        - float(right_growth.get("growth")),
                    }
                )

    period_sets = [set(item.get("periods") or []) for item in series if item.get("periods")]
    period_alignment = (
        len({tuple(sorted(periods)) for periods in period_sets}) <= 1 if period_sets else None
    )
    return {
        "share_of_total": share_of_total,
        "growth_rate": growth_rate,
        "growth_difference": growth_difference,
        "period_alignment": period_alignment,
        "unit_consistency": unit_consistency,
    }


def _projection_research_pack(
    results: Sequence[Mapping[str, Any]],
    diagnostics: Mapping[str, Any],
    *,
    query_frame: Mapping[str, Any],
    answerability: Mapping[str, Any],
) -> dict[str, Any]:
    strategy = diagnostics.get("search_strategy") or diagnostics.get("projection") or {}
    candidates: list[dict[str, Any]] = []
    for item in results[:10]:
        obj = item.get("object") if isinstance(item.get("object"), Mapping) else {}
        candidates.append(
            {
                "id": item.get("id") or obj.get("id"),
                "ticker": item.get("ticker") or obj.get("ticker"),
                "period": item.get("period") or obj.get("period"),
                "type": item.get("type") or obj.get("type"),
                "summary": item.get("text") or _display_text(dict(obj)),
                "trace_status": item.get("trace_status"),
            }
        )
    directness_guard = _research_directness_guard(query_frame, answerability)
    return {
        "mode": strategy.get("mode"),
        "table": strategy.get("table") or strategy.get("projection_used"),
        "matched_terms": strategy.get("matched_terms") or [],
        "result_count": len(candidates),
        "candidates": candidates,
        "directness": {
            **directness_guard,
            "projection_candidates_are_search_candidates_only": True,
        },
        "quality": {
            "fallback_used": bool(strategy.get("fallback_used")),
            "missing_parts": [] if candidates else ["projection_candidates_not_found"],
        },
    }


def _company_topic_research_pack(candidates: Sequence[Mapping[str, Any]]) -> dict[str, Any]:
    top_candidates: list[dict[str, Any]] = []
    for candidate in candidates:
        matched_topics = candidate.get("matched_topics") or []
        if isinstance(matched_topics, Sequence) and not isinstance(matched_topics, (str, bytes)):
            for topic in matched_topics:
                if not isinstance(topic, Mapping):
                    continue
                top_candidates.append(
                    {
                        "ticker": topic.get("ticker") or candidate.get("ticker"),
                        "period": topic.get("period"),
                        "document_type": topic.get("document_type"),
                        "tier": topic.get("tier")
                        or candidate.get("tier")
                        or (topic.get("match") or {}).get("tier"),
                        "trace_status": topic.get("trace_status") or candidate.get("trace_status"),
                        "topic_label": _public_topic_label(topic.get("topic_label")),
                        "topic_summary": topic.get("topic_summary"),
                        "topic_type": topic.get("topic_type"),
                        "primary_object_type": topic.get("primary_object_type"),
                        "score": topic.get("score") or candidate.get("score"),
                    }
                )
                if len(top_candidates) >= 8:
                    break
        if len(top_candidates) >= 8:
            break
        if candidate.get("topic_label") or candidate.get("topic_summary"):
            top_candidates.append(
                {
                    "ticker": candidate.get("ticker"),
                    "period": candidate.get("period"),
                    "document_type": candidate.get("document_type"),
                    "tier": candidate.get("tier"),
                    "trace_status": candidate.get("trace_status"),
                    "topic_label": _public_topic_label(candidate.get("topic_label")),
                    "topic_summary": candidate.get("topic_summary"),
                    "topic_type": candidate.get("topic_type"),
                    "primary_object_type": candidate.get("primary_object_type"),
                    "score": candidate.get("score"),
                }
            )
        if len(top_candidates) >= 8:
            break
    return {
        "top_candidates": top_candidates,
        "candidate_count": len(candidates),
    }


def _public_topic_label(value: Any) -> str | None:
    label = str(value or "").strip()
    if not label:
        return None
    if label in {
        "EvidenceQuote",
        "ResearchClaim",
        "MetricObservation",
        "BusinessActivity",
        "BusinessFactor",
    }:
        return None
    return label


def _business_profile_research_pack(
    *,
    route: Any,
    company_topic_pack: Mapping[str, Any],
    metric_series_pack: Mapping[str, Any] | None,
    projection_pack: Mapping[str, Any] | None,
    selected_candidates: Sequence[Mapping[str, Any]],
    preferred_answer_order: Mapping[str, Any],
    period_display_policy: Mapping[str, Any],
) -> dict[str, Any] | None:
    if str(getattr(route.intent, "value", route.intent)) not in {
        "company_overview",
        "general_research",
    }:
        return None
    topics = list(company_topic_pack.get("top_candidates") or [])
    business_segments: list[dict[str, Any]] = []
    for candidate in topics[:6]:
        business_segments.append(
            {
                "ticker": candidate.get("ticker"),
                "segment_or_topic": candidate.get("topic_label") or "filing business driver",
                "driver_summary": candidate.get("topic_summary"),
                "trace_status": candidate.get("trace_status"),
                "tier": candidate.get("tier"),
            }
        )
    representative_metrics: list[dict[str, Any]] = []
    if isinstance(metric_series_pack, Mapping):
        for series in metric_series_pack.get("series") or []:
            representative_metrics.append(
                {
                    "label": series.get("label"),
                    "metric_role": series.get("metric_role"),
                    "periods": series.get("periods") or [],
                    "point_count": series.get("point_count"),
                }
            )
    if isinstance(projection_pack, Mapping):
        for candidate in projection_pack.get("candidates") or []:
            if len(business_segments) >= 8:
                break
            business_segments.append(
                {
                    "ticker": candidate.get("ticker"),
                    "segment_or_topic": candidate.get("type"),
                    "driver_summary": candidate.get("summary"),
                    "trace_status": candidate.get("trace_status"),
                    "tier": "projection_candidate",
                }
            )
    return {
        "mode": "ontology_business_profile",
        "answer_order": (
            preferred_answer_order.get("answer_order")
            if isinstance(preferred_answer_order, Mapping)
            else list(preferred_answer_order or [])
        )
        or [],
        "period_display_policy": period_display_policy,
        "business_segments": business_segments,
        "annual_revenue_mix": representative_metrics,
        "current_drivers": business_segments[:4],
        "key_caveats": [
            "Use filing-grounded business activity and metric lineage; avoid unsupported market-share or valuation claims."
        ],
        "render_hints": {
            "structure": "current_drivers -> annual_revenue_mix -> business_segments -> caveats",
            "latest_first_when_requested": bool(period_display_policy.get("latest_first")),
            "do_not_requery_company_context_by_default": True,
        },
        "quality": {
            "candidate_count": len(selected_candidates),
            "missing_parts": [] if business_segments else ["business_profile_candidates_not_found"],
        },
    }


def _risk_mechanism_research_pack(
    *,
    route: Any,
    projection_pack: Mapping[str, Any] | None,
    company_topic_pack: Mapping[str, Any],
    chain_pack: Mapping[str, Any],
) -> dict[str, Any] | None:
    if str(getattr(route.intent, "value", route.intent)) != "risk_thesis":
        return None
    source_candidates: list[Mapping[str, Any]] = []
    if isinstance(projection_pack, Mapping):
        source_candidates.extend(
            candidate
            for candidate in projection_pack.get("candidates") or []
            if isinstance(candidate, Mapping)
        )
    source_candidates.extend(
        candidate
        for candidate in company_topic_pack.get("top_candidates") or []
        if isinstance(candidate, Mapping)
    )
    risk_channels: list[dict[str, Any]] = []
    for candidate in source_candidates[:6]:
        summary = str(candidate.get("summary") or candidate.get("topic_summary") or "")
        risk_label = str(candidate.get("topic_label") or candidate.get("type") or "risk channel")
        risk_channels.append(
            {
                "risk": risk_label,
                "support_summary": summary[:700],
                "financial_path": _risk_financial_path(summary),
                "affected_metrics": _risk_affected_metrics(summary),
                "implication": _risk_implication(summary),
                "directness": candidate.get("tier") or "related_context",
                "trace_status": candidate.get("trace_status"),
            }
        )
    return {
        "mode": "risk_mechanism",
        "risk_channels": risk_channels,
        "representative_metrics": [],
        "metric_depth": "representative_only",
        "chain_roots": chain_pack.get("root_candidates") if isinstance(chain_pack, Mapping) else [],
        "render_hints": {
            "structure": "support_summary -> financial_path -> implication",
            "do_not_run_deep_metric_series": True,
            "chain_is_lazy": True,
        },
        "quality": {
            "missing_parts": [] if risk_channels else ["risk_mechanism_candidates_not_found"],
        },
    }


def _comparison_research_view(
    *,
    route: Any,
    metric_series_pack: Mapping[str, Any] | None,
    projection_pack: Mapping[str, Any] | None,
    company_topic_pack: Mapping[str, Any],
) -> dict[str, Any] | None:
    if str(getattr(route.intent, "value", route.intent)) != "comparison":
        return None
    rows: list[dict[str, Any]] = []
    if isinstance(metric_series_pack, Mapping):
        for series in metric_series_pack.get("series") or []:
            rows.append(
                {
                    "item": series.get("label"),
                    "basis": "metric_series",
                    "metric_role": series.get("metric_role"),
                    "periods": series.get("periods") or [],
                    "point_count": series.get("point_count"),
                }
            )
    if isinstance(projection_pack, Mapping):
        for candidate in projection_pack.get("candidates") or []:
            rows.append(
                {
                    "item": candidate.get("ticker") or candidate.get("type"),
                    "basis": "projection_candidate",
                    "summary": candidate.get("summary"),
                    "trace_status": candidate.get("trace_status"),
                }
            )
    if not rows:
        for candidate in company_topic_pack.get("top_candidates") or []:
            rows.append(
                {
                    "item": candidate.get("ticker") or candidate.get("topic_label"),
                    "basis": "company_topic",
                    "summary": candidate.get("topic_summary"),
                    "trace_status": candidate.get("trace_status"),
                }
            )
    return {
        "mode": "comparison_view",
        "comparison_type": "metric_or_context_comparison",
        "comparison_basis": "same-context evidence rows; final conclusion remains with the AI analyst",
        "rows": rows[:8],
        "conclusion_hint": "Compare only on rows with matching basis and period/context.",
        "quality": {"missing_parts": [] if rows else ["comparison_candidates_not_found"]},
    }


def _direct_exposure_research_pack(
    *,
    route: Any,
    directness_guard: Mapping[str, Any],
    projection_pack: Mapping[str, Any] | None,
    company_topic_pack: Mapping[str, Any],
) -> dict[str, Any] | None:
    if str(getattr(route.intent, "value", route.intent)) != "direct_exposure":
        return None
    direct_candidates: list[dict[str, Any]] = []
    related_candidates: list[dict[str, Any]] = []
    if isinstance(projection_pack, Mapping):
        for candidate in projection_pack.get("candidates") or []:
            target = (
                direct_candidates
                if directness_guard.get("direct_answerable")
                else related_candidates
            )
            target.append(dict(candidate))
    for candidate in company_topic_pack.get("top_candidates") or []:
        related_candidates.append(dict(candidate))
    return {
        "mode": "direct_exposure",
        "strong_claim_allowed": bool(directness_guard.get("strong_claim_allowed")),
        "negative_answer_supported": bool(directness_guard.get("negative_answer_supported")),
        "direct_candidates": direct_candidates[:5],
        "related_candidates": related_candidates[:5],
        "answer_policy": (
            "Use direct exposure wording only when strong_claim_allowed is true; otherwise answer as no direct evidence with related context."
        ),
        "quality": {
            "missing_parts": []
            if direct_candidates or related_candidates
            else ["direct_exposure_candidates_not_found"],
        },
    }


def _cross_company_signal_research_pack(
    *,
    question: str,
    route: Any,
    requested_tickers: Sequence[str] | None,
    company_topic_pack: Mapping[str, Any],
    projection_pack: Mapping[str, Any] | None,
    chain_pack: Mapping[str, Any],
    recommended_tools: Sequence[Mapping[str, Any]],
    period_display_policy: Mapping[str, Any],
) -> dict[str, Any] | None:
    topic_rows = [
        row for row in company_topic_pack.get("top_candidates") or [] if isinstance(row, Mapping)
    ]
    projection_rows = [
        row
        for row in (
            (projection_pack or {}).get("candidates")
            if isinstance(projection_pack, Mapping)
            else []
        )
        or []
        if isinstance(row, Mapping)
    ]
    ticker_basket = _cross_company_ticker_basket(requested_tickers, topic_rows, projection_rows)
    if not _is_cross_company_signal_question(question, route=route, ticker_basket=ticker_basket):
        return None

    evidence_rows: list[dict[str, Any]] = []
    for row in projection_rows[:8]:
        evidence_rows.append(_cross_company_evidence_row(row, source="projection_candidate"))
    for row in topic_rows[:10]:
        evidence_rows.append(_cross_company_evidence_row(row, source="company_topic"))

    deduped_rows: list[dict[str, Any]] = []
    seen: set[tuple[Any, ...]] = set()
    for row in evidence_rows:
        key = (
            row.get("ticker"),
            row.get("period"),
            row.get("signal"),
            row.get("commentary_summary"),
        )
        if key in seen:
            continue
        seen.add(key)
        deduped_rows.append(row)
        if len(deduped_rows) >= 12:
            break

    signals = _cross_company_signal_rows(deduped_rows)
    trace_roots = [
        str(tool.get("object_id")) for tool in recommended_tools[:3] if tool.get("object_id")
    ]
    chain_roots = [
        str(root.get("root_object_id"))
        for root in (chain_pack.get("root_candidates") or [])
        if isinstance(root, Mapping) and root.get("root_object_id")
    ]
    quality = _cross_company_signal_quality(deduped_rows)
    missing_parts = []
    if not deduped_rows:
        missing_parts.append("cross_company_evidence_rows_not_found")
    if len(ticker_basket) < 2:
        missing_parts.append("multi_company_ticker_basket_not_found")
    quality["missing_parts"] = missing_parts

    return {
        "mode": "cross_company_signal_synthesis",
        "question_focus": _cross_company_question_focus(question),
        "ticker_basket": ticker_basket,
        "latest_period_anchor": _latest_period_anchor(deduped_rows, period_display_policy),
        "signals": signals,
        "company_evidence_rows": deduped_rows,
        "recommended_trace_roots": trace_roots,
        "recommended_chain_roots": chain_roots,
        "answer_policy": (
            "Use this as compact cross-company evidence. Final macro/sector/thesis judgment remains with the AI analyst."
        ),
        "quality": quality,
    }


def _is_cross_company_signal_question(
    question: str, *, route: Any, ticker_basket: Sequence[str]
) -> bool:
    text = str(question or "").lower()
    intent = str(getattr(getattr(route, "intent", None), "value", getattr(route, "intent", "")))
    if len(set(ticker_basket)) >= 2 and intent in {
        "comparison",
        "risk_thesis",
        "discovery",
        "general_research",
        "company_overview",
    }:
        return True
    return any(
        term in text
        for term in (
            "sector",
            "macro",
            "read-through",
            "read through",
            "industry",
            "cycle",
            "cross-company",
            "companies",
            "consumer",
            "semiconductor",
            "credit environment",
            "섹터",
            "업종",
            "매크로",
            "기업들",
            "소비재",
            "반도체",
            "신용 환경",
            "사이클",
        )
    )


def _cross_company_ticker_basket(
    requested_tickers: Sequence[str] | None,
    topic_rows: Sequence[Mapping[str, Any]],
    projection_rows: Sequence[Mapping[str, Any]],
) -> list[str]:
    tickers: list[str] = []
    for ticker in requested_tickers or []:
        if ticker:
            tickers.append(str(ticker).upper())
    for row in list(projection_rows) + list(topic_rows):
        ticker = row.get("ticker")
        if ticker:
            tickers.append(str(ticker).upper())
    return [str(value) for value in _unique(tickers)[:12]]


def _cross_company_evidence_row(row: Mapping[str, Any], *, source: str) -> dict[str, Any]:
    summary = str(
        row.get("summary") or row.get("topic_summary") or row.get("topic_label") or ""
    ).strip()
    object_type = row.get("type") or row.get("primary_object_type") or row.get("topic_type")
    signal = _cross_company_signal_name(summary, object_type=object_type)
    evidence_strength = _cross_company_evidence_strength(row, summary=summary)
    return {
        "ticker": row.get("ticker"),
        "period": row.get("period"),
        "document_type": row.get("document_type"),
        "signal": signal,
        "evidence_type": _cross_company_evidence_type(row, source=source),
        "commentary_summary": summary[:700],
        "financial_channel": _cross_company_financial_channels(summary),
        "evidence_strength": evidence_strength,
        "specificity_score": row.get("specificity_score"),
        "boilerplate_score": row.get("boilerplate_score"),
        "trace_status": row.get("trace_status"),
        "tier": row.get("tier"),
        "source": source,
        "trace_root": row.get("id") or row.get("primary_object_id"),
        "chain_root": row.get("id") or row.get("primary_object_id"),
    }


def _cross_company_evidence_type(row: Mapping[str, Any], *, source: str) -> str:
    object_type = str(
        row.get("type") or row.get("primary_object_type") or row.get("topic_type") or ""
    )
    if object_type in {"MetricObservation", "Calculation", "XBRLFact"}:
        return "numeric_metric"
    if object_type in {"ResearchClaim", "EvidenceQuote"}:
        return "company_filing_commentary"
    if object_type in {"ExternalFactorExposure", "BusinessFactor", "Headwind", "RiskFactor"}:
        return "risk_or_factor_channel"
    if source == "projection_candidate":
        return "typed_projection_candidate"
    return "company_topic"


def _cross_company_evidence_strength(row: Mapping[str, Any], *, summary: str) -> str:
    tier = str(row.get("tier") or "").lower()
    trace_status = str(row.get("trace_status") or "").lower()
    evidence_grade = str(row.get("evidence_grade") or row.get("evidence_strength") or "").lower()
    specificity = _metric_observation_number(row.get("specificity_score"))
    boilerplate = _metric_observation_number(row.get("boilerplate_score"))
    if (
        tier in {"traceable_direct", "traceable_metric_lineage"}
        or evidence_grade == "direct"
        or trace_status == "traceable"
    ):
        return "strong"
    if (
        specificity is not None
        and specificity >= 0.65
        and (boilerplate is None or boilerplate <= 0.45)
    ):
        return "strong"
    if _looks_like_generic_boilerplate(summary) or (
        boilerplate is not None and boilerplate >= 0.65
    ):
        return "weak"
    if tier in {"traceable_related", "untraced_direct_candidate"} or evidence_grade in {
        "indirect",
        "derived",
    }:
        return "medium"
    return "medium"


def _looks_like_generic_boilerplate(text: str) -> bool:
    lowered = text.lower()
    generic_patterns = (
        "may change",
        "may adversely affect",
        "could adversely affect",
        "competition may",
        "costs may increase",
        "consumer demand may",
        "uncertain",
        "subject to",
    )
    return any(pattern in lowered for pattern in generic_patterns)


def _cross_company_signal_name(summary: str, *, object_type: Any) -> str:
    text = summary.lower()
    if any(term in text for term in ("price", "pricing", "pass through", "가격", "전가")):
        return "pricing_power"
    if any(
        term in text
        for term in (
            "volume",
            "traffic",
            "comparable sales",
            "unit",
            "demand",
            "수요",
            "판매량",
            "트래픽",
        )
    ):
        return "demand_or_volume"
    if any(
        term in text
        for term in ("cost", "input", "commodity", "freight", "cogs", "원가", "비용", "운임")
    ):
        return "input_cost_pressure"
    if any(term in text for term in ("margin", "gross", "operating income", "마진", "이익률")):
        return "margin_pressure"
    if any(term in text for term in ("rate", "credit", "interest", "funding", "금리", "신용")):
        return "rates_or_credit"
    if any(
        term in text for term in ("regulation", "tariff", "geopolitical", "관세", "규제", "지정학")
    ):
        return "policy_or_geopolitical"
    if str(object_type or "") in {"MetricObservation", "Calculation", "XBRLFact"}:
        return "numeric_operating_signal"
    return "business_or_macro_signal"


def _cross_company_financial_channels(summary: str) -> list[str]:
    text = summary.lower()
    channels: list[str] = []
    if any(
        term in text
        for term in ("revenue", "sales", "demand", "volume", "traffic", "매출", "수요", "판매량")
    ):
        channels.append("revenue_or_volume")
    if any(
        term in text for term in ("gross margin", "margin", "operating income", "마진", "영업이익")
    ):
        channels.append("margin")
    if any(term in text for term in ("cost", "expense", "cogs", "input", "원가", "비용")):
        channels.append("cost")
    if any(term in text for term in ("cash", "working capital", "현금")):
        channels.append("cash_flow")
    return channels or ["business_performance"]


def _cross_company_signal_rows(evidence_rows: Sequence[Mapping[str, Any]]) -> list[dict[str, Any]]:
    grouped: dict[str, list[Mapping[str, Any]]] = {}
    for row in evidence_rows:
        grouped.setdefault(str(row.get("signal") or "business_or_macro_signal"), []).append(row)
    signal_rows: list[dict[str, Any]] = []
    for signal, rows in grouped.items():
        strengths = [str(row.get("evidence_strength") or "medium") for row in rows]
        companies = [
            str(value)
            for value in _unique(str(row.get("ticker")) for row in rows if row.get("ticker"))
        ]
        channels: list[str] = []
        for row in rows:
            channels.extend(str(channel) for channel in row.get("financial_channel") or [])
        signal_rows.append(
            {
                "signal": signal,
                "companies_supporting": companies,
                "evidence_count": len(rows),
                "strength": _combined_evidence_strength(strengths),
                "financial_channel": [str(value) for value in _unique(channels)],
                "interpretation_hint": _cross_company_interpretation_hint(signal),
            }
        )
    signal_rows.sort(
        key=lambda row: (-int(row.get("evidence_count") or 0), str(row.get("signal") or ""))
    )
    return signal_rows[:8]


def _combined_evidence_strength(strengths: Sequence[str]) -> str:
    if "strong" in strengths:
        return "strong"
    if "medium" in strengths:
        return "medium"
    return "weak"


def _cross_company_interpretation_hint(signal: str) -> str:
    hints = {
        "pricing_power": "Evaluate whether companies can still pass costs through without losing volume.",
        "demand_or_volume": "Separate broad demand uncertainty from actual volume, traffic, or comparable-sales evidence.",
        "input_cost_pressure": "Connect input-cost commentary to gross margin and operating margin before making a macro claim.",
        "margin_pressure": "Check whether margin pressure is from cost, pricing, volume, or mix.",
        "rates_or_credit": "Treat rate or credit comments as macro sensitivity unless linked to current operating metrics.",
        "policy_or_geopolitical": "Use as a risk channel unless latest commentary or numbers show current impact.",
        "numeric_operating_signal": "Use same-period, same-unit numeric comparisons before drawing cross-company conclusions.",
    }
    return hints.get(
        signal,
        "Use multiple company signals and current filing commentary before making a broad conclusion.",
    )


def _cross_company_signal_quality(evidence_rows: Sequence[Mapping[str, Any]]) -> dict[str, Any]:
    strengths = [str(row.get("evidence_strength") or "medium") for row in evidence_rows]
    return {
        "strong_evidence_count": sum(1 for strength in strengths if strength == "strong"),
        "medium_evidence_count": sum(1 for strength in strengths if strength == "medium"),
        "weak_evidence_count": sum(1 for strength in strengths if strength == "weak"),
        "generic_boilerplate_count": sum(
            1
            for row in evidence_rows
            if _looks_like_generic_boilerplate(str(row.get("commentary_summary") or ""))
        ),
        "latest_evidence_count": sum(
            1 for row in evidence_rows if _is_latestish_period(row.get("period"))
        ),
        "numeric_support_count": sum(
            1 for row in evidence_rows if row.get("evidence_type") == "numeric_metric"
        ),
    }


def _latest_period_anchor(
    evidence_rows: Sequence[Mapping[str, Any]],
    period_display_policy: Mapping[str, Any],
) -> str | None:
    periods = [str(row.get("period")) for row in evidence_rows if row.get("period")]
    if periods:
        return sorted(set(periods))[-1]
    if period_display_policy.get("latest_first"):
        return "latest_available_filing"
    return None


def _is_latestish_period(period: Any) -> bool:
    text = str(period or "").upper()
    return "CY2026" in text or "CY2025" in text or text == "ALL"


def _cross_company_question_focus(question: str) -> str:
    normalized = " ".join(str(question or "").split())
    return normalized[:180] or "cross-company signal synthesis"


def _scope_guard_research_pack(route: Any) -> dict[str, Any] | None:
    if str(getattr(route.intent, "value", route.intent)) != "valuation_or_price_target":
        return None
    return {
        "mode": "scope_guard",
        "out_of_scope": True,
        "reason": "Final target price, investment opinion, or fair value requires external market/valuation assumptions outside the filing ontology.",
        "allowed_filing_support": [
            "revenue durability assumptions",
            "margin and capex evidence",
            "risk factors",
            "cash flow and balance sheet inputs",
        ],
    }


def _research_evidence_index(
    *,
    trace_candidates: Sequence[Mapping[str, Any]],
    chain_pack: Mapping[str, Any],
    metric_series_pack: Mapping[str, Any] | None,
    projection_pack: Mapping[str, Any] | None,
) -> dict[str, Any]:
    entries: list[dict[str, Any]] = []
    for index, tool in enumerate(trace_candidates[:3]):
        entries.append(
            {
                "role": "trace_candidate",
                "rank": index,
                "object_id": tool.get("object_id"),
                "ticker": tool.get("ticker"),
                "tool": tool.get("tool") or "krw_ontology_trace",
                "purpose": tool.get("purpose"),
            }
        )
    if isinstance(chain_pack, Mapping):
        for root in chain_pack.get("root_candidates") or []:
            entries.append(
                {
                    "role": "chain_root",
                    "object_id": root.get("root_object_id"),
                    "chain_depth": chain_pack.get("allowed_additional_chain_depth"),
                }
            )
    for pack_name, pack in (
        ("metric_series_pack", metric_series_pack),
        ("projection_pack", projection_pack),
    ):
        if not isinstance(pack, Mapping):
            continue
        for item in pack.get("observations") or pack.get("candidates") or []:
            entries.append(
                {
                    "role": pack_name,
                    "object_id": item.get("id"),
                    "ticker": item.get("ticker"),
                    "period": item.get("period"),
                    "trace_status": item.get("trace_status"),
                }
            )
    return {
        "mode": "compact_evidence_index",
        "entries": entries[:12],
        "trace_policy": {
            "fast": 0,
            "standard": "1-2 selected trace/chain calls only when recommended",
            "deep": "multiple selected roots allowed; no chain-all behavior",
        },
    }


def _risk_financial_path(summary: str) -> list[str]:
    text = summary.lower()
    if any(
        term in text
        for term in ("cost", "expense", "wage", "labor", "input", "freight", "비용", "인건비")
    ):
        return [
            "cost pressure",
            "operating expense increase",
            "operating margin pressure",
            "weaker earnings conversion",
        ]
    if any(term in text for term in ("rate", "deposit", "interest", "funding", "금리", "예금")):
        return [
            "rate sensitivity",
            "funding or yield pressure",
            "net interest income impact",
            "earnings volatility",
        ]
    if any(term in text for term in ("demand", "volume", "sales", "revenue", "수요", "매출")):
        return [
            "demand change",
            "revenue growth impact",
            "operating leverage change",
            "margin implication",
        ]
    if any(
        term in text
        for term in ("regulation", "regulatory", "approval", "litigation", "규제", "소송")
    ):
        return [
            "regulatory or legal constraint",
            "timing/cost uncertainty",
            "revenue or margin impact",
            "valuation assumption risk",
        ]
    return ["business risk", "revenue/cost channel", "margin or cash-flow implication"]


def _risk_affected_metrics(summary: str) -> list[str]:
    text = summary.lower()
    metrics: list[str] = []
    if any(term in text for term in ("revenue", "sales", "demand", "매출", "수요")):
        metrics.append("revenue growth")
    if any(term in text for term in ("margin", "gross", "operating", "마진")):
        metrics.append("margin")
    if any(term in text for term in ("cost", "expense", "비용")):
        metrics.append("operating expense")
    if any(term in text for term in ("cash", "capex", "현금", "설비")):
        metrics.append("cash flow")
    return metrics or ["revenue", "margin", "cash flow"]


def _risk_implication(summary: str) -> str:
    path = _risk_financial_path(summary)
    return f"Mechanism to evaluate: {' -> '.join(path)}."


def _research_directness_guard(
    query_frame: Mapping[str, Any], answerability: Mapping[str, Any]
) -> dict[str, Any]:
    direct_answerable = bool(answerability.get("direct_answerable"))
    return {
        "requires_direct_match": bool(
            query_frame.get("question_requires_direct_match")
            or query_frame.get("requires_direct_match")
        ),
        "direct_answerable": direct_answerable,
        "related_context_available": bool(answerability.get("related_context_available")),
        "negative_answer_supported": bool(answerability.get("negative_answer_supported")),
        "recommended_answer_mode": answerability.get("recommended_answer_mode"),
        "strong_claim_allowed": direct_answerable,
        "strong_claim_requires": ["traceable_direct", "traceable_metric_lineage"],
    }


def _research_chain_roots(
    *,
    selected_candidates: Sequence[Mapping[str, Any]],
    recommended_tools: Sequence[Mapping[str, Any]],
    metric_series_pack: Mapping[str, Any] | None,
    projection_pack: Mapping[str, Any] | None,
    limit: int,
) -> list[str]:
    ids: list[str] = []
    for tool in recommended_tools:
        if tool.get("object_id"):
            ids.append(str(tool["object_id"]))
    ids.extend(_object_ids_from_query_context({"ticker_candidates": list(selected_candidates)}))
    for pack in (metric_series_pack, projection_pack):
        if not pack:
            continue
        for item in pack.get("observations") or pack.get("candidates") or []:
            if item.get("id"):
                ids.append(str(item["id"]))
    deduped: list[str] = []
    seen: set[str] = set()
    for object_id in ids:
        if object_id and object_id not in seen:
            seen.add(object_id)
            deduped.append(object_id)
        if len(deduped) >= max(1, int(limit)):
            break
    return deduped


def _research_missing_parts(research_pack: Mapping[str, Any]) -> list[str]:
    missing: list[str] = []
    metric_pack = research_pack.get("metric_series_pack") or {}
    projection_pack = research_pack.get("projection_pack") or {}
    metric_quality = metric_pack.get("quality") if isinstance(metric_pack, Mapping) else {}
    projection_quality = (
        projection_pack.get("quality") if isinstance(projection_pack, Mapping) else {}
    )
    for part in (metric_quality or {}).get("missing_parts") or []:
        missing.append(str(part))
    for part in (projection_quality or {}).get("missing_parts") or []:
        missing.append(str(part))
    return sorted(set(missing))


def _research_status_from_pack(
    *,
    answerability: Mapping[str, Any],
    candidates: Sequence[Mapping[str, Any]],
    research_pack: Mapping[str, Any],
) -> str:
    scope_guard_pack = research_pack.get("scope_guard_pack")
    if isinstance(scope_guard_pack, Mapping) and scope_guard_pack.get("out_of_scope"):
        return "out_of_scope_for_filing_ontology"
    metric_pack = research_pack.get("metric_series_pack")
    if isinstance(metric_pack, Mapping) and metric_pack.get("result_count"):
        if not (metric_pack.get("quality") or {}).get("missing_parts"):
            return "sufficient_for_default_answer"
        return "partial_answer_possible"
    business_profile_pack = research_pack.get("business_profile_pack")
    if isinstance(business_profile_pack, Mapping) and (
        business_profile_pack.get("business_segments")
        or business_profile_pack.get("current_drivers")
    ):
        return "sufficient_but_trace_recommended"
    risk_mechanism_pack = research_pack.get("risk_mechanism_pack")
    if isinstance(risk_mechanism_pack, Mapping) and risk_mechanism_pack.get("risk_channels"):
        return "sufficient_but_trace_recommended"
    direct_exposure_pack = research_pack.get("direct_exposure_pack")
    if isinstance(direct_exposure_pack, Mapping) and (
        direct_exposure_pack.get("direct_candidates")
        or direct_exposure_pack.get("related_candidates")
        or direct_exposure_pack.get("negative_answer_supported")
    ):
        return "sufficient_but_trace_recommended"
    comparison_view = research_pack.get("comparison_view")
    if isinstance(comparison_view, Mapping) and comparison_view.get("rows"):
        return "sufficient_but_trace_recommended"
    projection_pack = research_pack.get("projection_pack")
    if isinstance(projection_pack, Mapping) and projection_pack.get("result_count"):
        if not (projection_pack.get("quality") or {}).get("missing_parts"):
            return "sufficient_but_trace_recommended"
        return "partial_answer_possible"
    if answerability.get("direct_answerable"):
        return "sufficient_but_trace_recommended"
    if answerability.get("recommended_answer_mode") == "no_direct_evidence_with_related_context":
        return "sufficient_but_trace_recommended"
    if candidates:
        return (
            "partial_answer_possible"
            if not answerability.get("related_context_available")
            else "sufficient_but_trace_recommended"
        )
    return "needs_targeted_followup"


def _research_pack_can_skip_discovery(
    research_status: str,
    research_pack: Mapping[str, Any],
    *,
    question: str,
) -> bool:
    if _research_question_needs_company_topic_context(question):
        return False
    if research_status == "sufficient_for_default_answer":
        return True
    projection_pack = research_pack.get("projection_pack")
    if not isinstance(projection_pack, Mapping) or not projection_pack.get("result_count"):
        return False
    directness = (
        projection_pack.get("directness")
        if isinstance(projection_pack.get("directness"), Mapping)
        else {}
    )
    if directness.get("requires_direct_match"):
        return False
    return research_status == "sufficient_but_trace_recommended"


def _research_question_needs_company_topic_context(question: str | None) -> bool:
    text = str(question or "").lower()
    return any(
        term in text
        for term in (
            "driver",
            "drivers",
            "factor",
            "factors",
            "impact",
            "mechanism",
            "risk",
            "risks",
            "regulatory",
            "why",
            "cause",
            "causes",
            "because",
            "리스크",
            "위험",
            "규제",
            "영향",
            "요인",
            "원인",
            "이유",
            "메커니즘",
        )
    )


def _research_pack_search_diagnostics(
    research_pack: Mapping[str, Any],
    *,
    mode: str,
    discovery_skipped: bool,
    query_context_started_at: float,
    planning_started_at: float,
) -> dict[str, Any]:
    diagnostics: dict[str, Any] = {
        "mode": mode,
        "discovery_skipped": discovery_skipped,
        "timing_ms": {
            "query_context_planning": int((time.perf_counter() - planning_started_at) * 1000),
            "query_context_total": int((time.perf_counter() - query_context_started_at) * 1000),
        },
    }
    metric_pack = research_pack.get("metric_series_pack")
    if isinstance(metric_pack, Mapping):
        diagnostics["metric_series"] = {
            "mode": metric_pack.get("mode"),
            "result_count": metric_pack.get("result_count"),
            "roles": metric_pack.get("roles") or [],
            "dimension_anchors": metric_pack.get("dimension_anchors") or [],
            "period_years": metric_pack.get("period_years") or [],
            "quality": metric_pack.get("quality") or {},
        }
    projection_pack = research_pack.get("projection_pack")
    if isinstance(projection_pack, Mapping):
        diagnostics["typed_projection"] = {
            "projection_used": projection_pack.get("table"),
            "projection_candidate_count": projection_pack.get("result_count"),
            "projection_sufficient": bool(projection_pack.get("result_count")),
            "matched_terms": projection_pack.get("matched_terms") or [],
            "quality": projection_pack.get("quality") or {},
        }
    return diagnostics


def _research_agent_autonomy(
    research_status: str,
    *,
    needs_trace: bool,
    missing_parts: Sequence[str] | None = None,
) -> dict[str, Any]:
    missing = list(missing_parts or [])
    if research_status == "out_of_scope_for_filing_ontology":
        return {
            "mode": "bounded",
            "may_continue_research": False,
            "allowed_next_tools": [],
            "disallowed_next_tools": [
                "krw_ontology_query",
                "krw_ontology_retrieve",
                "krw_ontology_compare",
                "krw_ontology_trace",
                "krw_ontology_chain",
            ],
            "max_additional_tool_calls": 0,
        }
    if research_status == "sufficient_for_default_answer":
        return {
            "mode": "bounded",
            "may_continue_research": False,
            "allowed_next_tools": [],
            "disallowed_next_tools": [
                "krw_ontology_retrieve",
                "unscoped_krw_ontology_query",
                "krw_ontology_trace",
                "krw_ontology_chain",
            ],
            "max_additional_tool_calls": 0,
            "purpose": "Research state is sufficient for the default answer; do not restart search.",
        }
    if research_status == "sufficient_but_trace_recommended":
        return {
            "mode": "bounded",
            "may_continue_research": True,
            "allowed_next_tools": [
                "krw_ontology_query",
                "krw_ontology_trace",
                "krw_ontology_chain",
            ],
            "disallowed_next_tools": ["krw_ontology_retrieve", "unscoped_krw_ontology_query"],
            "max_additional_tool_calls": 3,
            "purpose": "Verify selected roots and use bounded chain expansion. Use one targeted query only when the selected roots miss the requested latest period or a listed missing part.",
        }
    return {
        "mode": "bounded",
        "may_continue_research": True,
        "allowed_next_tools": ["krw_ontology_query", "krw_ontology_trace", "krw_ontology_chain"],
        "disallowed_next_tools": ["broad_unscoped_retrieve"],
        "max_additional_tool_calls": 3 if missing else 2,
        "purpose": "Fill only listed missing parts with targeted searches.",
    }


def _research_do_not_call(research_status: str) -> list[str]:
    if research_status == "out_of_scope_for_filing_ontology":
        return [
            "krw_ontology_query",
            "krw_ontology_retrieve",
            "krw_ontology_compare",
            "krw_ontology_trace",
            "krw_ontology_chain",
        ]
    if research_status == "sufficient_for_default_answer":
        return [
            "krw_ontology_retrieve",
            "unscoped_krw_ontology_query",
            "krw_ontology_trace",
            "krw_ontology_chain",
        ]
    if research_status == "sufficient_but_trace_recommended":
        return ["krw_ontology_retrieve", "unscoped_krw_ontology_query"]
    return ["broad_unscoped_retrieve"]


def _merge_bundle_lists(
    *bundle_lists: Sequence[Mapping[str, Any]], limit: int
) -> list[dict[str, Any]]:
    merged: list[dict[str, Any]] = []
    seen: set[str] = set()
    for bundles in bundle_lists:
        for bundle in bundles:
            object_id = str(bundle.get("id") or "")
            if not object_id or object_id in seen:
                continue
            seen.add(object_id)
            merged.append(dict(bundle))
            if len(merged) >= max(1, int(limit)):
                return merged
    return merged


def _comparison_evaluation_from_items(
    items: Sequence[Mapping[str, Any]], *, metric: str | None
) -> dict[str, Any]:
    if not items:
        return {
            "direct_answerable": False,
            "related_context_available": False,
            "negative_answer_supported": False,
            "recommended_answer_mode": "not_answerable",
            "tier": "not_answerable",
            "trace_status": "untraced",
            "why_tier": "No matching ontology objects were returned for this comparison key.",
        }
    first = items[0]
    tier = "traceable_metric_lineage" if metric else None
    evidence = first.get("evidence") or {}
    if tier is None:
        if evidence.get("metric_lineage"):
            tier = "traceable_metric_lineage"
        elif first.get("type") in {"EvidenceQuote", "ResearchClaim"}:
            tier = "traceable_direct"
        elif evidence.get("claims") or evidence.get("quotes"):
            tier = "traceable_related"
        else:
            tier = "untraced_direct_candidate"
    trace_status = (
        "traceable"
        if tier in {"traceable_direct", "traceable_metric_lineage", "traceable_related"}
        else "untraced"
    )
    return {
        "direct_answerable": tier in {"traceable_direct", "traceable_metric_lineage"},
        "related_context_available": tier == "traceable_related",
        "negative_answer_supported": False,
        "recommended_answer_mode": "direct_answer"
        if tier in {"traceable_direct", "traceable_metric_lineage"}
        else "related_context_only",
        "semantic_relevance": "direct"
        if tier in {"traceable_direct", "traceable_metric_lineage"}
        else "related",
        "trace_status": trace_status,
        "tier": tier,
        "matched_required_facets": [],
        "missing_required_facets": [],
        "why_tier": "Metric comparison uses traceable metric lineage."
        if tier == "traceable_metric_lineage"
        else "Comparison candidate inferred from returned evidence support.",
        "evidence_chain_count": 1 if trace_status == "traceable" else 0,
        "support_quote_count": len(evidence.get("quotes") or []),
        "support_claim_count": len(evidence.get("claims") or []),
    }


def _apply_comparison_evaluation(
    items: Sequence[dict[str, Any]], evaluation: Mapping[str, Any]
) -> None:
    for item in items:
        for key in (
            "semantic_relevance",
            "trace_status",
            "tier",
            "matched_required_facets",
            "missing_required_facets",
            "why_tier",
            "evidence_chain_count",
            "support_depth",
            "support_quote_count",
            "support_claim_count",
        ):
            if item.get(key) is None and evaluation.get(key) is not None:
                item[key] = evaluation.get(key)


def _question_requires_direct_match(question: str) -> bool:
    normalized = str(question or "").lower()
    return any(
        term in normalized
        for term in (
            "direct exposure",
            "directly exposed",
            "directly affect",
            "직접",
            "직접 노출",
            "직접 영향",
            "직접적으로",
        )
    )


def _recommended_trace_tools(
    candidates: Sequence[Mapping[str, Any]], *, limit: int
) -> list[dict[str, Any]]:
    occurrences: list[tuple[str, str | None]] = []
    for candidate in candidates:
        ticker = str(candidate.get("ticker") or "").strip().upper() or None
        for key in ("top_traceable_object_ids", "source_object_ids", "primary_object_id"):
            for value in _collect_values([candidate], key):
                if isinstance(value, list):
                    occurrences.extend((str(item), ticker) for item in value if item)
                elif value:
                    occurrences.append((str(value), ticker))
    selected = list(dict.fromkeys(occurrences))
    selected = sorted(
        selected,
        key=lambda occurrence: (_object_id_recency_key(occurrence[0]), occurrence[1] or ""),
        reverse=True,
    )[:limit]
    return [
        {
            "tool": "krw_ontology_trace",
            "object_id": object_id,
            **({"ticker": ticker} if ticker else {}),
            "purpose": "verify_final_or_related_context",
            "internal_only": True,
        }
        for object_id, ticker in selected
    ]


def _object_id_recency_key(object_id: str) -> tuple[int, int, int]:
    text = str(object_id or "").upper()
    match = re.search(r"(?:CY|FY)?(20\d{2}|19\d{2})(?:Q([1-4]))?", text)
    year = int(match.group(1)) if match else 0
    quarter = int(match.group(2) or 0) if match else 0
    doc_score = (
        2 if "10-Q" in text or "10Q" in text else 1 if "10-K" in text or "10K" in text else 0
    )
    return (year, quarter, doc_score)


def _final_answer_guidance(
    answerability: Mapping[str, Any], candidates: Sequence[Mapping[str, Any]]
) -> dict[str, Any]:
    missing: list[str] = []
    for value in _collect_values(candidates, "missing_required_facets"):
        if isinstance(value, list):
            missing.extend(str(item) for item in value if item)
        elif value:
            missing.append(str(value))
    guidance = {
        "use_direct_evidence_only_when": "tier is traceable_direct or traceable_metric_lineage",
        "do_not_expose_internal_ids": True,
    }
    if answerability.get("recommended_answer_mode") == "no_direct_evidence_with_related_context":
        guidance["safe_answer_pattern"] = (
            "공시자료에서 요청한 직접 노출은 확인되지 않습니다. "
            "다만 관련 맥락은 별도로 구분해 설명할 수 있습니다."
        )
        guidance["missing_required_facets"] = sorted(set(missing))
    elif answerability.get("direct_answerable"):
        guidance["safe_answer_pattern"] = (
            "공시자료에서 직접 확인되는 내용과 영향을 받는 사업/재무 채널을 함께 설명합니다."
        )
    else:
        guidance["safe_answer_pattern"] = (
            "현재 검색 맥락만으로는 단정하지 말고 확인되는 관련 맥락만 제한적으로 설명합니다."
        )
    return guidance


def _collect_values(value: Any, key: str) -> list[Any]:
    found: list[Any] = []
    if isinstance(value, Mapping):
        if key in value:
            found.append(value[key])
        for child in value.values():
            found.extend(_collect_values(child, key))
    elif isinstance(value, list):
        for child in value:
            found.extend(_collect_values(child, key))
    return found


def _ticker_coverage(conn: sqlite3.Connection) -> dict[str, dict[str, Any]]:
    document_counts = {
        str(row["ticker"]): int(row["count"])
        for row in conn.execute(
            "SELECT ticker, COUNT(*) AS count FROM documents GROUP BY ticker"
        ).fetchall()
    }
    object_counts = {
        str(row["ticker"]): int(row["count"])
        for row in conn.execute(
            "SELECT ticker, COUNT(*) AS count FROM objects GROUP BY ticker"
        ).fetchall()
    }
    coverage: dict[str, dict[str, Any]] = {}
    for ticker in sorted(set(document_counts) | set(object_counts)):
        document_count = document_counts.get(ticker, 0)
        object_count = object_counts.get(ticker, 0)
        status = "ready"
        warnings: list[str] = []
        if object_count and not document_count:
            status = "inconsistent"
            warnings.append("objects_without_documents")
        elif document_count and not object_count:
            status = "inconsistent"
            warnings.append("documents_without_objects")
        elif not document_count and not object_count:
            status = "not_indexed"
        coverage[ticker] = {
            "has_documents": bool(document_count),
            "has_objects": bool(object_count),
            "document_count": document_count,
            "object_count": object_count,
            "status": status,
            "warnings": warnings,
        }
    return coverage


def _normalize_period(period: str) -> list[str]:
    """Expand a period label to include both FY and CY variants.

    Filings are indexed by calendar year (CY2023) but the AI may request
    fiscal year (FY2023). Return both forms so the SQL IN filter matches
    regardless of which convention the caller used.
    """
    raw = str(period or "").strip().upper()
    if not raw:
        return []
    # Already CY — return as-is plus FY variant
    if raw.startswith("CY"):
        return [raw, "FY" + raw[2:]]
    # FY — return as-is plus CY variant
    if raw.startswith("FY"):
        return [raw, "CY" + raw[2:]]
    # Bare year (e.g. "2023") — expand to both FY and CY
    if raw.isdigit():
        return [f"FY{raw}", f"CY{raw}"]
    return [raw]


def _normalize_periods(periods: Iterable[str] | None) -> list[str]:
    """Expand period labels to FY+CY variants and deduplicate."""
    if not periods:
        return []
    expanded: list[str] = []
    seen: set[str] = set()
    for period in periods:
        for variant in _normalize_period(period):
            if variant not in seen:
                seen.add(variant)
                expanded.append(variant)
    return expanded


def _object_filters(
    *,
    tickers: Iterable[str] | None,
    document_types: Iterable[str] | None,
    periods: Iterable[str] | None,
    object_types: Iterable[str] | None,
    include_rejected: bool,
) -> tuple[str, list[Any]]:
    parts = ["1 = 1"]
    params: list[Any] = []
    _add_in_filter(parts, params, "objects.ticker", [t.upper() for t in tickers or []])
    _add_in_filter(parts, params, "objects.document_type", list(document_types or []))
    _add_in_filter(parts, params, "objects.period", _normalize_periods(periods))
    _add_in_filter(parts, params, "objects.type", list(object_types or []))
    if not include_rejected:
        parts.append("(objects.review_status IS NULL OR objects.review_status != 'rejected')")
    return f"WHERE {' AND '.join(parts)}", params


def _company_topic_filters(
    *,
    tickers: Iterable[str] | None,
    document_types: Iterable[str] | None,
    periods: Iterable[str] | None,
) -> tuple[str, list[Any]]:
    parts = ["1 = 1"]
    params: list[Any] = []
    _add_in_filter(parts, params, "company_topic_index.ticker", [t.upper() for t in tickers or []])
    _add_in_filter(parts, params, "company_topic_index.document_type", list(document_types or []))
    _add_in_filter(parts, params, "company_topic_index.period", _normalize_periods(periods))
    return f"WHERE {' AND '.join(parts)}", params


def _combined_discovery_tier(semantic_relevance: str, trace_status: str) -> str:
    if semantic_relevance == "insufficient":
        return "insufficient"
    if trace_status in {"traceable", "traceable_metric_lineage"}:
        return "traceable_direct" if semantic_relevance == "direct" else "traceable_related"
    if semantic_relevance == "direct":
        return "untraced_direct_candidate"
    if trace_status in {"orphan", "untraced", "untraced_metric_candidate", "unknown"}:
        return "untraced_related"
    return "untraced_related"


def _why_discovery_tier(semantic_relevance: str, trace_status: str, tier: str) -> str:
    if tier == "traceable_direct":
        return "Matched query core premise and has an explicit evidence or metric trace."
    if tier == "untraced_direct_candidate":
        return "Matched query core premise, but no explicit evidence trace was available in the serving index."
    if tier == "traceable_related":
        return "Related to the query and has an explicit evidence or metric trace."
    if tier == "untraced_related":
        return "Related to the query, but evidence traceability is missing or weak."
    return f"semantic_relevance={semantic_relevance}, trace_status={trace_status}"


def _is_traceable_status(trace_status: Any) -> bool:
    return str(trace_status or "") in {"traceable", "traceable_metric_lineage"}


def _ticker_trace_status(topics: Iterable[Mapping[str, Any]]) -> str:
    statuses = [str(topic.get("trace_status") or "unknown") for topic in topics]
    if any(_is_traceable_status(status) for status in statuses):
        return "traceable"
    if any(status == "orphan" for status in statuses):
        return "orphan"
    return statuses[0] if statuses else "unknown"


def _topic_trace_counts(topics: Iterable[Mapping[str, Any]]) -> dict[str, int]:
    counts = {"traceable": 0, "untraced": 0, "orphan": 0, "evidence_chains": 0}
    for topic in topics:
        status = str(topic.get("trace_status") or "unknown")
        if _is_traceable_status(status):
            counts["traceable"] += 1
        elif status == "orphan":
            counts["orphan"] += 1
        else:
            counts["untraced"] += 1
        counts["evidence_chains"] += int(topic.get("evidence_chain_count") or 0)
    return counts


def _traceable_object_ids(topics: Iterable[Mapping[str, Any]]) -> list[str]:
    ids: list[str] = []
    for topic in topics:
        if not _is_traceable_status(topic.get("trace_status")):
            continue
        object_id = topic.get("primary_object_id")
        if object_id and object_id not in ids:
            ids.append(str(object_id))
    return ids


def _untraced_object_ids(topics: Iterable[Mapping[str, Any]]) -> list[str]:
    ids: list[str] = []
    for topic in topics:
        if _is_traceable_status(topic.get("trace_status")):
            continue
        object_id = topic.get("primary_object_id")
        if object_id and object_id not in ids:
            ids.append(str(object_id))
    return ids


def _topic_object_counts(topics: Iterable[Mapping[str, Any]]) -> dict[str, int]:
    counts: dict[str, int] = {}
    for topic in topics:
        object_types = topic.get("dominant_object_types") or [topic.get("primary_object_type")]
        for object_type in object_types:
            key = str(object_type or "Unknown")
            counts[key] = counts.get(key, 0) + 1
    return dict(sorted(counts.items()))


def _topic_evidence_counts(topics: Iterable[Mapping[str, Any]]) -> dict[str, int]:
    counts = {"quotes": 0, "claims": 0, "topics": 0}
    for topic in topics:
        counts["topics"] += 1
        counts["quotes"] += int(topic.get("support_quote_count") or 0)
        counts["claims"] += int(topic.get("support_claim_count") or 0)
    return counts


def _topic_reason(topic: Mapping[str, Any]) -> dict[str, Any]:
    match = topic.get("match") or {}
    reason: dict[str, Any] = {
        "topic_id": topic.get("topic_id"),
        "topic_label": topic.get("topic_label"),
        "object_id": topic.get("primary_object_id"),
        "object_type": topic.get("primary_object_type"),
        "tier": match.get("tier"),
        "semantic_relevance": match.get("semantic_relevance"),
        "trace_status": topic.get("trace_status"),
        "trace_method": topic.get("trace_method"),
        "evidence_chain_count": topic.get("evidence_chain_count") or 0,
        "support_depth": topic.get("support_depth"),
        "score": match.get("score"),
        "matched_core_terms": match.get("matched_core_terms") or [],
        "matched_mechanisms": match.get("matched_mechanisms") or [],
        "matched_impact_channels": match.get("matched_impact_channels") or [],
        "matched_generic_terms": match.get("matched_generic_terms") or [],
        "missing_required_facets": match.get("missing_required_facets") or [],
        "evidence_strength": topic.get("evidence_strength"),
        "source_object_ids": topic.get("source_object_ids") or [],
    }
    if match.get("why_direct"):
        reason["why_direct"] = match.get("why_direct")
    if match.get("why_not_direct"):
        reason["why_not_direct"] = match.get("why_not_direct")
    if match.get("why_tier"):
        reason["why_tier"] = match.get("why_tier")
    return reason


def _topic_payload_from_compact_bundle(
    bundle: Mapping[str, Any],
    *,
    query_frame: Any,
    source: str,
) -> dict[str, Any] | None:
    trace_status = str(bundle.get("trace_status") or "unknown")
    topic_like = {
        "topic_id": f"{source}:{bundle.get('id')}",
        "topic_label": str(bundle.get("type") or "Object evidence"),
        "topic_summary": str(bundle.get("text") or "")[:500],
        "topic_text": str(bundle.get("text") or ""),
        "facet_text": str(bundle.get("text") or ""),
        "impact_channels": [],
        "primary_object_id": bundle.get("id"),
        "primary_object_type": bundle.get("type"),
        "ticker": bundle.get("ticker"),
        "evidence_strength": (bundle.get("quality") or {}).get("evidence_grade") or source,
        "support_quote_count": bundle.get("support_quote_count") or 0,
        "support_claim_count": bundle.get("support_claim_count") or 0,
        "specificity_score": 0.6,
    }
    match = classify_topic_match(query_frame, build_evidence_frame(topic_like))
    if match["tier"] == "insufficient":
        return None
    semantic_relevance = str(match.get("tier") or "insufficient")
    final_tier = _combined_discovery_tier(semantic_relevance, trace_status)
    traceable = _is_traceable_status(trace_status)
    match = {
        **match,
        "semantic_relevance": semantic_relevance,
        "trace_status": trace_status,
        "tier": final_tier,
        "why_tier": _why_discovery_tier(semantic_relevance, trace_status, final_tier),
        "projection_source": source,
    }
    object_id = bundle.get("id")
    return {
        "topic_id": topic_like["topic_id"],
        "ticker": bundle.get("ticker"),
        "period": bundle.get("period"),
        "document_type": bundle.get("document_type"),
        "topic_label": topic_like["topic_label"],
        "topic_summary": topic_like["topic_summary"],
        "primary_object_id": object_id,
        "primary_object_type": bundle.get("type"),
        "source_object_ids": [object_id] if object_id else [],
        "dominant_object_types": [bundle.get("type")] if bundle.get("type") else [],
        "impact_channels": [],
        "evidence_strength": source,
        "support_quote_count": bundle.get("support_quote_count") or 0,
        "support_claim_count": bundle.get("support_claim_count") or 0,
        "trace_status": trace_status,
        "evidence_chain_count": bundle.get("evidence_chain_count") or 0,
        "support_depth": bundle.get("support_depth"),
        "support_link_count": bundle.get("support_link_count") or 0,
        "trace_method": source,
        "metric_lineage_status": bundle.get("metric_lineage_status"),
        "answer_candidate": bool(bundle.get("answer_candidate")),
        "specificity_score": 0.6,
        "generic_score": 0.0,
        "boilerplate_score": 0.0,
        "materiality_hint": None,
        "materiality_score": None,
        "topic_type": source,
        "topic_family": source,
        "factor_terms": [],
        "metric_terms": [],
        "entity_terms": [],
        "mechanism_terms": [],
        "scenario_terms": [],
        "top_traceable_object_ids": [object_id] if traceable and object_id else [],
        "untraced_object_ids": [] if traceable or not object_id else [object_id],
        "match": match,
    }


def _scope_where(
    *,
    ticker: str | None,
    tickers: Iterable[str] | None = None,
    document_types: Iterable[str] | None,
) -> tuple[str, list[Any]]:
    parts: list[str] = []
    params: list[Any] = []
    if ticker:
        parts.append("ticker = ?")
        params.append(ticker.upper())
    elif tickers:
        ticker_values = list(
            dict.fromkeys(
                str(value).strip().upper() for value in tickers if str(value or "").strip()
            )
        )
        _add_in_filter(parts, params, "ticker", ticker_values)
    if document_types:
        values = [str(value) for value in document_types if str(value or "").strip()]
        _add_in_filter(parts, params, "document_type", values)
    return (f"WHERE {' AND '.join(parts)}" if parts else ""), params


def _add_in_filter(parts: list[str], params: list[Any], column: str, values: list[Any]) -> None:
    if not values:
        return
    placeholders = ",".join("?" for _ in values)
    parts.append(f"{column} IN ({placeholders})")
    params.extend(values)


def _fts_query(topic: str, *, operator: str) -> str:
    terms = _query_terms(topic)
    if not terms:
        return ""
    if operator == "OR":
        return " OR ".join(f"{term}*" for term in terms)
    return " ".join(f"{term}*" for term in terms)


def _scoped_fts_query(
    fts_query: str,
    *,
    tickers: Iterable[str] | None = None,
    document_types: Iterable[str] | None = None,
    periods: Iterable[str] | None = None,
    object_types: Iterable[str] | None = None,
) -> str:
    scope_terms = [
        *(_scope_tokens("ticker", tickers) if tickers else []),
        *(_scope_tokens("doctype", document_types) if document_types else []),
        *(_scope_tokens("period", _normalize_periods(periods)) if periods else []),
        *(_scope_tokens("otype", object_types) if object_types else []),
    ]
    if not fts_query or not scope_terms:
        return fts_query
    scope = " AND ".join(scope_terms)
    if " OR " in fts_query:
        return f"{scope} AND ({fts_query})"
    return f"{scope} AND {fts_query}"


def _scope_tokens(prefix: str, values: Iterable[Any]) -> list[str]:
    tokens: list[str] = []
    for value in values:
        token = _scope_token(prefix, value)
        if token:
            tokens.append(token)
    return _unique(tokens)


def _scope_token(prefix: str, value: Any) -> str:
    normalized = re.sub(r"[^a-z0-9]+", "_", str(value or "").lower()).strip("_")
    if not normalized:
        return ""
    return f"{prefix}_{normalized}"


def _discovery_search_topic(topic: str | None) -> str:
    terms = [term for term in _query_terms(topic) if term not in _SPLIT_TOPIC_STOP_TERMS]
    if len(terms) >= 2:
        return " ".join(terms)
    return " ".join(str(topic or "").split())


def _query_terms(topic: str | None) -> list[str]:
    return _unique(term.lower() for term in _TERM_RE.findall(topic or "") if len(term) > 1)


def _planned_retrieval_terms(
    retrieval_query: str,
    *,
    retrieval_terms: Iterable[str] | None,
) -> list[str]:
    """Return bounded lexical tokens without domain-specific query expansion."""
    hint_terms = _unique(
        term for value in retrieval_terms or [] for term in _planned_query_terms(str(value))
    )
    if hint_terms:
        return hint_terms[:64]
    return _planned_query_terms(retrieval_query)[:64]


def _planned_evidence_terms(
    retrieval_query: str,
    *,
    retrieval_terms: Iterable[str] | None,
) -> list[str]:
    hint_terms = _unique(
        term for value in retrieval_terms or [] for term in _planned_query_terms(str(value))
    )
    return (hint_terms or _planned_query_terms(retrieval_query))[:64]


def _planned_query_terms(value: str | None) -> list[str]:
    return _unique(
        term.casefold() for term in _PLANNED_TERM_RE.findall(value or "") if len(term) > 1
    )


def _planned_metric_period_coordinates(
    periods: Iterable[str],
) -> list[tuple[int, int | None]]:
    coordinates: list[tuple[int, int | None]] = []
    for period in periods:
        match = re.search(
            r"(?:CY|FY)?(19\d{2}|20\d{2})(?:Q([1-4]))?",
            str(period),
            flags=re.IGNORECASE,
        )
        if not match:
            continue
        value = (int(match.group(1)), int(match.group(2)) if match.group(2) else None)
        if value not in coordinates:
            coordinates.append(value)
    return coordinates


def _select_planned_metric_rows(
    rows: Sequence[sqlite3.Row],
    *,
    limit_per_group: int,
    calculation_window: str | None,
    comparison_axes: set[str],
    requested_periods: Sequence[str],
) -> list[sqlite3.Row]:
    """Select atomic conflict sets or compatible period bundles per metric series."""
    temporal = calculation_window in {"period_over_period", "year_over_year"} and (
        not comparison_axes
        or bool(comparison_axes.intersection({"absolute_change", "growth_rate"}))
    )
    grouped: dict[tuple[str, str], list[sqlite3.Row]] = {}
    for row in rows:
        metric = str(row["planned_metric_canonical_metric"] or row["metric_name"] or "").casefold()
        ticker = str(row["ticker"] or "").upper()
        grouped.setdefault((ticker, metric), []).append(row)

    selected_by_group: dict[tuple[str, str], list[sqlite3.Row]] = {}
    for group_key, group_rows in grouped.items():
        by_series: dict[tuple[Any, ...], list[sqlite3.Row]] = {}
        for row in group_rows:
            by_series.setdefault(
                (
                    str(row["planned_metric_unit"] or ""),
                    str(row["planned_metric_dimensions_json"] or ""),
                    int(row["planned_metric_is_company_total"] or 0),
                    str(row["planned_metric_observation_period_type"] or "unknown"),
                ),
                [],
            ).append(row)

        bundles: list[tuple[bool, tuple[int, int, str], str, list[sqlite3.Row]]] = []
        for series_key, series_rows in by_series.items():
            by_context: dict[str, list[sqlite3.Row]] = {}
            for row in series_rows:
                by_context.setdefault(
                    str(row["planned_metric_observation_context_key"] or ""),
                    [],
                ).append(row)
            context_rows = [
                sorted(values, key=lambda row: str(row["id"])) for values in by_context.values()
            ]
            context_rows.sort(
                key=lambda values: _planned_metric_row_period_key(values[0]),
                reverse=True,
            )
            requested_contexts = [
                values
                for values in context_rows
                if not requested_periods
                or _planned_metric_row_matches_periods(values[0], requested_periods)
            ]
            considered = requested_contexts or context_rows
            conflict_contexts = [
                values
                for values in considered
                if int(values[0]["planned_metric_conflict_value_count"] or 0) > 1
            ]
            if conflict_contexts:
                conflict = conflict_contexts[0]
                bundles.append(
                    (
                        True,
                        _planned_metric_row_period_key(conflict[0]),
                        repr(series_key),
                        conflict,
                    )
                )
                continue

            representatives = [values[0] for values in considered]
            if temporal:
                compatible_pairs = [
                    (previous, current)
                    for previous_index, previous in enumerate(reversed(representatives))
                    for current in list(reversed(representatives))[previous_index + 1 :]
                    if _planned_metric_rows_match_window(
                        previous,
                        current,
                        calculation_window=calculation_window,
                    )
                ]
                if requested_periods and len(representatives) >= 2:
                    bundle = sorted(
                        representatives,
                        key=_planned_metric_row_period_key,
                        reverse=True,
                    )
                elif compatible_pairs:
                    previous, current = max(
                        compatible_pairs,
                        key=lambda pair: (
                            _planned_metric_row_period_key(pair[1]),
                            _planned_metric_row_period_key(pair[0]),
                        ),
                    )
                    bundle = [current, previous]
                else:
                    bundle = representatives[:2]
            else:
                bundle = representatives[: max(2, len(requested_periods))]
            if bundle:
                bundles.append(
                    (
                        False,
                        max(_planned_metric_row_period_key(row) for row in bundle),
                        repr(series_key),
                        bundle,
                    )
                )

        bundles.sort(
            key=lambda item: (
                not item[0],
                tuple(-value if isinstance(value, int) else value for value in item[1][:2]),
                item[2],
            )
        )
        selected: list[sqlite3.Row] = []
        for _conflict, _period_key, _series_key, bundle in bundles:
            remaining = limit_per_group - len(selected)
            if remaining <= 0:
                break
            selected.extend(bundle[:remaining])
        selected_by_group[group_key] = selected

    result: list[sqlite3.Row] = []
    group_keys = sorted(selected_by_group)
    row_index = 0
    while True:
        added = False
        for group_key in group_keys:
            group_rows = selected_by_group[group_key]
            if row_index >= len(group_rows):
                continue
            result.append(group_rows[row_index])
            added = True
        if not added:
            break
        row_index += 1
    return result


def _planned_metric_row_period_key(row: sqlite3.Row) -> tuple[int, int, str]:
    return (
        int(row["planned_metric_fiscal_year"] or 0),
        int(row["planned_metric_fiscal_quarter"] or 0),
        str(row["planned_metric_observation_period"] or ""),
    )


def _planned_metric_row_matches_periods(
    row: sqlite3.Row,
    periods: Sequence[str],
) -> bool:
    observation_period = str(row["planned_metric_observation_period"] or "").casefold()
    if observation_period in {str(period).casefold() for period in periods}:
        return True
    coordinates = _planned_metric_period_coordinates(periods)
    row_coordinate = (
        int(row["planned_metric_fiscal_year"] or 0),
        int(row["planned_metric_fiscal_quarter"])
        if row["planned_metric_fiscal_quarter"] is not None
        else None,
    )
    return row_coordinate in coordinates


def _planned_metric_rows_match_window(
    previous: sqlite3.Row,
    current: sqlite3.Row,
    *,
    calculation_window: str | None,
) -> bool:
    previous_year, previous_quarter, _ = _planned_metric_row_period_key(previous)
    current_year, current_quarter, _ = _planned_metric_row_period_key(current)
    if not previous_year or not current_year:
        return False
    period_type = str(previous["planned_metric_observation_period_type"] or "unknown")
    if period_type != str(current["planned_metric_observation_period_type"] or "unknown"):
        return False
    if period_type == "year_to_date" and calculation_window != "year_over_year":
        return False
    if previous_quarter == current_quarter == 0:
        return current_year - previous_year == 1
    if not previous_quarter or not current_quarter:
        return False
    if calculation_window == "year_over_year":
        return current_year - previous_year == 1 and current_quarter == previous_quarter
    return (current_year * 4 + current_quarter) - (previous_year * 4 + previous_quarter) == 1


def _query_wants_current_document_prior(topic: str | None) -> bool:
    text = str(topic or "").strip()
    if not text:
        return False
    lowered = text.lower()
    terms = set(_query_terms(text))
    if terms.intersection(_CURRENT_FILING_INTENT_TERMS):
        return True
    return any(marker in lowered or marker in text for marker in _CURRENT_FILING_INTENT_MARKERS)


def _query_has_explicit_period(topic: str | None, periods: Iterable[str] | None) -> bool:
    if any(str(period or "").strip() for period in periods or []):
        return True
    text = str(topic or "")
    return bool(re.search(r"\b(?:CY|FY)?(?:19|20)\d{2}(?:Q[1-4])?\b", text, flags=re.IGNORECASE))


def _current_document_prior_candidate_limit(limit: int, prior: Mapping[str, Any]) -> int:
    resolved = max(1, int(limit))
    if not prior.get("enabled"):
        return resolved
    return max(resolved, min(max(resolved * 4, resolved + 12), 80))


def _period_recency_key(period: Any) -> tuple[int, int]:
    text = str(period or "").upper().strip()
    match = re.search(r"(?:CY|FY)?((?:19|20)\d{2})(?:\s*Q([1-4]))?", text)
    if not match:
        match = re.search(r"Q([1-4]).*((?:19|20)\d{2})", text)
        if match:
            return int(match.group(2)), int(match.group(1))
        return 0, 0
    year = int(match.group(1))
    quarter = int(match.group(2) or 5)
    return year, quarter


def _document_type_recency_priority(document_type: Any) -> int:
    return _DOCUMENT_TYPE_RECENCY_PRIORITY.get(str(document_type or "").upper(), 0)


def _document_recency_key(document: Mapping[str, Any]) -> tuple[tuple[int, int], int]:
    return (
        _period_recency_key(document.get("period")),
        _document_type_recency_priority(
            document.get("document_type") or document.get("doc_type_key")
        ),
    )


def _document_anchor(document: Mapping[str, Any], *, ticker: str, role: str) -> dict[str, Any]:
    document_type = document.get("document_type") or document.get("doc_type_key")
    period = document.get("period")
    return {
        "ticker": ticker,
        "period": period,
        "document_type": document_type,
        "role": role,
        "source_label": " ".join(
            str(part)
            for part in (
                ticker,
                period,
                document_type,
            )
            if part
        ),
    }


def filing_document_roles_from_documents(
    documents: Sequence[Mapping[str, Any]],
    *,
    tickers: Iterable[str] | None = None,
) -> dict[str, dict[str, Any]]:
    requested = {str(ticker).upper() for ticker in tickers or [] if str(ticker or "").strip()}
    by_ticker: dict[str, list[Mapping[str, Any]]] = {}
    for document in documents:
        ticker = str(document.get("ticker") or "").upper()
        document_type = str(
            document.get("document_type") or document.get("doc_type_key") or ""
        ).upper()
        period = str(document.get("period") or "").strip()
        if not ticker or (requested and ticker not in requested):
            continue
        if document_type not in _FILING_ROLE_DOCUMENT_TYPES:
            continue
        if not period or period.upper() == "ALL":
            continue
        by_ticker.setdefault(ticker, []).append(document)

    roles: dict[str, dict[str, Any]] = {}
    for ticker, ticker_documents in sorted(by_ticker.items()):
        latest_available = max(ticker_documents, key=_document_recency_key)
        annual_candidates = [
            document
            for document in ticker_documents
            if str(document.get("document_type") or document.get("doc_type_key") or "").upper()
            == "10-K"
        ]
        annual_baseline = (
            max(annual_candidates, key=_document_recency_key) if annual_candidates else None
        )
        role_payload: dict[str, Any] = {
            "ticker": ticker,
            "current_driver": _document_anchor(
                latest_available, ticker=ticker, role="current_driver"
            ),
            "latest_available": _document_anchor(
                latest_available, ticker=ticker, role="latest_available"
            ),
            "available_document_types": sorted(
                {
                    str(document.get("document_type") or document.get("doc_type_key") or "").upper()
                    for document in ticker_documents
                    if document.get("document_type") or document.get("doc_type_key")
                }
            ),
            "policy": (
                "Use current_driver for recent changes/current quarter evidence. "
                "Use annual_baseline for business mix, segment structure, and long-term baseline."
            ),
        }
        if annual_baseline is not None:
            role_payload["annual_baseline"] = _document_anchor(
                annual_baseline,
                ticker=ticker,
                role="annual_baseline",
            )
        else:
            role_payload["annual_baseline"] = None
        roles[ticker] = role_payload
    return roles


def _current_driver_anchors_from_roles(roles: Mapping[str, Any]) -> dict[str, dict[str, Any]]:
    anchors: dict[str, dict[str, Any]] = {}
    for ticker, role_payload in roles.items():
        if not isinstance(role_payload, Mapping):
            continue
        current_driver = role_payload.get("current_driver")
        if isinstance(current_driver, Mapping):
            anchors[str(ticker)] = dict(current_driver)
    return anchors


def latest_document_anchors_from_documents(
    documents: Sequence[Mapping[str, Any]],
    *,
    tickers: Iterable[str] | None = None,
) -> dict[str, dict[str, Any]]:
    roles = filing_document_roles_from_documents(documents, tickers=tickers)
    return _current_driver_anchors_from_roles(roles)


def _normalize_metric_lookup_topic(
    topic: str | None,
    periods: Iterable[str] | None,
) -> tuple[str | None, dict[str, Any]]:
    original = " ".join(str(topic or "").split())
    if not original:
        return topic, {}
    period_years = _metric_lookup_years(periods)
    removed_tokens: list[str] = []

    def replace_period_token(match: re.Match[str]) -> str:
        token = match.group(0)
        removed_tokens.append(token)
        return " "

    normalized = re.sub(
        r"\b(?:CY|FY)?(?:19|20)\d{2}(?:Q[1-4])?\b",
        replace_period_token,
        original,
        flags=re.IGNORECASE,
    )
    topic_years = sorted({int(year) for year in re.findall(r"\b((?:19|20)\d{2})\b", original)})
    normalized = " ".join(normalized.split())
    diagnostics: dict[str, Any] = {
        "original_topic": original,
        "normalized_topic": normalized,
        "removed_period_tokens": removed_tokens,
        "reason": "metric_query_period_tokens_are_filters_not_fts_terms",
    }
    if period_years:
        diagnostics["period_years"] = period_years
        topic_only_years = [year for year in topic_years if year not in period_years]
        if topic_only_years:
            diagnostics["warning"] = "topic_years_differ_from_period_filters"
            diagnostics["topic_only_years"] = topic_only_years
    return normalized or topic, diagnostics


def _metric_lookup_period_filters(topic: str | None, periods: Iterable[str] | None) -> list[str]:
    explicit_periods = [str(period) for period in periods or [] if str(period or "").strip()]
    if explicit_periods:
        return _unique(explicit_periods)
    years = _metric_lookup_topic_years(topic)
    return [str(year) for year in years]


def _metric_lookup_research_period_filters(
    topic: str | None, periods: Iterable[str] | None
) -> list[str]:
    explicit_periods = _metric_lookup_period_filters(None, periods)
    topic_periods = _metric_lookup_period_filters(topic, None)
    if topic_periods and len(topic_periods) > len(explicit_periods):
        return topic_periods
    return explicit_periods or topic_periods


def _metric_lookup_period_filters_are_annual(periods: Iterable[str] | None) -> bool:
    values = [str(period or "").upper() for period in periods or [] if str(period or "").strip()]
    return bool(values) and all("Q" not in value for value in values)


def _metric_lookup_topic_years(topic: str | None) -> list[int]:
    text = str(topic or "")
    years = {int(year) for year in re.findall(r"(?<!\d)((?:19|20)\d{2})(?!\d)", text)}
    range_pattern = re.compile(
        r"(?<!\d)((?:19|20)\d{2})\s*(?:[-~]|to|through)\s*((?:19|20)\d{2})(?!\d)",
        re.IGNORECASE,
    )
    for match in range_pattern.finditer(text):
        start = int(match.group(1))
        end = int(match.group(2))
        if start > end:
            start, end = end, start
        if end - start <= 20:
            years.update(range(start, end + 1))
    return sorted(years)


def _metric_lookup_base_metric_terms(terms: Sequence[str]) -> list[str]:
    base_terms = [term for term in terms if term not in _METRIC_LOOKUP_CALCULATION_TERMS]
    return base_terms or list(terms)


def _metric_lookup_topic_is_metric_like(topic: str | None) -> bool:
    terms = set(_query_terms(topic))
    metric_markers = {
        "capex",
        "cash",
        "cost",
        "costs",
        "earnings",
        "ebit",
        "ebitda",
        "expense",
        "expenses",
        "flow",
        "gross",
        "income",
        "margin",
        "margins",
        "net",
        "nii",
        "nim",
        "opex",
        "operating",
        "profit",
        "revenue",
        "revenues",
        "sales",
    }
    return bool(terms & metric_markers)


def _metric_lookup_search_terms(topic: str | None) -> list[str]:
    terms = []
    for term in _query_terms(topic):
        if re.fullmatch(r"(?:19|20)\d{2}", term):
            continue
        if term in _SPLIT_TOPIC_STOP_TERMS or term in _METRIC_FAST_PATH_GENERIC_TERMS:
            continue
        if len(term) <= 2 and term not in {"ni"}:
            continue
        terms.append(term)
    return _unique(terms)


def _metric_lookup_dimension_anchors(topic: str | None) -> list[str]:
    anchors: list[str] = []
    for term in _metric_lookup_search_terms(topic):
        if term in _METRIC_LOOKUP_METRIC_TERMS:
            continue
        canonical = _canonical_metric_name(term)
        if canonical in _METRIC_LOOKUP_METRIC_TERMS:
            continue
        anchors.append(term)
    return _unique(anchors)


def _metric_lookup_fallback_object_types(
    object_types: Sequence[str],
    *,
    search_strategy: Mapping[str, Any] | None,
) -> tuple[str, ...]:
    if not search_strategy or not search_strategy.get("dimension_metric_not_found"):
        return tuple(object_types)
    return tuple(
        object_type for object_type in object_types if object_type not in _METRIC_FAST_PATH_TYPES
    )


def _metric_lookup_wants_total(topic: str | None) -> bool:
    terms = set(_query_terms(topic))
    return bool(terms & {"company", "consolidated", "total"})


def _metric_lookup_needs_denominator(topic: str | None) -> bool:
    text = str(topic or "").lower()
    terms = set(_query_terms(topic))
    return bool(terms & {"share", "shares", "percentage", "percent", "ratio"}) or "비중" in text


def _metric_dimension_key(value: Any) -> str:
    return normalize_dimension_key(value)


def _metric_lookup_period_values(periods: Iterable[str] | None) -> list[str]:
    values: list[str] = []
    for period in periods or []:
        raw = str(period or "").upper()
        if not raw:
            continue
        values.append(raw)
        match = re.search(r"(20\d{2}|19\d{2})(Q[1-4])?$", raw)
        if match:
            year, quarter = match.group(1), match.group(2) or ""
            values.extend([f"CY{year}{quarter}", f"FY{year}{quarter}", f"{year}{quarter}"])
    return _unique(values)


def _metric_lookup_years(periods: Iterable[str] | None) -> list[int]:
    years: list[int] = []
    for period in periods or []:
        for year in re.findall(r"(20\d{2}|19\d{2})", str(period or "")):
            years.append(int(year))
    return sorted(set(years))


def _dedupe_metric_lookup_rows(rows: Sequence[sqlite3.Row], *, limit: int) -> list[sqlite3.Row]:
    seen: set[tuple[Any, ...]] = set()
    deduped: list[sqlite3.Row] = []
    for row in rows:
        key = (
            row["metric_lookup_canonical_metric"],
            row["metric_lookup_fiscal_year"],
            row["metric_lookup_value_text"],
            row["metric_lookup_unit"],
            row["metric_lookup_segment_name"],
            row["metric_lookup_product_name"],
            row["metric_lookup_geography_name"],
            row["metric_lookup_is_company_total"],
        )
        if key in seen:
            continue
        seen.add(key)
        deduped.append(row)
        if len(deduped) >= limit:
            break
    return deduped


def _dedupe_object_rows(rows: Sequence[sqlite3.Row], *, limit: int) -> list[sqlite3.Row]:
    seen: set[str] = set()
    deduped: list[sqlite3.Row] = []
    for row in rows:
        object_id = str(row["id"] or "")
        if not object_id or object_id in seen:
            continue
        seen.add(object_id)
        deduped.append(row)
        if len(deduped) >= limit:
            break
    return deduped


def _canonical_metric_name(metric: str | None) -> str:
    """Normalize human metric input to the canonical metric_name token shape."""
    return canonical_metric_name(metric)


def _expanded_topic(topic: str) -> str:
    topic = " ".join(str(topic or "").split())
    if not topic:
        return ""
    topic_lower = topic.lower()
    expansions = [
        expansion for needle, expansion in _QUERY_EXPANSION_RULES if needle in topic_lower
    ]
    if not expansions:
        return topic
    return " ".join(_unique([topic, *expansions]))


def _split_topic_queries(topic: str, *, limit: int = 12) -> list[str]:
    terms = [
        term
        for term in _query_terms(topic)
        if term not in _SPLIT_TOPIC_STOP_TERMS and len(term) > 2
    ]
    chunks: list[str] = []
    chunks.extend(term for term in terms if "_" in term)
    chunks.extend(f"{left} {right}" for left, right in zip(terms, terms[1:]) if left != right)
    chunks.extend(terms)
    return _unique(chunks)[:limit]


def _limited_or_topic(topic: str, *, max_terms: int) -> str:
    terms = [
        term
        for term in _query_terms(topic)
        if term not in _SPLIT_TOPIC_STOP_TERMS and len(term) > 2
    ]
    if not terms:
        terms = _query_terms(topic)
    return " ".join(_unique(terms)[: max(1, int(max_terms))])


def _search_diagnostics(
    topic: str | None,
    *,
    result_count: int | None,
    search_strategy: dict[str, Any] | None = None,
) -> dict[str, Any]:
    original_topic = topic or None
    terms = _query_terms(topic)
    fts_query = _fts_query(topic or "", operator="AND") if topic else None
    warnings: list[str] = []
    suggestions: list[str] = []
    expanded_terms = (search_strategy or {}).get("expanded_terms") or terms

    if topic and not terms:
        if result_count:
            warnings.append("topic_rewritten_for_search")
        else:
            warnings.append("empty_topic_after_tokenization")
            suggestions.append(
                "Use English/canonical company exposure terms or call krw_ontology_topic_map first."
            )
    if topic and terms and result_count == 0 and len(terms) >= 3:
        warnings.append("strict_and_query_may_be_too_narrow")
        suggestions.append("Split the topic into shorter focused queries.")
    elif topic and len(terms) >= _STRICT_TOPIC_WARNING_TERM_COUNT:
        warnings.append("long_strict_topic")
        suggestions.append("Prefer several small topic queries over one long topic.")

    return {
        "original_topic": original_topic,
        "normalized_terms": terms,
        "expanded_terms": expanded_terms,
        "fts_query": fts_query,
        "operator": "AND" if topic else None,
        "result_count": result_count,
        "search_strategy": search_strategy or {},
        "warnings": warnings,
        "suggestions": _unique(suggestions),
    }


def _object_from_row(row: sqlite3.Row) -> dict[str, Any]:
    obj = json.loads(row["json"])
    if row["review_status"]:
        obj["review_status"] = row["review_status"]
    return obj


def _dedupe_objects(objects: Iterable[dict[str, Any]]) -> list[dict[str, Any]]:
    seen: set[str] = set()
    output: list[dict[str, Any]] = []
    for obj in objects:
        object_id = obj.get("id")
        if not object_id or object_id in seen:
            continue
        seen.add(object_id)
        output.append(obj)
    return output


def _document_from_row(row: sqlite3.Row) -> dict[str, Any]:
    return {
        "ticker": row["ticker"],
        "document_type": row["document_type"],
        "doc_type_key": row["doc_type_key"],
        "period": row["period"],
        "artifact_index_path": row["artifact_index_path"],
        "ontology_dir": row["ontology_dir"],
        "counts": json.loads(row["counts_json"]),
        "section_quality_status": row["section_quality_status"],
        "section_quality": json.loads(row["section_quality_json"]),
    }


def _quality_from_row(row: sqlite3.Row) -> dict[str, Any]:
    return {
        "id": row["id"],
        "ticker": row["ticker"],
        "document_type": row["document_type"],
        "period": row["period"],
        "severity": row["severity"],
        "category": row["category"],
        "object_id": row["object_id"],
        "stage": row["stage"],
        "message": row["message"],
        "payload": json.loads(row["json"]),
    }


def _edge_from_row(row: sqlite3.Row) -> dict[str, Any]:
    edge = json.loads(row["json"])
    for key in (
        "id",
        "ticker",
        "document_type",
        "period",
        "source_document_id",
        "from_id",
        "to_id",
        "relation_id",
        "relation_name",
        "confidence",
        "review_status",
    ):
        if row[key] is not None:
            edge[key] = row[key]
    return edge


def _compact_edge(edge: dict[str, Any]) -> dict[str, Any]:
    return {
        "id": edge.get("id"),
        "relation_id": edge.get("relation_id"),
        "relation_name": edge.get("relation_name"),
        "from_id": edge.get("from_id"),
        "to_id": edge.get("to_id"),
        "edge_class": edge.get("edge_class"),
        "evidence_level": edge.get("evidence_level"),
        "generation_method": edge.get("generation_method"),
        "confidence": edge.get("confidence"),
        "review_status": edge.get("review_status"),
        "rationale": edge.get("rationale"),
    }


def _chain_node(obj: dict[str, Any], *, include_quote_text: bool) -> dict[str, Any]:
    node = _compact_object(obj)
    if obj.get("type") in {"EvidenceQuote", "SourceSpan"} and not include_quote_text:
        node.pop("text", None)
        node["text_available"] = bool(_display_text(obj))
    return node


def _normalize_chain_direction(direction: str) -> str:
    value = str(direction or "both").strip().lower()
    if value in {"incoming", "in", "upstream", "up"}:
        return "incoming"
    if value in {"outgoing", "out", "downstream", "down"}:
        return "outgoing"
    return "both"


def _temporal_references_object(candidate: dict[str, Any], object_ids: set[str | None]) -> bool:
    if not object_ids:
        return False
    candidate_type = candidate.get("type")
    if candidate_type == "TemporalLink":
        return (
            candidate.get("from_object_id") in object_ids
            or candidate.get("to_object_id") in object_ids
        )
    if candidate_type == "TrendObservation":
        return bool(set(candidate.get("supported_by_objects") or []).intersection(object_ids))
    if candidate_type == "ChangeEvent":
        referenced_ids = {
            *(candidate.get("affected_objects") or []),
            *(candidate.get("supported_by_claims") or []),
            *(candidate.get("supported_by_quotes") or []),
        }
        return bool(referenced_ids.intersection(object_ids))
    return False


def _chain_warnings(
    obj: dict[str, Any],
    evidence: dict[str, list[dict[str, Any]]],
    quality: dict[str, Any],
) -> list[str]:
    warnings: list[str] = []
    if obj.get("review_status") == "rejected":
        warnings.append("object_is_rejected")
    if (
        not evidence.get("claims")
        and not evidence.get("quotes")
        and obj.get("type")
        not in {
            "EvidenceQuote",
            "ResearchClaim",
            "SourceSpan",
            "MetricObservation",
            "Calculation",
            "XBRLFact",
        }
    ):
        warnings.append("no_supporting_evidence_found")
    evidence_grade = obj.get("evidence_grade")
    if evidence_grade in {"derived", "unsupported"}:
        warnings.append(f"weak_evidence_grade:{evidence_grade}")
    if quality.get("section_quality") in {"warn", "fail"}:
        warnings.append(f"section_quality:{quality.get('section_quality')}")
    if quality.get("events"):
        warnings.append("quality_events_present")
    return _unique(warnings)


def _display_text(obj: dict[str, Any]) -> str:
    if obj.get("type") == "MetricObservation" or obj.get("metric_name"):
        return format_metric_compact(obj)
    for key in (
        "claim_text",
        "quote_text",
        "description",
        "mechanism",
        "business_model_summary",
        "interpretation",
        "assumption_text",
        "text",
        "raw_text",
    ):
        value = obj.get(key)
        if value:
            return str(value)
    if obj.get("metric_name"):
        return f"{obj['metric_name']}: {obj.get('value')} {obj.get('unit')}"
    return obj.get("name") or obj.get("id", "")


def _string_values(value: Any) -> list[str]:
    if value is None:
        return []
    if isinstance(value, str):
        return [value] if value.strip() else []
    if isinstance(value, (list, tuple, set)):
        output: list[str] = []
        for item in value:
            output.extend(_string_values(item))
        return output
    return [str(value)] if str(value).strip() else []


def _object_metrics(obj: dict[str, Any]) -> list[str]:
    return _unique(
        [
            *_string_values(obj.get("related_metrics")),
            *_string_values(obj.get("affects")),
            *_string_values(obj.get("impact_channel")),
            *_string_values(obj.get("impact_channels")),
        ]
    )


def _explicit_project_asset_values(obj: dict[str, Any]) -> list[str]:
    values: list[str] = []
    for key in (
        "projects",
        "key_projects",
        "major_projects",
        "project_names",
        "assets",
        "key_assets",
        "major_assets",
        "projects_assets",
    ):
        values.extend(_string_values(obj.get(key)))
    return _unique(values)


def _canonical_topic_term(value: Any) -> str | None:
    values = _string_values(value)
    if not values:
        return None
    term = values[0].strip()
    return term or None


def _search_terms_for(value: Any) -> list[str]:
    terms: list[str] = []
    for item in _string_values(value):
        cleaned = " ".join(item.replace("_", " ").replace("-", " ").split())
        if cleaned:
            terms.append(cleaned)
        if item and item != cleaned:
            terms.append(item)
    return _unique(terms)


def _merge_topic_entry(
    collection: dict[str, dict[str, Any]],
    term_value: Any,
    *,
    source_id: str | None = None,
    search_terms: Iterable[Any] | None = None,
    related_channels: Iterable[str] | None = None,
    related_metrics: Iterable[str] | None = None,
    evidence_grade: str | None = None,
) -> None:
    term = _canonical_topic_term(term_value)
    if not term:
        return
    key = term.lower().replace(" ", "_").replace("-", "_")
    entry = collection.setdefault(
        key,
        {
            "term": key,
            "label": term,
            "search_terms": [],
            "related_channels": [],
            "related_metrics": [],
            "source_object_ids": [],
            "evidence_grades": [],
        },
    )
    entry["search_terms"] = _unique(
        [
            *entry["search_terms"],
            *_search_terms_for(term),
            *[
                search_term
                for value in search_terms or []
                for search_term in _search_terms_for(value)
            ],
        ]
    )
    entry["related_channels"] = _unique(
        [*entry["related_channels"], *_string_values(list(related_channels or []))]
    )
    entry["related_metrics"] = _unique(
        [*entry["related_metrics"], *_string_values(list(related_metrics or []))]
    )
    if source_id:
        entry["source_object_ids"] = _unique([*entry["source_object_ids"], source_id])
    if evidence_grade:
        entry["evidence_grades"] = _unique([*entry["evidence_grades"], evidence_grade])


def _topic_entries(collection: dict[str, dict[str, Any]], *, limit: int) -> list[dict[str, Any]]:
    entries = sorted(
        collection.values(),
        key=lambda item: (-len(item.get("source_object_ids") or []), item.get("term") or ""),
    )
    compacted: list[dict[str, Any]] = []
    for entry in entries[:limit]:
        compacted.append(
            {
                "term": entry["term"],
                "label": entry["label"],
                "search_terms": entry["search_terms"][:6],
                "related_channels": entry["related_channels"][:8],
                "related_metrics": entry["related_metrics"][:8],
                "source_object_ids": entry["source_object_ids"][:8],
                "evidence_grades": entry["evidence_grades"][:4],
            }
        )
    return compacted


def _suggest_topic_queries(
    *,
    external_factors: dict[str, dict[str, Any]],
    business_activities: dict[str, dict[str, Any]],
) -> list[dict[str, Any]]:
    suggestions: list[dict[str, Any]] = []
    for entry in _topic_entries(external_factors, limit=5):
        topic = (entry.get("search_terms") or [entry["label"]])[0]
        suggestions.append(
            {
                "topic": topic,
                "object_types": ["ExternalFactorExposure", "BusinessFactor", "ResearchClaim"],
                "source_object_ids": entry.get("source_object_ids", [])[:3],
            }
        )
    for entry in _topic_entries(business_activities, limit=5):
        topic = (entry.get("search_terms") or [entry["label"]])[0]
        suggestions.append(
            {
                "topic": topic,
                "object_types": ["BusinessActivity", "ResearchClaim", "EvidenceQuote"],
                "source_object_ids": entry.get("source_object_ids", [])[:3],
            }
        )
    return suggestions


def _compact_object(obj: dict[str, Any]) -> dict[str, Any]:
    return {
        "id": obj.get("id"),
        "type": obj.get("type"),
        "ticker": obj.get("ticker"),
        "document_type": obj.get("document_type"),
        "period": obj.get("period"),
        "section": obj.get("section_name") or obj.get("section_key"),
        "text": _display_text(obj),
    }


def _unique(values: Iterable[Any]) -> list[Any]:
    seen = set()
    output = []
    for value in values:
        if value in seen:
            continue
        seen.add(value)
        output.append(value)
    return output


def _compare_ticker_cache_get(
    key: tuple[Any, ...],
) -> tuple[list[dict[str, Any]], dict[str, Any], dict[str, Any]] | None:
    with _COMPARE_TICKER_CACHE_LOCK:
        cached = _COMPARE_TICKER_CACHE.get(key)
        if cached is None:
            return None
        _COMPARE_TICKER_CACHE.move_to_end(key)
        return cached


def _compare_ticker_cache_set(
    key: tuple[Any, ...],
    value: tuple[list[dict[str, Any]], dict[str, Any], dict[str, Any]],
) -> None:
    with _COMPARE_TICKER_CACHE_LOCK:
        _COMPARE_TICKER_CACHE[key] = value
        _COMPARE_TICKER_CACHE.move_to_end(key)
        while len(_COMPARE_TICKER_CACHE) > _COMPARE_TICKER_CACHE_MAX:
            _COMPARE_TICKER_CACHE.popitem(last=False)


def agent_index_cache_status() -> dict[str, dict[str, int]]:
    """Return in-process retrieval cache sizes for MCP health/debugging."""
    with _COMPARE_TICKER_CACHE_LOCK:
        compare_size = len(_COMPARE_TICKER_CACHE)
    with _DISCOVERY_CACHE_LOCK:
        discovery_size = len(_DISCOVERY_CACHE)
    with _QUERY_COMPACT_CACHE_LOCK:
        query_compact_size = len(_QUERY_COMPACT_CACHE)
    with _QUERY_CONTEXT_CACHE_LOCK:
        query_context_size = len(_QUERY_CONTEXT_CACHE)
    return {
        "compare_ticker": {
            "size": compare_size,
            "max": _COMPARE_TICKER_CACHE_MAX,
        },
        "discovery": {
            "size": discovery_size,
            "max": _DISCOVERY_CACHE_MAX,
        },
        "query_compact": {
            "size": query_compact_size,
            "max": _QUERY_COMPACT_CACHE_MAX,
        },
        "query_context": {
            "size": query_context_size,
            "max": _QUERY_CONTEXT_CACHE_MAX,
        },
    }


def _discovery_cache_key(
    index_path: Path,
    *,
    question: str,
    tickers: Iterable[str] | None,
    document_types: Iterable[str] | None,
    periods: Iterable[str] | None,
    limit_groups: int,
    limit_per_group: int,
    limit: int,
) -> tuple[Any, ...]:
    try:
        index_mtime_ns = index_path.stat().st_mtime_ns
    except OSError:
        index_mtime_ns = None
    return (
        str(index_path),
        index_mtime_ns,
        question,
        _cache_tuple(tickers, upper=True),
        _cache_tuple(document_types, upper=True),
        _cache_tuple(periods, upper=True),
        int(limit_groups),
        int(limit_per_group),
        int(limit),
    )


def _cache_tuple(values: Iterable[str] | None, *, upper: bool = False) -> tuple[str, ...]:
    if not values:
        return ()
    output = []
    for value in values:
        text = str(value)
        output.append(text.upper() if upper else text)
    return tuple(output)


def _discovery_cache_get(key: tuple[Any, ...]) -> dict[str, Any] | None:
    with _DISCOVERY_CACHE_LOCK:
        cached = _DISCOVERY_CACHE.get(key)
        if cached is None:
            return None
        _DISCOVERY_CACHE.move_to_end(key)
        return copy.deepcopy(cached)


def _discovery_cache_set(key: tuple[Any, ...], value: dict[str, Any]) -> None:
    with _DISCOVERY_CACHE_LOCK:
        _DISCOVERY_CACHE[key] = copy.deepcopy(value)
        _DISCOVERY_CACHE.move_to_end(key)
        while len(_DISCOVERY_CACHE) > _DISCOVERY_CACHE_MAX:
            _DISCOVERY_CACHE.popitem(last=False)


def _query_compact_cache_key(
    index_path: Path,
    *,
    topic: str | None,
    tickers: Iterable[str] | None,
    document_types: Iterable[str] | None,
    periods: Iterable[str] | None,
    object_types: Iterable[str] | None,
    include_rejected: bool,
    limit: int,
) -> tuple[Any, ...]:
    try:
        index_mtime_ns = index_path.stat().st_mtime_ns
    except OSError:
        index_mtime_ns = None
    return (
        str(index_path),
        index_mtime_ns,
        topic,
        _cache_tuple(tickers, upper=True),
        _cache_tuple(document_types, upper=True),
        _cache_tuple(periods, upper=True),
        _cache_tuple(object_types, upper=False),
        bool(include_rejected),
        int(limit),
    )


def _query_compact_cache_get(
    key: tuple[Any, ...],
) -> tuple[list[dict[str, Any]], dict[str, Any]] | None:
    with _QUERY_COMPACT_CACHE_LOCK:
        cached = _QUERY_COMPACT_CACHE.get(key)
        if cached is None:
            return None
        _QUERY_COMPACT_CACHE.move_to_end(key)
        return copy.deepcopy(cached)


def _query_compact_cache_set(
    key: tuple[Any, ...],
    value: tuple[list[dict[str, Any]], dict[str, Any]],
) -> None:
    with _QUERY_COMPACT_CACHE_LOCK:
        _QUERY_COMPACT_CACHE[key] = copy.deepcopy(value)
        _QUERY_COMPACT_CACHE.move_to_end(key)
        while len(_QUERY_COMPACT_CACHE) > _QUERY_COMPACT_CACHE_MAX:
            _QUERY_COMPACT_CACHE.popitem(last=False)


def _query_context_cache_key(
    index_path: Path,
    *,
    question: str,
    requested_tickers: Sequence[str] | None,
    document_types: Iterable[str] | None,
    periods: Iterable[str] | None,
    universe: str | None,
    limit_results: int,
    limit_tickers: int,
    include_internal_ids: bool,
) -> tuple[Any, ...]:
    try:
        index_mtime_ns = index_path.stat().st_mtime_ns
    except OSError:
        index_mtime_ns = None
    return (
        str(index_path),
        index_mtime_ns,
        question,
        tuple(requested_tickers or ()),
        _cache_tuple(document_types, upper=True),
        _cache_tuple(periods, upper=True),
        universe,
        int(limit_results),
        int(limit_tickers),
        bool(include_internal_ids),
    )


def _query_context_cache_get(key: tuple[Any, ...]) -> dict[str, Any] | None:
    with _QUERY_CONTEXT_CACHE_LOCK:
        cached = _QUERY_CONTEXT_CACHE.get(key)
        if cached is None:
            return None
        _QUERY_CONTEXT_CACHE.move_to_end(key)
        return copy.deepcopy(cached)


def _query_context_cache_set(key: tuple[Any, ...], value: dict[str, Any]) -> None:
    with _QUERY_CONTEXT_CACHE_LOCK:
        _QUERY_CONTEXT_CACHE[key] = copy.deepcopy(value)
        _QUERY_CONTEXT_CACHE.move_to_end(key)
        while len(_QUERY_CONTEXT_CACHE) > _QUERY_CONTEXT_CACHE_MAX:
            _QUERY_CONTEXT_CACHE.popitem(last=False)


def _query_context_discovery_limit(
    *,
    limit_results: int,
    limit_tickers: int,
    requested_tickers: Sequence[str] | None,
) -> int:
    if requested_tickers and len(requested_tickers) <= max(1, int(limit_tickers)):
        return max(int(limit_results) * 5, 20)
    return max(int(limit_results) * 10, 40)


def _ticker_guard_query_diagnostics(
    topic: str | None,
    unavailable_tickers: Sequence[str],
    *,
    compact: bool = False,
) -> dict[str, Any]:
    diagnostics = _search_diagnostics(topic, result_count=0)
    diagnostics.setdefault("warnings", [])
    diagnostics["warnings"].append("ticker_not_available")
    diagnostics["unavailable_tickers"] = list(unavailable_tickers)
    diagnostics["ticker_guard"] = True
    if compact:
        diagnostics["compact_fast_path"] = True
    return diagnostics
