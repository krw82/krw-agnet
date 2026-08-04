---
name: research-ontology-schema-reference
description: "Ontology schema reference. Detailed object, metric, event, and evidence type specifications."
when_to_use: "when the relevant analysis context arises"
---

# Ontology Schema Reference

This reference describes the current KRW ontology object schema for internal research reasoning. It is based on `src/krw_ontology/schema/objects.py`.

Do not expose schema names, field names, object IDs, scores, or internal lineage mechanics in normal user-facing answers. Use this reference to choose the right evidence path and then translate findings into analyst language.

## Global object conventions

Most filing-grounded objects carry these context fields:

```text
id
type
object_type when present
ticker
source_document_id
document_type
period
schema_version
```

Common governance/evidence-quality fields:

```text
confidence
review_status
supported_by_claims
supported_by_quotes
materiality_hint
materiality_basis
specificity_score
boilerplate_score
ranking_score
```

Interpretation:

```text
confidence/review_status = internal quality controls
specificity/ranking/boilerplate scores = internal ranking aids
supported_by_* = evidence lineage hints
```

These fields should guide tool choice and caveat strength, not appear in normal prose.

## Governance layer

### RunManifest

Purpose: identifies the ontology extraction/build run for a filing.

Important fields:

```text
run_id
pipeline_version
ontology_schema_version
ontology_registry_version
agent_index_schema_version
model
stage_models
source_hash
clean_text_hash
created_at
```

Use for:

```text
debugging build/version issues
coverage audit
reproducibility checks
```

Do not use for normal research answers.

### OntologyRegistrySnapshot

Purpose: captures registry/configuration information used for an extraction.

Important fields:

```text
registry_version
canonical_artifacts
text_fields_by_type
created_at
```

Use for:

```text
debugging canonical artifact paths
schema/text-field audit
serving index validation
```

### ValidationReport

Purpose: summarizes validation counts and rejection/acceptance state.

Important fields:

```text
validation_scope
total_input
total_accepted
total_rejected
summary
created_at
```

Use for:

```text
quality/audit questions
coverage/reliability checks
```

Avoid in normal answers unless the user asks about data quality.

### TaxonomyTerm

Purpose: normalized vocabulary for metrics, factors, activities, sectors, and aliases.

Important fields:

```text
taxonomy
term_type
canonical_name
display_name
aliases
parent_term_id
sector_tags
```

Use for:

```text
query normalization
metric/factor alias resolution
sector/theme matching
universe selection hints
```

## Source layer

### SourceDocument

Purpose: filing-level source metadata.

Important fields:

```text
document_type
period
cik
accession_number
company_name
filing_date
period_end_date
fiscal_year
fiscal_quarter
source_url
raw_text_hash
clean_text_hash
metadata_ref
```

Use for:

```text
latest filing selection
period labeling
source freshness
document coverage
```

### SourceSpan

Purpose: section-level text span from the filing.

Important fields:

```text
section_name
section_number
section_key
section_instance
span_index
start_char
end_char
text
text_hash
char_count
section_detection_confidence
section_detection_method
```

Use for:

```text
trace grounding
section-aware evidence checks
quote provenance
```

### SourceLocation

Purpose: source coordinate pointer into filing text/table boundaries.

Important fields:

```text
source_boundary
source_span_id
source_table_id
source_table_cell_id
section_name
section_path
start_char
end_char
text_hash
clean_text_hash_at_extraction
extraction_run_id
```

Use for:

```text
low-level provenance
debugging exact source location
```

### SourceTable and SourceTableCell

Purpose: table and cell representation.

SourceTable fields:

```text
source_span_id
section_name
table_index
caption
```

SourceTableCell fields:

```text
source_table_id
row_index
column_index
raw_text
normalized_text
```

Use for:

```text
table-derived metric support
numeric lineage
debugging extraction of table values
```

## Evidence layer

### EvidenceQuote

Purpose: accepted quote-level filing evidence.

Important fields:

```text
source_span_id
quote_text
quote_type
section_name
start_char
end_char
absolute_start_char
absolute_end_char
confidence
language_signals
```

Use for:

```text
strong claim support
direct quote grounding
risk/business mechanism evidence
trace output
```

Normal answers should paraphrase quotes. Do not dump long quote text unless explicitly asked.

### LanguageSignal and embedded LanguageSignalEmbedded

Purpose: extracted signal inside or linked to evidence quotes.

Important fields:

```text
signal_text
signal_type
strength
direction
certainty
temporal_scope
exact_match_verified
```

Use for:

```text
directional interpretation
certainty/caveat strength
temporal framing
```

### SupportLink

Purpose: explicit evidence/provenance relationship between objects.

Important fields:

```text
from_id
to_id
target_object_id
target_object_type
support_object_id
support_object_type
support_type
support_role
stance
support_strength
inference_level
evidence_grade
explanation
evidence_strength
requires_inference
created_by
confidence
```

Use for:

```text
claim-to-quote lineage
semantic object-to-claim lineage
metric/evidence support checks
traceability gate
```

SupportLink is provenance/evidence support. It is not the same as a graph/semantic Edge.

## Entity layer

### CanonicalEntity

Purpose: normalized entity.

Important fields:

```text
entity_type
canonical_name
aliases
ticker_scope
sector_tags
status
```

Use for:

```text
supplier/customer/party normalization
agreement parties
entity-aware risk and exposure interpretation
```

### EntityMention

Purpose: mention of a canonical entity in a source object.

Important fields:

```text
canonical_entity_id
mention_text
source_object_id
source_object_type
confidence
```

Use for:

```text
entity grounding
agreement/event/exposure support
```

## Claim layer

### ResearchClaim

Purpose: normalized filing-grounded claim.

Important fields:

```text
claim_text
claim_type
supported_by_quotes
related_metrics
object_type_hints
theme_hint
factor_hint
activity_hint
benchmark_hint
impact_channels
effect_direction
materiality_hint
time_horizon
sector_hint
requires_inference
evidence_strength
materiality_basis
specificity_score
boilerplate_score
ranking_score
confidence
```

Use for:

```text
bridging raw quotes to business/risk interpretation
supporting BusinessActivity, BusinessFactor, ExternalFactorExposure, AgreementTerm, BusinessEvent
finding impact channels and caveats
```

Claims are useful, but strong user-facing claims should still be traceable to quotes or metric lineage.

## Semantic business layer

### BusinessActivity

Purpose: describes what the company does and how it may generate revenue/costs.

Important fields:

```text
name
activity_type
description
revenue_relevance
cost_relevance
related_metrics
sector_tags
supported_by_claims
supported_by_quotes
confidence
```

Use for:

```text
business model answers
revenue driver explanation
segment/activity context
company overview
sector/theme universe matching
```

### BusinessFactor

Purpose: normalized factor that can act as risk, driver, headwind, tailwind, uncertainty, or premise.

Important fields:

```text
name
description
factor_roles
category
occurrence_status
company_polarity
modality
time_horizon
affected_channels
related_activity_ids
related_entity_ids
related_factor_term_ids
related_metric_term_ids
source_object_ids
supported_by_claims
supported_by_quotes
materiality_hint
materiality_basis
specificity_score
boilerplate_score
ranking_score
confidence
```

Use for:

```text
risk/thesis questions
growth driver/headwind analysis
affected financial channel mapping
```

Compatibility names:

```text
RiskFactor
GrowthDriver
Headwind
```

These are views/roles around BusinessFactor-style semantics. Avoid exposing these names unless the user asks for raw schema.

### ExternalFactorExposure

Purpose: maps external factors to company impact channels.

Important fields:

```text
factor
factor_category
benchmark
direction
impact_channel
effect_direction
mechanism
evidence_grade
materiality
time_horizon
related_business_activities
related_metrics
sector_tags
scenario_effects
pass_through_mechanism
offsetting_factors
materiality_basis
specificity_score
boilerplate_score
ranking_score
supported_by_claims
supported_by_quotes
confidence
```

ScenarioEffect fields:

```text
factor_change
affected_channel
metric_direction
company_effect
effect_certainty
lag
conditions
```

Use for:

```text
macro/commodity/rate/FX/regulatory/geopolitical exposure
direct vs related exposure reasoning
risk -> financial channel -> implication explanation
sector/global macro read-through
```

Guardrail:

```text
ExternalFactorExposure can support a pressure channel.
It does not automatically prove a direct quantified impact.
```

### AgreementTerm

Purpose: contractual, financing, lease, covenant, purchase, or commercial terms.

Important fields:

```text
agreement_type
agreement_subtype
name
party_entity_ids
term_start
term_end
date_certainty
pricing_mechanism
volume_commitment
minimum_commitment
termination_condition
principal_amount
currency
interest_rate_type
maturity_date
economic_role
affected_channels
related_entity_ids
supported_by_claims
supported_by_quotes
materiality_hint
materiality_basis
confidence
```

Use for:

```text
contracts
debt/maturity/interest exposure
take-or-pay or purchase commitments
supplier/customer agreements
lease/covenant-style questions
```

### BusinessEvent

Purpose: event-like business, regulatory, operational, guidance, product, or transaction events.

Important fields:

```text
event_type
event_subtype
name
event_status
date_expression
date_type
date_start
date_end
date_certainty
authority_entity_id
related_entity_ids
affected_channels
source_object_ids
supported_by_claims
supported_by_quotes
materiality_hint
materiality_basis
confidence
```

Use for:

```text
recent events
regulatory proceedings
guidance/milestones
product launches
M&A/transaction context
```

### AssumptionCandidate

Purpose: filing-derived assumption candidate for scenario or model framing.

Important fields:

```text
name
assumption_text
assumption_type
value_hint
supported_by_claims
supported_by_quotes
related_metrics
unmapped_metrics
confidence
review_status
```

Use for:

```text
scenario framing
valuation assumption support
sensitivity discussion
```

Do not use it to produce final price targets or investment recommendations.

## Numeric layer

### XBRLFact

Purpose: raw inline XBRL fact.

Important fields:

```text
taxonomy_tag
safe_taxonomy_tag
value
unit
context_ref
source_filing_detail
decimals
```

Use for:

```text
raw reported numeric grounding
metric lineage
debugging numeric extraction
```

### MetricObservation

Purpose: normalized reported or calculated metric observation.

Important fields:

```text
metric_term_id
metric_name
value
unit
scale
fiscal_year
fiscal_period
period_start
period_end
period_type
source_type
source_fact_ids
source_metric_ids
normalization
dimensions
calculation_id
confidence
```

MetricDimensions:

```text
segment
geography
product
customer_type
```

Use for:

```text
revenue/margin/growth/share questions
segment/product/geography tables
period-aligned series
metric lineage
```

Guardrails:

```text
Check unit, scale, period alignment, and dimensions.
Do not use a total-company metric as a segment/product metric.
Do not mix annual and quarterly periods without labeling.
Use CY-style labels in user-facing answers.
```

### Calculation

Purpose: deterministic calculation output linking input metrics to output metrics.

Important fields:

```text
calculation_type
formula
input_metric_ids
output_metric_id
calculation_method
rounding_policy
validation_status
```

Use for:

```text
yoy growth
share of total
growth difference
margin calculation
derived ratios
```

### Metric, FinancialMetricValue, DerivedMetricValue, NumericEvidence, CalculatedNumericSupport

Purpose: compatibility or supporting numeric object families.

Use preference:

```text
Prefer MetricObservation + Calculation + XBRLFact for current answers.
Use compatibility numeric object names only for old artifacts/debug contexts.
```

## Company context layer

### CompanyBusinessProfile

Purpose: compact business profile synthesized from company evidence.

Important fields:

```text
sector
business_model_summary
primary_business_activities
primary_revenue_sources
primary_cost_sources
key_external_factors
key_exposures
key_metrics
key_uncertainties
source_object_ids
confidence
```

Use for:

```text
company overview
business model
sector/global universe selection hints
revenue/cost driver framing
```

### TemporalLink

Purpose: links related objects across periods.

Important fields:

```text
from_object_id
to_object_id
from_period
to_period
relation
rationale
confidence
```

Use for:

```text
trend continuity
period-over-period change explanation
```

### TrendObservation

Purpose: explicit trend between periods.

Important fields:

```text
subject
metric_or_factor
from_period
to_period
direction
magnitude_text
interpretation
supported_by_objects
confidence
```

Use for:

```text
growth/decline direction
qualitative trend interpretation
business factor trend
```

### ChangeEvent

Purpose: observed change in company context.

Important fields:

```text
event_type
event_date
description
affected_objects
supported_by_claims
supported_by_quotes
confidence
```

Use for:

```text
what changed recently
business model changes
risk/event transitions
```

## Graph layer

### Edge

Purpose: semantic/graph relation between objects.

Important fields:

```text
from_id
to_id
relation_name
relation_id
edge_class
evidence_level
generation_method
rationale
confidence
```

Use for:

```text
chain expansion
semantic neighbors
business mechanism paths
temporal/contextual relationships
```

Guardrail:

```text
Edge = relationship/navigation.
SupportLink = evidence/provenance.
Do not treat every Edge as direct evidence.
```

## Which schema objects to prefer by question

```text
Exact quote or filing language:
EvidenceQuote -> SourceSpan/SourceDocument

Business model / revenue driver:
CompanyBusinessProfile -> BusinessActivity -> ResearchClaim -> EvidenceQuote

Metric table / growth / share / margin:
MetricObservation -> Calculation -> XBRLFact -> SupportLink

Risk / thesis / headwind / driver:
BusinessFactor -> ExternalFactorExposure -> ResearchClaim -> EvidenceQuote

Direct external exposure:
ExternalFactorExposure with direct support -> ResearchClaim/EvidenceQuote; separate related context from direct evidence

Contract / debt / commitment / covenant:
AgreementTerm -> ResearchClaim/EvidenceQuote

Event / regulatory / milestone / guidance:
BusinessEvent or ChangeEvent -> ResearchClaim/EvidenceQuote

Sector/global/macro read-through:
CompanyBusinessProfile + BusinessActivity + BusinessFactor + ExternalFactorExposure + selected MetricObservation, synthesized across a bounded covered universe
```

## User-facing translation rule

Never answer with schema inventory.

```text
Bad:
ExternalFactorExposure and BusinessFactor objects indicate related_context.

Good:
The covered companies point to input-cost inflation and pricing/volume trade-offs as the main macro pressure channels.
```

Use schema knowledge to choose evidence and caveat strength. Use analyst language for the final answer.
