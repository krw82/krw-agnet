"""Deterministic research routing and context policy.

The router is deliberately not an agent. It selects one primary context and a
small set of allowed/forbidden paths so MCP tools avoid expensive irrelevant
searches while the outer AI agent keeps analytical judgment.
"""

from __future__ import annotations

import re
from typing import Any, Mapping, Sequence

from krw_capability_runtime.agent_index.research_types import ResearchIntent, ResearchRoute


_VALUATION_TERMS = (
    "target price",
    "price target",
    "12-month target",
    "12 month target",
    "fair value",
    "valuation",
    "투자의견",
    "목표가",
    "목표치",
    "12개월 목표",
)
_DIRECT_TERMS = (
    "direct exposure",
    "directly exposed",
    "direct impact",
    "direct benefit",
    "직접 노출",
    "직접 영향",
    "직접 수혜",
    "직접적으로",
)
_COMPARISON_TERMS = (" vs ", " versus ", "compare", "comparison", "비교", "대비")
_DISCOVERY_TERMS = (
    "which companies",
    "find companies",
    "screen",
    "찾아줘",
    "어떤 기업",
    "수혜 기업",
    "영향 큰",
    "피해 큰",
)
_OVERVIEW_TERMS = (
    "business model",
    "make money",
    "revenue driver",
    "revenue drivers",
    "사업 구조",
    "사업모델",
    "사업 모델",
    "어떻게 돈",
    "돈을 벌",
    "매출원",
    "매출 동인",
    "성장 동인",
    "어디에서 성장",
    "어디에서 매출",
    "성장과 매출이 나오는지",
    "매출과 이익에 어떻게 연결",
    "매출과 마진에 어떻게 연결",
    "성장과 마진에 어떻게 연결",
    "매출 성장에 어떻게 연결",
    "매출 성장에 어떻게 기여",
    "성장에 어떻게 기여",
    "어떻게 연결되는지",
    "어떻게 기여하는지",
    "어떻게 만드는지",
)
_RECENT_TERMS = (
    "recent",
    "latest",
    "most recent",
    "latest quarter",
    "recent quarter",
    "current quarter",
    "최근",
    "최신",
    "최근 분기",
    "최근 매출",
    "최근 매출 동인",
    "최근 동인",
    "최근 실적",
)
_RISK_TERMS = (
    "risk",
    "thesis",
    "pressure",
    "headwind",
    "burden",
    "disruption",
    "exposure",
    "cyber",
    "regulatory",
    "litigation",
    "supply chain",
    "리스크",
    "위험",
    "부담",
    "압박",
    "흔드는",
    "갉아먹",
    "충격",
    "노출",
    "불확실",
    "사이버",
)
_METRIC_TERMS = (
    "revenue",
    "sales",
    "net sales",
    "margin",
    "gross margin",
    "operating margin",
    "cash flow",
    "capex",
    "eps",
    "growth",
    "growth rate",
    "cagr",
    "share",
    "ratio",
    "nii",
    "nim",
    "debt",
    "asset",
    "매출",
    "수익",
    "마진",
    "영업이익",
    "순이익",
    "현금흐름",
    "비중",
    "성장률",
    "증가율",
    "총부채",
    "총자산",
)
_EXPLICIT_CALCULATION_TERMS = (
    "calculate",
    "calculation",
    "trend",
    "series",
    "quarterly",
    "annual",
    "계산",
    "산출",
    "추이",
    "시계열",
    "분기별",
    "연도별",
)


def route_research(question: str, *, tickers: Sequence[str] | None = None) -> ResearchRoute:
    """Return a fast deterministic route for a natural-language question."""
    raw = str(question or "")
    text = raw.casefold()
    ticker_count = len(list(tickers or []))

    if _contains_any(text, _VALUATION_TERMS):
        return ResearchRoute(
            intent=ResearchIntent.VALUATION_STOP,
            confidence=0.96,
            primary_context="valuation_stop_context",
            forbidden_contexts=["broad_retrieve", "unscoped_query", "deep_metric_series"],
            why_route="valuation_or_price_target_terms",
        )
    if _contains_any(text, _DIRECT_TERMS):
        return ResearchRoute(
            intent=ResearchIntent.DIRECT_EXPOSURE,
            confidence=0.90,
            primary_context="direct_exposure_context",
            secondary_contexts=["risk_context_optional"],
            forbidden_contexts=["deep_metric_series", "broad_direct_promotion"],
            why_route="direct_exposure_terms",
        )
    has_risk = _contains_any(text, _RISK_TERMS)
    has_metric = _contains_any(text, _METRIC_TERMS)
    has_overview = _contains_any(text, _OVERVIEW_TERMS)
    has_period = bool(re.search(r"(?:19|20)\d{2}|(?:fy|cy)\d{4}", text))
    explicit_calculation = _contains_any(text, _EXPLICIT_CALCULATION_TERMS)
    if ticker_count >= 2:
        return ResearchRoute(
            intent=ResearchIntent.COMPARISON,
            confidence=0.90,
            primary_context="compare_context",
            secondary_contexts=["metric_context_or_risk_context_by_compare_type"],
            forbidden_contexts=["raw_fts_winner_by_hit_count"],
            why_route="multiple_tickers",
        )
    if has_risk and not explicit_calculation:
        return ResearchRoute(
            intent=ResearchIntent.RISK_THESIS,
            confidence=0.82,
            primary_context="risk_context",
            secondary_contexts=["metric_context_optional"],
            forbidden_contexts=["deep_metric_series", "all_xbrl_search"],
            why_route="risk_terms_without_explicit_metric_series",
        )
    if has_overview and not (has_period or explicit_calculation):
        return ResearchRoute(
            intent=ResearchIntent.COMPANY_OVERVIEW,
            confidence=0.84,
            primary_context="company_overview_context",
            secondary_contexts=["top_level_metric_context_optional"],
            forbidden_contexts=["deep_metric_series", "not_answerable_without_topic_attempt"],
            why_route="company_overview_terms_before_metric_terms",
        )
    if has_metric and (has_period or explicit_calculation):
        return ResearchRoute(
            intent=ResearchIntent.METRIC_SERIES,
            confidence=0.88,
            primary_context="metric_context",
            forbidden_contexts=["broad_retrieve", "company_topic_discovery_first"],
            why_route="metric_terms_with_period_or_calculation_terms",
        )
    if _contains_any(text, _COMPARISON_TERMS):
        return ResearchRoute(
            intent=ResearchIntent.COMPARISON,
            confidence=0.86,
            primary_context="compare_context",
            secondary_contexts=["metric_context_or_risk_context_by_compare_type"],
            forbidden_contexts=["raw_fts_winner_by_hit_count"],
            why_route="comparison_terms_or_multiple_tickers",
        )
    if _contains_any(text, _DISCOVERY_TERMS):
        return ResearchRoute(
            intent=ResearchIntent.DISCOVERY,
            confidence=0.84,
            primary_context="discovery_context",
            forbidden_contexts=["deep_metric_series", "per_ticker_full_loop"],
            why_route="discovery_terms",
        )
    if has_overview:
        return ResearchRoute(
            intent=ResearchIntent.COMPANY_OVERVIEW,
            confidence=0.82,
            primary_context="company_overview_context",
            secondary_contexts=["top_level_metric_context_optional"],
            forbidden_contexts=["deep_metric_series", "not_answerable_without_topic_attempt"],
            why_route="company_overview_terms",
        )

    if has_risk:
        return ResearchRoute(
            intent=ResearchIntent.RISK_THESIS,
            confidence=0.72,
            primary_context="risk_context",
            secondary_contexts=["metric_context_optional"],
            forbidden_contexts=["deep_metric_series", "all_xbrl_search"],
            why_route="risk_terms",
        )
    if has_metric:
        return ResearchRoute(
            intent=ResearchIntent.METRIC_SERIES,
            confidence=0.68,
            primary_context="metric_context",
            forbidden_contexts=["broad_retrieve"],
            why_route="metric_terms",
        )
    return ResearchRoute(
        intent=ResearchIntent.GENERAL,
        confidence=0.55,
        primary_context="company_overview_context",
        secondary_contexts=["risk_context_optional"],
        forbidden_contexts=["open_ended_tool_loop"],
        why_route="fallback_general_research",
    )


def intent_profile_from_route(route: ResearchRoute) -> dict[str, Any]:
    return {
        "intent": route.intent.value,
        "primary_context": route.primary_context,
        "confidence": route.confidence,
        "secondary_contexts": list(route.secondary_contexts),
        "forbidden_contexts": list(route.forbidden_contexts),
        "depth": route.depth,
        "why_route": route.why_route,
    }


def period_display_policy_for_question(question: str, route: ResearchRoute | Mapping[str, Any] | Any) -> dict[str, Any]:
    """Return user-facing period label and recency policy for answer synthesis.

    The ontology serving period keys are CY-style labels. Source filings may use
    fiscal-year wording, but normal user-facing answers should keep the serving
    label and only mention fiscal calendar details as an optional parenthetical.
    """
    route = route if isinstance(route, ResearchRoute) else route_from_intent(route.get("intent") if isinstance(route, Mapping) else route)
    text = str(question or "").casefold()
    latest_first = _contains_any(text, _RECENT_TERMS)
    overview_requested = _contains_any(text, _OVERVIEW_TERMS) or route.intent == ResearchIntent.COMPANY_OVERVIEW
    return {
        "user_label_style": "CY",
        "avoid_fy_label": True,
        "source_fiscal_note": "optional_parenthetical_only",
        "latest_first": latest_first,
        "latest_source_priority": ["10-Q", "10-K"] if latest_first else ["10-K", "10-Q"],
        "annual_baseline_required": bool(latest_first and overview_requested),
        "annual_baseline_role": "business_mix_context" if latest_first and overview_requested else None,
        "quarter_label_template": "{period} {document_type} 기준",
        "annual_label_template": "{period} {document_type} 기준",
        "do_not_write": ["FY as the primary user-facing period label"],
    }


def preferred_answer_order_for_question(question: str, route: ResearchRoute | Mapping[str, Any] | Any) -> list[str]:
    route = route if isinstance(route, ResearchRoute) else route_from_intent(route.get("intent") if isinstance(route, Mapping) else route)
    policy = period_display_policy_for_question(question, route)
    if policy.get("latest_first") and policy.get("annual_baseline_required"):
        return ["latest_quarter_drivers", "annual_revenue_mix", "segment_interpretation"]
    if policy.get("latest_first"):
        return ["latest_period_evidence", "supporting_context", "interpretation"]
    if route.intent == ResearchIntent.COMPANY_OVERVIEW:
        return ["annual_revenue_mix", "business_driver_summary", "recent_context_if_available"]
    if route.intent == ResearchIntent.METRIC_SERIES:
        return ["metric_table", "growth_or_share_calculation", "interpretation"]
    if route.intent == ResearchIntent.RISK_THESIS:
        return ["risk_channel", "evidence_to_financial_path", "implication"]
    return ["answer", "supporting_evidence", "caveats"]


def route_from_intent(intent: Any) -> ResearchRoute:
    intent_value = str(intent.value if isinstance(intent, ResearchIntent) else intent or ResearchIntent.GENERAL.value)
    for candidate in ResearchIntent:
        if candidate.value == intent_value:
            return ResearchRoute(
                intent=candidate,
                confidence=0.70,
                primary_context=_primary_context_for_intent(candidate),
                forbidden_contexts=_forbidden_contexts_for_intent(candidate),
                why_route="intent_policy_lookup",
            )
    return ResearchRoute(
        intent=ResearchIntent.GENERAL,
        confidence=0.55,
        primary_context="company_overview_context",
        forbidden_contexts=["open_ended_tool_loop"],
        why_route="unknown_intent_policy_fallback",
    )


def context_policy_for_route(route: ResearchRoute | Mapping[str, Any] | Any) -> dict[str, Any]:
    if not isinstance(route, ResearchRoute):
        route = route_from_intent(route.get("intent") if isinstance(route, Mapping) else route)
    intent = route.intent
    policy: dict[str, Any] = {
        "primary_context": route.primary_context,
        "run_metric_series": True,
        "run_typed_projection": True,
        "run_company_topics": True,
        "run_object_fallback": False,
        "allowed_next_tools": ["krw_ontology_trace", "krw_ontology_chain"],
        "do_not_call": list(route.forbidden_contexts),
        "max_additional_tool_calls": 2,
    }
    if intent == ResearchIntent.METRIC_SERIES:
        policy.update(
            {
                "run_typed_projection": False,
                "run_company_topics": False,
                "allowed_next_tools": ["krw_ontology_trace"],
                "do_not_call": [*policy["do_not_call"], "broad_retrieve"],
                "max_additional_tool_calls": 1,
            }
        )
    elif intent in {ResearchIntent.RISK_THESIS, ResearchIntent.DIRECT_EXPOSURE, ResearchIntent.COMPANY_OVERVIEW, ResearchIntent.DISCOVERY, ResearchIntent.COMPARISON}:
        policy["run_metric_series"] = False
        policy["do_not_call"] = [
            *policy["do_not_call"],
            "deep_metric_series",
            "broad_retrieve",
            "unscoped_query",
        ]
    if intent == ResearchIntent.VALUATION_STOP:
        policy.update(
            {
                "run_metric_series": False,
                "run_typed_projection": False,
                "run_company_topics": False,
                "allowed_next_tools": [],
                "do_not_call": [
                    "krw_ontology_query",
                    "krw_ontology_retrieve",
                    "krw_ontology_compare",
                    "krw_ontology_trace",
                    "krw_ontology_chain",
                ],
                "max_additional_tool_calls": 0,
            }
        )
    policy["do_not_call"] = _dedupe(policy["do_not_call"])
    return policy


def _primary_context_for_intent(intent: ResearchIntent) -> str:
    return {
        ResearchIntent.METRIC_SERIES: "metric_context",
        ResearchIntent.RISK_THESIS: "risk_context",
        ResearchIntent.COMPARISON: "compare_context",
        ResearchIntent.DISCOVERY: "discovery_context",
        ResearchIntent.DIRECT_EXPOSURE: "direct_exposure_context",
        ResearchIntent.FACTUAL_LOOKUP: "factual_lookup_context",
        ResearchIntent.COMPANY_OVERVIEW: "company_overview_context",
        ResearchIntent.VALUATION_STOP: "valuation_stop_context",
        ResearchIntent.AUDIT_DEBUG: "audit_debug_context",
        ResearchIntent.QUALITY_CHECK: "quality_check_context",
        ResearchIntent.GENERAL: "company_overview_context",
    }[intent]


def _forbidden_contexts_for_intent(intent: ResearchIntent) -> list[str]:
    if intent == ResearchIntent.METRIC_SERIES:
        return ["broad_retrieve", "company_topic_discovery_first"]
    if intent == ResearchIntent.VALUATION_STOP:
        return ["broad_retrieve", "unscoped_query", "deep_metric_series"]
    if intent == ResearchIntent.COMPARISON:
        return ["raw_fts_winner_by_hit_count"]
    if intent in {ResearchIntent.RISK_THESIS, ResearchIntent.DIRECT_EXPOSURE, ResearchIntent.COMPANY_OVERVIEW, ResearchIntent.DISCOVERY}:
        return ["deep_metric_series", "broad_retrieve", "unscoped_query"]
    return ["open_ended_tool_loop"]


def _contains_any(text: str, terms: Sequence[str]) -> bool:
    return any(term in text for term in terms)


def _dedupe(values: Sequence[Any]) -> list[str]:
    output: list[str] = []
    seen: set[str] = set()
    for value in values:
        text = str(value)
        if text and text not in seen:
            seen.add(text)
            output.append(text)
    return output
