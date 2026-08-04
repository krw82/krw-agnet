---
name: research-query-context-contract
description: "query_context 계약 (SearchPlan 명세, ResearchState 해석)"
when_to_use: "company_research 리서치에서 필요할 때"
---

# Query Context Contract

Use `krw_ontology_query_context` as the first ontology research call. It accepts
one public input only:

```json
{
  "search_plan": {
    "question": "original user question",
    "intent": "stable_lowercase_intent",
    "clauses": [
      {
        "clause_id": "demand_support",
        "retrieval_query": "customer demand supports revenue growth",
        "required_concepts": ["customer demand", "revenue growth"],
        "required_predicates": ["supports"],
        "required": true,
        "tickers": [],
        "directness": "direct_preferred",
        "object_types": [],
        "metrics": [],
        "metric_dimensions": [],
        "metric_scope": "company_total",
        "calculation_window": null
      }
    ],
    "tickers": [],
    "document_types": [],
    "periods": [],
    "universe": "covered",
    "comparison_axes": [],
    "answer_scope": "direct",
    "uncertainty": "low",
    "limit_results": 12,
    "limit_tickers": 20
  }
}
```

Do not call it with `question`, `ticker`, `tickers`, `response_detail`,
`response_format`, or any other legacy top-level argument. There is no v1
compatibility path.

If MCP rejects a plan as structurally invalid, repair the named field or clause
and retry with the same public `{search_plan}` shape. Do not bypass the repair
with a legacy query or let the server invent a replacement plan.

## MCP Input Corrections

An invalid `krw_ontology_query_context` call is not executed. MCP returns one
English JSON `isError` result directly to the active SDK run. It batches every
deterministic input problem the server can identify without guessing the user's
intent. Repair **every** listed violation, then make the next allowed
`krw_ontology_query_context` call. MCP never rewrites a plan or starts another
run; the runner does not insert a semantic repair.

```json
{
  "status": "input_correction_required",
  "code": "search_plan_validation_failed",
  "message": "The SearchPlan has 2 correctable input error(s). Correct every listed violation before calling krw_ontology_query_context again.",
  "violations": [
    {
      "field": "clauses[0].retrieval_query",
      "rule": "missing_literal_term",
      "message": "The metric_dimensions literal(s) 'Home and Accessories' are missing from clauses[0].retrieval_query.",
      "required_change": "Include every exact phrase in required_literals in clauses[0].retrieval_query, or remove those values from clauses[0].metric_dimensions.",
      "required_literals": ["Home and Accessories"]
    },
    {
      "field": "clauses[0].calculation_window",
      "rule": "missing_calculation_window",
      "message": "A required metric clause needs calculation_window when comparison_axes requests absolute_change or growth_rate.",
      "required_change": "Set calculation_window to period_over_period or year_over_year for this required metric clause, or remove the temporal comparison axis.",
      "required_literals": []
    }
  ],
  "allowed_next_tools": ["krw_ontology_query_context"]
}
```

`code`, every `violations[*].field`, and every
`violations[*].required_change` are the repair contract. Do not treat a
correction as evidence, an answer-quality rating, or a user-visible status.
Do not repeat unchanged input or ask MCP to guess the intended semantics.

Before the first call, make a literal ledger for each clause:

- List each `required_concepts`, `required_predicates`, and
  `metric_dimensions` phrase. Copy every one verbatim into that clause's
  `retrieval_query`, or remove it from the corresponding requirement field.
- Keep a metric clause pure: use `metrics` and `metric_dimensions` there; move
  causal, risk, lending, or other qualitative concepts and predicates into a
  separate qualitative clause.
- When requesting `absolute_change` or `growth_rate`, set a valid
  `calculation_window` on every required metric clause before calling MCP.

This checklist avoids avoidable correction turns; it does not let the server
invent missing requirements or silently alter the authored SearchPlan.

The active DeepSeek agent must author the complete `SearchPlan` before the
first ontology call. `krw_ontology_plan_query` is validation/debug only: it
normalizes and validates a plan but performs no retrieval. Do not insert it as
a mandatory preflight before every normal query.

## Runtime routing boundary

Author the complete plan and let `query_context` route it through the global
index into only the required company shards. Do not manually fan out the same
question company by company or repeat one query per keyword. Use explicit
`tickers` for known companies and `universe="covered"` for discovery.

Treat router scores, coherence diagnostics, and index locations as retrieval
control data, never as answer evidence. Claims still require ResearchState
coverage and source-backed evidence units.

## SearchPlan

Set these fields deliberately:

```text
question
  Preserve the original user question for synthesis and audit. It is not a
  substitute for clause retrieval queries.

intent
  Use a stable lowercase slug selected by the agent.

clauses
  Split the question into independently verifiable propositions. At least one
  clause must be required.

tickers / universe
  Use explicit uppercase tickers for known-company research. For tickerless
  discovery use universe="covered". They are mutually exclusive.

document_types / periods
  Add only filters requested by the user or required by the research design.

comparison_axes
  Use value, absolute_change, growth_rate, value_difference, directness, or
  evidence_grade only when the answer needs that axis.

answer_scope
  Use direct unless ontology evidence is only supporting context.

uncertainty
  Report planning uncertainty honestly; higher uncertainty preserves a wider
  retrieval candidate set.

limit_results / limit_tickers
  Choose adaptive evidence and discovery budgets for this plan.
```

Do not impose a universal top-5, limit-3, or hard tool-call policy. Set
`limit_results` high enough to cover every required clause, clause ticker,
metric period/pair, and potential conflicting observation. Set
`limit_tickers` high enough for the requested discovery breadth. Reduce these
budgets only after production logs and quality evaluation show that doing so
does not lose required coverage.

## QueryClause

Each clause supports:

```text
clause_id
retrieval_query
required_concepts
required_predicates
required
tickers
directness
object_types
metrics
metric_dimensions
metric_scope
calculation_window
```

Write `retrieval_query` as a self-contained, concise English filing query. For
a Korean request, translate the retrieval concepts into filing vocabulary such
as `management discussion`, `customer demand`, `revenue`, `gross margin`,
`operating cash flow`, `capital expenditures`, `backlog`, or `direct exposure`.
Do not use broad conversational Korean as the retrieval query.

Name every required concept, predicate, metric, and dimension in
`retrieval_query`. Every `required_concepts`, `required_predicates`, and
`metric_dimensions` value must appear there as its exact literal phrase after
whitespace normalization. Put canonical metric identifiers in `metrics`; readable
metric aliases may appear in `retrieval_query` when the metric dictionary maps
them to the same canonical identity.

Use the clause fields as follows:

```text
required_concepts
  The complete non-metric concepts the evidence must establish.

required_predicates
  Relation/mechanism terms that must co-occur with all required concepts in
  one traceable span. A qualitative clause with multiple concepts requires at
  least one predicate.

tickers
  An optional subset of SearchPlan.tickers for this clause. Empty inherits the
  plan scope; a subset cannot name a ticker outside the plan.

directness
  any, direct_preferred, or direct_required.

object_types
  Optional ontology object filters chosen from known index capabilities.

metrics
  Canonical metric identifiers required for exact metric lineage.

metric_dimensions
  Exact segment, geography, product, counterparty, or other dimensions.

metric_scope
  company_total, dimensioned, or intentionally broad any.

calculation_window
  period_over_period or year_over_year when a temporal calculation is needed.
```

Do not mix a metric proposition with a causal, mechanism, risk, or other
qualitative proposition in one clause. Metric clauses cannot set
`required_predicates`; split the qualitative statement into another clause.
For a pure metric clause, normally leave `required_concepts` empty and express
the identity through `metrics` and `metric_dimensions`.

Example qualitative clause:

```json
{
  "clause_id": "demand_to_margin",
  "retrieval_query": "customer demand affects gross margin management discussion",
  "required_concepts": ["customer demand", "gross margin"],
  "required_predicates": ["affects"],
  "required": true,
  "tickers": ["MSFT"],
  "directness": "direct_preferred",
  "object_types": ["ResearchClaim", "EvidenceQuote"],
  "metrics": [],
  "metric_dimensions": [],
  "metric_scope": "company_total"
}
```

Example metric clause:

```json
{
  "clause_id": "revenue_yoy",
  "retrieval_query": "revenue year over year",
  "required_concepts": [],
  "required_predicates": [],
  "required": true,
  "tickers": ["MSFT"],
  "directness": "direct_required",
  "object_types": ["MetricObservation"],
  "metrics": ["revenue"],
  "metric_dimensions": [],
  "metric_scope": "company_total",
  "calculation_window": "year_over_year"
}
```

Example dimensioned metric clause:

```json
{
  "clause_id": "home_accessories_sales",
  "retrieval_query": "Home and Accessories net sales",
  "required": true,
  "tickers": ["AAPL"],
  "directness": "direct_required",
  "object_types": ["MetricObservation"],
  "metrics": ["revenue"],
  "metric_dimensions": ["Home and Accessories"],
  "metric_scope": "dimensioned"
}
```

The following is invalid because the dimension is absent from
`retrieval_query`; the runtime returns a `missing_literal_term` violation
rather than silently adding it:

```json
{
  "retrieval_query": "net sales by product category",
  "metrics": ["revenue"],
  "metric_dimensions": ["Home and Accessories"],
  "metric_scope": "dimensioned"
}
```

## Metric and period semantics

`absolute_change` and `growth_rate` require `calculation_window` on every
required metric clause. Use:

```text
period_over_period
  Adjacent annual observations or adjacent quarter observations.

year_over_year
  The same annual period or same fiscal/calendar quarter one year apart.
  YTD observations support year_over_year only.
```

Require exact `metric_scope` and `metric_dimensions` for the requested series.
Do not mix FY and CY bases, quarter and YTD durations, annual and quarterly
amounts, different units/currencies, or total-company and dimensioned series.
Cross-company `value_difference` requires aligned metric identity, scope,
dimensions, period basis, duration, unit, and currency for every requested
ticker pair.

## ResearchState v2

`krw_ontology_query_context` returns `research-state/v2`. Read these fields
directly:

```text
resolved_scope
  Requested/resolved/unknown/missing/failed tickers plus document and period
  scope.

source_anchors
  Ticker, period, document type, role, and source label for current, annual,
  latest, or retrieved evidence anchors.

answerability.status / answerability.strong_claim_allowed
  Overall answer policy: answerable, partial, not_answerable, or
  supporting_context_only.

clause_coverage
  covered/partial/missing state, ticker coverage, best directness/evidence
  grade, and strong_claim_ready for every clause.

evidence_units
  Deduplicated facts and metrics with clause support and traceable source IDs.
  Their operational identity is (ticker, object_id); a shared canonical ID may
  have a distinct occurrence in more than one company.

computed_values / calculation_coverage
  Deterministic values plus covered/partial/missing calculation support for
  each requested metric axis.

missing_parts / recommended_actions
  Precise gaps and bounded next actions tied to a clause and occurrence. When
  an action supplies tool, ticker, and object_id, preserve all three fields in
  the follow-up invocation; clause_id is explanatory context.

continuation
  Whether bounded evidence was omitted and why.

warnings
  Conflicts, truncation, or other conditions that narrow safe interpretation.
```

Make a strong qualitative claim only when overall
`answerability.strong_claim_allowed` and the relevant required clause coverage
support it. Make a strong numeric or comparative claim only when the relevant
`calculation_coverage` is covered and its `computed_values` have complete,
aligned lineage. Related evidence is context, not proof of a requested
directional relation.

When the state is partial, continue only for specific `missing_parts` or
`recommended_actions`. Do not obey a fixed one-follow-up cap, and do not repeat
the same broad plan with cosmetic wording changes. Stop when required clause
and calculation coverage support the intended answer, or when another call
would add volume without changing correctness.
