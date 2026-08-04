---
name: research-tool-policy
description: "도구 정책 (query_context/query/trace/chain 용도별 사용)"
when_to_use: "company_research 리서치에서 필요할 때"
---

# MCP Tool Policy

Use KRW ontology MCP tools as read-only evidence tools. Do not use raw JSONL or
memory when indexed ontology evidence can answer or constrain the answer.

## Tool roles

```text
krw_ontology_query_context
  Default first ontology call. Accepts only {search_plan}; returns ResearchState v2.

krw_ontology_plan_query
  Optional SearchPlan validation/debug. Performs no retrieval and is not a
  required normal preflight.

krw_ontology_query
  Targeted structured follow-up for a specific missing clause, metric, object
  type, ticker, period, or dimension.

krw_ontology_compare
  Explicit comparison follow-up when the first ResearchState lacks a required
  aligned comparison.

krw_ontology_trace
  Evidence lineage for one selected (ticker, object_id) occurrence.

krw_ontology_chain
  Business, semantic, or temporal mechanism around one selected
  (ticker, object_id) occurrence. Inspect response_budget before treating its
  returned paths as complete.

krw_ontology_retrieve
  Rare bounded evidence expansion for a specific gap. It is not a substitute
  for an explicit SearchPlan and is not the default after query_context.

krw_ontology_company_context
  Company orientation when planning needs company-specific vocabulary.

krw_ontology_index_context / krw_ontology_catalog / krw_ontology_quality
  Debug, audit, capability, or coverage work only; not normal research.
```

## Normal workflow

```text
DeepSeek-authored SearchPlan
-> query_context({search_plan})
-> inspect ResearchState v2
-> targeted actions for missing_parts/recommended_actions when material
-> selected trace/chain when a load-bearing claim needs it
-> answer
```

Write the complete plan before the first ontology call. For tickerless
discovery, use `universe="covered"`; for explicit companies, use `tickers`.
Never combine both.

Let `query_context` use the global index to choose candidates and company
shards. Do not manually repeat the same plan per keyword or company. When
`recommended_actions` supplies `tool`, `ticker`, and `object_id`, forward that
occurrence identity unchanged. Never guess a ticker for an ambiguous shared
object ID.

Do not issue equivalent plans with cosmetic wording changes. Do not impose
fixed top-5, limit-3, one-follow-up, or hard tool-call caps. Choose adaptive
`limit_results` and `limit_tickers` large enough for the required clauses,
tickers, metric periods/pairs, and conflict detection; reduce them only after
logs and quality evaluation justify it.

## Strong claim rule

A strong qualitative claim requires
`answerability.strong_claim_allowed=true` and sufficient coverage/directness
for every load-bearing required clause. A strong numeric or comparison claim
also requires covered `calculation_coverage` and aligned `computed_values`.
Projection, topic routing, and related context are candidate paths, not proof.
