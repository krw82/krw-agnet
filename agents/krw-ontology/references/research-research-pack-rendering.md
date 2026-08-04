---
name: research-research-pack-rendering
description: "Research pack rendering. Defines ResearchState field purposes (resolved_scope, source_anchors, clause_coverage, evidence_units, computed_values)."
when_to_use: "when the relevant analysis context arises"
---

# ResearchState Rendering Policy

`ResearchState v2` is the model-visible response contract. It is not ontology
schema and its field names are not user-facing language.

Use it to decide what can be said, which evidence supports each proposition,
and whether a targeted continuation can materially improve correctness.

## Scope and source anchors

Read `resolved_scope` before synthesizing across companies, documents, or
periods. Do not silently treat unknown, missing, or failed tickers as covered.

Use `source_anchors` to distinguish the current driver, annual baseline,
latest available source, and neutral retrieved-evidence anchors. Do not invent a
document role when the returned role is only `retrieved_evidence`.

## Answerability and clause coverage

Read together:

```text
answerability.status
answerability.strong_claim_allowed
clause_coverage[].status
clause_coverage[].strong_claim_ready
clause_coverage[].best_directness
clause_coverage[].best_evidence_grade
```

Render a strong conclusion only when the overall answer policy and every
load-bearing required clause support it. If a required clause is partial or
missing, narrow the conclusion to the covered propositions.

For direct-exposure or directional-relation questions, related evidence is
context only. It does not establish the requested direction.

## Evidence units

Use `evidence_units` as the answer-ready evidence set. Match evidence to
clauses through `supports_clause_ids` and `clause_matches`; use source object,
quote, and span IDs only for internal traceability.

For company overview, mechanism, risk, and event questions, synthesize:

```text
evidence -> mechanism -> financial meaning -> investor interpretation
```

Do not render an object inventory or infer a relation from broad co-occurrence.

## Numeric rendering

For numeric and comparison answers, read together:

```text
evidence_units[].metric
evidence_units[].metric_points
evidence_units[].unit
evidence_units[].currency
evidence_units[].dimensions
evidence_units[].metric_scope
computed_values
calculation_coverage
```

Render a precise value, change, growth rate, or cross-company difference only
when the relevant `calculation_coverage.status` is `covered` and the aligned
`computed_values` support it.

Check metric identity, scope, dimensions, period basis, duration, unit,
currency, calculation window, and source lineage. Do not combine FY with CY,
quarter with YTD, annual with quarterly, or company-total with dimensioned
series.

If calculation coverage is partial or missing, omit the unsupported arithmetic
or state a narrower qualitative conclusion. Do not invent a table.

## Targeted continuation and stop

Use `missing_parts` and `recommended_actions` to choose a specific next
action. Use `continuation` and `warnings` to detect omitted evidence,
conflicts, or truncation that can weaken an otherwise plausible conclusion.

Do not apply a universal one-follow-up or fixed tool-call cap. Continue only
when a named gap could change answer correctness, directness, or calculation
coverage. Stop when required clause and calculation coverage support the
intended answer or when another call would only add volume.
