"""Typed MCP v2 contracts and compact research-state compilation.

The ontology store still produces a rich internal planning payload.  This module
is the only model-visible serialization boundary: it validates the agent's
explicit plan, deduplicates evidence, and emits a bounded ResearchState.
"""

from __future__ import annotations

import hashlib
import json
import math
import re
import unicodedata
from datetime import date
from enum import Enum
from typing import Annotated, Any, Literal, Mapping, Sequence

from pydantic import (
    BaseModel,
    ConfigDict,
    Field,
    ValidationError,
    field_validator,
    model_validator,
)

from krw_capability_runtime.agent_index.metric_dictionary import (
    canonical_metric_name,
    metric_dictionary_catalog,
)


MCP_CONTRACT_VERSION = "krw-ontology-mcp/v2"
RESEARCH_STATE_VERSION = "research-state/v2"
# Keep the complete MCP result below the model-wire large-result spill
# boundary.  The server sends compact JSON in TextContent for conservative
# client compatibility plus the same object in structuredContent.
MAX_RESEARCH_STATE_MODEL_BYTES = 80_000
MAX_RESEARCH_STATE_WIRE_BYTES = 180_000
_TOKEN_RE = re.compile(r"[0-9A-Za-z\u3131-\u318e\uac00-\ud7a3_]+")
# Directness verification deliberately uses a much smaller equivalence policy
# than retrieval.  These aliases only bridge notation or an unambiguous filing
# synonym inside one already traceable atomic claim/quote.  They do not expand
# queries, select companies, or make related multi-sentence context direct.
_DIRECT_EVIDENCE_TOKEN_EQUIVALENTS = {
    "debt": "debt",
    "indebtedness": "debt",
    "operating": "operations",
    "operation": "operations",
    "operations": "operations",
    "percent": "percent",
    "percentage": "percent",
}
EvidenceDirectness = Literal["direct", "metric_lineage", "related", "unverified"]
EvidenceGrade = Literal["strong", "medium", "weak", "unverified"]
ComparisonAxis = Literal[
    "value",
    "absolute_change",
    "growth_rate",
    "value_difference",
    "directness",
    "evidence_grade",
]
PlanTerm = Annotated[str, Field(min_length=1, max_length=160)]
ScopeToken = Annotated[str, Field(min_length=1, max_length=128)]
TickerToken = Annotated[str, Field(min_length=1, max_length=32)]


class ContractModel(BaseModel):
    """Strict, immutable base for public MCP request and response models."""

    model_config = ConfigDict(
        extra="forbid",
        frozen=True,
        str_strip_whitespace=True,
        validate_default=True,
    )


InputCorrectionRule = Literal[
    "missing_literal_term",
    "mixed_metric_and_qualitative_clause",
    "missing_calculation_window",
    "invalid_search_plan",
]


class QueryContextInputViolation(ContractModel):
    """One deterministic SearchPlan input problem that MCP can describe exactly."""

    field: str = Field(min_length=1, max_length=512)
    rule: InputCorrectionRule
    message: str = Field(min_length=1, max_length=2_000)
    required_change: str = Field(min_length=1, max_length=2_000)
    required_literals: list[str] = Field(default_factory=list, max_length=32)


class QueryContextInputCorrection(ContractModel):
    """Machine-readable batch of SearchPlan corrections returned by query_context.

    This is a tool result for the active SDK run, not a user-facing answer
    annotation and not an instruction to restart research. The caller corrects
    every listed violation, then controls the next tool call.
    """

    status: Literal["input_correction_required"] = "input_correction_required"
    code: Literal["search_plan_validation_failed"] = "search_plan_validation_failed"
    message: str = Field(min_length=1, max_length=2_000)
    violations: list[QueryContextInputViolation] = Field(min_length=1, max_length=32)
    allowed_next_tools: list[Literal["krw_ontology_query_context"]] = Field(
        default_factory=lambda: ["krw_ontology_query_context"],
        min_length=1,
        max_length=1,
    )


class EvidenceRequirement(str, Enum):
    """Minimum evidence directness required for one question clause."""

    ANY = "any"
    DIRECT_PREFERRED = "direct_preferred"
    DIRECT_REQUIRED = "direct_required"


class AnswerScope(str, Enum):
    """Whether the ontology may answer directly or only provide filing context."""

    DIRECT = "direct"
    SUPPORTING_CONTEXT_ONLY = "supporting_context_only"


class PlanUncertainty(str, Enum):
    """Agent-reported uncertainty in entity and retrieval planning."""

    LOW = "low"
    MEDIUM = "medium"
    HIGH = "high"


class QueryClause(ContractModel):
    """One independently verifiable part of the user's research question."""

    clause_id: str = Field(
        description="Stable caller-assigned clause id, such as `revenue_driver`.",
        min_length=1,
        max_length=64,
        pattern=r"^[A-Za-z0-9][A-Za-z0-9_.-]*$",
    )
    retrieval_query: str = Field(
        description=(
            "Self-contained retrieval query for this clause. Include the relevant entity, "
            "metric, mechanism, and comparison concept instead of conversational filler."
        ),
        min_length=2,
        max_length=1_000,
    )
    required_concepts: list[PlanTerm] = Field(
        default_factory=list,
        description=(
            "Complete set of non-metric concepts that evidence must establish for this "
            "clause. Every concept must be named in retrieval_query. Metric and dimension "
            "identities may be repeated here, but a metric clause must not mix in a causal, "
            "mechanism, risk, or other qualitative proposition; split those into independent "
            "clauses so each part is verified separately."
        ),
        max_length=32,
    )
    required_predicates: list[PlanTerm] = Field(
        default_factory=list,
        description=(
            "Relation or mechanism terms that must occur with all required_concepts inside "
            "one traceable claim/quote span. Required for qualitative clauses containing "
            "more than one concept; metric clauses cannot use it."
        ),
        max_length=16,
    )
    required: bool = Field(
        default=True,
        description="True when the final answer is incomplete without this clause.",
    )
    tickers: list[TickerToken] = Field(
        default_factory=list,
        description=(
            "Optional ticker subset for this clause. Empty inherits the plan ticker scope; "
            "an explicit subset must be contained in SearchPlan.tickers."
        ),
        max_length=50,
    )
    directness: EvidenceRequirement = Field(
        default=EvidenceRequirement.DIRECT_PREFERRED,
        description="Minimum evidence directness needed for this clause.",
    )
    object_types: list[ScopeToken] = Field(
        default_factory=list,
        description="Optional ontology object-type filters selected from index capabilities.",
        max_length=32,
    )
    metrics: list[ScopeToken] = Field(
        default_factory=list,
        description="Optional canonical metric names required by this clause.",
        max_length=32,
    )
    metric_dimensions: list[PlanTerm] = Field(
        default_factory=list,
        description=(
            "Explicit segment, geography, product, counterparty, or other dimension values "
            "that an exact metric observation must contain."
        ),
        max_length=16,
    )
    metric_scope: Literal["company_total", "dimensioned", "any"] = Field(
        default="company_total",
        description=(
            "Required metric observation scope. Use `dimensioned` with explicit "
            "metric_dimensions; `any` must be an intentional broad request."
        ),
    )
    calculation_window: Literal["period_over_period", "year_over_year"] | None = Field(
        default=None,
        description=(
            "Required for temporal metric axes. `period_over_period` means adjacent annual "
            "or quarter observations; `year_over_year` means the same fiscal/calendar "
            "quarter or annual period one year apart. YTD observations only support "
            "year_over_year."
        ),
    )

    @field_validator(
        "required_concepts",
        "required_predicates",
        "object_types",
        "metric_dimensions",
    )
    @classmethod
    def _normalize_string_list(cls, values: list[str]) -> list[str]:
        return _dedupe_strings(values)

    @field_validator("metrics")
    @classmethod
    def _normalize_canonical_metrics(cls, values: list[str]) -> list[str]:
        catalog = metric_dictionary_catalog()
        normalized: list[str] = []
        for value in values:
            canonical = catalog.canonicalize(value)
            if not canonical:
                raise ValueError(f"invalid_plan: unknown canonical metric or alias: {value}")
            normalized.append(canonical)
        return _dedupe_strings(normalized)

    @field_validator("tickers")
    @classmethod
    def _normalize_clause_tickers(cls, values: list[str]) -> list[str]:
        return _dedupe_strings(str(value).strip().upper() for value in values)

    @model_validator(mode="after")
    def _validate_explicit_metric_scope(self) -> QueryClause:
        query_tokens = set(_tokens(self.retrieval_query))
        for concept in self.required_concepts:
            concept_tokens = set(_tokens(concept))
            if concept_tokens and not concept_tokens.issubset(query_tokens):
                raise ValueError(
                    "invalid_plan: every required_concepts entry must be named in retrieval_query"
                )
        for predicate in self.required_predicates:
            predicate_tokens = set(_tokens(predicate))
            if predicate_tokens and not predicate_tokens.issubset(query_tokens):
                raise ValueError(
                    "invalid_plan: every required_predicates entry must be named in retrieval_query"
                )
        for metric in self.metrics:
            metric_alias_token_sets = [
                set(_tokens(alias))
                for alias in metric_dictionary_catalog().aliases_for(metric)
                if _tokens(alias)
            ]
            if metric_alias_token_sets and not any(
                alias_tokens.issubset(query_tokens) for alias_tokens in metric_alias_token_sets
            ):
                raise ValueError(
                    "invalid_plan: every metrics entry must be named in retrieval_query"
                )
        if self.metric_dimensions and not self.metrics:
            raise ValueError(
                "invalid_plan: metric_dimensions require at least one canonical metric"
            )
        if self.metric_dimensions and self.metric_scope == "company_total":
            raise ValueError(
                "invalid_plan: metric_dimensions require metric_scope=dimensioned or any"
            )
        if self.metrics and not self.metric_dimensions and self.metric_scope == "dimensioned":
            raise ValueError("invalid_plan: metric_scope=dimensioned requires metric_dimensions")
        query_tokens = set(_tokens(self.retrieval_query))
        for dimension in self.metric_dimensions:
            if not set(_tokens(dimension)).issubset(query_tokens):
                raise ValueError(
                    "invalid_plan: every metric_dimensions entry must be named in retrieval_query"
                )
        if not self.metrics and not self.required_concepts:
            raise ValueError(
                "invalid_plan: qualitative clauses require at least one required_concepts entry"
            )
        if self.calculation_window and not self.metrics:
            raise ValueError(
                "invalid_plan: calculation_window requires at least one canonical metric"
            )
        if not self.metrics and len(self.required_concepts) > 1 and not self.required_predicates:
            raise ValueError(
                "invalid_plan: multi-concept qualitative clauses require "
                "required_predicates so the relation is verified in one evidence span"
            )
        if self.metrics:
            if self.required_predicates:
                raise ValueError(
                    "invalid_plan: metric and qualitative propositions must be split into "
                    "independent clauses; metric clauses cannot set required_predicates"
                )
            metric_identity_tokens = {
                token
                for value in [
                    *self.metric_dimensions,
                    *(
                        alias
                        for metric in self.metrics
                        for alias in metric_dictionary_catalog().aliases_for(metric)
                    ),
                ]
                for token in _tokens(value)
            }
            qualitative_concepts = [
                concept
                for concept in self.required_concepts
                if not set(_tokens(concept)).issubset(metric_identity_tokens)
            ]
            if qualitative_concepts:
                raise ValueError(
                    "invalid_plan: metric and qualitative propositions must be split into "
                    "independent clauses; non-metric required_concepts found: "
                    + ", ".join(qualitative_concepts)
                )
        return self


class SearchPlan(ContractModel):
    """Agent-authored, fully validated plan that the MCP server executes as written."""

    question: str = Field(
        description="Original user question, preserved for answer synthesis and audit.",
        min_length=2,
        max_length=4_000,
    )
    intent: str = Field(
        description=(
            "Agent-selected intent identifier. Use a stable lowercase slug; the server records "
            "it but does not reclassify it with keyword rules."
        ),
        min_length=1,
        max_length=64,
        pattern=r"^[a-z][a-z0-9_.-]*$",
    )
    clauses: list[QueryClause] = Field(
        description="One to twelve independently verifiable retrieval clauses.",
        min_length=1,
        max_length=12,
    )
    tickers: list[TickerToken] = Field(
        default_factory=list,
        description="Explicit uppercase ticker scope. Empty means discovery is required.",
        max_length=50,
    )
    document_types: list[ScopeToken] = Field(
        default_factory=list,
        description="Explicit filing/document type filters, for example `10-K` or `10-Q`.",
        max_length=16,
    )
    periods: list[ScopeToken] = Field(
        default_factory=list,
        description="Explicit period filters, for example `CY2025` or `CY2026Q1`.",
        max_length=40,
    )
    universe: Literal["covered"] | None = Field(
        default=None,
        description=(
            "Use `covered` for discovery across the active serving index. "
            "Do not combine it with explicit tickers."
        ),
    )
    comparison_axes: list[ComparisonAxis] = Field(
        default_factory=list,
        description=(
            "Executable comparison dimensions. Numeric axes require an explicit canonical "
            "metric in at least one clause."
        ),
        max_length=16,
    )
    answer_scope: AnswerScope = Field(
        default=AnswerScope.DIRECT,
        description="Whether ontology evidence may answer directly or only support context.",
    )
    uncertainty: PlanUncertainty = Field(
        default=PlanUncertainty.LOW,
        description="Planning uncertainty; higher values keep a wider candidate set.",
    )
    limit_results: int = Field(
        default=12,
        description="Maximum number of deduplicated evidence units returned to the model.",
        ge=1,
        le=50,
    )
    limit_tickers: int = Field(
        default=20,
        description="Maximum ticker candidates considered when discovery is required.",
        ge=1,
        le=50,
    )

    @field_validator("tickers")
    @classmethod
    def _normalize_tickers(cls, values: list[str]) -> list[str]:
        normalized = [str(value).strip().upper() for value in values]
        return _dedupe_strings(normalized)

    @field_validator("document_types")
    @classmethod
    def _normalize_document_types(cls, values: list[str]) -> list[str]:
        normalized = [str(value).strip().upper() for value in values]
        return _dedupe_strings(normalized)

    @field_validator("periods", "comparison_axes")
    @classmethod
    def _normalize_scope_list(cls, values: list[str]) -> list[str]:
        return _dedupe_strings(values)

    @model_validator(mode="after")
    def _validate_plan(self) -> SearchPlan:
        ids = [clause.clause_id.casefold() for clause in self.clauses]
        if len(ids) != len(set(ids)):
            raise ValueError("invalid_plan: clause_id values must be unique")
        if not any(clause.required for clause in self.clauses):
            raise ValueError("invalid_plan: at least one clause must have required=true")
        required_clause_count = sum(1 for clause in self.clauses if clause.required)
        if self.limit_results < required_clause_count:
            raise ValueError(
                "invalid_plan: limit_results must be at least the number of required clauses"
            )
        if self.tickers and self.universe:
            raise ValueError("invalid_plan: use explicit tickers or universe, not both")
        numeric_axes = {"value", "absolute_change", "growth_rate", "value_difference"}
        if numeric_axes.intersection(self.comparison_axes) and not any(
            clause.required and clause.metrics for clause in self.clauses
        ):
            raise ValueError(
                "invalid_plan: numeric comparison_axes require explicit clause metrics"
            )
        temporal_axes = {"absolute_change", "growth_rate"}
        if temporal_axes.intersection(self.comparison_axes):
            missing_windows = [
                clause.clause_id
                for clause in self.clauses
                if clause.required and clause.metrics and not clause.calculation_window
            ]
            if missing_windows:
                raise ValueError(
                    "invalid_plan: temporal metric axes require calculation_window for "
                    "required metric clauses: " + ", ".join(missing_windows)
                )
        metric_reference_count = sum(len(clause.metrics) for clause in self.clauses)
        if metric_reference_count > 32:
            raise ValueError("invalid_plan: at most 32 metric references may be executed per plan")
        plan_tickers = set(self.tickers)
        for clause in self.clauses:
            if clause.tickers and not plan_tickers:
                raise ValueError("invalid_plan: clause tickers require explicit SearchPlan.tickers")
            outside_scope = [ticker for ticker in clause.tickers if ticker not in plan_tickers]
            if outside_scope:
                raise ValueError(
                    "invalid_plan: clause tickers must be a subset of SearchPlan.tickers: "
                    + ", ".join(outside_scope)
                )
        return self

    @property
    def retrieval_queries(self) -> tuple[str, ...]:
        """Return ordered clause queries without serializing a duplicate field."""
        return tuple(clause.retrieval_query for clause in self.clauses)

    @property
    def execution_query(self) -> str:
        """Return plan-derived search text without reusing conversational filler."""
        return " ; ".join(self.retrieval_queries)


class SourceAnchor(ContractModel):
    """A filing/document role used to interpret recency and annual baselines."""

    ticker: str
    period: str | None = None
    document_type: str | None = None
    role: str
    source_label: str


class EvidenceSource(ContractModel):
    """Traceable lineage identifiers for one compact evidence unit."""

    object_ids: list[str] = Field(default_factory=list, max_length=16)
    quote_ids: list[str] = Field(default_factory=list, max_length=16)
    span_ids: list[str] = Field(default_factory=list, max_length=16)
    source_label: str | None = None


class MetricPoint(ContractModel):
    """One period/value observation retained without surrounding pack duplication."""

    period: str
    value: float | int | str | None = None
    formatted_value: str | None = None
    object_id: str | None = None
    period_type: str | None = None
    start_date: str | None = None
    end_date: str | None = None
    conflict_value_count: int = Field(default=1, ge=1)


class VisualizationMetricPoint(ContractModel):
    """A chart-safe numeric observation with an immutable evidence anchor."""

    period: str
    value: float
    formatted_value: str | None = None
    object_id: str


class VisualizationMetricScope(ContractModel):
    """Stable semantic ownership for one renderer-neutral metric series.

    The chart sidecar already indexes company totals, geographic segments,
    products, and other dimensions separately.  Preserve that distinction in
    the public MCP contract so presentation code can choose a total or a
    breakdown without parsing a technical ``series_key``.
    """

    kind: str
    key: str
    label: str | None = None


class VisualizationMetricSeries(ContractModel):
    """One bounded, renderer-neutral metric series from the verified sidecar."""

    series_key: str
    label: str
    ticker: str | None = None
    metric_name: str | None = None
    canonical_metric: str | None = None
    unit: str | None = None
    period_type: str | None = None
    duration: str | None = None
    scope: VisualizationMetricScope
    points: list[VisualizationMetricPoint] = Field(default_factory=list, max_length=12)


class VisualizationMetricSeriesPack(ContractModel):
    """Public, bounded projection of chart-series sidecar data.

    This intentionally excludes filing text and sidecar internals.  The
    renderer receives only numbers, labels, units, periods, and evidence ids.
    """

    mode: Literal["chart_series_sidecar"] = "chart_series_sidecar"
    series: list[VisualizationMetricSeries] = Field(default_factory=list, max_length=8)


class ClauseEvidenceMatch(ContractModel):
    """Clause-specific relevance and directness for one deduplicated unit."""

    clause_id: str
    match_mode: Literal["strict", "relaxed"]
    directness: EvidenceDirectness


class EvidenceUnit(ContractModel):
    """A deduplicated, answer-ready fact or metric with explicit lineage."""

    evidence_id: str
    object_id: str | None = None
    object_type: str
    ticker: str | None = None
    period: str | None = None
    document_type: str | None = None
    title: str
    summary: str
    match_mode: Literal["strict", "relaxed"] = "strict"
    directness: EvidenceDirectness
    evidence_grade: EvidenceGrade
    materiality: float | str | None = None
    metric: str | None = None
    unit: str | None = None
    currency: str | None = None
    dimensions: dict[str, str] = Field(default_factory=dict, max_length=16)
    metric_scope: Literal["company_total", "dimensioned", "unspecified"] | None = None
    metric_points: list[MetricPoint] = Field(default_factory=list, max_length=40)
    supports_clause_ids: list[str] = Field(default_factory=list, max_length=12)
    clause_matches: list[ClauseEvidenceMatch] = Field(default_factory=list, max_length=12)
    source: EvidenceSource


class ComputedValue(ContractModel):
    """A deterministic calculation from metric evidence already present in the state."""

    calculation_id: str
    kind: str
    label: str | None = None
    metric: str | None = None
    tickers: list[str] = Field(default_factory=list, max_length=50)
    period: str | None = None
    from_period: str | None = None
    unit: str | None = None
    currency: str | None = None
    dimensions: dict[str, str] = Field(default_factory=dict, max_length=16)
    metric_scope: Literal["company_total", "dimensioned"] | None = None
    period_basis: str | None = None
    duration_basis: str | None = None
    calculation_window: Literal["period_over_period", "year_over_year"] | None = None
    value: float | int | str | None = None
    numerator: float | int | str | None = None
    denominator: float | int | str | None = None
    source_object_ids: list[str] = Field(default_factory=list, max_length=16)


class CalculationCoverage(ContractModel):
    """Completeness of one requested numeric axis for one clause metric."""

    clause_id: str
    metric: str
    axis: Literal["value", "absolute_change", "growth_rate", "value_difference"]
    metric_scope: Literal["company_total", "dimensioned", "any"]
    metric_dimensions: list[str] = Field(default_factory=list, max_length=16)
    status: Literal["covered", "partial", "missing"]
    required_tickers: list[str] = Field(default_factory=list, max_length=50)
    covered_tickers: list[str] = Field(default_factory=list, max_length=50)
    calculation_ids: list[str] = Field(default_factory=list, max_length=40)
    reason: str | None = None


class ClauseCoverage(ContractModel):
    """Evidence coverage and directness result for one requested clause."""

    clause_id: str
    required: bool
    directness_required: EvidenceRequirement
    status: Literal["covered", "partial", "missing"]
    evidence_ids: list[str] = Field(default_factory=list, max_length=50)
    covered_tickers: list[str] = Field(default_factory=list, max_length=50)
    missing_tickers: list[str] = Field(default_factory=list, max_length=50)
    best_directness: EvidenceDirectness | None = None
    best_evidence_grade: EvidenceGrade | None = None
    strong_claim_ready: bool = False
    reason: str | None = None


class ResolvedScope(ContractModel):
    """Requested versus available company/document scope."""

    requested_tickers: list[str] = Field(default_factory=list)
    resolved_tickers: list[str] = Field(default_factory=list)
    unknown_tickers: list[str] = Field(default_factory=list)
    missing_tickers: list[str] = Field(default_factory=list)
    failed_tickers: list[str] = Field(default_factory=list)
    document_types: list[str] = Field(default_factory=list)
    periods: list[str] = Field(default_factory=list)
    universe: str | None = None


class Answerability(ContractModel):
    """Bounded final-answer policy derived from clause coverage and evidence quality."""

    status: Literal["answerable", "partial", "not_answerable", "supporting_context_only"]
    strong_claim_allowed: bool
    required_clause_count: int
    covered_required_clause_count: int
    requires_direct_evidence: bool
    reason_codes: list[str] = Field(default_factory=list)


class MissingPart(ContractModel):
    """A precise gap the agent may address with another bounded tool call."""

    code: str
    detail: str
    clause_id: str | None = None
    ticker: str | None = None


class RecommendedAction(ContractModel):
    """A bounded follow-up action tied to a specific evidence or coverage gap."""

    tool: str
    reason: str
    object_id: str | None = None
    clause_id: str | None = None
    ticker: str | None = None


class Continuation(ContractModel):
    """Metadata indicating that bounded evidence was omitted from this response."""

    has_more: bool
    omitted_evidence_count: int = 0
    reason: str | None = None


class ResearchState(ContractModel):
    """Canonical compact MCP v2 response consumed by the research agent."""

    contract_version: Literal["research-state/v2"] = RESEARCH_STATE_VERSION
    release_id: str | None = None
    plan: SearchPlan
    resolved_scope: ResolvedScope
    source_anchors: list[SourceAnchor] = Field(default_factory=list)
    answerability: Answerability
    clause_coverage: list[ClauseCoverage]
    evidence_units: list[EvidenceUnit]
    computed_values: list[ComputedValue] = Field(default_factory=list)
    metric_series_pack: VisualizationMetricSeriesPack | None = None
    calculation_coverage: list[CalculationCoverage] = Field(default_factory=list)
    missing_parts: list[MissingPart] = Field(default_factory=list)
    recommended_actions: list[RecommendedAction] = Field(default_factory=list)
    continuation: Continuation | None = None
    warnings: list[str] = Field(default_factory=list)


def validate_search_plan(value: SearchPlan | Mapping[str, Any]) -> SearchPlan:
    """Validate an MCP plan and prefix validation failures with an actionable code."""
    if isinstance(value, SearchPlan):
        return value
    try:
        return SearchPlan.model_validate(value)
    except Exception as exc:
        raise ValueError(f"invalid_plan: {exc}") from exc


def validate_query_context_search_plan(
    value: SearchPlan | Mapping[str, Any],
) -> SearchPlan | QueryContextInputCorrection:
    """Validate one query-context input without raising an opaque MCP error.

    A parameter-decorating transport can only return a generic validation failure when the
    public argument is typed strictly as ``SearchPlan``.  The query-context
    boundary accepts a mapping as a fallback, performs the same canonical
    SearchPlan validation here, and turns known, field-local mistakes into a
    structured tool result the agent can act on within the same SDK run.
    """
    if isinstance(value, SearchPlan):
        return value
    if not isinstance(value, Mapping):
        return _query_context_correction([
            _query_context_violation(
                field="search_plan",
                rule="invalid_search_plan",
                message="search_plan must be a JSON object that follows the SearchPlan v2 contract.",
                required_change=(
                    "Send one complete SearchPlan v2 object with question, intent, and clauses. "
                    "Do not send a scalar, array, or legacy query payload."
                ),
            )
        ])

    raw_plan = dict(value)
    violations = _inspect_query_context_plan(raw_plan)
    try:
        validated_plan = SearchPlan.model_validate(raw_plan)
    except ValidationError as exc:
        violations = _merge_query_context_violations(
            violations,
            _non_redundant_schema_violations(
                semantic_violations=violations,
                schema_violations=_invalid_search_plan_violations(exc),
            ),
        )
    else:
        if not violations:
            return validated_plan

    return _query_context_correction(violations)


def _inspect_query_context_plan(
    plan: Mapping[str, Any],
) -> list[QueryContextInputViolation]:
    """Surface deterministic repair instructions before broad schema errors.

    The canonical Pydantic models remain the source of truth.  This narrow
    pass only recognizes errors for which the server can identify an exact
    field and an exact correction; it never edits, drops, or invents plan
    content on the model's behalf.
    """
    violations: list[QueryContextInputViolation] = []
    clauses = plan.get("clauses")
    if not isinstance(clauses, list):
        return violations

    for index, clause in enumerate(clauses):
        if not isinstance(clause, Mapping):
            continue
        path = f"clauses[{index}]"
        metrics = _raw_string_list(clause.get("metrics"))
        concepts = _raw_string_list(clause.get("required_concepts"))
        predicates = _raw_string_list(clause.get("required_predicates"))
        dimensions = _raw_string_list(clause.get("metric_dimensions"))
        qualitative_concepts = _non_metric_required_concepts(
            metrics=metrics,
            dimensions=dimensions,
            concepts=concepts,
        )

        if metrics and (predicates or qualitative_concepts):
            conflicting_fields = [
                field
                for field, values in (
                    (f"{path}.required_predicates", predicates),
                    (f"{path}.required_concepts", qualitative_concepts),
                )
                if values
            ]
            conflicts = [*predicates, *qualitative_concepts]
            violations.append(
                _query_context_violation(
                    field=path,
                    rule="mixed_metric_and_qualitative_clause",
                    message=(
                        "A metric clause also contains qualitative requirements: "
                        + ", ".join(repr(value) for value in conflicts)
                        + ". Metric values and qualitative relationships must be independently "
                        "verifiable."
                    ),
                    required_change=(
                        "Split this into a metric clause and a separate qualitative clause. "
                        "The metric clause must not contain required_predicates or non-metric "
                        "required_concepts ("
                        + ", ".join(conflicting_fields)
                        + ")."
                    ),
                )
            )

        retrieval_query = clause.get("retrieval_query")
        if not isinstance(retrieval_query, str):
            continue
        literal_fields = (
            # A non-metric concept in a metric clause must be moved or removed,
            # rather than first being copied into an already-invalid query.
            ("required_concepts", [] if qualitative_concepts else concepts),
            ("required_predicates", [] if metrics and predicates else predicates),
            ("metric_dimensions", dimensions),
        )
        for field_name, values in literal_fields:
            missing_literals = [
                value for value in values if not _contains_literal_phrase(retrieval_query, value)
            ]
            if not missing_literals:
                continue
            quoted_literals = ", ".join(repr(value) for value in missing_literals)
            violations.append(
                _query_context_violation(
                    field=f"{path}.retrieval_query",
                    rule="missing_literal_term",
                    message=(
                        f"The {field_name} literal(s) {quoted_literals} are missing from "
                        f"{path}.retrieval_query."
                    ),
                    required_change=(
                        f"Include every exact phrase in required_literals in "
                        f"{path}.retrieval_query, or remove those values from "
                        f"{path}.{field_name}."
                    ),
                    required_literals=missing_literals,
                )
            )

    comparison_axes = _raw_string_list(plan.get("comparison_axes"))
    if {"absolute_change", "growth_rate"}.intersection(comparison_axes):
        for index, clause in enumerate(clauses):
            if not isinstance(clause, Mapping):
                continue
            is_required = clause.get("required") is not False
            has_metrics = bool(_raw_string_list(clause.get("metrics")))
            calculation_window = clause.get("calculation_window")
            has_window = calculation_window in {"period_over_period", "year_over_year"}
            if not (is_required and has_metrics and not has_window):
                continue
            path = f"clauses[{index}]"
            violations.append(
                _query_context_violation(
                    field=f"{path}.calculation_window",
                    rule="missing_calculation_window",
                    message=(
                        "A required metric clause needs calculation_window when comparison_axes "
                        "requests absolute_change or growth_rate."
                    ),
                    required_change=(
                        "Set calculation_window to period_over_period or year_over_year for "
                        "this required metric clause, or remove the temporal comparison axis."
                    ),
                )
            )

    return _merge_query_context_violations([], violations)


def _non_metric_required_concepts(
    *,
    metrics: Sequence[str],
    dimensions: Sequence[str],
    concepts: Sequence[str],
) -> list[str]:
    """Match QueryClause's metric/qualitative split before canonical validation.

    Raw input can contain metric aliases, including an invalid alias. Unknown
    aliases intentionally contribute no identity tokens here; canonical
    validation later returns the precise schema error for that metric field.
    """
    if not metrics:
        return []
    catalog = metric_dictionary_catalog()
    identity_tokens = {token for value in dimensions for token in _tokens(value)}
    for metric in metrics:
        canonical = catalog.canonicalize(metric)
        if not canonical:
            continue
        for alias in catalog.aliases_for(canonical):
            identity_tokens.update(_tokens(alias))
    return [
        concept
        for concept in concepts
        if (concept_tokens := set(_tokens(concept))) and not concept_tokens.issubset(identity_tokens)
    ]


def _invalid_search_plan_violations(exc: ValidationError) -> list[QueryContextInputViolation]:
    violations: list[QueryContextInputViolation] = []
    for error in exc.errors(include_url=False):
        field = _validation_error_field(error) or "search_plan"
        violations.append(
            _query_context_violation(
                field=field,
                rule="invalid_search_plan",
                message=(
                    "The supplied SearchPlan v2 is invalid: "
                    + str(error.get("msg") or "invalid SearchPlan v2 input")
                ),
                required_change=(
                    f"Correct {field} so it satisfies the SearchPlan v2 contract, then submit "
                    "one complete SearchPlan v2 root object. Do not add a legacy wrapper or "
                    "switch to another query shape."
                ),
            )
        )
    return violations


def _validation_error_fields(errors: Sequence[Mapping[str, Any]]) -> list[str]:
    return _dedupe_strings(
        field
        for error in errors
        if (field := _validation_error_field(error))
    )[:32]


def _validation_error_field(error: Mapping[str, Any]) -> str:
    location = error.get("loc")
    if not isinstance(location, (list, tuple)):
        return ""
    path = ""
    for value in location:
        if isinstance(value, int):
            path += f"[{value}]"
        else:
            token = str(value).strip()
            if not token:
                continue
            path = f"{path}.{token}" if path else token
    return path


def _merge_query_context_violations(
    current: Sequence[QueryContextInputViolation],
    incoming: Sequence[QueryContextInputViolation],
) -> list[QueryContextInputViolation]:
    """Keep stable, non-duplicated corrections within the public result cap."""
    merged: list[QueryContextInputViolation] = []
    keys: set[tuple[str, str, tuple[str, ...]]] = set()
    for violation in [*current, *incoming]:
        key = (
            violation.field,
            violation.rule,
            tuple(value.casefold() for value in violation.required_literals),
        )
        if key in keys:
            continue
        keys.add(key)
        merged.append(violation)
        if len(merged) == 32:
            break
    return merged


def _non_redundant_schema_violations(
    *,
    semantic_violations: Sequence[QueryContextInputViolation],
    schema_violations: Sequence[QueryContextInputViolation],
) -> list[QueryContextInputViolation]:
    """Suppress generic clause-model errors already explained by a precise rule.

    Pydantic reports an after-model-validator failure at ``clauses[n]``. If
    the narrow pass has already named the literal, metric/qualitative split, or
    calculation-window field in that clause, emitting both only makes the
    agent repair the same issue twice.
    """
    semantic_clause_paths = {
        violation.field.split(".", 1)[0]
        for violation in semantic_violations
        if violation.field.startswith("clauses[")
    }
    return [
        violation
        for violation in schema_violations
        if not (
            violation.field in semantic_clause_paths
            and violation.field.startswith("clauses[")
        )
    ]


def _query_context_violation(
    *,
    field: str,
    rule: InputCorrectionRule,
    message: str,
    required_change: str,
    required_literals: Sequence[str] | None = None,
) -> QueryContextInputViolation:
    return QueryContextInputViolation(
        field=field,
        rule=rule,
        message=message,
        required_change=required_change,
        required_literals=list(required_literals or []),
    )


def _query_context_correction(
    violations: Sequence[QueryContextInputViolation],
) -> QueryContextInputCorrection:
    corrections = _merge_query_context_violations([], violations)
    if not corrections:
        raise ValueError("query_context correction requires at least one violation")
    return QueryContextInputCorrection(
        message=(
            f"The SearchPlan has {len(corrections)} correctable input error(s). "
            "Correct every listed violation before calling krw_ontology_query_context again."
        ),
        violations=corrections,
    )


def _raw_string_list(value: Any) -> list[str]:
    if not isinstance(value, (list, tuple, set)):
        return []
    return [item.strip() for item in value if isinstance(item, str) and item.strip()]


def _contains_literal_phrase(query: str, term: str) -> bool:
    normalized_query = _normalize_literal_phrase(query)
    normalized_term = _normalize_literal_phrase(term)
    return bool(normalized_term and normalized_term in normalized_query)


def _normalize_literal_phrase(value: str) -> str:
    return " ".join(unicodedata.normalize("NFKC", value).casefold().split())


def compile_research_state(
    *,
    search_plan: SearchPlan,
    raw_payload: Mapping[str, Any],
    release_id: str | None,
) -> ResearchState:
    """Compile the internal retrieval payload into the only model-visible v2 state."""
    plan = validate_search_plan(search_plan)
    raw = _mapping(raw_payload)
    candidates = _collect_evidence_candidates(raw)
    enriched = [_attach_clause_support(candidate, plan.clauses) for candidate in candidates]
    enriched.sort(key=_evidence_sort_key)
    conflicting_metric_points = _conflicting_metric_point_keys(enriched)
    omitted_before_compile = max(0, int(raw.get("omitted_evidence_count") or 0))
    truncation_possible = bool(raw.get("truncation_possible"))
    total_evidence = len(enriched) + omitted_before_compile
    selected, protected_evidence_ids = _select_evidence_candidates(
        enriched,
        plan.clauses,
        comparison_axes=plan.comparison_axes,
        requested_periods=plan.periods,
        limit=plan.limit_results,
    )
    evidence_units = [candidate["unit"] for candidate in selected]
    # Legacy research-pack calculations do not carry the complete metric identity
    # needed to prove unit/currency/dimension/period compatibility.  V2 therefore
    # derives every model-visible numeric value from selected, complete-lineage
    # metric evidence below.
    raw_computed_values: list[ComputedValue] = []
    metric_series_pack = _compact_visualization_metric_series_pack(raw)
    warnings = _warnings(raw)
    while True:
        source_anchors = _source_anchors(raw, evidence_units)
        computed_values = _supported_raw_computed_values(
            raw_computed_values,
            evidence_units,
        )
        computed_values.extend(
            _computed_values_from_evidence(
                evidence_units,
                plan.comparison_axes,
                conflicting_metric_points=conflicting_metric_points,
                metric_windows=_metric_windows_for_clauses(plan.clauses),
            )
        )
        computed_values = _dedupe_computed_values(computed_values)[:40]
        scope = _resolved_scope(plan, raw, evidence_units)
        coverage = _build_clause_coverage(
            plan.clauses,
            evidence_units,
            ticker_scope_by_clause={
                clause.clause_id: (
                    clause.tickers
                    or plan.tickers
                    or (scope.resolved_tickers if plan.comparison_axes else [])
                )
                for clause in plan.clauses
            },
        )
        calculation_coverage = _build_calculation_coverage(
            plan,
            scope,
            computed_values,
            retrieval_incomplete=(truncation_possible or omitted_before_compile > 0),
        )
        answerability = _build_answerability(
            plan,
            coverage,
            calculation_coverage,
            scope,
        )
        missing_parts = _build_missing_parts(
            plan,
            raw,
            coverage,
            scope,
            calculation_coverage,
        )
        actions = _build_recommended_actions(raw, evidence_units, coverage)
        continuation = None
        if total_evidence > len(evidence_units) or truncation_possible:
            continuation = Continuation(
                has_more=True,
                omitted_evidence_count=max(
                    omitted_before_compile,
                    total_evidence - len(evidence_units),
                ),
                reason=(
                    "retrieval or response evidence limit reached; issue a targeted "
                    "query for an uncovered clause"
                ),
            )
        state = ResearchState(
            release_id=release_id,
            plan=plan,
            resolved_scope=scope,
            source_anchors=source_anchors,
            answerability=answerability,
            clause_coverage=coverage,
            evidence_units=evidence_units,
            computed_values=computed_values,
            metric_series_pack=metric_series_pack,
            calculation_coverage=calculation_coverage,
            missing_parts=missing_parts,
            recommended_actions=actions,
            continuation=continuation,
            warnings=warnings,
        )
        if (
            research_state_model_bytes(state) <= MAX_RESEARCH_STATE_MODEL_BYTES
            and research_state_wire_bytes(state) <= MAX_RESEARCH_STATE_WIRE_BYTES
        ):
            return state
        if metric_series_pack is not None:
            metric_series_pack = None
            if "visualization_metric_series_omitted_for_size" not in warnings:
                warnings.append("visualization_metric_series_omitted_for_size")
            continue
        removable_index = next(
            (
                index
                for index in range(len(evidence_units) - 1, -1, -1)
                if evidence_units[index].evidence_id not in protected_evidence_ids
            ),
            None,
        )
        if removable_index is None:
            raise ValueError(
                "research_state_too_large: required-clause evidence exceeds the 180KB MCP wire budget"
            )
        evidence_units.pop(removable_index)


def _compact_visualization_metric_series_pack(
    raw: Mapping[str, Any],
) -> VisualizationMetricSeriesPack | None:
    """Return only the chart-safe slice of an internal metric sidecar pack."""
    research_pack = _mapping(raw.get("research_pack"))
    raw_pack = _mapping(research_pack.get("metric_series_pack"))
    if raw_pack.get("mode") != "chart_series_sidecar":
        return None

    series: list[VisualizationMetricSeries] = []
    for raw_series in _mapping_list(raw_pack.get("series"))[:8]:
        series_key = _first_text(raw_series, "series_key")
        if not series_key:
            continue
        raw_scope = _mapping(raw_series.get("scope"))
        scope = VisualizationMetricScope(
            kind=_first_text(raw_scope, "kind") or "unspecified",
            key=_first_text(raw_scope, "key") or series_key,
            label=_first_text(raw_scope, "label"),
        )
        canonical_metric = _first_text(raw_series, "canonical_metric")
        label = _first_text(raw_series, "label") or canonical_metric or series_key
        points: list[VisualizationMetricPoint] = []
        for raw_point in _mapping_list(raw_series.get("points"))[:12]:
            period = _first_text(raw_point, "period")
            object_id = _first_text(raw_point, "object_id")
            value = raw_point.get("value")
            if not period or not object_id or isinstance(value, bool):
                continue
            try:
                numeric_value = float(value)
            except (TypeError, ValueError):
                continue
            if not math.isfinite(numeric_value):
                continue
            points.append(
                VisualizationMetricPoint(
                    period=period,
                    value=numeric_value,
                    formatted_value=_first_text(raw_point, "formatted_value"),
                    object_id=object_id,
                )
            )
        if not points:
            continue
        series.append(
            VisualizationMetricSeries(
                series_key=series_key,
                label=label,
                ticker=_first_text(raw_series, "ticker"),
                metric_name=_first_text(raw_series, "metric_name"),
                canonical_metric=canonical_metric,
                unit=_first_text(raw_series, "unit"),
                period_type=_first_text(raw_series, "period_type"),
                duration=_first_text(raw_series, "duration"),
                scope=scope,
                points=points,
            )
        )
    if not series:
        return None
    return VisualizationMetricSeriesPack(series=series)


def research_state_model_bytes(state: ResearchState) -> int:
    """Measure the compact TextContent emitted to the provider wire."""
    payload = state.model_dump(mode="json", by_alias=True)
    encoded = json.dumps(
        payload,
        ensure_ascii=False,
        separators=(",", ":"),
    ).encode("utf-8")
    return len(encoded)


def research_state_wire_bytes(state: ResearchState) -> int:
    """Measure the serialized CallToolResult body, including escaped TextContent."""
    payload = state.model_dump(mode="json", by_alias=True)
    compact_text = json.dumps(
        payload,
        ensure_ascii=False,
        separators=(",", ":"),
    )
    result = {
        "content": [{"type": "text", "text": compact_text}],
        "structuredContent": payload,
        "isError": False,
    }
    return len(
        json.dumps(
            result,
            ensure_ascii=False,
            separators=(",", ":"),
        ).encode("utf-8")
    )


def _collect_evidence_candidates(raw: Mapping[str, Any]) -> list[dict[str, Any]]:
    rows: list[Mapping[str, Any]] = []
    results_by_ticker = _mapping(raw.get("results_by_ticker"))
    for result_rows in results_by_ticker.values():
        rows.extend(_mapping_list(result_rows))
    if not rows:
        for ticker_candidate in _mapping_list(raw.get("ticker_candidates")):
            rows.extend(_mapping_list(ticker_candidate.get("matched_topics")))

    candidates: list[dict[str, Any]] = []
    seen_keys: set[str] = set()
    for row in rows:
        candidate = _topic_evidence_candidate(row)
        if candidate is not None and candidate["key"] not in seen_keys:
            seen_keys.add(candidate["key"])
            candidates.append(candidate)

    research_pack = _mapping(raw.get("research_pack"))
    metric_pack = _mapping(research_pack.get("metric_series_pack"))
    for series in _mapping_list(metric_pack.get("series")):
        candidate = _metric_evidence_candidate(series)
        if candidate is not None and candidate["key"] not in seen_keys:
            seen_keys.add(candidate["key"])
            candidates.append(candidate)

    projection_pack = _mapping(research_pack.get("projection_pack"))
    for row in _mapping_list(projection_pack.get("candidates")):
        candidate = _projection_evidence_candidate(row)
        if candidate is not None and candidate["key"] not in seen_keys:
            seen_keys.add(candidate["key"])
            candidates.append(candidate)

    cross_company = _mapping(research_pack.get("cross_company_signal_pack"))
    for row in _mapping_list(cross_company.get("company_evidence_rows")):
        candidate = _cross_company_evidence_candidate(row)
        if candidate is not None and candidate["key"] not in seen_keys:
            seen_keys.add(candidate["key"])
            candidates.append(candidate)
    return candidates


def _topic_evidence_candidate(row: Mapping[str, Any]) -> dict[str, Any] | None:
    object_id = _first_text(row, "primary_object_id", "object_id", "id")
    if not object_id:
        return None
    match = _mapping(row.get("match"))
    trace_status = _first_text(row, "trace_status")
    base_semantic = _first_text(match, "semantic_relevance") or _first_text(
        row,
        "semantic_relevance",
    )
    object_payload = _mapping(row.get("object"))
    object_type = _first_text(row, "primary_object_type", "object_type", "type") or "OntologyObject"
    dimensions = _metric_dimensions(object_payload.get("dimensions"))
    if not dimensions:
        dimensions = _metric_dimensions(object_payload.get("dimension"))
    is_company_total = _truthy(object_payload.get("is_company_total"))
    row_metric_scope = (
        "company_total" if is_company_total else "dimensioned" if dimensions else "unspecified"
    )
    raw_clause_matches = _mapping_list(row.get("_plan_clause_matches"))
    if not raw_clause_matches:
        raw_clause_matches = [
            {
                "clause_id": clause_id,
                "planned_match_mode": _first_text(row, "planned_match_mode") or "strict",
                "planned_evidence_terms": _string_list(row.get("planned_evidence_terms")),
                "planned_predicate_terms": _string_list(row.get("planned_predicate_terms")),
                "planned_metric_terms": _string_list(row.get("planned_metric_terms")),
            }
            for clause_id in _string_list(row.get("_plan_clause_ids"))
        ]
    clause_matches: list[ClauseEvidenceMatch] = []
    for raw_clause_match in raw_clause_matches:
        clause_id = _first_text(raw_clause_match, "clause_id")
        if not clause_id:
            continue
        clause_match_mode = (
            "relaxed"
            if _first_text(raw_clause_match, "planned_match_mode") == "relaxed"
            else "strict"
        )
        semantic = base_semantic
        evidence_terms = _string_list(raw_clause_match.get("planned_evidence_terms"))
        predicate_terms = _string_list(raw_clause_match.get("planned_predicate_terms"))
        metric_terms = _string_list(raw_clause_match.get("planned_metric_terms"))
        metric_clause_match = (
            object_type in {"MetricObservation", "XBRLFact", "FinancialMetricValue", "NumericEvidence"}
            and bool(metric_terms)
        )
        relation_verified = _truthy(raw_clause_match.get("planned_relation_verified"))
        relevance_terms = metric_terms if metric_clause_match else evidence_terms
        planned_metric_scope = _first_text(raw_clause_match, "planned_metric_scope") or "any"
        scope_visible = (
            object_type not in {"MetricObservation", "XBRLFact", "FinancialMetricValue", "NumericEvidence"}
            or planned_metric_scope == "any"
            or planned_metric_scope == row_metric_scope
        )
        if metric_clause_match:
            terms_visible = scope_visible and _planned_terms_visible_in_evidence(
                row,
                relevance_terms,
            )
        else:
            terms_visible = scope_visible and _planned_terms_visible_in_atomic_evidence(
                row,
                [*relevance_terms, *predicate_terms],
            )
        if predicate_terms and not relation_verified:
            # Token order/co-occurrence cannot prove subject→predicate→object
            # direction. Keep relational clauses as related until a structured
            # relation verifier explicitly marks this clause match.
            semantic = None
        if metric_terms and not metric_clause_match:
            # A qualitative mention of a metric name cannot satisfy a numeric
            # metric clause without an exact MetricObservation and lineage.
            semantic = None
        if (
            not semantic
            and not metric_terms
            and (not predicate_terms or relation_verified)
            and clause_match_mode == "strict"
            and terms_visible
            and bool(row.get("answer_candidate"))
            and trace_status in {"traceable", "traceable_metric_lineage"}
            and object_type in {"ResearchClaim", "EvidenceQuote"}
            and (
                object_type == "EvidenceQuote"
                or int(row.get("support_quote_count") or 0) > 0
                or int(row.get("support_claim_count") or 0) > 0
            )
        ):
            semantic = "direct"
        clause_matches.append(
            ClauseEvidenceMatch(
                clause_id=clause_id,
                match_mode=clause_match_mode,
                directness=_directness(
                    semantic,
                    trace_status,
                    (_first_text(row, "metric_lineage_status") if metric_clause_match else None),
                    match_mode=clause_match_mode,
                    relevance_verified=terms_visible,
                ),
            )
        )
    directness = _conservative_directness(
        [clause_match.directness for clause_match in clause_matches]
        or [
            _directness(
                base_semantic,
                trace_status,
                _first_text(row, "metric_lineage_status"),
                match_mode=_first_text(row, "planned_match_mode") or "strict",
            )
        ]
    )
    source_ids = _string_list(row.get("source_object_ids"))
    source_ids.extend(_string_list(object_payload.get("supported_by_claims")))
    source_ids = _dedupe_strings(source_ids)
    if object_id not in source_ids:
        source_ids.insert(0, object_id)
    ticker = _first_text(row, "ticker") or _ticker_from_object_id(object_id)
    row_period = _first_text(row, "period")
    period = (
        _first_text(row, "filing_period")
        or _first_text(object_payload, "filing_period")
        or row_period
        if object_type == "MetricObservation"
        else row_period
    )
    document_type = _first_text(row, "document_type")
    title = (
        _first_text(row, "topic_label", "title", "label")
        or _first_text(object_payload, "name", "title", "label", "canonical_metric", "metric_name")
        or ("Evidence quote" if object_type == "EvidenceQuote" else object_id)
    )
    summary = _bounded_text(
        _first_text(row, "topic_summary", "summary", "formatted_value", "text") or title,
        1_200,
    )
    quality = _mapping(row.get("quality"))
    evidence_grade = _evidence_grade(
        _first_text(row, "evidence_strength", "evidence_grade")
        or _first_text(quality, "evidence_grade")
    )
    metric_lineage_status = _first_text(row, "metric_lineage_status")
    if evidence_grade == "unverified" and trace_status == "traceable":
        evidence_grade = "strong" if int(row.get("support_quote_count") or 0) > 0 else "medium"
    elif (
        evidence_grade == "unverified"
        and trace_status == "traceable_metric_lineage"
        and _metric_lineage_complete(metric_lineage_status)
    ):
        evidence_grade = "strong"
    quote_ids = _string_list(row.get("quote_ids"))
    quote_ids.extend(_string_list(object_payload.get("supported_by_quotes")))
    if object_type == "EvidenceQuote":
        quote_ids.insert(0, object_id)
    quote_ids = _dedupe_strings(quote_ids)
    span_ids = _string_list(row.get("span_ids"))
    if object_type == "EvidenceSpan":
        span_ids.insert(0, object_id)
    raw_metric = _first_text(object_payload, "canonical_metric", "metric_name")
    metric = canonical_metric_name(raw_metric) if raw_metric else None
    metric_scope = None
    if metric:
        metric_scope = (
            "company_total" if is_company_total else "dimensioned" if dimensions else "unspecified"
        )
    metric_points: list[MetricPoint] = []
    observation_period = (
        _first_text(object_payload, "observation_period")
        or _first_text(row, "planned_metric_observation_period")
        or row_period
        or period
    )
    if object_type == "MetricObservation" and observation_period:
        metric_context = _mapping(object_payload.get("context"))
        metric_points.append(
            MetricPoint(
                period=observation_period,
                value=object_payload.get("value"),
                formatted_value=_first_text(row, "text"),
                object_id=object_id,
                period_type=(
                    _first_text(object_payload, "period_type", "duration")
                    or _first_text(metric_context, "period_type", "duration")
                ),
                start_date=(
                    _first_text(object_payload, "period_start", "start_date")
                    or _first_text(metric_context, "period_start", "start_date")
                ),
                end_date=(
                    _first_text(object_payload, "period_end", "end_date", "instant")
                    or _first_text(metric_context, "period_end", "end_date", "instant")
                ),
                conflict_value_count=max(
                    1,
                    int(object_payload.get("metric_conflict_value_count") or 1),
                ),
            )
        )
    source_label = _first_text(row, "source_label") or _source_label(ticker, period, document_type)
    occurrence_key = _occurrence_identity(ticker, object_id)
    unit = EvidenceUnit(
        evidence_id=_stable_id("ev", occurrence_key),
        object_id=object_id,
        object_type=object_type,
        ticker=ticker,
        period=period,
        document_type=document_type,
        title=_bounded_text(title, 240),
        summary=summary,
        match_mode=(
            "relaxed" if any(item.match_mode == "relaxed" for item in clause_matches) else "strict"
        ),
        directness=directness,
        evidence_grade=evidence_grade,
        materiality=row.get("materiality_score") or row.get("materiality_hint"),
        metric=metric,
        unit=_first_text(object_payload, "unit"),
        currency=_first_text(object_payload, "currency", "currency_code"),
        dimensions=dimensions,
        metric_scope=metric_scope,
        metric_points=metric_points,
        clause_matches=clause_matches,
        source=EvidenceSource(
            object_ids=source_ids[:16],
            quote_ids=quote_ids[:16],
            span_ids=_dedupe_strings(span_ids)[:16],
            source_label=source_label,
        ),
    )
    return {
        "key": occurrence_key,
        "unit": unit,
        "search_text": _search_text(row, unit),
        "raw_clause_ids": _string_list(row.get("_plan_clause_ids")),
        "clause_matches": clause_matches,
    }


def _metric_evidence_candidate(series: Mapping[str, Any]) -> dict[str, Any] | None:
    series_key = _first_text(series, "series_key")
    raw_metric = _first_text(series, "canonical_metric", "metric_name")
    metric = canonical_metric_name(raw_metric) if raw_metric else None
    if not series_key and not metric:
        return None
    points = [
        MetricPoint(
            period=_first_text(point, "period") or "unknown",
            value=point.get("value"),
            formatted_value=_first_text(point, "formatted_value"),
            object_id=_first_text(point, "object_id"),
            period_type=(
                _first_text(point, "period_type", "duration")
                or _first_text(series, "period_type", "duration")
            ),
            start_date=_first_text(point, "period_start", "start_date"),
            end_date=_first_text(point, "period_end", "end_date", "instant"),
            conflict_value_count=max(
                1,
                int(point.get("metric_conflict_value_count") or 1),
            ),
        )
        for point in _mapping_list(series.get("points"))[:40]
    ]
    object_ids = _dedupe_strings([point.object_id for point in points if point.object_id])
    key = (
        series_key
        or f"{metric}:{_first_text(series, 'ticker')}:{','.join(p.period for p in points)}"
    )
    ticker = _first_text(series, "ticker")
    occurrence_key = _occurrence_identity(ticker, f"metric:{key}")
    periods = [point.period for point in points]
    title = _first_text(series, "label") or metric or key
    summary_values = [point.formatted_value for point in points if point.formatted_value]
    summary = "; ".join(summary_values) or f"{title} metric series"
    lineage = _first_text(series, "metric_lineage_status")
    lineage_complete = _metric_lineage_complete(lineage)
    dimensions = _metric_dimensions(series.get("dimensions"))
    is_company_total = _truthy(series.get("is_company_total"))
    unit = EvidenceUnit(
        evidence_id=_stable_id("ev", occurrence_key),
        object_id=object_ids[0] if len(object_ids) == 1 else None,
        object_type="MetricSeries",
        ticker=ticker,
        period=periods[-1] if periods else None,
        document_type=_first_text(series, "document_type"),
        title=_bounded_text(title, 240),
        summary=_bounded_text(summary, 1_200),
        directness="metric_lineage" if lineage_complete else "unverified",
        evidence_grade="strong" if lineage_complete else "unverified",
        metric=metric,
        unit=_first_text(series, "unit"),
        currency=_first_text(series, "currency", "currency_code"),
        dimensions=dimensions,
        metric_scope=(
            "company_total" if is_company_total else "dimensioned" if dimensions else "unspecified"
        ),
        metric_points=points,
        source=EvidenceSource(
            object_ids=object_ids[:16],
            source_label=_source_label(ticker, periods[-1] if periods else None, None),
        ),
    )
    return {"key": occurrence_key, "unit": unit, "search_text": _search_text(series, unit)}


def _projection_evidence_candidate(row: Mapping[str, Any]) -> dict[str, Any] | None:
    object_id = _first_text(row, "id", "object_id")
    if not object_id:
        return None
    trace_status = _first_text(row, "trace_status")
    summary = _first_text(row, "summary", "label", "title") or object_id
    ticker = _first_text(row, "ticker") or _ticker_from_object_id(object_id)
    period = _first_text(row, "period")
    occurrence_key = _occurrence_identity(ticker, object_id)
    unit = EvidenceUnit(
        evidence_id=_stable_id("ev", occurrence_key),
        object_id=object_id,
        object_type=_first_text(row, "type", "object_type") or "ProjectionCandidate",
        ticker=ticker,
        period=period,
        document_type=_first_text(row, "document_type"),
        title=_bounded_text(_first_text(row, "label", "title") or summary, 240),
        summary=_bounded_text(summary, 1_200),
        directness="related" if trace_status == "traceable" else "unverified",
        evidence_grade="medium" if trace_status == "traceable" else "unverified",
        source=EvidenceSource(
            object_ids=[object_id],
            source_label=_source_label(ticker, period, _first_text(row, "document_type")),
        ),
    )
    return {"key": occurrence_key, "unit": unit, "search_text": _search_text(row, unit)}


def _cross_company_evidence_candidate(row: Mapping[str, Any]) -> dict[str, Any] | None:
    ticker = _first_text(row, "ticker")
    summary = _first_text(row, "commentary_summary", "summary")
    if not ticker or not summary:
        return None
    period = _first_text(row, "period")
    document_type = _first_text(row, "document_type")
    source_ids = _string_list(row.get("source_object_ids"))
    key = f"cross-company:{ticker}:{period}:{hashlib.sha256(summary.encode()).hexdigest()[:12]}"
    unit = EvidenceUnit(
        evidence_id=_stable_id("ev", key),
        object_type="CrossCompanySignal",
        ticker=ticker,
        period=period,
        document_type=document_type,
        title=f"{ticker} filing signal",
        summary=_bounded_text(summary, 1_200),
        directness="related",
        evidence_grade=_evidence_grade(_first_text(row, "evidence_strength")),
        source=EvidenceSource(
            object_ids=source_ids[:16],
            source_label=_first_text(row, "source_label")
            or _source_label(ticker, period, document_type),
        ),
    )
    return {"key": key, "unit": unit, "search_text": _search_text(row, unit)}


def _attach_clause_support(
    candidate: dict[str, Any],
    clauses: Sequence[QueryClause],
) -> dict[str, Any]:
    unit: EvidenceUnit = candidate["unit"]
    explicit_support = set(_string_list(candidate.get("raw_clause_ids")))
    clause_ids = {clause.clause_id for clause in clauses}
    clause_matches = [
        match
        for match in candidate.get("clause_matches") or unit.clause_matches
        if isinstance(match, ClauseEvidenceMatch) and match.clause_id in clause_ids
    ]
    explicit_support.update(match.clause_id for match in clause_matches)
    supports = [clause.clause_id for clause in clauses if clause.clause_id in explicit_support]
    candidate["unit"] = unit.model_copy(
        update={
            "supports_clause_ids": supports,
            "clause_matches": clause_matches,
        }
    )
    candidate["coverage_count"] = len(supports)
    return candidate


def _evidence_sort_key(candidate: Mapping[str, Any]) -> tuple[Any, ...]:
    unit: EvidenceUnit = candidate["unit"]
    return (
        -int(candidate.get("coverage_count") or 0),
        _directness_rank(unit.directness),
        _grade_rank(unit.evidence_grade),
        unit.ticker or "",
        unit.evidence_id,
    )


def _directness_rank(value: EvidenceDirectness) -> int:
    return {"direct": 0, "metric_lineage": 1, "related": 2, "unverified": 3}[value]


def _grade_rank(value: EvidenceGrade) -> int:
    return {"strong": 0, "medium": 1, "weak": 2, "unverified": 3}[value]


def _select_evidence_candidates(
    candidates: Sequence[dict[str, Any]],
    clauses: Sequence[QueryClause],
    *,
    comparison_axes: Sequence[ComparisonAxis],
    requested_periods: Sequence[str],
    limit: int,
) -> tuple[list[dict[str, Any]], set[str]]:
    """Reserve one best unit per required clause, then quality-fill the budget."""
    selected: list[dict[str, Any]] = []
    selected_ids: set[str] = set()
    protected_ids: set[str] = set()

    def add(candidate: dict[str, Any], *, protected: bool) -> None:
        unit: EvidenceUnit = candidate["unit"]
        if unit.evidence_id not in selected_ids:
            selected.append(candidate)
            selected_ids.add(unit.evidence_id)
        if protected:
            protected_ids.add(unit.evidence_id)

    for clause in [item for item in clauses if item.required]:
        matching = [
            candidate
            for candidate in candidates
            if any(
                match.clause_id == clause.clause_id for match in candidate["unit"].clause_matches
            )
        ]
        if not matching:
            continue
        eligible = [
            candidate
            for candidate in matching
            if any(
                match.clause_id == clause.clause_id
                and _meets_directness(match.directness, clause.directness)
                for match in candidate["unit"].clause_matches
            )
        ]
        already_selected = next(
            (candidate for candidate in selected if candidate in (eligible or matching)),
            None,
        )
        if already_selected is not None:
            protected_ids.add(already_selected["unit"].evidence_id)
            continue

        def clause_key(candidate: dict[str, Any]) -> tuple[Any, ...]:
            unit: EvidenceUnit = candidate["unit"]
            match = next(item for item in unit.clause_matches if item.clause_id == clause.clause_id)
            return (
                0 if _meets_directness(match.directness, clause.directness) else 1,
                0 if _metric_matches_clause(unit, clause) else 1,
                _directness_rank(match.directness),
                _grade_rank(unit.evidence_grade),
                -len(unit.supports_clause_ids),
                unit.evidence_id,
            )

        add(min(matching, key=clause_key), protected=True)

    requested_axes = set(comparison_axes)
    metric_candidates = [
        candidate
        for candidate in candidates
        if _candidate_is_required_metric_evidence(candidate, clauses)
    ]
    metric_windows = _metric_windows_for_clauses(clauses)
    if requested_axes.intersection({"absolute_change", "growth_rate"}):
        by_series: dict[tuple[Any, ...], list[dict[str, Any]]] = {}
        for candidate in metric_candidates:
            unit: EvidenceUnit = candidate["unit"]
            for point in unit.metric_points:
                key = _metric_series_key(unit, point)
                by_series.setdefault(key, []).append(candidate)
        for series_key, group in sorted(by_series.items(), key=lambda item: repr(item[0])):
            deduped_group = _dedupe_candidates(group)
            periods = sorted(
                {
                    point.period
                    for candidate in deduped_group
                    for point in candidate["unit"].metric_points
                },
                key=_period_key,
            )
            target_periods: set[str] = set()
            for calculation_window in metric_windows.get(str(series_key[1]), set()):
                compatible_pairs = [
                    (previous, current)
                    for previous_index, previous in enumerate(periods)
                    for current in periods[previous_index + 1 :]
                    if _period_pair_matches_window(
                        previous,
                        current,
                        period_basis=str(series_key[6]),
                        duration_basis=str(series_key[7]),
                        calculation_window=calculation_window,
                    )
                ]
                if compatible_pairs:
                    requested = set(requested_periods)
                    requested_pairs = [
                        pair
                        for pair in compatible_pairs
                        if requested and set(pair).issubset(requested)
                    ]
                    if requested_pairs:
                        for pair in requested_pairs:
                            target_periods.update(pair)
                    else:
                        previous, current = max(
                            compatible_pairs,
                            key=lambda pair: (
                                _period_key(pair[1]),
                                _period_key(pair[0]),
                            ),
                        )
                        target_periods.update((previous, current))
            for candidate in sorted(
                deduped_group,
                key=lambda item: item["unit"].evidence_id,
            ):
                unit: EvidenceUnit = candidate["unit"]
                if not any(point.period in target_periods for point in unit.metric_points):
                    continue
                if len(selected) >= limit and unit.evidence_id not in selected_ids:
                    break
                add(candidate, protected=True)

    if requested_axes.intersection({"value", "value_difference"}):
        by_comparison: dict[tuple[Any, ...], list[dict[str, Any]]] = {}
        for candidate in metric_candidates:
            unit: EvidenceUnit = candidate["unit"]
            for point in unit.metric_points:
                key = _metric_comparison_key(unit, point)
                by_comparison.setdefault(key, []).append(candidate)
        by_identity: dict[
            tuple[Any, ...],
            list[tuple[str, list[dict[str, Any]]]],
        ] = {}
        for key, group in by_comparison.items():
            identity = (key[0], *key[2:])
            by_identity.setdefault(identity, []).append((str(key[1]), group))
        for _identity, period_groups in sorted(
            by_identity.items(),
            key=lambda item: repr(item[0]),
        ):
            _period, group = max(
                period_groups,
                key=lambda item: (
                    len(
                        {
                            candidate["unit"].ticker
                            for candidate in item[1]
                            if candidate["unit"].ticker
                        }
                    ),
                    _period_key(item[0]),
                ),
            )
            tickers: set[str] = set()
            for candidate in sorted(
                _dedupe_candidates(group),
                key=lambda item: (
                    item["unit"].ticker or "",
                    item["unit"].evidence_id,
                ),
            ):
                unit: EvidenceUnit = candidate["unit"]
                if not unit.ticker or unit.ticker in tickers:
                    continue
                if len(selected) >= limit and unit.evidence_id not in selected_ids:
                    break
                add(candidate, protected=True)
                tickers.add(unit.ticker)

    for candidate in candidates:
        if len(selected) >= limit:
            break
        add(candidate, protected=False)
    return selected[:limit], protected_ids


def _metric_matches_clause(unit: EvidenceUnit, clause: QueryClause) -> bool:
    if not clause.metrics:
        return False
    unit_metric = _metric_key(unit.metric)
    return bool(unit_metric) and unit_metric in {_metric_key(metric) for metric in clause.metrics}


def _unit_has_complete_metric_lineage(unit: EvidenceUnit) -> bool:
    """Return whether a selected unit is safe for deterministic arithmetic."""
    return unit.directness == "metric_lineage" or any(
        match.directness == "metric_lineage" for match in unit.clause_matches
    )


def _candidate_is_required_metric_evidence(
    candidate: Mapping[str, Any],
    clauses: Sequence[QueryClause],
) -> bool:
    unit = candidate.get("unit")
    if not isinstance(unit, EvidenceUnit) or not unit.metric_points:
        return False
    matches = {match.clause_id: match for match in unit.clause_matches}
    return any(
        clause.required
        and _metric_matches_clause(unit, clause)
        and clause.clause_id in matches
        and matches[clause.clause_id].directness == "metric_lineage"
        for clause in clauses
    )


def _metric_key(value: str | None) -> str:
    return "_".join(_tokens(value or ""))


def _metric_windows_for_clauses(
    clauses: Sequence[QueryClause],
) -> dict[str, set[Literal["period_over_period", "year_over_year"]]]:
    windows: dict[
        str,
        set[Literal["period_over_period", "year_over_year"]],
    ] = {}
    for clause in clauses:
        if not clause.calculation_window:
            continue
        for metric in clause.metrics:
            windows.setdefault(_metric_key(metric), set()).add(clause.calculation_window)
    return windows


def _metric_series_key(unit: EvidenceUnit, point: MetricPoint) -> tuple[Any, ...]:
    return (
        unit.ticker or "",
        _metric_key(unit.metric),
        unit.unit or "",
        unit.currency or "",
        tuple(sorted((str(key), str(value)) for key, value in unit.dimensions.items())),
        unit.metric_scope or "",
        _period_basis(point.period),
        _metric_duration_basis(point),
    )


def _metric_comparison_key(unit: EvidenceUnit, point: MetricPoint) -> tuple[Any, ...]:
    duration_basis = _metric_duration_basis(point)
    if duration_basis == "unknown":
        # Unknown/derived duration is safe for a single ticker's raw value, but
        # must never align across companies for value comparison arithmetic.
        duration_basis = f"unknown:{unit.ticker or 'unscoped'}"
    return (
        _metric_key(unit.metric),
        point.period,
        unit.unit or "",
        unit.currency or "",
        tuple(sorted((str(key), str(value)) for key, value in unit.dimensions.items())),
        unit.metric_scope or "",
        _period_basis(point.period),
        duration_basis,
    )


def _candidate_latest_period(candidate: Mapping[str, Any]) -> tuple[int, int, str]:
    unit = candidate.get("unit")
    if not isinstance(unit, EvidenceUnit) or not unit.metric_points:
        return (0, 0, "")
    return max((_period_key(point.period) for point in unit.metric_points), default=(0, 0, ""))


def _dedupe_candidates(
    candidates: Sequence[dict[str, Any]],
) -> list[dict[str, Any]]:
    result: list[dict[str, Any]] = []
    seen: set[str] = set()
    for candidate in candidates:
        unit: EvidenceUnit = candidate["unit"]
        if unit.evidence_id in seen:
            continue
        seen.add(unit.evidence_id)
        result.append(candidate)
    return result


def _conflicting_metric_point_keys(
    candidates: Sequence[Mapping[str, Any]],
) -> set[tuple[Any, ...]]:
    """Detect conflicts before evidence truncation so limits cannot hide them."""
    observations: dict[tuple[Any, ...], list[float | None]] = {}
    for candidate in candidates:
        unit = candidate.get("unit")
        if not isinstance(unit, EvidenceUnit) or not _unit_has_complete_metric_lineage(unit):
            continue
        for point in unit.metric_points:
            key = (*_metric_series_key(unit, point), point.period)
            observations.setdefault(key, []).append(_numeric_value(point.value))
            if point.conflict_value_count > 1:
                observations[key].append(None)
    return {
        key
        for key, values in observations.items()
        if any(value is None for value in values)
        or len({value for value in values if value is not None}) > 1
    }


def _build_clause_coverage(
    clauses: Sequence[QueryClause],
    units: Sequence[EvidenceUnit],
    *,
    ticker_scope_by_clause: Mapping[str, Sequence[str]],
) -> list[ClauseCoverage]:
    rows: list[ClauseCoverage] = []
    for clause in clauses:
        comparison_tickers = list(ticker_scope_by_clause.get(clause.clause_id) or [])
        # Primary path: evidence with explicit clause_matches from the search
        # engine.
        matching = [
            (unit, match)
            for unit in units
            for match in unit.clause_matches
            if match.clause_id == clause.clause_id
        ]
        # Fallback path: MetricSeries evidence units are aggregated at render
        # time and do not carry clause_matches from the search engine. If a
        # clause has no explicit matches, check whether any evidence unit's
        # metric identity or required concepts overlap with the clause's
        # required_concepts. This unblocks metric-series evidence that was
        # correctly retrieved but never linked to a clause.
        if not matching:
            clause_concepts_lower = {
                concept.lower() for concept in (clause.required_concepts or [])
            }
            clause_metric_terms = set(clause.metrics or [])
            for unit in units:
                if unit.clause_matches:
                    continue  # already handled by primary path
                unit_metric = (unit.metric or "").lower()
                unit_text = f"{unit.title} {unit.summary}".lower()
                unit_tokens = set(unit_text.replace("_", " ").split())
                # Match if the unit's metric identity overlaps with clause
                # metrics, or if any required concept token appears in the
                # unit's prose. Token-level matching is used because evidence
                # titles like "Service Revenue" won't contain the exact phrase
                # "segment revenue" but share the "revenue" token.
                metric_overlap = (
                    bool(unit_metric)
                    and bool(clause_metric_terms)
                    and any(
                        term.lower() == unit_metric
                        or unit_metric in term.lower()
                        or term.lower() in unit_metric
                        for term in clause_metric_terms
                    )
                )
                concept_visible = False
                if clause_concepts_lower:
                    concept_tokens: set[str] = set()
                    for concept in clause_concepts_lower:
                        concept_tokens.update(concept.replace("_", " ").split())
                    concept_visible = bool(concept_tokens & unit_tokens)
                if metric_overlap or concept_visible:
                    from krw_capability_runtime.mcp_server.contracts import (
                        ClauseEvidenceMatch as _CEM,
                    )
                    fallback_directness = (
                        "metric_lineage"
                        if unit.directness == "metric_lineage"
                        else ("direct" if bool(unit.metric_points) else "related")
                    )
                    matching.append(
                        (
                            unit,
                            _CEM(
                                clause_id=clause.clause_id,
                                match_mode="relaxed",
                                directness=fallback_directness,
                            ),
                        )
                    )
        eligible = [
            (unit, match)
            for unit, match in matching
            if (
                _metric_matches_clause(unit, clause)
                and bool(unit.metric_points)
                and match.directness == "metric_lineage"
                if clause.metrics
                else _meets_directness(match.directness, clause.directness)
            )
        ]
        # Evidence that matched a clause (primary path via clause_matches, or
        # the relaxed fallback above) is sufficient to mark the clause covered.
        # The eligible filter is preserved only to pick the *best* evidence
        # unit for rendering (best_pair/strong_pairs below); it must NOT
        # downgrade matched evidence back to missing/partial, because that
        # causes the planner to reject evidence it already retrieved.
        if matching:
            status: Literal["covered", "partial", "missing"] = "covered"
            reason = None
        else:
            status = "missing"
            reason = "no returned evidence unit covers this clause"
        covered_tickers = [
            ticker
            for ticker in comparison_tickers
            if any(unit.ticker == ticker for unit, _match in eligible)
        ]
        missing_tickers = [ticker for ticker in comparison_tickers if ticker not in covered_tickers]
        if missing_tickers and eligible:
            status = "partial"
            reason = "required comparison evidence is missing for: " + ", ".join(missing_tickers)
        best_pair = min(
            eligible or matching,
            key=lambda item: (
                _directness_rank(item[1].directness),
                _grade_rank(item[0].evidence_grade),
                item[0].evidence_id,
            ),
            default=None,
        )
        strong_pairs = [
            (unit, match)
            for unit, match in matching
            if (
                _metric_matches_clause(unit, clause)
                and bool(unit.metric_points)
                and match.directness == "metric_lineage"
                if clause.metrics
                else match.directness in {"direct", "metric_lineage"}
            )
            and unit.evidence_grade in {"strong", "medium"}
        ]
        strong_claim_ready = (
            bool(strong_pairs)
            and not missing_tickers
            and all(
                any(unit.ticker == ticker for unit, _match in strong_pairs)
                for ticker in comparison_tickers
            )
            if comparison_tickers
            else bool(strong_pairs)
        )
        rows.append(
            ClauseCoverage(
                clause_id=clause.clause_id,
                required=clause.required,
                directness_required=clause.directness,
                status=status,
                evidence_ids=[unit.evidence_id for unit, _match in eligible or matching],
                covered_tickers=covered_tickers,
                missing_tickers=missing_tickers,
                best_directness=best_pair[1].directness if best_pair else None,
                best_evidence_grade=best_pair[0].evidence_grade if best_pair else None,
                strong_claim_ready=strong_claim_ready,
                reason=reason,
            )
        )
    return rows


def _build_answerability(
    plan: SearchPlan,
    coverage: Sequence[ClauseCoverage],
    calculation_coverage: Sequence[CalculationCoverage],
    scope: ResolvedScope,
) -> Answerability:
    required = [row for row in coverage if row.required]
    covered = [row for row in required if row.status == "covered"]
    partial = [row for row in required if row.status == "partial"]
    requires_direct = any(
        row.directness_required == EvidenceRequirement.DIRECT_REQUIRED for row in required
    )
    if plan.answer_scope == AnswerScope.SUPPORTING_CONTEXT_ONLY:
        status: Literal["answerable", "partial", "not_answerable", "supporting_context_only"] = (
            "supporting_context_only"
        )
    elif len(covered) == len(required):
        status = "answerable"
    elif covered or partial:
        status = "partial"
    else:
        status = "not_answerable"
    reason_codes: list[str] = []
    if any(row.status == "missing" for row in required):
        reason_codes.append("required_clause_missing")
    if any(row.status == "partial" for row in required):
        reason_codes.append("directness_requirement_not_met")
    strong_claim_ready = bool(required) and all(row.strong_claim_ready for row in required)
    if not strong_claim_ready:
        reason_codes.append("strong_claim_evidence_not_ready")
    if plan.answer_scope == AnswerScope.SUPPORTING_CONTEXT_ONLY:
        reason_codes.append("ontology_supporting_context_only")
    incomplete_calculations = [row for row in calculation_coverage if row.status != "covered"]
    if incomplete_calculations:
        reason_codes.append("required_metric_calculation_missing")
        if status == "answerable":
            status = "partial"
        strong_claim_ready = False
    if scope.unknown_tickers or scope.missing_tickers or scope.failed_tickers:
        reason_codes.append("requested_scope_incomplete")
        if status == "answerable":
            status = "partial"
        strong_claim_ready = False
    return Answerability(
        status=status,
        strong_claim_allowed=(
            status == "answerable"
            and strong_claim_ready
            and plan.answer_scope == AnswerScope.DIRECT
        ),
        required_clause_count=len(required),
        covered_required_clause_count=len(covered),
        requires_direct_evidence=requires_direct,
        reason_codes=reason_codes,
    )


def _resolved_scope(
    plan: SearchPlan,
    raw: Mapping[str, Any],
    units: Sequence[EvidenceUnit],
) -> ResolvedScope:
    unknown = _string_list(raw.get("unknown_tickers"))
    partial = _mapping(raw.get("partial_answerability"))
    unknown.extend(_string_list(partial.get("missing_tickers")))
    unknown = _dedupe_strings(unknown)
    routing = _mapping(raw.get("routing"))
    unknown.extend(_string_list(routing.get("unknown_tickers")))
    unknown = _dedupe_strings(unknown)
    missing_shards = _mapping(routing.get("missing_shards"))
    missing_tickers = _dedupe_strings(str(ticker) for ticker in missing_shards)
    failed_tickers = _string_list(routing.get("failed_tickers"))
    failed_tickers.extend(str(ticker) for ticker in _mapping(routing.get("shard_errors")))
    failed_tickers = _dedupe_strings(failed_tickers)
    resolved = _dedupe_strings(
        [unit.ticker for unit in units if unit.ticker]
        + _string_list(partial.get("available_tickers"))
        + _string_list(routing.get("resolved_tickers"))
    )
    if plan.tickers:
        unavailable = set(unknown).union(missing_tickers).union(failed_tickers)
        resolved = [
            ticker
            for ticker in plan.tickers
            if ticker in set(resolved) and ticker not in unavailable
        ]
    return ResolvedScope(
        requested_tickers=plan.tickers,
        resolved_tickers=resolved,
        unknown_tickers=unknown,
        missing_tickers=missing_tickers,
        failed_tickers=failed_tickers,
        document_types=plan.document_types,
        periods=plan.periods,
        universe=plan.universe,
    )


def _build_missing_parts(
    plan: SearchPlan,
    raw: Mapping[str, Any],
    coverage: Sequence[ClauseCoverage],
    scope: ResolvedScope,
    calculation_coverage: Sequence[CalculationCoverage],
) -> list[MissingPart]:
    parts: list[MissingPart] = []
    for row in coverage:
        if row.required and row.status != "covered":
            parts.append(
                MissingPart(
                    code=(
                        "direct_evidence_missing"
                        if row.status == "partial"
                        else "required_clause_missing"
                    ),
                    detail=row.reason or "required evidence is missing",
                    clause_id=row.clause_id,
                )
            )
        for ticker in row.missing_tickers:
            parts.append(
                MissingPart(
                    code="comparison_ticker_evidence_missing",
                    detail=(
                        f"{ticker} has no qualifying evidence for comparison clause {row.clause_id}"
                    ),
                    clause_id=row.clause_id,
                    ticker=ticker,
                )
            )
    for ticker in scope.unknown_tickers:
        parts.append(
            MissingPart(
                code="ticker_not_available",
                detail=f"{ticker} is not available in the active ontology release",
                ticker=ticker,
            )
        )
    for ticker in scope.missing_tickers:
        parts.append(
            MissingPart(
                code="ticker_shard_missing",
                detail=f"{ticker} is declared but its company shard is unavailable",
                ticker=ticker,
            )
        )
    for ticker in scope.failed_tickers:
        parts.append(
            MissingPart(
                code="ticker_shard_query_failed",
                detail=f"{ticker} shard could not be queried in this request",
                ticker=ticker,
            )
        )
    for calculation in calculation_coverage:
        if calculation.status == "covered":
            continue
        parts.append(
            MissingPart(
                code="metric_calculation_unavailable",
                detail=calculation.reason
                or (
                    f"{calculation.axis} for {calculation.metric} requires aligned "
                    "metric observations with matching unit, currency, dimension "
                    "scope, and period basis"
                ),
                clause_id=calculation.clause_id,
            )
        )
    for item in raw.get("missing_parts") or []:
        if isinstance(item, Mapping):
            code = _first_text(item, "code", "reason", "type") or "retrieval_gap"
            detail = _first_text(item, "detail", "message", "reason") or code
            parts.append(
                MissingPart(
                    code=_slug(code),
                    detail=_bounded_text(detail, 500),
                    clause_id=_first_text(item, "clause_id"),
                    ticker=_first_text(item, "ticker"),
                )
            )
        elif str(item).strip():
            parts.append(
                MissingPart(
                    code=_slug(str(item)),
                    detail=_bounded_text(str(item), 500),
                )
            )
    deduped: list[MissingPart] = []
    seen: set[tuple[str, str | None, str | None]] = set()
    for part in parts:
        key = (part.code, part.clause_id, part.ticker)
        if key not in seen:
            seen.add(key)
            deduped.append(part)
    return deduped[:24]


def _build_recommended_actions(
    raw: Mapping[str, Any],
    units: Sequence[EvidenceUnit],
    coverage: Sequence[ClauseCoverage],
) -> list[RecommendedAction]:
    actions: list[RecommendedAction] = []
    unit_object_ids = {
        object_id
        for unit in units
        for object_id in ([unit.object_id] if unit.object_id else []) + unit.source.object_ids
    }
    unit_by_evidence_id = {unit.evidence_id: unit for unit in units}
    for item in _mapping_list(raw.get("recommended_tools")):
        tool = _first_text(item, "tool")
        object_id = _first_text(item, "object_id")
        if not tool or (object_id and object_id not in unit_object_ids):
            continue
        actions.append(
            RecommendedAction(
                tool=tool,
                object_id=object_id,
                clause_id=_first_text(item, "clause_id") or None,
                ticker=_first_text(item, "ticker") or None,
                reason=_first_text(item, "purpose", "reason") or "verify selected evidence lineage",
            )
        )
    for row in coverage:
        if row.status == "partial":
            candidate_unit = next(
                (
                    unit_by_evidence_id[evidence_id]
                    for evidence_id in row.evidence_ids
                    if evidence_id in unit_by_evidence_id
                ),
                None,
            )
            object_id = None
            if candidate_unit is not None:
                object_id = candidate_unit.object_id or next(
                    iter(candidate_unit.source.object_ids),
                    None,
                )
            if not object_id:
                continue
            actions.append(
                RecommendedAction(
                    tool="krw_ontology_trace",
                    clause_id=row.clause_id,
                    object_id=object_id,
                    ticker=candidate_unit.ticker if candidate_unit is not None else None,
                    reason="verify a direct filing lineage before making a strong claim",
                )
            )
    deduped: list[RecommendedAction] = []
    seen: set[tuple[str, str | None, str | None]] = set()
    for action in actions:
        # Deduplicate the actual tool invocation. ``clause_id`` is explanatory
        # context, while ticker is part of the occurrence identity and must
        # never be collapsed for shared semantic object ids.
        key = (action.tool, action.object_id, action.ticker)
        if key not in seen:
            seen.add(key)
            deduped.append(action)
    return deduped[:8]


def _source_anchors(
    raw: Mapping[str, Any],
    units: Sequence[EvidenceUnit],
) -> list[SourceAnchor]:
    roles_by_ticker = _mapping(raw.get("filing_document_roles"))
    anchors: list[SourceAnchor] = []
    for ticker, roles_value in sorted(roles_by_ticker.items()):
        roles = _mapping(roles_value)
        for role in ("current_driver", "annual_baseline", "latest_available"):
            anchor = _mapping(roles.get(role))
            if not anchor:
                continue
            anchor_ticker = _first_text(anchor, "ticker") or str(ticker)
            period = _first_text(anchor, "period")
            document_type = _first_text(anchor, "document_type")
            anchors.append(
                SourceAnchor(
                    ticker=anchor_ticker,
                    period=period,
                    document_type=document_type,
                    role=role,
                    source_label=_first_text(anchor, "source_label")
                    or _source_label(anchor_ticker, period, document_type)
                    or anchor_ticker,
                )
            )
    if anchors:
        return anchors
    current = _mapping(raw.get("current_document_anchors"))
    for ticker, value in sorted(current.items()):
        anchor = _mapping(value)
        period = _first_text(anchor, "period")
        document_type = _first_text(anchor, "document_type")
        anchors.append(
            SourceAnchor(
                ticker=_first_text(anchor, "ticker") or str(ticker),
                period=period,
                document_type=document_type,
                role=_first_text(anchor, "role") or "current_driver",
                source_label=_first_text(anchor, "source_label")
                or _source_label(str(ticker), period, document_type)
                or str(ticker),
            )
        )
    if anchors:
        return anchors

    # The v2 planned path deliberately bypasses the legacy keyword-based
    # "current filing" classifier.  Preserve neutral document anchors directly
    # from selected evidence instead of guessing annual/current roles.
    seen: set[tuple[str, str | None, str | None]] = set()
    for unit in units:
        if not unit.ticker:
            continue
        key = (unit.ticker, unit.period, unit.document_type)
        if key in seen:
            continue
        seen.add(key)
        anchors.append(
            SourceAnchor(
                ticker=unit.ticker,
                period=unit.period,
                document_type=unit.document_type,
                role="retrieved_evidence",
                source_label=unit.source.source_label
                or _source_label(unit.ticker, unit.period, unit.document_type)
                or unit.ticker,
            )
        )
        if len(anchors) >= 16:
            break
    return anchors


def _computed_values(raw: Mapping[str, Any]) -> list[ComputedValue]:
    research_pack = _mapping(raw.get("research_pack"))
    metric_pack = _mapping(research_pack.get("metric_series_pack"))
    calculations = _mapping(metric_pack.get("calculations"))
    source_ids_by_series: dict[str, list[str]] = {}
    for series in _mapping_list(metric_pack.get("series")):
        key = _first_text(series, "series_key")
        if key:
            source_ids_by_series[key] = _dedupe_strings(
                _first_text(point, "object_id")
                for point in _mapping_list(series.get("points"))
                if _first_text(point, "object_id")
            )
    values: list[ComputedValue] = []
    for kind, rows in calculations.items():
        if not isinstance(rows, list):
            continue
        for row in _mapping_list(rows):
            series_key = _first_text(row, "series_key")
            value = row.get("value")
            if value is None:
                value = row.get("share")
            if value is None:
                value = row.get("growth_rate")
            if value is None:
                value = row.get("growth_difference")
            stable = f"{kind}:{series_key}:{_first_text(row, 'label')}:{_first_text(row, 'period')}"
            values.append(
                ComputedValue(
                    calculation_id=_stable_id("calc", stable),
                    kind=str(kind),
                    label=_first_text(row, "label"),
                    period=_first_text(row, "period"),
                    value=value,
                    numerator=row.get("numerator"),
                    denominator=row.get("denominator"),
                    source_object_ids=source_ids_by_series.get(series_key or "", [])[:16],
                )
            )
    return values[:40]


def _supported_raw_computed_values(
    values: Sequence[ComputedValue],
    units: Sequence[EvidenceUnit],
) -> list[ComputedValue]:
    visible_object_ids = {
        object_id
        for unit in units
        if _unit_has_complete_metric_lineage(unit)
        for object_id in (([unit.object_id] if unit.object_id else []) + unit.source.object_ids)
    }
    return [
        value
        for value in values
        if value.source_object_ids
        and value.metric
        and value.period
        and value.metric_scope in {"company_total", "dimensioned"}
        and set(value.source_object_ids).issubset(visible_object_ids)
    ]


def _build_calculation_coverage(
    plan: SearchPlan,
    scope: ResolvedScope,
    values: Sequence[ComputedValue],
    *,
    retrieval_incomplete: bool,
) -> list[CalculationCoverage]:
    numeric_axes = [
        axis
        for axis in plan.comparison_axes
        if axis in {"value", "absolute_change", "growth_rate", "value_difference"}
    ]
    if not numeric_axes:
        return []
    default_required_tickers = scope.resolved_tickers or plan.tickers
    rows: list[CalculationCoverage] = []
    for clause in [item for item in plan.clauses if item.required]:
        required_tickers = clause.tickers or default_required_tickers
        for metric in clause.metrics:
            for axis in numeric_axes:
                matching = [
                    value
                    for value in values
                    if value.kind == axis
                    and _metric_key(value.metric) == _metric_key(metric)
                    and _computed_value_matches_scope(value, clause)
                    and (
                        axis not in {"absolute_change", "growth_rate"}
                        or value.calculation_window == clause.calculation_window
                    )
                ]
                aligned_groups: dict[tuple[Any, ...], list[ComputedValue]] = {}
                for value in matching:
                    aligned_groups.setdefault(
                        _calculation_alignment_key(value),
                        [],
                    ).append(value)

                def rank_group(group: Sequence[ComputedValue]) -> tuple[Any, ...]:
                    return (
                        len(
                            set(required_tickers).intersection(
                                ticker for value in group for ticker in value.tickers
                            )
                        ),
                        len(
                            set(plan.periods).intersection(
                                period
                                for value in group
                                for period in (value.from_period, value.period)
                                if period
                            )
                        ),
                        max(
                            (_period_key(value.period or "") for value in group),
                            default=(0, 0, ""),
                        ),
                    )

                complete_groups = [
                    group
                    for group in aligned_groups.values()
                    if _calculation_group_is_complete(
                        axis,
                        group,
                        required_tickers,
                    )
                ]
                if complete_groups and plan.periods:
                    selected_groups = complete_groups
                elif complete_groups:
                    selected_groups = [max(complete_groups, key=rank_group)]
                elif aligned_groups:
                    selected_groups = [max(aligned_groups.values(), key=rank_group)]
                else:
                    selected_groups = []
                selected_values = [value for group in selected_groups for value in group]
                group_tickers = {ticker for value in selected_values for ticker in value.tickers}
                covered_tickers = [ticker for ticker in required_tickers if ticker in group_tickers]
                observed_periods = {
                    period
                    for value in selected_values
                    for period in (value.from_period, value.period)
                    if period
                }
                missing_tickers = [
                    ticker for ticker in required_tickers if ticker not in covered_tickers
                ]
                missing_periods = [
                    period for period in plan.periods if period not in observed_periods
                ]
                if not matching:
                    status: Literal["covered", "partial", "missing"] = "missing"
                elif not complete_groups or missing_periods or retrieval_incomplete:
                    status = "partial"
                else:
                    status = "covered"
                reasons: list[str] = []
                if not matching:
                    reasons.append("no compatible calculation was produced")
                elif not selected_groups:
                    reasons.append("no aligned calculation group was produced")
                if missing_tickers:
                    reasons.append(
                        "no single compatible period/unit/currency/dimension group covers "
                        "tickers: " + ", ".join(missing_tickers)
                    )
                if axis == "value_difference" and matching and not complete_groups:
                    reasons.append(
                        "not every requested ticker pair has an aligned value difference"
                    )
                if missing_periods:
                    reasons.append("missing periods: " + ", ".join(missing_periods))
                if retrieval_incomplete:
                    reasons.append(
                        "upstream metric retrieval was truncated; hidden duplicate or "
                        "conflicting observations cannot be ruled out"
                    )
                rows.append(
                    CalculationCoverage(
                        clause_id=clause.clause_id,
                        metric=metric,
                        axis=axis,
                        metric_scope=clause.metric_scope,
                        metric_dimensions=clause.metric_dimensions,
                        status=status,
                        required_tickers=required_tickers,
                        covered_tickers=covered_tickers,
                        calculation_ids=[value.calculation_id for value in selected_values][:40],
                        reason="; ".join(reasons) or None,
                    )
                )
    return rows


def _calculation_alignment_key(value: ComputedValue) -> tuple[Any, ...]:
    """Identity that must match before values may satisfy one comparison axis."""
    return (
        value.kind,
        _metric_key(value.metric),
        value.from_period,
        value.period,
        value.unit or "",
        value.currency or "",
        tuple(sorted((str(key), str(item)) for key, item in value.dimensions.items())),
        value.metric_scope or "",
        value.period_basis or "",
        value.duration_basis or "",
        value.calculation_window or "",
    )


def _calculation_group_is_complete(
    axis: ComparisonAxis,
    group: Sequence[ComputedValue],
    required_tickers: Sequence[str],
) -> bool:
    if not group:
        return False
    required = set(required_tickers)
    observed = {ticker for value in group for ticker in value.tickers}
    if required and not required.issubset(observed):
        return False
    if axis != "value_difference":
        return True
    if len(required) < 2:
        return False
    expected_pairs = {
        tuple(sorted((left, right)))
        for left_index, left in enumerate(sorted(required))
        for right in sorted(required)[left_index + 1 :]
    }
    observed_pairs = {tuple(sorted(value.tickers)) for value in group if len(value.tickers) == 2}
    return expected_pairs.issubset(observed_pairs)


def _computed_value_matches_scope(
    value: ComputedValue,
    clause: QueryClause,
) -> bool:
    if clause.metric_scope != "any" and value.metric_scope != clause.metric_scope:
        return False
    if not clause.metric_dimensions:
        return True
    haystack = " ".join(f"{key} {item}" for key, item in value.dimensions.items())
    visible_tokens = set(_tokens(haystack))
    required_tokens = {
        token for dimension in clause.metric_dimensions for token in _tokens(dimension)
    }
    return bool(required_tokens) and required_tokens.issubset(visible_tokens)


def _computed_values_from_evidence(
    units: Sequence[EvidenceUnit],
    axes: Sequence[ComparisonAxis],
    *,
    conflicting_metric_points: set[tuple[Any, ...]] | None = None,
    metric_windows: Mapping[
        str,
        set[Literal["period_over_period", "year_over_year"]],
    ]
    | None = None,
) -> list[ComputedValue]:
    requested = set(axes)
    if not requested.intersection({"value", "absolute_change", "growth_rate", "value_difference"}):
        return []

    series: dict[
        tuple[Any, ...],
        dict[str, list[tuple[float, str | None]]],
    ] = {}
    comparisons: dict[
        tuple[Any, ...],
        dict[str, list[tuple[float, str | None]]],
    ] = {}
    for unit in units:
        if (
            not unit.metric
            or not unit.ticker
            or not unit.unit
            or unit.metric_scope not in {"company_total", "dimensioned"}
            or not _unit_has_complete_metric_lineage(unit)
        ):
            continue
        for point in unit.metric_points:
            value = _numeric_value(point.value)
            duration_basis = _metric_duration_basis(point)
            series_key = _metric_series_key(unit, point)
            if value is None or (*series_key, point.period) in (conflicting_metric_points or set()):
                continue
            if duration_basis != "unknown":
                series.setdefault(series_key, {}).setdefault(point.period, []).append(
                    (value, point.object_id)
                )
            comparison_key = _metric_comparison_key(unit, point)
            comparisons.setdefault(comparison_key, {}).setdefault(
                unit.ticker,
                [],
            ).append((value, point.object_id))

    values: list[ComputedValue] = []
    for series_key, points_by_period in sorted(
        series.items(),
        key=lambda item: repr(item[0]),
    ):
        ticker = str(series_key[0])
        metric = str(series_key[1])
        unit_value = str(series_key[2]) or None
        currency = str(series_key[3]) or None
        dimensions = dict(series_key[4])
        metric_scope = str(series_key[5])
        period_basis = str(series_key[6]) or None
        duration_basis = str(series_key[7]) or None
        identity_label = _metric_identity_label(metric, dimensions)
        ordered = [
            (period, reconciled)
            for period, observations in points_by_period.items()
            if (reconciled := _reconcile_metric_observations(observations)) is not None
        ]
        ordered.sort(key=lambda point: _period_key(point[0]))
        requested_windows = sorted((metric_windows or {}).get(metric, set()))
        for previous_index, previous in enumerate(ordered):
            for current in ordered[previous_index + 1 :]:
                previous_period, (previous_value, previous_object_ids) = previous
                current_period, (current_value, current_object_ids) = current
                if previous_period == current_period:
                    continue
                for calculation_window in requested_windows:
                    if not _period_pair_matches_window(
                        previous_period,
                        current_period,
                        period_basis=period_basis or "unknown",
                        duration_basis=duration_basis or "unknown",
                        calculation_window=calculation_window,
                    ):
                        continue
                    source_ids = _dedupe_strings([*previous_object_ids, *current_object_ids])
                    identity = repr(series_key)
                    if "absolute_change" in requested:
                        stable = (
                            f"absolute_change:{calculation_window}:{identity}:"
                            f"{previous_period}:{current_period}"
                        )
                        values.append(
                            ComputedValue(
                                calculation_id=_stable_id("calc", stable),
                                kind="absolute_change",
                                label=(
                                    f"{ticker} {identity_label} {calculation_window} "
                                    f"{previous_period} to {current_period}"
                                ),
                                metric=metric,
                                tickers=[ticker],
                                period=current_period,
                                from_period=previous_period,
                                unit=unit_value,
                                currency=currency,
                                dimensions=dimensions,
                                metric_scope=metric_scope,
                                period_basis=period_basis,
                                duration_basis=duration_basis,
                                calculation_window=calculation_window,
                                value=current_value - previous_value,
                                numerator=current_value,
                                denominator=previous_value,
                                source_object_ids=source_ids,
                            )
                        )
                    # Percentage growth is not economically meaningful across a
                    # zero/negative base or a crossing into a negative value.
                    if "growth_rate" in requested and previous_value > 0 and current_value >= 0:
                        stable = (
                            f"growth_rate:{calculation_window}:{identity}:"
                            f"{previous_period}:{current_period}"
                        )
                        values.append(
                            ComputedValue(
                                calculation_id=_stable_id("calc", stable),
                                kind="growth_rate",
                                label=(
                                    f"{ticker} {identity_label} {calculation_window} "
                                    f"{previous_period} to {current_period}"
                                ),
                                metric=metric,
                                tickers=[ticker],
                                period=current_period,
                                from_period=previous_period,
                                currency=None,
                                dimensions=dimensions,
                                metric_scope=metric_scope,
                                period_basis=period_basis,
                                duration_basis=duration_basis,
                                calculation_window=calculation_window,
                                unit="ratio",
                                value=(current_value - previous_value) / previous_value,
                                numerator=current_value - previous_value,
                                denominator=previous_value,
                                source_object_ids=source_ids,
                            )
                        )

    if requested.intersection({"value", "value_difference"}):
        for comparison_key, observations_by_ticker in sorted(
            comparisons.items(),
            key=lambda item: repr(item[0]),
        ):
            metric = str(comparison_key[0])
            period = str(comparison_key[1])
            unit_value = str(comparison_key[2]) or None
            currency = str(comparison_key[3]) or None
            dimensions = dict(comparison_key[4])
            metric_scope = str(comparison_key[5])
            period_basis = str(comparison_key[6]) or None
            duration_basis = str(comparison_key[7]) or None
            identity_label = _metric_identity_label(metric, dimensions)
            ordered = [
                (ticker, reconciled)
                for ticker, observations in observations_by_ticker.items()
                if (reconciled := _reconcile_metric_observations(observations)) is not None
            ]
            ordered.sort(key=lambda point: point[0])
            if "value" in requested:
                for ticker, (point_value, point_object_ids) in ordered:
                    stable = f"value:{repr(comparison_key)}:{ticker}"
                    values.append(
                        ComputedValue(
                            calculation_id=_stable_id("calc", stable),
                            kind="value",
                            label=f"{ticker} {identity_label} {period}",
                            metric=metric,
                            tickers=[ticker],
                            period=period,
                            unit=unit_value,
                            currency=currency,
                            dimensions=dimensions,
                            metric_scope=metric_scope,
                            period_basis=period_basis,
                            duration_basis=duration_basis,
                            value=point_value,
                            numerator=point_value,
                            source_object_ids=point_object_ids,
                        )
                    )
                    if len(values) >= 40:
                        return values[:40]
            if "value_difference" not in requested:
                continue
            for left_index, left in enumerate(ordered):
                for right in ordered[left_index + 1 :]:
                    left_ticker, (left_value, left_object_ids) = left
                    right_ticker, (right_value, right_object_ids) = right
                    if left_ticker == right_ticker:
                        continue
                    stable = f"value_difference:{repr(comparison_key)}:{left_ticker}:{right_ticker}"
                    values.append(
                        ComputedValue(
                            calculation_id=_stable_id("calc", stable),
                            kind="value_difference",
                            label=(f"{left_ticker} minus {right_ticker} {identity_label}"),
                            metric=metric,
                            tickers=[left_ticker, right_ticker],
                            period=period,
                            unit=unit_value,
                            currency=currency,
                            dimensions=dimensions,
                            metric_scope=metric_scope,
                            period_basis=period_basis,
                            duration_basis=duration_basis,
                            value=left_value - right_value,
                            numerator=left_value,
                            denominator=right_value,
                            source_object_ids=_dedupe_strings(
                                [*left_object_ids, *right_object_ids]
                            ),
                        )
                    )
                    if len(values) >= 40:
                        return values[:40]
    return values[:40]


def _dedupe_computed_values(values: Sequence[ComputedValue]) -> list[ComputedValue]:
    result: list[ComputedValue] = []
    seen: set[str] = set()
    for value in values:
        if value.calculation_id in seen:
            continue
        seen.add(value.calculation_id)
        result.append(value)
    return result


def _numeric_value(value: float | int | str | None) -> float | None:
    if isinstance(value, bool) or value is None:
        return None
    if isinstance(value, (int, float)):
        number = float(value)
        return number if math.isfinite(number) else None
    normalized = str(value).strip().replace(",", "")
    try:
        number = float(normalized)
        return number if math.isfinite(number) else None
    except ValueError:
        return None


def _reconcile_metric_observations(
    observations: Sequence[tuple[float, str | None]],
) -> tuple[float, list[str]] | None:
    if not observations:
        return None
    distinct_values = {value for value, _object_id in observations}
    if len(distinct_values) != 1:
        return None
    value = next(iter(distinct_values))
    object_ids = _dedupe_strings(object_id for _value, object_id in observations if object_id)
    return value, object_ids


def _metric_identity_label(metric: str, dimensions: Mapping[str, str]) -> str:
    if not dimensions:
        return metric
    dimension_label = ", ".join(f"{key}={value}" for key, value in sorted(dimensions.items()))
    return f"{metric} [{dimension_label}]"


def _period_key(value: str) -> tuple[int, int, str]:
    match = re.search(r"(?:CY|FY)?(19\d{2}|20\d{2})(?:Q([1-4]))?", value.upper())
    if not match:
        return (0, 0, value)
    return (int(match.group(1)), int(match.group(2) or 0), value)


def _period_pair_matches_window(
    previous_period: str,
    current_period: str,
    *,
    period_basis: str,
    duration_basis: str,
    calculation_window: Literal["period_over_period", "year_over_year"],
) -> bool:
    previous_year, previous_quarter, _ = _period_key(previous_period)
    current_year, current_quarter, _ = _period_key(current_period)
    if not previous_year or not current_year:
        return False
    if duration_basis == "year_to_date" and calculation_window != "year_over_year":
        return False
    if period_basis.endswith("_annual"):
        return previous_quarter == current_quarter == 0 and current_year - previous_year == 1
    if not period_basis.endswith("_quarterly"):
        return False
    if not previous_quarter or not current_quarter:
        return False
    if calculation_window == "year_over_year":
        return current_year - previous_year == 1 and current_quarter == previous_quarter
    previous_ordinal = previous_year * 4 + previous_quarter
    current_ordinal = current_year * 4 + current_quarter
    return current_ordinal - previous_ordinal == 1


def _period_basis(value: str) -> str:
    normalized = value.upper()
    match = re.search(r"(?:(FY|CY))?(?:19\d{2}|20\d{2})(Q[1-4])?", normalized)
    if match:
        convention = {
            "FY": "fiscal",
            "CY": "calendar",
        }.get(match.group(1) or "", "unspecified")
        cadence = "quarterly" if match.group(2) else "annual"
        return f"{convention}_{cadence}"
    return "unknown"


def _metric_duration_basis(point: MetricPoint) -> str:
    """Normalize XBRL duration semantics without mixing YTD, quarter, and instant facts."""
    normalized = re.sub(
        r"[^a-z0-9]+",
        "_",
        str(point.period_type or "").strip().casefold(),
    ).strip("_")
    aliases = {
        "annual": "annual",
        "year": "annual",
        "yearly": "annual",
        "quarter": "quarter",
        "quarterly": "quarter",
        "three_month": "quarter",
        "three_months": "quarter",
        "year_to_date": "year_to_date",
        "ytd": "year_to_date",
        "instant": "instant",
        "point_in_time": "instant",
        "ttm": "ttm",
        "trailing_twelve_months": "ttm",
    }
    if normalized in aliases:
        return aliases[normalized]
    if point.start_date and point.end_date:
        try:
            days = (
                date.fromisoformat(point.end_date) - date.fromisoformat(point.start_date)
            ).days + 1
        except ValueError:
            days = 0
        if point.start_date == point.end_date:
            return "instant"
        if 70 <= days <= 115:
            return "quarter"
        if 160 <= days <= 300:
            return "year_to_date"
        if 330 <= days <= 400:
            return "annual"
    # A plain annual period label is sufficiently specific; a quarter label is
    # not, because it may hold either a single-quarter or YTD XBRL context.
    if not normalized and _period_basis(point.period).endswith("_annual"):
        return "annual"
    return "unknown"


def _warnings(raw: Mapping[str, Any]) -> list[str]:
    warnings = _string_list(raw.get("warnings"))
    routing = _mapping(raw.get("routing"))
    if routing.get("fallback_used"):
        warnings.append("serving_index_fallback_used")
    return _dedupe_strings(warnings)[:16]


def _meets_directness(
    directness: EvidenceDirectness,
    requirement: EvidenceRequirement,
) -> bool:
    if requirement == EvidenceRequirement.ANY:
        return directness != "unverified"
    if requirement == EvidenceRequirement.DIRECT_PREFERRED:
        # direct_preferred accepts related evidence (traceable context that
        # does not meet the full direct-quote bar) so that causal and
        # qualitative clauses are not permanently stuck at "missing" just
        # because the evidence directness is "related" rather than "direct".
        # direct_required below remains strict.
        return directness in {"direct", "metric_lineage", "related"}
    return directness in {"direct", "metric_lineage"}


def _directness(
    semantic: str | None,
    trace_status: str | None,
    metric_lineage_status: str | None,
    *,
    match_mode: str = "strict",
    relevance_verified: bool = True,
) -> Literal["direct", "metric_lineage", "related", "unverified"]:
    if match_mode == "relaxed":
        return (
            "related" if trace_status in {"traceable", "traceable_metric_lineage"} else "unverified"
        )
    if _metric_lineage_complete(metric_lineage_status):
        if relevance_verified:
            return "metric_lineage"
        return (
            "related" if trace_status in {"traceable", "traceable_metric_lineage"} else "unverified"
        )
    if semantic == "direct" and trace_status in {"traceable", "traceable_metric_lineage"}:
        return "direct" if relevance_verified else "related"
    if trace_status in {"traceable", "traceable_metric_lineage"}:
        return "related"
    return "unverified"


def _metric_lineage_complete(value: str | None) -> bool:
    normalized = str(value or "").strip().casefold()
    return normalized in {
        "complete",
        "verified",
        "traceable_metric_lineage",
        "complete_metric_lineage",
    }


def _evidence_grade(
    value: str | None,
) -> Literal["strong", "medium", "weak", "unverified"]:
    normalized = str(value or "").casefold()
    if normalized in {"strong", "a", "high"}:
        return "strong"
    if normalized in {"medium", "b", "moderate"}:
        return "medium"
    if normalized in {"weak", "c", "low"}:
        return "weak"
    return "unverified"


def _search_text(row: Mapping[str, Any], unit: EvidenceUnit) -> str:
    parts: list[str] = [unit.title, unit.summary, unit.object_type]
    for key in (
        "topic_family",
        "factor_terms",
        "metric_terms",
        "entity_terms",
        "mechanism_terms",
        "scenario_terms",
        "impact_channels",
        "canonical_metric",
        "metric_name",
        "dimensions",
        "text",
    ):
        value = row.get(key)
        if isinstance(value, Mapping):
            parts.extend(f"{item_key} {item_value}" for item_key, item_value in value.items())
        elif isinstance(value, Sequence) and not isinstance(value, (str, bytes)):
            parts.extend(str(item) for item in value)
        elif value is not None:
            parts.append(str(value))
    return " ".join(parts)


def _mapping(value: Any) -> Mapping[str, Any]:
    return value if isinstance(value, Mapping) else {}


def _mapping_list(value: Any) -> list[Mapping[str, Any]]:
    if not isinstance(value, list):
        return []
    return [item for item in value if isinstance(item, Mapping)]


def _metric_dimensions(value: Any) -> dict[str, str]:
    if not isinstance(value, Mapping):
        return {}
    dimensions: dict[str, str] = {}
    for key, item in value.items():
        normalized_key = str(key).strip()
        if not normalized_key or item is None:
            continue
        normalized_value = str(item).strip()
        if normalized_value:
            dimensions[normalized_key] = normalized_value
        if len(dimensions) >= 16:
            break
    return dimensions


def _truthy(value: Any) -> bool:
    if value is True or value == 1:
        return True
    return str(value or "").strip().casefold() in {"true", "yes", "1"}


def _first_text(mapping: Mapping[str, Any], *keys: str) -> str | None:
    for key in keys:
        value = mapping.get(key)
        if value is not None and str(value).strip():
            return str(value).strip()
    return None


def _string_list(value: Any) -> list[str]:
    if not isinstance(value, (list, tuple, set)):
        return []
    return _dedupe_strings(str(item) for item in value if str(item).strip())


def _tokens(value: str) -> list[str]:
    return [
        token.casefold()
        for token in _TOKEN_RE.findall(value.replace("_", " "))
        if len(token) >= 2 or token.isdigit()
    ]


def _dedupe_strings(values: Sequence[str] | Any) -> list[str]:
    result: list[str] = []
    seen: set[str] = set()
    for value in values:
        normalized = str(value).strip()
        if not normalized:
            continue
        key = normalized.casefold()
        if key in seen:
            continue
        seen.add(key)
        result.append(normalized)
    return result


def _stable_id(prefix: str, value: str) -> str:
    return f"{prefix}:{hashlib.sha256(value.encode('utf-8')).hexdigest()[:12]}"


def _occurrence_identity(ticker: str | None, object_id: str) -> str:
    """Return the canonical agent identity for one ticker-scoped occurrence."""
    return f"{str(ticker or '').strip().upper()}\x1f{object_id}"


def _bounded_text(value: str, limit: int) -> str:
    normalized = " ".join(str(value or "").split())
    if len(normalized) <= limit:
        return normalized
    return normalized[: max(0, limit - 1)].rstrip() + "…"


def _source_label(
    ticker: str | None,
    period: str | None,
    document_type: str | None,
) -> str | None:
    parts = [part for part in (ticker, period, document_type) if part]
    return " ".join(parts) if parts else None


def _ticker_from_object_id(object_id: str) -> str | None:
    parts = object_id.split(":")
    return parts[1].upper() if len(parts) > 1 and parts[1] else None


def _slug(value: str) -> str:
    normalized = re.sub(r"[^a-z0-9]+", "_", value.casefold()).strip("_")
    return normalized[:64] or "retrieval_gap"


def _planned_terms_visible_in_evidence(
    row: Mapping[str, Any],
    terms: Sequence[str],
) -> bool:
    terms = _dedupe_strings(terms)
    ticker = _first_text(row, "ticker")
    if ticker:
        terms = [term for term in terms if term.casefold() != ticker.casefold()]
    if not terms:
        return False
    object_payload = _mapping(row.get("object"))
    dimension_text = " ".join(
        f"{key} {value}"
        for key, value in {
            **_metric_dimensions(object_payload.get("dimensions")),
            **_metric_dimensions(object_payload.get("dimension")),
        }.items()
    )
    visible_text = " ".join(
        value
        for value in (
            _first_text(row, "text", "summary", "topic_summary", "title", "label"),
            _first_text(
                object_payload,
                "name",
                "title",
                "label",
                "description",
                "summary",
                "claim_text",
                "quote_text",
                "canonical_metric",
                "metric_name",
            ),
            dimension_text,
        )
        if value
    )
    visible_tokens = _direct_evidence_tokens(visible_text)
    required_tokens = set().union(*(_direct_evidence_tokens(term) for term in terms))
    return bool(required_tokens) and required_tokens.issubset(visible_tokens)


def _planned_terms_visible_in_atomic_evidence(
    row: Mapping[str, Any],
    terms: Sequence[str],
) -> bool:
    """Require every proposition token inside one claim/quote sentence or span."""
    terms = _dedupe_strings(terms)
    ticker = _first_text(row, "ticker")
    if ticker:
        terms = [term for term in terms if term.casefold() != ticker.casefold()]
    required_tokens = set().union(*(_direct_evidence_tokens(term) for term in terms))
    if not required_tokens:
        return False
    object_payload = _mapping(row.get("object"))
    texts = _dedupe_strings(
        value
        for value in (
            _first_text(
                object_payload,
                "claim_text",
                "quote_text",
                "text",
                "description",
                "summary",
            ),
            _first_text(row, "text", "summary", "topic_summary"),
        )
        if value
    )
    for text in texts:
        for span in re.split(r"(?:[.!?。！？]+|\n+)", text):
            visible_tokens = _direct_evidence_tokens(span)
            if required_tokens.issubset(visible_tokens):
                return True
    return False


def _direct_evidence_tokens(value: str) -> set[str]:
    """Return conservative token identities for one atomic evidence span."""
    tokens = {
        _DIRECT_EVIDENCE_TOKEN_EQUIVALENTS.get(token.casefold(), token.casefold())
        for token in _TOKEN_RE.findall(value)
        if len(token) >= 2 or token.isdigit()
    }
    if "%" in value:
        tokens.add("percent")
    return tokens


def _conservative_directness(
    values: Sequence[EvidenceDirectness],
) -> EvidenceDirectness:
    rank: dict[EvidenceDirectness, int] = {
        "direct": 0,
        "metric_lineage": 0,
        "related": 1,
        "unverified": 2,
    }
    return max(values, key=lambda value: rank[value])
