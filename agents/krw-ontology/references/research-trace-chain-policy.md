---
name: research-trace-chain-policy
description: "Trace and chain policy. trace = evidence verification for one selected object; chain = mechanism expansion. Defines when to use each."
when_to_use: "when the relevant analysis context arises"
---

# Trace And Chain Policy

`trace` and `chain` are different tools.

```text
trace = evidence lineage
chain = connected business meaning
```

## Use trace when

```text
exact value matters
exact date matters
contract term matters
metric lineage matters
strong claim needs support
direct exposure claim needs verification
```

## Use chain when

```text
mechanism matters
risk -> financial path matters
business activity -> metric/channel link matters
temporal context matters
semantic neighbors are useful
```

## Limits

Trace/chain only selected roots. Do not trace or chain every candidate.

Use `ResearchState.recommended_actions` and the source IDs in the relevant
`evidence_units` when a load-bearing claim needs stronger lineage or mechanism
support. Continue only while that action can change clause coverage,
directness, calculation support, or answer correctness; do not apply a fixed
root count.

## Occurrence identity

Treat every evidence root as `(ticker, object_id)`, not as `object_id` alone.
Canonical IDs may occur in more than one company shard. Preserve the ticker
returned in `evidence_units`, `recommended_actions`, trace results, and chain
nodes.

When a recommended action supplies `tool`, `ticker`, and `object_id`, forward
that invocation identity unchanged:

```json
{"object_id":"shared_object_id","ticker":"AAPL"}
```

Pass the evidence unit's ticker to both `krw_ontology_trace` and
`krw_ontology_chain` even when the ID appears unique. If an unscoped lookup
returns ticker candidates or `ambiguous_object_id`, do not guess; repeat the
call with the intended ticker.

## Bounded chain responses

Read `response_budget.truncated` and `response_budget.omitted_counts` on chain
results. Truncation removes lower-ranked surrounding paths, not the preserved
direct evidence floor. Never interpret an omitted path as proof that a
relationship does not exist. Make another targeted `(ticker, object_id)` call
only when the omitted path could change a material conclusion.

Keep chain/trace internals out of normal answers.
