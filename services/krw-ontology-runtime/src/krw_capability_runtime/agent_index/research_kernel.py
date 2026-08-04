"""Deterministic research-state kernel for MCP tools.

The kernel is not a final-answer generator and it does not run an internal
agent loop. It produces route, budget, directness, and bounded-autonomy state
that can be attached to the stable MCP v1 response envelope.
"""

from __future__ import annotations

import os
import time
from typing import Any, Mapping, Sequence

from krw_capability_runtime.agent_index.research_budget import budget_for_context
from krw_capability_runtime.agent_index.research_router import (
    context_policy_for_route,
    intent_profile_from_route,
    period_display_policy_for_question,
    preferred_answer_order_for_question,
    route_research,
)
from krw_capability_runtime.agent_index.research_types import ResearchRequest, ResearchRoute


KERNEL_VERSION = "research_kernel_v0.1"
KERNEL_CONTRACT_VERSION = "kernel.v1alpha"


class ResearchKernel:
    """Small deterministic control layer shared by MCP research tools."""

    def __init__(self, store: Any | None = None):
        self.store = store
        self.enabled = _env_flag("KRW_RESEARCH_KERNEL_ENABLED", default=True)
        self.shadow = _env_flag("KRW_RESEARCH_KERNEL_SHADOW", default=False)
        self.debug_timing = _env_flag("KRW_RESEARCH_KERNEL_DEBUG_TIMING", default=True)

    def route(self, request: ResearchRequest) -> ResearchRoute:
        return route_research(request.question, tickers=request.tickers)

    def build_envelope(
        self,
        request: ResearchRequest,
        *,
        route: ResearchRoute | None = None,
        research_status: str,
        answer_mode: str | None = None,
        research_pack: Mapping[str, Any] | None = None,
        answerability: Mapping[str, Any] | None = None,
        missing_parts: Sequence[Any] | None = None,
        recommended_tools: Sequence[Mapping[str, Any]] | None = None,
        agent_autonomy: Mapping[str, Any] | None = None,
        do_not_call: Sequence[Any] | None = None,
        started_at: float | None = None,
        timing_ms: Mapping[str, Any] | None = None,
    ) -> dict[str, Any]:
        route = route or self.route(request)
        policy = context_policy_for_route(route)
        period_display_policy = period_display_policy_for_question(request.question, route)
        preferred_answer_order = preferred_answer_order_for_question(request.question, route)
        budget = budget_for_context(route.primary_context, route.depth)
        autonomy = agent_autonomy if isinstance(agent_autonomy, Mapping) else {}
        if "allowed_next_tools" in autonomy:
            allowed_next_tools = list(autonomy.get("allowed_next_tools") or [])
        else:
            allowed_next_tools = list(policy.get("allowed_next_tools") or [])
        max_additional_tool_calls = int(
            autonomy.get("max_additional_tool_calls")
            if autonomy.get("max_additional_tool_calls") is not None
            else policy.get("max_additional_tool_calls") or 0
        )
        combined_do_not_call = _dedupe([*(do_not_call or []), *(policy.get("do_not_call") or [])])
        timing_payload = dict(timing_ms or {})
        if started_at is not None:
            timing_payload.setdefault("total_ms", int((time.perf_counter() - started_at) * 1000))
        research_pack = research_pack if isinstance(research_pack, Mapping) else {}
        answerability = answerability if isinstance(answerability, Mapping) else {}
        return {
            "version": KERNEL_VERSION,
            "contract_version": KERNEL_CONTRACT_VERSION,
            "enabled": self.enabled,
            "shadow": self.shadow,
            **intent_profile_from_route(route),
            "status": research_status,
            "answer_mode": answer_mode or _answer_mode_for_route(route, research_status, answerability),
            "allowed_next_tools": allowed_next_tools,
            "do_not_call": combined_do_not_call,
            "max_additional_tool_calls": max_additional_tool_calls,
            "missing_parts": [str(part) for part in (missing_parts or [])],
            "recommended_trace_count": len(list(recommended_tools or [])),
            "period_display_policy": period_display_policy,
            "preferred_answer_order": preferred_answer_order,
            "context_policy": policy,
            "budget": {
                "budget_ms": budget.max_ms,
                "max_topic_candidates": budget.max_topic_candidates,
                "max_object_candidates": budget.max_object_candidates,
                "max_metric_candidates": budget.max_metric_candidates,
                "max_trace_roots": budget.max_trace_roots,
                "max_chain_roots": budget.max_chain_roots,
                "max_fallback_passes": budget.max_fallback_passes,
                "exhausted": False,
            },
            "timing_ms": timing_payload,
            "packs_present": sorted(str(key) for key, value in research_pack.items() if value),
        }


def research_request_from_plan_fields(
    *,
    question: str,
    tickers: Sequence[str] | None,
    document_types: Sequence[str] | None,
    periods: Sequence[str] | None,
    universe: str | None,
    limit_results: int,
    limit_tickers: int,
    mode: str = "context",
) -> ResearchRequest:
    return ResearchRequest(
        question=question,
        tickers=[str(ticker).upper() for ticker in (tickers or [])],
        document_types=[str(value) for value in (document_types or [])],
        periods=[str(value).upper() for value in (periods or [])],
        universe=universe,
        limit_results=int(limit_results),
        limit_tickers=int(limit_tickers),
        mode=mode if mode in {"context", "retrieve", "compare", "query"} else "context",
    )


def _answer_mode_for_route(route: ResearchRoute, research_status: str, answerability: Mapping[str, Any]) -> str:
    if research_status == "out_of_scope_for_filing_ontology":
        return "filing_assumption_support_only"
    if route.intent.value == "metric_series":
        return "metric_series_answer"
    if route.intent.value == "risk_thesis":
        return "risk_mechanism_answer"
    if route.intent.value == "direct_exposure":
        if answerability.get("direct_answerable"):
            return "direct_exposure_answer"
        if answerability.get("related_context_available"):
            return "no_direct_evidence_with_related_context"
        return "no_direct_evidence"
    if route.intent.value == "company_overview":
        return "company_overview_answer"
    if route.intent.value == "comparison":
        return "comparison_research_state"
    if route.intent.value == "discovery":
        return "discovery_research_state"
    return "filing_research_answer"


def _env_flag(name: str, *, default: bool) -> bool:
    raw = os.getenv(name)
    if raw is None:
        return default
    return raw.strip().lower() not in {"0", "false", "no", "off"}


def _dedupe(values: Sequence[Any]) -> list[str]:
    output: list[str] = []
    seen: set[str] = set()
    for value in values:
        text = str(value)
        if text and text not in seen:
            seen.add(text)
            output.append(text)
    return output
