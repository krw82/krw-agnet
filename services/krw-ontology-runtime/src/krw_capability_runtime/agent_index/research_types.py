"""Internal research-kernel data contracts.

These types are serving-layer contracts only. They do not change the
canonical ontology schema or the SQLite index schema.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Literal


class ResearchIntent(str, Enum):
    """Deterministic research route selected before evidence lookup."""

    METRIC_SERIES = "metric_series"
    RISK_THESIS = "risk_thesis"
    COMPARISON = "comparison"
    DISCOVERY = "discovery"
    DIRECT_EXPOSURE = "direct_exposure"
    FACTUAL_LOOKUP = "factual_lookup"
    COMPANY_OVERVIEW = "company_overview"
    VALUATION_STOP = "valuation_or_price_target"
    AUDIT_DEBUG = "audit_debug"
    QUALITY_CHECK = "quality_check"
    GENERAL = "general_research"


class ResearchStatus(str, Enum):
    """Research-state status exposed through the stable MCP envelope."""

    SUFFICIENT = "sufficient_for_default_answer"
    TRACE_RECOMMENDED = "sufficient_but_trace_recommended"
    PARTIAL = "partial_answer_possible"
    NEEDS_TARGETED_FOLLOWUP = "needs_targeted_followup"
    NO_DIRECT_EVIDENCE_WITH_RELATED_CONTEXT = "no_direct_evidence_with_related_context"
    NOT_ANSWERABLE = "not_answerable_from_filings"
    OUT_OF_SCOPE = "out_of_scope_for_filing_ontology"


@dataclass(frozen=True)
class ResearchRequest:
    """Normalized request passed into the deterministic research kernel."""

    question: str
    tickers: list[str] = field(default_factory=list)
    document_types: list[str] = field(default_factory=list)
    periods: list[str] = field(default_factory=list)
    universe: str | None = None
    limit_results: int = 10
    limit_tickers: int = 20
    mode: Literal["context", "retrieve", "compare", "query"] = "context"
    response_detail: str = "compact"


@dataclass(frozen=True)
class ResearchRoute:
    """Selected context route and the contexts that must not be run."""

    intent: ResearchIntent
    confidence: float
    primary_context: str
    secondary_contexts: list[str] = field(default_factory=list)
    forbidden_contexts: list[str] = field(default_factory=list)
    depth: Literal["shallow", "deep", "audit"] = "shallow"
    why_route: str | None = None


@dataclass(frozen=True)
class ResearchBudget:
    """Static budget policy for a single research route."""

    max_ms: int
    max_topic_candidates: int = 30
    max_object_candidates: int = 20
    max_metric_candidates: int = 20
    max_trace_roots: int = 3
    max_chain_roots: int = 2
    max_fallback_passes: int = 1


@dataclass
class AgentAutonomy:
    """Bounded autonomy hints for the outer AI agent."""

    may_continue_research: bool = True
    allowed_next_tools: list[str] = field(default_factory=list)
    do_not_call: list[str] = field(default_factory=list)
    max_additional_tool_calls: int = 2


@dataclass
class ResearchResult:
    """Internal result shape for future context executors."""

    request: ResearchRequest
    route: ResearchRoute
    status: ResearchStatus
    answer_mode: str
    packs: dict[str, Any] = field(default_factory=dict)
    candidates: list[dict[str, Any]] = field(default_factory=list)
    missing_parts: list[str] = field(default_factory=list)
    recommended_trace_object_ids: list[str] = field(default_factory=list)
    agent_autonomy: AgentAutonomy = field(default_factory=AgentAutonomy)
    timing_ms: dict[str, int] = field(default_factory=dict)
    budget: dict[str, Any] = field(default_factory=dict)
