---
name: research-ontology-layer-map
description: "온톨로지 레이어 맵 (객체 타입별 용도)"
when_to_use: "company_research 리서치에서 필요할 때"
---

# Ontology Layer Map

Use this map for internal reasoning only. Do not expose these layer names in normal user-facing answers.

## Mental model

The MCP server reads a v3 release serving index. The canonical ontology artifacts remain the source of truth, while serving tables are optimized routes into that evidence.

```text
canonical ontology artifacts = source of truth
global_spine.sqlite = cross-company spine, ranking, and object locator
companies/<TICKER>.sqlite = ticker-local evidence shard
shard_manifest.json = shard routing and integrity manifest
projection/index tables = route candidates
trace/metric lineage = evidence grounding
```

## Layers

```text
Governance layer:
RunManifest, OntologyRegistrySnapshot, TaxonomyTerm, ValidationReport

Source layer:
SourceDocument, SourceLocation, SourceSpan, SourceTable, SourceTableCell

Evidence layer:
EvidenceQuote, LanguageSignal, SupportLink

Entity layer:
CanonicalEntity, EntityMention

Claim layer:
ResearchClaim

Numeric layer:
XBRLFact, MetricObservation, Calculation

Semantic business layer:
BusinessActivity, BusinessFactor, ExternalFactorExposure, AssumptionCandidate, AgreementTerm, BusinessEvent

Company context layer:
CompanyBusinessProfile, TemporalLink, TrendObservation, ChangeEvent

Graph layer:
Edge
```

## Compatibility names

Some user language or generated views may map to canonical types.

```text
RiskFactor / GrowthDriver / Headwind -> BusinessFactor role views
FinancialMetricValue / DerivedMetricValue -> MetricObservation views
NumericEvidence / CalculatedNumericSupport -> MetricObservation + Calculation + SupportLink views
ProjectMilestone / RegulatoryProceeding / GuidanceItem -> BusinessEvent subtypes
ContractTerm / CapitalStructureItem / Lease / Covenant -> AgreementTerm subtypes
SegmentPerformance -> MetricObservation dimensions
```

## Use by question type

```text
exact facts, dates, values, terms, table numbers:
prefer source/evidence/numeric/contract/event objects, then trace

metric trend, share, growth, margin:
prefer MetricObservation + Calculation + metric lineage

risk, scenario, thesis, business model, trend:
prefer semantic business and company context objects, then trace support

direct exposure:
separate direct evidence from related context and negative evidence

sector/global/macro:
use company context and semantic objects to synthesize common signals across a bounded covered universe
```

## Guardrail

Projection/index hits are not the answer. They are candidate routes into evidence. The final answer should be an analyst explanation grounded in the supported route, not a description of the route.
