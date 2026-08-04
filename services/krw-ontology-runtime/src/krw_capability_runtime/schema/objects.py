"""Pydantic v2 models for all 12 object types matching objects.yaml."""

from __future__ import annotations

from pydantic import BaseModel, Field


SCHEMA_VERSION = "1.0.0-alpha"


class SourceDocument(BaseModel):
    id: str
    type: str = "SourceDocument"
    object_type: str = "SourceDocument"
    ticker: str
    source_document_id: str | None = None
    document_type: str
    period: str
    cik: str | None = None
    accession_number: str | None = None
    company_name: str | None = None
    filing_date: str | None = None
    period_end_date: str | None = None
    fiscal_year: int | None = None
    fiscal_quarter: str | None = None
    source_url: str | None = None
    raw_text_hash: str | None = None
    clean_text_hash: str | None = None
    metadata_ref: str | None = None
    schema_version: str = SCHEMA_VERSION


class TaxonomyTerm(BaseModel):
    id: str
    type: str = "TaxonomyTerm"
    object_type: str = "TaxonomyTerm"
    ticker: str | None = None
    source_document_id: str | None = None
    document_type: str | None = None
    period: str | None = None
    taxonomy: str
    term_type: str
    canonical_name: str
    display_name: str | None = None
    aliases: list[str] | None = None
    parent_term_id: str | None = None
    sector_tags: list[str] | None = None
    schema_version: str = SCHEMA_VERSION


class SourceLocation(BaseModel):
    id: str
    type: str = "SourceLocation"
    object_type: str = "SourceLocation"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    source_boundary: str = "sec_filing"
    source_span_id: str | None = None
    source_table_id: str | None = None
    source_table_cell_id: str | None = None
    section_name: str | None = None
    section_path: str | None = None
    start_char: int | None = None
    end_char: int | None = None
    text_hash: str | None = None
    clean_text_hash_at_extraction: str | None = None
    extraction_run_id: str | None = None
    schema_version: str = SCHEMA_VERSION


class SourceTable(BaseModel):
    id: str
    type: str = "SourceTable"
    object_type: str = "SourceTable"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    source_span_id: str | None = None
    section_name: str | None = None
    table_index: int
    caption: str | None = None
    schema_version: str = SCHEMA_VERSION


class SourceTableCell(BaseModel):
    id: str
    type: str = "SourceTableCell"
    object_type: str = "SourceTableCell"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    source_table_id: str
    row_index: int
    column_index: int
    raw_text: str
    normalized_text: str | None = None
    schema_version: str = SCHEMA_VERSION


class SourceSpan(BaseModel):
    id: str
    type: str = "SourceSpan"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    section_name: str
    section_number: str
    section_key: str | None = None
    section_instance: int | None = None
    span_index: int
    start_char: int
    end_char: int
    text: str
    text_hash: str
    char_count: int
    section_detection_confidence: str
    section_detection_method: str
    schema_version: str = SCHEMA_VERSION


class LanguageSignalEmbedded(BaseModel):
    """Embedded language signal within an EvidenceQuote."""
    signal_text: str
    signal_type: str
    strength: str
    direction: str
    certainty: str
    temporal_scope: str
    exact_match_verified: bool = False


class EvidenceQuote(BaseModel):
    id: str
    type: str = "EvidenceQuote"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    source_span_id: str
    quote_text: str
    quote_type: str
    section_name: str
    start_char: int | None = None
    end_char: int | None = None
    absolute_start_char: int | None = None
    absolute_end_char: int | None = None
    confidence: str
    review_status: str = "accepted"
    language_signals: list[LanguageSignalEmbedded] | None = None
    schema_version: str = SCHEMA_VERSION


class LanguageSignal(BaseModel):
    id: str
    type: str = "LanguageSignal"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    source_quote_id: str
    signal_text: str
    signal_type: str
    strength: str
    direction: str
    certainty: str
    temporal_scope: str
    exact_match_verified: bool
    schema_version: str = SCHEMA_VERSION


class CanonicalEntity(BaseModel):
    id: str
    type: str = "CanonicalEntity"
    object_type: str = "CanonicalEntity"
    ticker: str | None = None
    source_document_id: str | None = None
    document_type: str | None = None
    period: str | None = None
    entity_type: str
    canonical_name: str
    aliases: list[str] | None = None
    ticker_scope: str | None = None
    sector_tags: list[str] | None = None
    status: str = "active"
    schema_version: str = SCHEMA_VERSION


class EntityMention(BaseModel):
    id: str
    type: str = "EntityMention"
    object_type: str = "EntityMention"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    canonical_entity_id: str
    mention_text: str
    source_object_id: str
    source_object_type: str
    confidence: str = "medium"
    schema_version: str = SCHEMA_VERSION


class ResearchClaim(BaseModel):
    id: str
    type: str = "ResearchClaim"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    claim_text: str
    claim_type: str
    supported_by_quotes: list[str] = Field(min_length=1)
    related_metrics: list[str] | None = None
    object_type_hints: list[str] | None = None
    theme_hint: str | None = None
    factor_hint: str | None = None
    activity_hint: str | None = None
    benchmark_hint: str | None = None
    impact_channels: list[str] | None = None
    effect_direction: str | None = None
    materiality_hint: str | None = None
    time_horizon: str | None = None
    sector_hint: str | None = None
    requires_inference: bool | None = None
    evidence_strength: str | None = None
    materiality_basis: list[str] | None = None
    specificity_score: float | None = None
    boilerplate_score: float | None = None
    ranking_score: float | None = None
    confidence: str
    review_status: str = "accepted"
    schema_version: str = SCHEMA_VERSION


class ResearchObject(BaseModel):
    """Shared schema for RiskFactor, GrowthDriver, Headwind. Type field discriminates."""
    id: str
    type: str
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    name: str
    category: str
    description: str
    supported_by_claims: list[str] | None = None
    supported_by_quotes: list[str] | None = None
    affects: list[str] | None = None
    unmapped_impacts: list[str] | None = None
    unmapped_metrics: list[str] | None = None
    qualitative_impact: str
    materiality_hint: str | None = None
    materiality_basis: list[str] | None = None
    specificity_score: float | None = None
    boilerplate_score: float | None = None
    ranking_score: float | None = None
    confidence: str
    review_status: str = "accepted"
    schema_version: str = SCHEMA_VERSION


class BusinessActivity(BaseModel):
    id: str
    type: str = "BusinessActivity"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    name: str
    activity_type: str
    description: str
    revenue_relevance: str = "unknown"
    cost_relevance: str = "unknown"
    related_metrics: list[str] | None = None
    sector_tags: list[str] | None = None
    supported_by_claims: list[str] | None = None
    supported_by_quotes: list[str] | None = None
    confidence: str
    review_status: str = "accepted"
    schema_version: str = SCHEMA_VERSION


class ExternalFactorExposure(BaseModel):
    id: str
    type: str = "ExternalFactorExposure"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    factor: str
    factor_category: str
    benchmark: str | None = None
    direction: str = "unknown"
    impact_channel: str
    effect_direction: str = "uncertain"
    mechanism: str
    evidence_grade: str = "unknown"
    materiality: str = "unknown"
    time_horizon: str = "unknown"
    related_business_activities: list[str] | None = None
    related_metrics: list[str] | None = None
    sector_tags: list[str] | None = None
    scenario_effects: list[ScenarioEffect] | None = None
    pass_through_mechanism: str | None = None
    offsetting_factors: list[str] | None = None
    materiality_basis: list[str] | None = None
    specificity_score: float | None = None
    boilerplate_score: float | None = None
    ranking_score: float | None = None
    supported_by_claims: list[str] | None = None
    supported_by_quotes: list[str] | None = None
    confidence: str
    review_status: str = "accepted"
    schema_version: str = SCHEMA_VERSION


class ScenarioEffect(BaseModel):
    factor_change: str
    affected_channel: str
    metric_direction: str = "unknown"
    company_effect: str = "uncertain"
    effect_certainty: str = "unknown"
    lag: str | None = None
    conditions: list[str] | None = None


class AssumptionCandidate(BaseModel):
    id: str
    type: str = "AssumptionCandidate"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    name: str
    assumption_text: str
    assumption_type: str
    value_hint: str | None = None
    supported_by_claims: list[str] | None = None
    supported_by_quotes: list[str] | None = None
    related_metrics: list[str] | None = None
    unmapped_metrics: list[str] | None = None
    confidence: str
    review_status: str = "needs_review"
    schema_version: str = SCHEMA_VERSION


class Metric(BaseModel):
    id: str
    type: str = "Metric"
    name: str
    category: str
    unit: str
    description: str
    schema_version: str = SCHEMA_VERSION


class XBRLFact(BaseModel):
    id: str
    type: str = "XBRLFact"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    taxonomy_tag: str
    safe_taxonomy_tag: str
    value: float
    unit: str
    context_ref: str
    source_filing_detail: str
    decimals: int | None = None
    schema_version: str = SCHEMA_VERSION


class FinancialMetricValue(BaseModel):
    id: str
    type: str = "FinancialMetricValue"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    metric_name: str
    value: float
    unit: str
    fiscal_year: int | None = None
    fiscal_period: str | None = None
    period_type: str | None = None
    start_date: str | None = None
    end_date: str | None = None
    source_xbrl_fact_id: str
    source: str = "filing_inline_xbrl"
    schema_version: str = SCHEMA_VERSION


class DerivedMetricValue(BaseModel):
    id: str
    type: str = "DerivedMetricValue"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    metric_name: str
    value: float
    unit: str
    fiscal_year: int | None = None
    fiscal_period: str | None = None
    period_type: str | None = None
    formula: str
    input_metric_ids: list[str]
    source: str = "code_calculated"
    schema_version: str = SCHEMA_VERSION


class NumericEvidence(BaseModel):
    id: str
    type: str = "NumericEvidence"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    numeric_kind: str
    evidence_role: str
    value: float
    raw_text: str
    unit: str
    source_object_id: str
    source_field: str
    source_method: str
    source_quote_id: str | None = None
    formula: str | None = None
    input_object_ids: list[str] | None = None
    schema_version: str = SCHEMA_VERSION


class CalculatedNumericSupport(BaseModel):
    id: str
    type: str = "CalculatedNumericSupport"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    calculation_type: str
    value: float
    unit: str
    formula: str
    input_object_ids: list[str]
    display_value: str | None = None
    source: str = "code_calculated"
    schema_version: str = SCHEMA_VERSION


class MetricDimensions(BaseModel):
    segment: str | None = None
    geography: str | None = None
    product: str | None = None
    customer_type: str | None = None


class MetricObservation(BaseModel):
    id: str
    type: str = "MetricObservation"
    object_type: str = "MetricObservation"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    metric_term_id: str | None = None
    metric_name: str
    value: float
    unit: str
    scale: str = "ones"
    fiscal_year: int | None = None
    fiscal_period: str | None = None
    period_start: str | None = None
    period_end: str | None = None
    period_type: str | None = None
    source_type: str
    source_fact_ids: list[str] | None = None
    source_metric_ids: list[str] | None = None
    normalization: str = "reported"
    dimensions: MetricDimensions | None = None
    calculation_id: str | None = None
    confidence: float = 1.0
    schema_version: str = SCHEMA_VERSION


class Calculation(BaseModel):
    id: str
    type: str = "Calculation"
    object_type: str = "Calculation"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    calculation_type: str
    formula: str
    input_metric_ids: list[str]
    output_metric_id: str
    calculation_method: str = "deterministic_code"
    rounding_policy: str = "full_precision_then_display"
    validation_status: str = "passed"
    schema_version: str = SCHEMA_VERSION


class CompanyBusinessProfile(BaseModel):
    id: str
    type: str = "CompanyBusinessProfile"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    sector: str
    business_model_summary: str
    primary_business_activities: list[str] | None = None
    primary_revenue_sources: list[str] | None = None
    primary_cost_sources: list[str] | None = None
    key_external_factors: list[str] | None = None
    key_exposures: list[str] | None = None
    key_metrics: list[str] | None = None
    key_uncertainties: list[str] | None = None
    source_object_ids: list[str] | None = None
    confidence: str
    review_status: str = "accepted"
    schema_version: str = SCHEMA_VERSION


class TemporalLink(BaseModel):
    id: str
    type: str = "TemporalLink"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    from_object_id: str
    to_object_id: str
    from_period: str
    to_period: str
    relation: str
    rationale: str
    confidence: str
    review_status: str = "accepted"
    schema_version: str = SCHEMA_VERSION


class TrendObservation(BaseModel):
    id: str
    type: str = "TrendObservation"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    subject: str
    metric_or_factor: str
    from_period: str
    to_period: str
    direction: str
    magnitude_text: str | None = None
    interpretation: str
    supported_by_objects: list[str]
    confidence: str
    review_status: str = "accepted"
    schema_version: str = SCHEMA_VERSION


class ChangeEvent(BaseModel):
    id: str
    type: str = "ChangeEvent"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    event_type: str
    event_date: str | None = None
    description: str
    affected_objects: list[str] | None = None
    supported_by_claims: list[str] | None = None
    supported_by_quotes: list[str] | None = None
    confidence: str
    review_status: str = "accepted"
    schema_version: str = SCHEMA_VERSION


class Edge(BaseModel):
    id: str
    type: str = "Edge"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    from_id: str
    to_id: str
    relation_name: str
    relation_id: str
    edge_class: str | None = None
    evidence_level: str | None = None
    generation_method: str | None = None
    rationale: str | None = None
    confidence: str
    review_status: str = "accepted"
    schema_version: str = SCHEMA_VERSION


class SupportLink(BaseModel):
    id: str
    type: str = "SupportLink"
    object_type: str = "SupportLink"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    from_id: str
    to_id: str
    target_object_id: str | None = None
    target_object_type: str | None = None
    support_object_id: str | None = None
    support_object_type: str | None = None
    support_type: str
    support_role: str
    stance: str = "supports"
    support_strength: str | None = None
    inference_level: str | None = None
    evidence_grade: str | None = None
    explanation: str | None = None
    evidence_strength: str = "unknown"
    requires_inference: bool = False
    created_by: str = "deterministic_projection"
    confidence: str = "high"
    review_status: str = "accepted"
    schema_version: str = SCHEMA_VERSION


class RunManifest(BaseModel):
    id: str
    type: str = "RunManifest"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    run_id: str
    pipeline_version: str
    ontology_schema_version: str
    ontology_registry_version: str
    agent_index_schema_version: str
    model: str
    stage_models: dict[str, str] | None = None
    source_hash: str | None = None
    clean_text_hash: str | None = None
    created_at: str
    schema_version: str = SCHEMA_VERSION


class OntologyRegistrySnapshot(BaseModel):
    id: str
    type: str = "OntologyRegistrySnapshot"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    registry_version: str
    canonical_artifacts: dict[str, str]
    text_fields_by_type: dict[str, list[str]]
    created_at: str
    schema_version: str = SCHEMA_VERSION


class ValidationReport(BaseModel):
    id: str
    type: str = "ValidationReport"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    validation_scope: str
    total_input: int
    total_accepted: int
    total_rejected: int
    summary: dict
    created_at: str
    schema_version: str = SCHEMA_VERSION


class BusinessFactor(BaseModel):
    id: str
    type: str = "BusinessFactor"
    object_type: str = "BusinessFactor"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    name: str
    description: str
    factor_roles: list[str] = Field(min_length=1)
    category: str
    occurrence_status: str = "unknown"
    company_polarity: str = "unknown"
    modality: str = "unknown"
    time_horizon: str = "unknown"
    affected_channels: list[str] | None = None
    related_activity_ids: list[str] | None = None
    related_entity_ids: list[str] | None = None
    related_factor_term_ids: list[str] | None = None
    related_metric_term_ids: list[str] | None = None
    source_object_ids: list[str] | None = None
    supported_by_claims: list[str] | None = None
    supported_by_quotes: list[str] | None = None
    materiality_hint: str | None = None
    materiality_basis: list[str] | None = None
    specificity_score: float | None = None
    boilerplate_score: float | None = None
    ranking_score: float | None = None
    confidence: str = "medium"
    review_status: str = "accepted"
    schema_version: str = SCHEMA_VERSION


class AgreementTerm(BaseModel):
    id: str
    type: str = "AgreementTerm"
    object_type: str = "AgreementTerm"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    agreement_type: str
    agreement_subtype: str | None = None
    name: str
    party_entity_ids: list[str] | None = None
    term_start: str | None = None
    term_end: str | None = None
    date_certainty: str | None = None
    pricing_mechanism: str | None = None
    volume_commitment: str | None = None
    minimum_commitment: str | None = None
    termination_condition: str | None = None
    principal_amount: float | None = None
    currency: str | None = None
    interest_rate_type: str | None = None
    maturity_date: str | None = None
    economic_role: str = "unknown"
    affected_channels: list[str] | None = None
    related_entity_ids: list[str] | None = None
    supported_by_claims: list[str] | None = None
    supported_by_quotes: list[str] | None = None
    materiality_hint: str | None = None
    materiality_basis: list[str] | None = None
    confidence: str = "medium"
    review_status: str = "accepted"
    schema_version: str = SCHEMA_VERSION


class BusinessEvent(BaseModel):
    id: str
    type: str = "BusinessEvent"
    object_type: str = "BusinessEvent"
    ticker: str
    source_document_id: str
    document_type: str
    period: str
    event_type: str
    event_subtype: str | None = None
    name: str
    event_status: str = "mentioned"
    date_expression: str | None = None
    date_type: str | None = None
    date_start: str | None = None
    date_end: str | None = None
    date_certainty: str | None = None
    authority_entity_id: str | None = None
    related_entity_ids: list[str] | None = None
    affected_channels: list[str] | None = None
    source_object_ids: list[str] | None = None
    supported_by_claims: list[str] | None = None
    supported_by_quotes: list[str] | None = None
    materiality_hint: str | None = None
    materiality_basis: list[str] | None = None
    confidence: str = "medium"
    review_status: str = "accepted"
    schema_version: str = SCHEMA_VERSION


# Type discriminator mapping
OBJECT_TYPE_MODELS = {
    "SourceDocument": SourceDocument,
    "TaxonomyTerm": TaxonomyTerm,
    "SourceLocation": SourceLocation,
    "SourceTable": SourceTable,
    "SourceTableCell": SourceTableCell,
    "SourceSpan": SourceSpan,
    "EvidenceQuote": EvidenceQuote,
    "LanguageSignal": LanguageSignal,
    "CanonicalEntity": CanonicalEntity,
    "EntityMention": EntityMention,
    "ResearchClaim": ResearchClaim,
    "RiskFactor": ResearchObject,
    "GrowthDriver": ResearchObject,
    "Headwind": ResearchObject,
    "BusinessActivity": BusinessActivity,
    "ExternalFactorExposure": ExternalFactorExposure,
    "AssumptionCandidate": AssumptionCandidate,
    "Metric": Metric,
    "XBRLFact": XBRLFact,
    "FinancialMetricValue": FinancialMetricValue,
    "DerivedMetricValue": DerivedMetricValue,
    "NumericEvidence": NumericEvidence,
    "CalculatedNumericSupport": CalculatedNumericSupport,
    "MetricObservation": MetricObservation,
    "Calculation": Calculation,
    "BusinessFactor": BusinessFactor,
    "AgreementTerm": AgreementTerm,
    "BusinessEvent": BusinessEvent,
    "CompanyBusinessProfile": CompanyBusinessProfile,
    "TemporalLink": TemporalLink,
    "TrendObservation": TrendObservation,
    "ChangeEvent": ChangeEvent,
    "Edge": Edge,
    "SupportLink": SupportLink,
    "RunManifest": RunManifest,
    "OntologyRegistrySnapshot": OntologyRegistrySnapshot,
    "ValidationReport": ValidationReport,
}
