---
name: provider_proposal_contract
description: "Use before the planner role authors a ResearchProposal v4. Defines the exact provider call shape and common invalid shapes."
when_to_use: "when authoring the planner's provider-facing ResearchProposal v4 call envelope"
---

# Provider proposal contract

There are three deliberately separate shapes:

```text
Flash function arguments       { "proposal": ResearchProposalV4 }
Kernel canonical value          ResearchProposalV4
Physical MCP arguments          SearchPlan v2 root object
```

Flash authors only the first shape. The kernel performs the one closed
provider-wire adaptation (unwrap `proposal`), validates the canonical value,
then compiles and sends the MCP request. This is not a legacy wrapper and it
does not reduce research autonomy.

## Exact normal call

When the advertised function accepts a research proposal, call it with exactly
one root key, `proposal`:

```json
{
  "proposal": {
    "intent": "revenue_trend_10k",
    "answer_scope": "direct",
    "uncertainty": "low",
    "document_types": ["10-K"],
    "periods": ["FY2022", "FY2023", "FY2024"],
    "objectives": [
      {
        "priority": "required",
        "alternatives": [
          { "terms": ["net sales"] },
          { "terms": ["revenue"] },
          { "terms": ["total net sales"] },
          { "terms": ["annual revenue"] }
        ],
        "directness": "direct_required",
        "object_types": ["MetricObservation"],
        "goal": {
          "kind": "metric_time_series",
          "metric": "revenue",
          "metric_dimensions": []
        }
      }
    ]
  }
}
```

The authenticated ticker, question text, SearchPlan clauses, and retrieval
queries are intentionally absent. The kernel supplies or derives them.

## Invalid shapes

The following are different contracts and must not be sent to the provider
function:

```json
{ "intent": "revenue_trend_10k" }
```

```json
{ "proposal": { "search_plan": { "clauses": [] } } }
```

```json
{
  "proposal": { "intent": "revenue_trend_10k" },
  "ticker": "AAPL"
}
```

The first omits the required provider envelope, the second tries to author the
physical MCP format, and the third adds an unauthorised top-level scope field.
All are corrected by making one complete `proposal`-only call.

## Field choices

`intent` is a stable lowercase slug. `document_types` and `periods` state an
explicit user-requested evidence boundary; leave them empty for a recent/latest
question without a named filing or period. `alternatives` are interchangeable
literal filing phrases, one phrase per alternative, not additional research
topics. Each objective allows 1–6 alternatives; prefer the most distinct 1–3
phrases. `object_types` is a required ontology-filter
array; use `[]` when no filter is needed. `goal` is exactly one of the five
tagged goal types documented in the examples. Every objective has `priority`,
`alternatives`, `directness`, `object_types`, and `goal`; every metric goal
includes `metric_dimensions`, including an empty array when no dimension is
requested.
