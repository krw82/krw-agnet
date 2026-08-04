"""Natural-language retrieval planner over the ontology store.

This layer does not let an agent write SQL or inspect raw JSONL files. It
converts a user question into a constrained QueryPlan, executes the plan
through OntologyStore, and returns evidence bundles with an audit trail.
"""

from __future__ import annotations

from collections import OrderedDict
import copy
from dataclasses import asdict, dataclass, field
import re
from threading import Lock
from typing import Any, Callable, Iterable, Mapping, Protocol, Sequence

from krw_capability_runtime.agent_index.discovery import (
    build_evidence_frame,
    build_query_frame,
    classify_topic_match,
)
from krw_capability_runtime.agent_index.store import DEFAULT_QUERY_TYPES, OntologyStore

PlannerFn = Callable[[str, dict[str, Any]], "QueryPlan"]
RerankerFn = Callable[[list[dict[str, Any]], "QueryPlan"], list[dict[str, Any]]]

_TOKEN_RE = re.compile(r"[A-Za-z][A-Za-z0-9.:-]*")
_PERIOD_RE = re.compile(r"\bFY\d{4}(?:Q[1-4?])?\b", re.IGNORECASE)
_RETRIEVE_COMPACT_CACHE_MAX = 256
_RETRIEVE_COMPACT_CACHE: OrderedDict[tuple[Any, ...], dict[str, Any]] = OrderedDict()
_RETRIEVE_COMPACT_CACHE_LOCK = Lock()

QUESTION_TYPE_OBJECTS: dict[str, list[str]] = {
    "risk": ["BusinessFactor", "ExternalFactorExposure", "ResearchClaim", "EvidenceQuote"],
    "driver": [
        "BusinessFactor",
        "BusinessActivity",
        "ExternalFactorExposure",
        "ResearchClaim",
        "EvidenceQuote",
    ],
    "scenario": [
        "ExternalFactorExposure",
        "BusinessFactor",
        "ResearchClaim",
        "EvidenceQuote",
        "MetricObservation",
    ],
    "event": ["BusinessEvent", "ChangeEvent", "ResearchClaim", "EvidenceQuote"],
    "change": [
        "ChangeEvent",
        "TrendObservation",
        "TemporalLink",
        "ResearchClaim",
        "EvidenceQuote",
        "MetricObservation",
    ],
    "metric": ["MetricObservation", "Calculation", "ResearchClaim"],
    "agreement": ["AgreementTerm", "ResearchClaim", "EvidenceQuote"],
    "business_model": [
        "CompanyBusinessProfile",
        "BusinessActivity",
        "ExternalFactorExposure",
        "BusinessFactor",
        "ResearchClaim",
    ],
}


@dataclass(frozen=True)
class QueryPlan:
    """Constrained retrieval plan produced from a user question."""

    question: str
    intent: str = "evidence_search"
    tickers: list[str] = field(default_factory=list)
    document_types: list[str] = field(default_factory=list)
    periods: list[str] = field(default_factory=list)
    period_policy: str = "all"
    topics: list[str] = field(default_factory=list)
    metric: str | None = None
    object_types: list[str] = field(default_factory=lambda: list(DEFAULT_QUERY_TYPES))
    include_rejected: bool = False
    require_trace: bool = True
    limit: int = 20

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


class QueryPlanner(Protocol):
    def plan(self, question: str, catalog: dict[str, Any]) -> QueryPlan:
        """Produce a constrained QueryPlan."""


class DefaultQueryPlanner:
    """Deterministic planner used before an LLM planner/reranker is wired in."""

    def plan(self, question: str, catalog: dict[str, Any]) -> QueryPlan:
        question_lower = question.lower()
        key_risk_question = _asks_key_risks(question_lower)
        tickers = _extract_tickers(question, catalog.get("companies", []))
        document_types = _extract_document_types(question_lower)
        if key_risk_question and not document_types:
            document_types = ["10-K"]
        periods = [period.upper() for period in _PERIOD_RE.findall(question)]
        period_policy = "latest" if _asks_latest(question_lower) or key_risk_question else "all"
        metric = _extract_metric(question_lower)
        intent = _extract_intent(question_lower, tickers, metric)
        topics = _extract_topics(question, question_lower, metric, key_risk_question=key_risk_question)
        object_types = _extract_object_types(
            question_lower,
            key_risk_question=key_risk_question,
            metric=metric,
        )
        include_rejected = _asks_include_rejected(question_lower)

        return QueryPlan(
            question=question,
            intent=intent,
            tickers=tickers,
            document_types=document_types,
            periods=periods,
            period_policy=period_policy,
            topics=topics,
            metric=metric,
            object_types=object_types,
            include_rejected=include_rejected,
            limit=20,
        )


class AgentRetriever:
    """Agent-safe retrieval facade over OntologyStore."""

    def __init__(
        self,
        store: OntologyStore,
        *,
        planner: QueryPlanner | PlannerFn | None = None,
        reranker: RerankerFn | None = None,
    ):
        self.store = store
        self.planner = planner or DefaultQueryPlanner()
        self.reranker = reranker

    def plan(
        self,
        question: str,
        *,
        tickers: Iterable[str] | None = None,
        document_types: Iterable[str] | None = None,
        periods: Iterable[str] | None = None,
        include_rejected: bool | None = None,
        limit: int | None = None,
    ) -> QueryPlan:
        """Return a QueryPlan, applying explicit caller constraints last."""
        catalog = self.catalog()
        if callable(self.planner) and not hasattr(self.planner, "plan"):
            plan = self.planner(question, catalog)
        else:
            plan = self.planner.plan(question, catalog)  # type: ignore[union-attr]

        return QueryPlan(
            question=plan.question,
            intent=plan.intent,
            tickers=[ticker.upper() for ticker in (tickers or plan.tickers)],
            document_types=list(document_types or plan.document_types),
            periods=[period.upper() for period in (periods or plan.periods)],
            period_policy=plan.period_policy,
            topics=plan.topics,
            metric=plan.metric,
            object_types=plan.object_types,
            include_rejected=plan.include_rejected if include_rejected is None else include_rejected,
            require_trace=plan.require_trace,
            limit=limit or plan.limit,
        )

    def retrieve(
        self,
        question: str,
        *,
        tickers: Iterable[str] | None = None,
        document_types: Iterable[str] | None = None,
        periods: Iterable[str] | None = None,
        include_rejected: bool | None = None,
        limit: int | None = None,
        include_evidence_bundle: bool = True,
    ) -> dict[str, Any]:
        """Plan and execute an agent-safe retrieval request."""
        import time

        started_at = time.perf_counter()
        plan = self.plan(
            question,
            tickers=tickers,
            document_types=document_types,
            periods=periods,
            include_rejected=include_rejected,
            limit=limit,
        )
        unavailable_tickers: list[str] = []
        if plan.tickers:
            available_tickers = self.store._available_tickers(plan.tickers)
            unavailable_tickers = [ticker for ticker in plan.tickers if ticker not in available_tickers]
            if not available_tickers:
                return _empty_retrieve_response(
                    plan,
                    started_at,
                    unavailable_tickers=unavailable_tickers,
                )
            if unavailable_tickers:
                plan = _replace_plan_tickers(
                    plan,
                    [ticker for ticker in plan.tickers if ticker in available_tickers],
                )
        resolved_periods = self._resolve_periods(plan)
        executed_queries: list[dict[str, Any]] = []

        if plan.intent == "quality_check":
            quality = self._quality_for_plan(plan, resolved_periods)
            return {
                "answerability": {
                    "direct_answerable": False,
                    "related_context_available": bool(quality["documents"]),
                    "negative_answer_supported": False,
                    "needs_user_clarification": False,
                    "recommended_answer_mode": "quality_report",
                    "question_requires_direct_match": False,
                    "query_frame": {},
                },
                "recommended_answer_mode": "quality_report",
                "query_frame": {},
                "plan": plan.to_dict(),
                "resolved_periods": resolved_periods,
                "direct_evidence": [],
                "related_context": [],
                "rejected_context": [],
                "quality": quality,
                "audit": {"executed_queries": executed_queries},
                "timing_ms": {"total": round((time.perf_counter() - started_at) * 1000, 3)},
            }

        if plan.intent == "compare":
            result = self._execute_compare(plan, resolved_periods, executed_queries)
            return {
                "answerability": {
                    "direct_answerable": bool(any(result["results"].values())),
                    "related_context_available": False,
                    "negative_answer_supported": False,
                    "needs_user_clarification": False,
                    "recommended_answer_mode": "comparison",
                    "question_requires_direct_match": False,
                    "query_frame": {},
                },
                "recommended_answer_mode": "comparison",
                "query_frame": {},
                "plan": plan.to_dict(),
                "resolved_periods": resolved_periods,
                "direct_evidence": [],
                "related_context": [],
                "rejected_context": [],
                "compare": result,
                "audit": {"executed_queries": executed_queries},
                "timing_ms": {"total": round((time.perf_counter() - started_at) * 1000, 3)},
            }

        if not include_evidence_bundle:
            cache_key = _retrieve_compact_cache_key(self.store.index_path, plan, resolved_periods)
            cached = _retrieve_compact_cache_get(cache_key)
            if cached is not None:
                result = copy.deepcopy(cached)
                result.setdefault("audit", {})["cache_hit"] = True
                result.setdefault("audit", {})["retrieve_compact_cache"] = True
                if unavailable_tickers:
                    result.setdefault("audit", {})["unavailable_tickers"] = unavailable_tickers
                result.setdefault("timing_ms", {})["total"] = round(
                    (time.perf_counter() - started_at) * 1000,
                    3,
                )
                return result
            result = self._retrieve_compact(plan, resolved_periods, executed_queries, started_at)
            if unavailable_tickers:
                result.setdefault("audit", {})["unavailable_tickers"] = unavailable_tickers
                result.setdefault("audit", {})["ticker_guard"] = True
            _retrieve_compact_cache_set(cache_key, result)
            return result

        candidates = self._execute_search(plan, resolved_periods, executed_queries)
        candidates = self._apply_graph_lift(candidates, plan)
        if self.reranker:
            candidates = self.reranker(candidates, plan)
        candidates = _dedupe_bundles(candidates)[: plan.limit]
        candidates, answerability = _annotate_retrieval_answerability(plan, candidates)
        retrieval_context = _split_retrieval_context(candidates)

        return {
            "answerability": answerability,
            "recommended_answer_mode": answerability.get("recommended_answer_mode"),
            "query_frame": answerability.get("query_frame", {}),
            "plan": plan.to_dict(),
            "resolved_periods": resolved_periods,
            **retrieval_context,
            "audit": {"executed_queries": executed_queries},
            "timing_ms": {"total": round((time.perf_counter() - started_at) * 1000, 3)},
        }

    def catalog(self) -> dict[str, Any]:
        documents = self.store.list_documents()
        return {
            "companies": self.store.list_companies(),
            "documents": documents,
            "document_types": sorted({doc["document_type"] for doc in documents}),
        }

    def _execute_search(
        self,
        plan: QueryPlan,
        resolved_periods: list[str],
        executed_queries: list[dict[str, Any]],
    ) -> list[dict[str, Any]]:
        topics = _expanded_retrieval_topics(plan)
        candidates: list[dict[str, Any]] = []
        per_topic_limit = max(plan.limit, 10)
        for topic in topics:
            query = {
                "topic": topic,
                "tickers": plan.tickers or None,
                "document_types": plan.document_types or None,
                "periods": resolved_periods or None,
                "object_types": plan.object_types,
                "include_rejected": plan.include_rejected,
                "limit": per_topic_limit,
            }
            executed_queries.append(query)
            candidates.extend(self.store.query(**query))
        return candidates

    def _retrieve_compact(
        self,
        plan: QueryPlan,
        resolved_periods: list[str],
        executed_queries: list[dict[str, Any]],
        started_at: float,
    ) -> dict[str, Any]:
        """Return answerability and compact trace candidates without eager bundles."""
        import time

        query_context_started = time.perf_counter()
        limit_results = max(1, min(plan.limit, 10))
        context = self.store.query_context(
            question=plan.question,
            tickers=plan.tickers or None,
            document_types=plan.document_types or None,
            periods=resolved_periods or None,
            limit_results=limit_results,
            limit_tickers=max(len(plan.tickers), 1) if plan.tickers else min(limit_results, 10),
            include_internal_ids=True,
        )
        query_context_ms = round((time.perf_counter() - query_context_started) * 1000, 3)
        fallback_ms = 0.0
        executed_queries.append(
            {
                "tool": "query_context",
                "question": plan.question,
                "tickers": plan.tickers or None,
                "document_types": plan.document_types or None,
                "periods": resolved_periods or None,
                "object_types": plan.object_types or None,
                "limit_results": limit_results,
                "include_internal_ids": True,
            }
        )

        raw_candidates = _compact_candidates_from_query_context(context, limit=plan.limit)
        if raw_candidates:
            candidates, answerability = _annotate_retrieval_answerability(plan, raw_candidates)
            candidates = _restore_compact_trace_metadata(candidates, raw_candidates)
        else:
            fallback_started = time.perf_counter()
            fallback_bundles = self._compact_bundle_fallback(plan, resolved_periods, executed_queries)
            fallback_ms = round((time.perf_counter() - fallback_started) * 1000, 3)
            if fallback_bundles:
                candidates, answerability = _annotate_retrieval_answerability(plan, fallback_bundles[: plan.limit])
            else:
                candidates = []
                answerability = dict(context.get("answerability") or {})
        retrieval_context = _split_retrieval_context(candidates)
        if not answerability:
            has_candidates = bool(candidates)
            answerability = {
                "direct_answerable": any(item.get("tier") == "traceable_direct" for item in candidates),
                "related_context_available": has_candidates,
                "negative_answer_supported": False,
                "needs_user_clarification": False,
                "recommended_answer_mode": "direct_answer" if has_candidates else "not_answerable",
            }

        recommended_trace_object_ids = _recommended_trace_object_ids_from_context(context, candidates)
        trace_required = _compact_retrieve_trace_required(plan, answerability)
        elapsed_since_query_context_start = (time.perf_counter() - query_context_started) * 1000
        timing_ms = {
            "query_context": query_context_ms,
            "bundle_fallback": fallback_ms,
            "candidate_formatting": round(
                max(0.0, elapsed_since_query_context_start - query_context_ms - fallback_ms),
                3,
            ),
            "total": round((time.perf_counter() - started_at) * 1000, 3),
        }

        return {
            "answerability": answerability,
            "recommended_answer_mode": answerability.get("recommended_answer_mode"),
            "query_frame": answerability.get("query_frame") or context.get("query_frame") or {},
            "plan": plan.to_dict(),
            "resolved_periods": resolved_periods,
            **retrieval_context,
            "recommended_trace_object_ids": recommended_trace_object_ids,
            "trace_required": trace_required,
            "trace_policy": {
                "mode": "lazy",
                "reason": "compact_retrieve_returns_trace_candidates_without_eager_evidence_bundles",
                "max_recommended_trace_objects": 3,
            },
            "search_diagnostics": context.get("search_diagnostics"),
            "audit": {
                "executed_queries": executed_queries,
                "compact_retrieve": True,
                "evidence_bundle_included": False,
            },
            "timing_ms": timing_ms,
        }

    def _compact_bundle_fallback(
        self,
        plan: QueryPlan,
        resolved_periods: list[str],
        executed_queries: list[dict[str, Any]],
    ) -> list[dict[str, Any]]:
        """Small capped bundle fallback when company-topic query_context is empty."""
        topics = _compact_fallback_topics(plan)
        candidates: list[dict[str, Any]] = []
        per_topic_limit = max(1, min(plan.limit, 3))
        for topic in topics[:2]:
            query = {
                "topic": topic,
                "tickers": plan.tickers or None,
                "document_types": plan.document_types or None,
                "periods": resolved_periods or None,
                "object_types": plan.object_types or None,
                "include_rejected": plan.include_rejected,
                "limit": per_topic_limit,
                "compact_fallback": True,
            }
            executed_queries.append(query)
            bundles, diagnostics = self.store.query_compact_with_diagnostics(
                **{key: value for key, value in query.items() if key != "compact_fallback"}
            )
            query["diagnostics"] = diagnostics
            candidates.extend(bundles)
            if len(candidates) >= plan.limit:
                break
        return _dedupe_bundles(candidates)[: plan.limit]

    def _apply_graph_lift(
        self,
        candidates: list[dict[str, Any]],
        plan: QueryPlan,
    ) -> list[dict[str, Any]]:
        """Promote semantic objects connected to top evidence results.

        This is not a word dictionary. It uses the ontology evidence graph: if
        an EvidenceQuote or ResearchClaim matches the query, connected semantic
        objects become stronger answer candidates for risk, driver, and scenario
        questions.
        """
        if not candidates:
            return candidates

        question_type = _infer_question_type(plan.question.lower()) or plan.intent
        if question_type not in {"risk", "driver", "scenario"}:
            return candidates
        promotion_types = _promotion_types(question_type, plan.object_types)
        if not promotion_types:
            return candidates

        promoted_ids: set[str] = set()
        promoted: list[dict[str, Any]] = []
        for bundle in candidates[:10]:
            evidence = bundle.get("evidence") or {}
            if bundle.get("type") in {"EvidenceQuote", "ResearchClaim"}:
                for related in evidence.get("related_objects") or []:
                    related_id = related.get("id")
                    if not related_id or related.get("type") not in promotion_types:
                        continue
                    if related_id in promoted_ids:
                        continue
                    promoted_ids.add(related_id)
                    promoted_bundle = self.store.bundle(related_id)
                    if not promoted_bundle.get("missing"):
                        promoted.append(promoted_bundle)
            elif bundle.get("type") in promotion_types:
                for evidence_obj in [
                    *(evidence.get("claims") or []),
                    *(evidence.get("quotes") or []),
                ]:
                    evidence_id = evidence_obj.get("id")
                    if not evidence_id or evidence_obj.get("type") not in {"ResearchClaim", "EvidenceQuote"}:
                        continue
                    if evidence_id in promoted_ids:
                        continue
                    promoted_ids.add(evidence_id)
                    promoted_bundle = self.store.bundle(evidence_id)
                    if not promoted_bundle.get("missing"):
                        promoted.append(promoted_bundle)

        if not promoted:
            return candidates

        combined = [*candidates, *promoted]
        return _rank_graph_lifted_bundles(
            combined,
            question_type=question_type,
            promoted_ids=promoted_ids,
        )

    def _execute_compare(
        self,
        plan: QueryPlan,
        resolved_periods: list[str],
        executed_queries: list[dict[str, Any]],
    ) -> dict[str, Any]:
        if len(plan.tickers) < 2:
            return {
                "mode": "compare",
                "topic": plan.topics[0] if plan.topics else None,
                "metric": plan.metric,
                "tickers": plan.tickers,
                "results": {},
                "warning": "compare requires at least two tickers",
            }
        topic = plan.topics[0] if plan.topics else None
        query = {
            "tickers": plan.tickers,
            "topic": topic,
            "metric": plan.metric,
            "document_types": plan.document_types or None,
            "periods": resolved_periods or None,
            "limit_per_ticker": max(1, min(plan.limit, 10)),
        }
        executed_queries.append(query)
        return self.store.compare(**query)

    def _quality_for_plan(self, plan: QueryPlan, resolved_periods: list[str]) -> dict[str, Any]:
        documents = []
        events = []
        summaries = []
        tickers = plan.tickers or [None]
        document_types = plan.document_types or [None]
        periods = resolved_periods or plan.periods or [None]
        for ticker in tickers:
            for document_type in document_types:
                for period in periods:
                    quality = self.store.quality(
                        ticker=ticker,
                        document_type=document_type,
                        period=period,
                    )
                    documents.extend(quality["documents"])
                    events.extend(quality["events"])
                    summaries.append(quality["summary"])
        return {
            "documents": _dedupe_documents(documents),
            "events": _dedupe_quality_events(events),
            "summary": {
                "documents": len(_dedupe_documents(documents)),
                "events": len(_dedupe_quality_events(events)),
                "rejected_objects": sum(summary["rejected_objects"] for summary in summaries),
                "batch_failures": sum(summary["batch_failures"] for summary in summaries),
                "section_warnings": sum(summary["section_warnings"] for summary in summaries),
            },
        }

    def _resolve_periods(self, plan: QueryPlan) -> list[str]:
        if plan.periods:
            return plan.periods
        if plan.period_policy != "latest":
            return []
        documents = []
        if plan.tickers:
            for ticker in plan.tickers:
                documents.extend(
                    self.store.list_documents(
                        ticker=ticker,
                        document_types=plan.document_types or None,
                    )
                )
        else:
            documents = self.store.list_documents(document_types=plan.document_types or None)

        latest_by_scope: dict[tuple[str, str], dict[str, Any]] = {}
        for doc in documents:
            key = (doc["ticker"], doc["document_type"])
            current = latest_by_scope.get(key)
            if current is None or _period_sort_key(doc["period"]) > _period_sort_key(current["period"]):
                latest_by_scope[key] = doc
        return sorted({doc["period"] for doc in latest_by_scope.values()})


def _expanded_retrieval_topics(plan: QueryPlan) -> list[str | None]:
    """Keep search recall broad while preserving the caller's first query."""
    query_frame = build_query_frame(plan.question)
    topics: list[str | None] = list(plan.topics) if plan.topics else []

    anchors = _facet_terms_for_query(query_frame.must_for_direct)
    should = _facet_terms_for_query(query_frame.should_for_direct)
    predicates = _facet_terms_for_query(query_frame.predicate_terms)
    context = _facet_terms_for_query(query_frame.context_facets)

    if anchors:
        topics.append(" ".join([*anchors, *should[:3]]) if should else " ".join(anchors))
        if predicates:
            topics.append(" ".join([*anchors, *predicates[:3]]))
    if context or should:
        topics.append(" ".join([*(context[:3]), *(should[:4])]))
    if not topics:
        topics.append(None)
    return _unique_nullable([topic for topic in topics if topic is None or str(topic).strip()])[:6]


def _facet_terms_for_query(facets: Iterable[str]) -> list[str]:
    return [str(facet).replace("_", " ") for facet in sorted(set(facets)) if str(facet).strip()]


def _extract_tickers(question: str, companies: Iterable[str]) -> list[str]:
    company_set = {company.upper() for company in companies}
    tokens = {token.upper().strip(".,:;()[]") for token in _TOKEN_RE.findall(question)}
    return sorted(token for token in tokens if token in company_set)


def _extract_document_types(question_lower: str) -> list[str]:
    doc_types = []
    if any(token in question_lower for token in ("10-q", "10q", "quarter", "분기")):
        doc_types.append("10-Q")
    if any(token in question_lower for token in ("10-k", "10k", "annual", "연간", "장기")):
        doc_types.append("10-K")
    return doc_types


def _asks_latest(question_lower: str) -> bool:
    return any(token in question_lower for token in ("latest", "recent", "current", "최근", "이번", "현재"))


def _asks_key_risks(question_lower: str) -> bool:
    return any(
        token in question_lower
        for token in (
            "key risks",
            "main risks",
            "major risks",
            "principal risks",
            "risk overview",
            "주요 리스크",
            "핵심 리스크",
            "가장 큰 리스크",
        )
    )


def _asks_include_rejected(question_lower: str) -> bool:
    return any(token in question_lower for token in ("include rejected", "rejected 포함", "리젝트 포함"))


def _extract_intent(question_lower: str, tickers: list[str], metric: str | None) -> str:
    if any(token in question_lower for token in ("quality", "품질", "rejected", "section_quality", "실패")):
        return "quality_check"
    if any(token in question_lower for token in ("compare", "comparison", " vs ", " versus ", "비교")):
        return "compare"
    if len(tickers) >= 2 and metric:
        return "compare"
    return "evidence_search"


def _extract_metric(question_lower: str) -> str | None:
    metric_aliases = {
        "capex": ("capex", "capital expenditure", "capital expenditures", "설비투자"),
        "revenue": ("revenue", "sales", "매출"),
        "gross_margin": ("gross margin", "매출총이익률"),
        "operating_margin": ("operating margin", "영업이익률"),
        "free_cash_flow": ("free cash flow", "fcf", "잉여현금흐름"),
    }
    for metric, aliases in metric_aliases.items():
        if any(alias in question_lower for alias in aliases):
            return metric
    return None


def _extract_topics(
    question: str,
    question_lower: str,
    metric: str | None,
    *,
    key_risk_question: bool = False,
) -> list[str]:
    if key_risk_question:
        return ["risk"]
    topic_aliases = [
        (
            ("margin pressure", "pricing pressure", "마진 압박", "가격 압박"),
            [
                "margin pressure",
                "pricing pressure",
                "lower prices",
                "competitive discounts",
                "margin compression",
            ],
        ),
        (
            ("credit risk", "신용 리스크", "credit loss"),
            ["credit risk", "credit losses", "allowance for credit losses"],
        ),
        (
            ("supply chain", "공급망"),
            ["supply chain", "supplier disruption", "component shortages"],
        ),
        (
            ("demand slowdown", "수요 둔화"),
            ["demand slowdown", "lower demand", "customer demand"],
        ),
        (
            ("capex", "capital expenditure", "capital expenditures", "설비투자"),
            ["capital expenditures", "capex"],
        ),
    ]
    topics: list[str] = []
    for aliases, expansions in topic_aliases:
        if any(alias in question_lower for alias in aliases):
            topics.extend(expansions)
    if not topics and metric:
        topics.append(metric.replace("_", " "))
    if not topics:
        cleaned = _clean_question_topic(question)
        if cleaned:
            topics.append(cleaned)
    return _unique(topics)


def _extract_object_types(
    question_lower: str,
    *,
    key_risk_question: bool = False,
    metric: str | None = None,
) -> list[str]:
    if key_risk_question:
        return ["BusinessFactor", "ExternalFactorExposure", "ResearchClaim"]
    if metric:
        return QUESTION_TYPE_OBJECTS["metric"]
    question_type = _infer_question_type(question_lower)
    if question_type in {"risk", "driver", "scenario"}:
        return QUESTION_TYPE_OBJECTS[question_type]
    mapping = {
        "EvidenceQuote": ("quote", "quotes", "근거", "인용"),
        "ResearchClaim": ("claim", "claims", "주장"),
        "BusinessFactor": ("risk", "리스크", "driver", "growth", "성장", "headwind", "부담", "역풍"),
        "ExternalFactorExposure": ("exposure", "factor", "요인", "노출"),
        "MetricObservation": ("metric", "metrics", "지표", "숫자"),
        "AssumptionCandidate": ("assumption", "가정"),
    }
    selected = [
        object_type
        for object_type, aliases in mapping.items()
        if any(alias in question_lower for alias in aliases)
    ]
    if selected:
        return selected
    if question_type:
        return QUESTION_TYPE_OBJECTS[question_type]
    return list(DEFAULT_QUERY_TYPES)


def _infer_question_type(question_lower: str) -> str | None:
    if any(
        token in question_lower
        for token in ("떨어지", "오르", "상승", "하락", "강화", "완화", "영향", "시나리오", "좋은가", "나쁜가")
    ):
        return "scenario"
    if any(token in question_lower for token in ("리스크", "위험", "규제", "소송", "제재", "제한", "risk")):
        return "risk"
    if any(token in question_lower for token in ("성장", "수요", "동인", "수혜", "드라이버", "driver")):
        return "driver"
    if any(token in question_lower for token in ("이벤트", "일정", "마일스톤", "승인", "가이던스", "event")):
        return "event"
    if any(token in question_lower for token in ("바뀐", "변화", "전년", "작년", "비교", "추세", "change")):
        return "change"
    if any(token in question_lower for token in ("지표", "매출", "마진", "이익", "현금", "부채", "capex")):
        return "metric"
    if any(token in question_lower for token in ("계약", "만기", "covenant", "리스", "채무", "spa")):
        return "agreement"
    if any(token in question_lower for token in ("사업", "뭐하는", "비즈니스", "모델")):
        return "business_model"
    return None


def _promotion_types(question_type: str, object_types: list[str]) -> set[str]:
    if question_type == "risk":
        return {"BusinessFactor", "ExternalFactorExposure"}
    if question_type == "driver":
        return {"BusinessFactor", "BusinessActivity", "ExternalFactorExposure"}
    if question_type == "scenario":
        return {"ExternalFactorExposure", "BusinessFactor", "MetricObservation"}
    return set(object_types).intersection(
        {
            "BusinessFactor",
            "ExternalFactorExposure",
            "BusinessActivity",
            "BusinessEvent",
            "ChangeEvent",
            "MetricObservation",
        }
    )


def _rank_graph_lifted_bundles(
    bundles: list[dict[str, Any]],
    *,
    question_type: str,
    promoted_ids: set[str],
) -> list[dict[str, Any]]:
    if question_type in {"risk", "driver", "scenario"}:
        return _interleave_evidence_and_semantic(bundles, question_type=question_type)

    best_by_id: dict[str, tuple[float, int, dict[str, Any]]] = {}
    for index, bundle in enumerate(bundles):
        bundle_id = bundle.get("id")
        if not bundle_id:
            continue
        score = _bundle_rank_score(
            bundle,
            base_rank=index + 1,
            question_type=question_type,
            promoted=bundle_id in promoted_ids,
        )
        current = best_by_id.get(bundle_id)
        if current is None or score > current[0]:
            best_by_id[bundle_id] = (score, index, bundle)
    return [
        item[2]
        for item in sorted(
            best_by_id.values(),
            key=lambda entry: (-entry[0], entry[1]),
        )
    ]


def _interleave_evidence_and_semantic(
    bundles: list[dict[str, Any]],
    *,
    question_type: str,
) -> list[dict[str, Any]]:
    unique_bundles = _dedupe_bundles(bundles)
    semantic_types = _promotion_types(question_type, QUESTION_TYPE_OBJECTS.get(question_type, []))
    evidence = [bundle for bundle in unique_bundles if bundle.get("type") in {"EvidenceQuote", "ResearchClaim"}]
    semantic = [
        bundle
        for bundle in unique_bundles
        if bundle.get("type") in semantic_types and bundle.get("type") not in {"EvidenceQuote", "ResearchClaim"}
    ]
    other = [
        bundle
        for bundle in unique_bundles
        if bundle not in evidence and bundle not in semantic
    ]
    semantic = sorted(
        semantic,
        key=lambda bundle: (
            -_semantic_priority(question_type, bundle),
            unique_bundles.index(bundle),
        ),
    )

    if question_type == "scenario":
        pattern = [semantic[:1], evidence[:1], semantic[1:3], evidence[1:3], semantic[3:], evidence[3:]]
    elif question_type == "driver":
        pattern = [semantic[:1], evidence[:1], semantic[1:3], evidence[1:3], semantic[3:], evidence[3:]]
    else:
        pattern = [evidence[:1], semantic[:2], evidence[1:3], semantic[2:], evidence[3:]]

    output: list[dict[str, Any]] = []
    seen: set[str] = set()
    for group in [*pattern, other]:
        for bundle in group:
            bundle_id = bundle.get("id")
            if not bundle_id or bundle_id in seen:
                continue
            seen.add(bundle_id)
            output.append(bundle)
    return output


def _semantic_priority(question_type: str, bundle: dict[str, Any]) -> float:
    bundle_type = bundle.get("type")
    obj = bundle.get("object") or {}
    if question_type == "scenario":
        if bundle_type == "ExternalFactorExposure":
            return 5.0 + (1.0 if obj.get("scenario_effects") else 0.0)
        if bundle_type == "BusinessFactor":
            return 3.0
        if bundle_type == "MetricObservation":
            return 2.0
    if question_type == "driver":
        return {
            "BusinessActivity": 5.0,
            "BusinessFactor": 4.0,
            "ExternalFactorExposure": 2.0,
        }.get(str(bundle_type), 0.0)
    if question_type == "risk":
        return {
            "BusinessFactor": 5.0,
            "ExternalFactorExposure": 4.0,
        }.get(str(bundle_type), 0.0)
    return 0.0


def _bundle_rank_score(
    bundle: dict[str, Any],
    *,
    base_rank: int,
    question_type: str,
    promoted: bool,
) -> float:
    score = 1_000.0 - base_rank
    if promoted:
        score += 180.0
    score += _intent_type_boost(question_type, bundle)
    evidence = bundle.get("evidence") or {}
    if evidence.get("quotes"):
        score += 15.0
    if evidence.get("claims"):
        score += 10.0
    return score


def _intent_type_boost(question_type: str, bundle: dict[str, Any]) -> float:
    bundle_type = bundle.get("type")
    obj = bundle.get("object") or {}
    if question_type == "risk":
        return {
            "BusinessFactor": 95.0,
            "ExternalFactorExposure": 90.0,
            "ResearchClaim": 55.0,
            "EvidenceQuote": 50.0,
        }.get(str(bundle_type), 0.0)
    if question_type == "driver":
        return {
            "BusinessFactor": 90.0,
            "BusinessActivity": 85.0,
            "ExternalFactorExposure": 60.0,
            "ResearchClaim": 65.0,
            "EvidenceQuote": 60.0,
        }.get(str(bundle_type), 0.0)
    if question_type == "scenario":
        boost = {
            "ExternalFactorExposure": 120.0,
            "BusinessFactor": 70.0,
            "ResearchClaim": 55.0,
            "EvidenceQuote": 50.0,
            "MetricObservation": 35.0,
        }.get(str(bundle_type), 0.0)
        if bundle_type == "ExternalFactorExposure" and obj.get("scenario_effects"):
            boost += 35.0
        return boost
    return 0.0


def _annotate_retrieval_answerability(
    plan: QueryPlan,
    candidates: Sequence[Mapping[str, Any]],
) -> tuple[list[dict[str, Any]], dict[str, Any]]:
    query_frame = build_query_frame(plan.question)
    requires_direct = _question_requires_direct_match(plan.question)
    annotated: list[dict[str, Any]] = []

    for candidate in candidates:
        bundle = dict(candidate)
        match = classify_topic_match(query_frame, build_evidence_frame(_bundle_topic_like(bundle)))
        trace_status, trace_counts = _bundle_trace_status(bundle)
        tier = _retrieval_tier(match, trace_status)
        missing_required = sorted(set(match.get("missing_required_facets", [])) | _missing_sector_facets(query_frame, match))
        trace_score = 1.0 if trace_status in {"traceable", "traceable_metric_lineage"} else 0.0
        directness_score = _final_directness_score(match, trace_score)
        matched_required = sorted(
            set(match.get("matched_required_facets", []))
            | set(match.get("matched_core_terms", []))
            | set(match.get("matched_mechanisms", []))
            | set(match.get("matched_sectors", []))
        )

        bundle.update(
            {
                "semantic_relevance": match.get("tier", "insufficient"),
                "trace_status": trace_status,
                "tier": tier,
                "evidence_chain_count": trace_counts["evidence_chain_count"],
                "support_depth": trace_counts["support_depth"],
                "support_quote_count": trace_counts["support_quote_count"],
                "support_claim_count": trace_counts["support_claim_count"],
                "directness_score": directness_score,
                "anchor_score": match.get("anchor_score", 0.0),
                "predicate_score": match.get("predicate_score", 0.0),
                "channel_score": match.get("channel_score", 0.0),
                "trace_score": trace_score,
                "specificity_score": match.get("specificity_score", 0.0),
                "generic_penalty": match.get("generic_penalty", 0.0),
                "matched_required_facets": matched_required,
                "matched_channel_facets": match.get("matched_channel_facets", []),
                "matched_predicates": match.get("matched_predicates", []),
                "missing_required_facets": missing_required,
                "why_tier": _retrieval_why_tier(tier, match, missing_required, requires_direct),
            }
        )
        annotated.append(bundle)

    if requires_direct or query_frame.must_for_direct:
        annotated.sort(key=_direct_answer_rank)

    direct_answerable = any(item.get("tier") == "traceable_direct" for item in annotated)
    related_context_available = any(
        item.get("tier")
        in {
            "traceable_related",
            "broad_related_candidate",
            "untraced_related",
            "untraced_direct_candidate",
        }
        for item in annotated
    )

    negative_answer_supported = bool(requires_direct and not direct_answerable and annotated)
    answer_mode = _recommended_answer_mode(
        direct_answerable=direct_answerable,
        related_context_available=related_context_available,
        negative_answer_supported=negative_answer_supported,
        has_candidates=bool(annotated),
    )

    return annotated, {
        "direct_answerable": bool(direct_answerable),
        "related_context_available": bool(related_context_available),
        "negative_answer_supported": bool(negative_answer_supported),
        "needs_user_clarification": False,
        "recommended_answer_mode": answer_mode,
        "question_requires_direct_match": bool(requires_direct),
        "query_frame": {
            "query_type": query_frame.query_type,
            "must_for_direct": sorted(query_frame.must_for_direct),
            "should_for_direct": sorted(query_frame.should_for_direct),
            "context_facets": sorted(query_frame.context_facets),
            "predicate_terms": sorted(query_frame.predicate_terms),
            "core_domain_terms": sorted(query_frame.core_domain_terms),
            "sector_terms": sorted(query_frame.sector_terms),
            "mechanism_terms": sorted(query_frame.mechanism_terms),
            "impact_channels": sorted(query_frame.impact_channels),
            "required_for_direct": sorted(query_frame.required_for_direct),
        },
    }


def _split_retrieval_context(candidates: Sequence[dict[str, Any]]) -> dict[str, list[dict[str, Any]]]:
    direct_evidence: list[dict[str, Any]] = []
    related_context: list[dict[str, Any]] = []
    rejected_context: list[dict[str, Any]] = []

    for candidate in candidates:
        tier = candidate.get("tier")
        if tier == "traceable_direct":
            direct_evidence.append(candidate)
        elif tier == "not_answerable":
            rejected_context.append(candidate)
        else:
            related_context.append(candidate)

    return {
        "direct_evidence": direct_evidence,
        "related_context": related_context,
        "rejected_context": rejected_context,
    }


def _compact_candidates_from_query_context(context: Mapping[str, Any], *, limit: int) -> list[dict[str, Any]]:
    """Flatten query_context topic candidates into retrieve-compatible compact rows."""
    candidates: list[dict[str, Any]] = []
    for ticker_candidate in context.get("ticker_candidates") or []:
        matched_topics = ticker_candidate.get("matched_topics") or []
        if not matched_topics:
            candidate = _compact_candidate_from_topic(ticker_candidate, ticker_candidate=ticker_candidate)
            if candidate:
                candidates.append(candidate)
            continue
        for topic in matched_topics:
            candidate = _compact_candidate_from_topic(topic, ticker_candidate=ticker_candidate)
            if candidate:
                candidates.append(candidate)

    if not candidates:
        for ticker, items in (context.get("results_by_ticker") or {}).items():
            for item in items or []:
                enriched = dict(item)
                enriched.setdefault("ticker", ticker)
                candidate = _compact_candidate_from_topic(enriched, ticker_candidate=enriched)
                if candidate:
                    candidates.append(candidate)

    return _dedupe_compact_candidates(candidates)[:limit]


def _compact_fallback_topics(plan: QueryPlan) -> list[str]:
    topics = [str(topic).strip() for topic in plan.topics if str(topic).strip()]
    if topics:
        return topics
    return [plan.question]


def _compact_candidate_from_topic(
    topic: Mapping[str, Any],
    *,
    ticker_candidate: Mapping[str, Any],
) -> dict[str, Any] | None:
    source_object_ids = _coerce_id_list(topic.get("source_object_ids"))
    top_traceable_object_ids = _coerce_id_list(topic.get("top_traceable_object_ids"))
    object_id = (
        topic.get("primary_object_id")
        or topic.get("object_id")
        or (top_traceable_object_ids[0] if top_traceable_object_ids else None)
        or (source_object_ids[0] if source_object_ids else None)
        or topic.get("topic_id")
        or ticker_candidate.get("object_id")
    )
    if not object_id:
        return None

    label = str(topic.get("topic_label") or topic.get("label") or topic.get("title") or "").strip()
    summary = str(
        topic.get("topic_summary")
        or topic.get("summary")
        or topic.get("compact_text")
        or topic.get("text")
        or label
    ).strip()
    if not summary:
        summary = str(object_id)

    trace_status = (
        topic.get("trace_status")
        or ticker_candidate.get("trace_status")
        or ("traceable" if top_traceable_object_ids else "untraced")
    )
    tier = topic.get("tier") or ticker_candidate.get("tier")
    if not tier:
        tier = "traceable_related" if trace_status in {"traceable", "traceable_metric_lineage"} else "broad_related_candidate"

    object_type = (
        topic.get("primary_object_type")
        or topic.get("object_type")
        or topic.get("type")
        or ticker_candidate.get("object_type")
        or ticker_candidate.get("type")
        or "CompanyTopic"
    )
    ticker = topic.get("ticker") or ticker_candidate.get("ticker")
    candidate = {
        "id": str(object_id),
        "object_id": str(object_id),
        "type": object_type,
        "object_type": object_type,
        "ticker": ticker,
        "period": topic.get("period") or ticker_candidate.get("period"),
        "document_type": topic.get("document_type") or topic.get("filing_type") or ticker_candidate.get("document_type"),
        "title": label or summary,
        "text": summary,
        "summary": summary,
        "topic_id": topic.get("topic_id"),
        "topic_label": label,
        "semantic_relevance": topic.get("semantic_relevance") or ticker_candidate.get("semantic_relevance"),
        "trace_status": trace_status,
        "tier": tier,
        "why_tier": topic.get("why_tier") or ticker_candidate.get("why_tier"),
        "evidence_chain_count": _coerce_int(
            topic.get("evidence_chain_count") or ticker_candidate.get("evidence_chain_count")
        ),
        "support_depth": _coerce_int(topic.get("support_depth") or ticker_candidate.get("support_depth")),
        "support_quote_count": _coerce_int(
            topic.get("support_quote_count") or ticker_candidate.get("support_quote_count")
        ),
        "support_claim_count": _coerce_int(
            topic.get("support_claim_count") or ticker_candidate.get("support_claim_count")
        ),
        "matched_required_facets": topic.get("matched_required_facets")
        or ticker_candidate.get("matched_required_facets")
        or [],
        "missing_required_facets": topic.get("missing_required_facets")
        or ticker_candidate.get("missing_required_facets")
        or [],
        "matched_related_facets": topic.get("matched_related_facets")
        or ticker_candidate.get("matched_related_facets")
        or [],
        "source_object_ids": source_object_ids,
        "top_traceable_object_ids": top_traceable_object_ids,
        "internal_only_fields": ["object_id", "topic_id", "source_object_ids", "top_traceable_object_ids"],
        "compact_only": True,
    }
    return candidate


def _coerce_id_list(value: Any) -> list[str]:
    if value is None:
        return []
    if isinstance(value, str):
        stripped = value.strip()
        if not stripped:
            return []
        if stripped.startswith("["):
            import json

            try:
                parsed = json.loads(stripped)
            except json.JSONDecodeError:
                return [stripped]
            return [str(item) for item in parsed if item]
        return [stripped]
    if isinstance(value, Sequence) and not isinstance(value, (bytes, bytearray)):
        return [str(item) for item in value if item]
    return [str(value)]


def _coerce_int(value: Any) -> int:
    try:
        return int(value or 0)
    except (TypeError, ValueError):
        return 0


def _dedupe_compact_candidates(candidates: Sequence[dict[str, Any]]) -> list[dict[str, Any]]:
    seen: set[str] = set()
    deduped: list[dict[str, Any]] = []
    for candidate in candidates:
        candidate_id = str(candidate.get("id") or candidate.get("object_id") or "")
        if not candidate_id or candidate_id in seen:
            continue
        seen.add(candidate_id)
        deduped.append(candidate)
    return deduped


def _restore_compact_trace_metadata(
    annotated: Sequence[dict[str, Any]],
    raw_candidates: Sequence[Mapping[str, Any]],
) -> list[dict[str, Any]]:
    raw_by_id = {str(item.get("id") or item.get("object_id")): item for item in raw_candidates}
    restored: list[dict[str, Any]] = []
    for candidate in annotated:
        item = dict(candidate)
        raw = raw_by_id.get(str(item.get("id") or item.get("object_id"))) or {}
        raw_trace_status = raw.get("trace_status")
        if raw_trace_status in {"traceable", "traceable_metric_lineage"}:
            item["trace_status"] = raw_trace_status
            if item.get("tier") in {"untraced_related", "broad_related_candidate", "untraced_direct_candidate"}:
                item["tier"] = "traceable_related"
            for field_name in (
                "evidence_chain_count",
                "support_depth",
                "support_quote_count",
                "support_claim_count",
            ):
                raw_value = _coerce_int(raw.get(field_name))
                if raw_value:
                    item[field_name] = raw_value
        restored.append(item)
    return restored


def _recommended_trace_object_ids_from_context(
    context: Mapping[str, Any],
    candidates: Sequence[Mapping[str, Any]],
) -> list[str]:
    trace_ids: list[str] = []
    for item in context.get("recommended_tools") or []:
        if not isinstance(item, Mapping):
            continue
        object_id = item.get("object_id") or item.get("target_object_id")
        if object_id:
            trace_ids.append(str(object_id))
    for candidate in candidates:
        trace_ids.extend(_coerce_id_list(candidate.get("top_traceable_object_ids")))
        trace_ids.extend(_coerce_id_list(candidate.get("source_object_ids"))[:1])
        object_id = candidate.get("object_id")
        if object_id:
            trace_ids.append(str(object_id))
    return _unique_preserve_order(trace_ids)[:3]


def _unique_preserve_order(values: Sequence[str]) -> list[str]:
    seen: set[str] = set()
    unique: list[str] = []
    for value in values:
        if not value or value in seen:
            continue
        seen.add(value)
        unique.append(value)
    return unique


def _compact_retrieve_trace_required(plan: QueryPlan, answerability: Mapping[str, Any]) -> bool:
    if plan.require_trace or answerability.get("direct_answerable"):
        return True
    question = plan.question.lower()
    exact_terms = {
        "date",
        "amount",
        "maturity",
        "covenant",
        "contract",
        "agreement",
        "guidance",
        "capacity",
        "target",
        "threshold",
        "when",
        "how much",
        "언제",
        "얼마",
        "금액",
        "만기",
        "계약",
        "약정",
        "가이던스",
        "용량",
        "목표",
        "기준",
    }
    return any(term in question for term in exact_terms)


def _bundle_topic_like(bundle: Mapping[str, Any]) -> Mapping[str, Any]:
    obj = bundle.get("object") if isinstance(bundle.get("object"), Mapping) else {}
    evidence = bundle.get("evidence") if isinstance(bundle.get("evidence"), Mapping) else {}
    quality = bundle.get("quality") if isinstance(bundle.get("quality"), Mapping) else {}
    primary_parts = [str(bundle.get("text") or ""), str(obj.get("text") or ""), str(obj.get("topic") or "")]
    evidence_parts: list[str] = []
    impact_channels: list[str] = []

    for key in ("external_factor", "factor", "metric", "metric_name", "impact", "impact_channel", "channel"):
        value = obj.get(key)
        if value:
            primary_parts.append(str(value))
            impact_channels.append(str(value))

    for key in ("claims", "quotes", "spans"):
        values = evidence.get(key)
        if isinstance(values, Sequence) and not isinstance(values, (str, bytes)):
            for value in values:
                if isinstance(value, Mapping):
                    evidence_parts.append(str(value.get("text") or value.get("claim_text") or value.get("quote") or ""))

    primary_text = " ".join(part for part in primary_parts if part).strip()
    evidence_text = " ".join(part for part in evidence_parts if part).strip()
    topic_text = " ".join(part for part in (primary_text, evidence_text) if part).strip()
    return {
        "topic_id": f"retrieve:{bundle.get('id')}",
        "topic_label": topic_text[:240],
        "topic_summary": topic_text,
        "topic_text": topic_text,
        "facet_text": " ".join(impact_channels),
        "impact_channels": impact_channels,
        "primary_object_id": bundle.get("id"),
        "primary_object_type": bundle.get("type"),
        "ticker": bundle.get("ticker"),
        "evidence_strength": quality.get("evidence_grade") or obj.get("evidence_strength") or obj.get("evidence_grade"),
        "support_quote_count": len(evidence.get("quotes") or []),
        "support_claim_count": len(evidence.get("claims") or []),
        "specificity_score": obj.get("specificity_score") or 0.5,
    }


def _bundle_trace_status(bundle: Mapping[str, Any]) -> tuple[str, dict[str, int | None]]:
    evidence = bundle.get("evidence") if isinstance(bundle.get("evidence"), Mapping) else {}
    obj = bundle.get("object") if isinstance(bundle.get("object"), Mapping) else {}
    quote_count = len(evidence.get("quotes") or [])
    claim_count = len(evidence.get("claims") or [])
    span_count = len(evidence.get("spans") or [])
    chain_count = quote_count + claim_count + span_count
    support_depth = 1 if chain_count else None
    trace_status = "traceable" if chain_count else "untraced"

    if bundle.get("type") == "MetricObservation" and not chain_count:
        lineage_fields = ("source_fact_ids", "source_metric_ids", "calculation_id", "calculation_ids")
        if any(obj.get(field) for field in lineage_fields):
            trace_status = "traceable_metric_lineage"
            chain_count = 1
            support_depth = 1

    return trace_status, {
        "evidence_chain_count": chain_count,
        "support_depth": support_depth,
        "support_quote_count": quote_count,
        "support_claim_count": claim_count,
    }


def _retrieval_tier(match: Mapping[str, Any], trace_status: str) -> str:
    semantic = str(match.get("tier") or "insufficient")
    traceable = trace_status in {"traceable", "traceable_metric_lineage"}
    if semantic == "direct":
        return "traceable_direct" if traceable else "untraced_direct_candidate"
    if semantic in {"strong_related", "related"}:
        return "traceable_related" if traceable else "untraced_related"
    if traceable:
        return "broad_related_candidate"
    return "not_answerable"


def _final_directness_score(match: Mapping[str, Any], trace_score: float) -> float:
    score = (
        0.35 * float(match.get("anchor_score") or 0.0)
        + 0.20 * float(match.get("predicate_score") or 0.0)
        + 0.15 * float(match.get("channel_score") or 0.0)
        + 0.20 * trace_score
        + 0.10 * float(match.get("specificity_score") or 0.0)
        - float(match.get("generic_penalty") or 0.0)
    )
    return round(max(0.0, min(1.0, score)), 4)


def _direct_answer_rank(bundle: Mapping[str, Any]) -> tuple[int, int, int]:
    tier_order = {
        "traceable_direct": 0,
        "untraced_direct_candidate": 1,
        "traceable_related": 2,
        "broad_related_candidate": 3,
        "untraced_related": 4,
        "not_answerable": 5,
    }
    return (
        tier_order.get(str(bundle.get("tier")), 9),
        -int(float(bundle.get("directness_score") or 0.0) * 1000),
        int(bundle.get("support_depth") or 99),
    )


def _retrieval_why_tier(
    tier: str,
    match: Mapping[str, Any],
    missing_required: Sequence[str],
    requires_direct: bool,
) -> str:
    if tier == "traceable_direct":
        return "Question premise and evidence premise match directly, with traceable support."
    if tier == "untraced_direct_candidate":
        return "Question premise appears to match directly, but no explicit evidence chain was found."
    if tier == "traceable_related":
        return "Evidence is traceable and related, but it is broader or less specific than the question premise."
    if tier == "broad_related_candidate":
        if missing_required:
            return (
                "Retrieved evidence is traceable but does not match required direct facets: "
                + ", ".join(missing_required)
                + "."
            )
        if requires_direct:
            return "Retrieved evidence is traceable, but it does not support a direct exposure answer."
        return "Retrieved evidence is traceable but only broadly related."
    if match.get("generic_only"):
        return "Only generic retrieval terms matched; this is not sufficient evidence."
    return "No sufficient direct or related evidence classification was found."


def _missing_sector_facets(query_frame: Any, match: Mapping[str, Any]) -> set[str]:
    sector_terms = set(getattr(query_frame, "sector_terms", set()) or set())
    matched_sectors = set(match.get("matched_sectors", []) or [])
    return sector_terms - matched_sectors


def _question_requires_direct_match(question: str) -> bool:
    lowered = question.lower()
    direct_markers = (
        "직접",
        "direct",
        "directly",
        "노출",
        "exposure",
        "exposed",
    )
    return any(marker in lowered for marker in direct_markers)


def _recommended_answer_mode(
    *,
    direct_answerable: bool,
    related_context_available: bool,
    negative_answer_supported: bool,
    has_candidates: bool,
) -> str:
    if direct_answerable:
        return "direct_evidence"
    if negative_answer_supported and related_context_available:
        return "no_direct_evidence_with_related_context"
    if negative_answer_supported:
        return "no_direct_evidence"
    if related_context_available:
        return "related_context"
    if has_candidates:
        return "retrieved_but_not_answerable"
    return "not_answerable"


def _clean_question_topic(question: str) -> str:
    cleaned = question
    for pattern in (
        r"\b10-[KQ]\b",
        r"\b10[KQ]\b",
        r"\bFY\d{4}(?:Q[1-4?])?\b",
        r"\b(latest|recent|current|compare|comparison|versus)\b",
    ):
        cleaned = re.sub(pattern, " ", cleaned, flags=re.IGNORECASE)
    cleaned = re.sub(r"\s+", " ", cleaned).strip(" ?.,")
    return cleaned


def _dedupe_bundles(bundles: Iterable[dict[str, Any]]) -> list[dict[str, Any]]:
    seen = set()
    output = []
    for bundle in bundles:
        bundle_id = bundle.get("id")
        if not bundle_id or bundle_id in seen:
            continue
        seen.add(bundle_id)
        output.append(bundle)
    return output


def _dedupe_documents(documents: Iterable[dict[str, Any]]) -> list[dict[str, Any]]:
    seen = set()
    output = []
    for document in documents:
        key = (document["ticker"], document["document_type"], document["period"])
        if key in seen:
            continue
        seen.add(key)
        output.append(document)
    return output


def _dedupe_quality_events(events: Iterable[dict[str, Any]]) -> list[dict[str, Any]]:
    seen = set()
    output = []
    for event in events:
        if event["id"] in seen:
            continue
        seen.add(event["id"])
        output.append(event)
    return output


def _period_sort_key(period: str) -> tuple[int, int, str]:
    match = re.match(r"FY(\d{4})(?:Q([1-4?]))?$", period, flags=re.IGNORECASE)
    if not match:
        return (0, 0, period)
    year = int(match.group(1))
    quarter_raw = match.group(2)
    quarter = int(quarter_raw) if quarter_raw and quarter_raw.isdigit() else 0
    return (year, quarter, period)


def _unique(values: Iterable[str]) -> list[str]:
    seen = set()
    output = []
    for value in values:
        if value in seen:
            continue
        seen.add(value)
        output.append(value)
    return output


def _unique_nullable(values: Iterable[str | None]) -> list[str | None]:
    seen = set()
    output: list[str | None] = []
    for value in values:
        key = value if value is None else str(value)
        if key in seen:
            continue
        seen.add(key)
        output.append(value)
    return output


def _replace_plan_tickers(plan: QueryPlan, tickers: list[str]) -> QueryPlan:
    return QueryPlan(
        question=plan.question,
        intent=plan.intent,
        tickers=tickers,
        document_types=plan.document_types,
        periods=plan.periods,
        period_policy=plan.period_policy,
        topics=plan.topics,
        metric=plan.metric,
        object_types=plan.object_types,
        include_rejected=plan.include_rejected,
        require_trace=plan.require_trace,
        limit=plan.limit,
    )


def _empty_retrieve_response(
    plan: QueryPlan,
    started_at: float,
    *,
    unavailable_tickers: Sequence[str],
) -> dict[str, Any]:
    import time

    answerability = {
        "direct_answerable": False,
        "related_context_available": False,
        "negative_answer_supported": True,
        "needs_user_clarification": False,
        "recommended_answer_mode": "not_answerable",
        "question_requires_direct_match": _question_requires_direct_match(plan.question),
        "query_frame": {},
    }
    return {
        "answerability": answerability,
        "recommended_answer_mode": "not_answerable",
        "query_frame": {},
        "plan": plan.to_dict(),
        "resolved_periods": [],
        "direct_evidence": [],
        "related_context": [],
        "rejected_context": [],
        "recommended_trace_object_ids": [],
        "trace_required": False,
        "trace_policy": {
            "mode": "lazy",
            "reason": "ticker_guard_no_index_rows",
            "max_recommended_trace_objects": 0,
        },
        "search_diagnostics": {
            "warnings": ["ticker_not_available"],
            "unavailable_tickers": list(unavailable_tickers),
        },
        "audit": {
            "executed_queries": [],
            "compact_retrieve": True,
            "evidence_bundle_included": False,
            "ticker_guard": True,
            "unavailable_tickers": list(unavailable_tickers),
        },
        "timing_ms": {"total": round((time.perf_counter() - started_at) * 1000, 3)},
    }


def _retrieve_compact_cache_key(
    index_path: Any,
    plan: QueryPlan,
    resolved_periods: Sequence[str],
) -> tuple[Any, ...]:
    path_text = str(index_path)
    try:
        index_mtime_ns = index_path.stat().st_mtime_ns
    except OSError:
        index_mtime_ns = None
    return (
        path_text,
        index_mtime_ns,
        plan.question,
        tuple(plan.tickers),
        tuple(plan.document_types),
        tuple(resolved_periods),
        tuple(plan.object_types),
        bool(plan.include_rejected),
        int(plan.limit),
    )


def _retrieve_compact_cache_get(key: tuple[Any, ...]) -> dict[str, Any] | None:
    with _RETRIEVE_COMPACT_CACHE_LOCK:
        cached = _RETRIEVE_COMPACT_CACHE.get(key)
        if cached is None:
            return None
        _RETRIEVE_COMPACT_CACHE.move_to_end(key)
        return copy.deepcopy(cached)


def _retrieve_compact_cache_set(key: tuple[Any, ...], value: dict[str, Any]) -> None:
    with _RETRIEVE_COMPACT_CACHE_LOCK:
        _RETRIEVE_COMPACT_CACHE[key] = copy.deepcopy(value)
        _RETRIEVE_COMPACT_CACHE.move_to_end(key)
        while len(_RETRIEVE_COMPACT_CACHE) > _RETRIEVE_COMPACT_CACHE_MAX:
            _RETRIEVE_COMPACT_CACHE.popitem(last=False)


def retriever_cache_status() -> dict[str, dict[str, int]]:
    """Return in-process retriever cache sizes for MCP health/debugging."""
    with _RETRIEVE_COMPACT_CACHE_LOCK:
        return {
            "retrieve_compact": {
                "size": len(_RETRIEVE_COMPACT_CACHE),
                "max": _RETRIEVE_COMPACT_CACHE_MAX,
            }
        }
