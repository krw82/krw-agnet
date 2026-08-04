---
name: research-grounded-investment-inference
description: "Grounded investment inference. Defines the estimate-plus-evidence-plus-limits pattern for partial evidence answers."
when_to_use: "when the relevant analysis context arises"
---

# Grounded Investment Inference

Preserve the analyst's freedom to choose questions, clause design, evidence
paths, comparisons, and business interpretation. The runtime, not the model's
confidence, decides what may be stated as a fact.

## Claim boundary

Treat every material statement as one of four kinds:

```text
fact
  Use a direct evidence unit with the same ticker and source identity.

calculation
  Use only a returned computed value with compatible metric, period, scope,
  dimension, unit, currency, basis, and duration.

inference
  Build from verified premises. State the material assumption and the signal
  that would weaken the interpretation. Do not convert plausibility into proof.

scenario
  State the condition explicitly. It is a decision frame, not a disclosed fact.
```

Do not fill an unsupported fact, number, period, causal link, or company
comparison from model memory. Do not use a shared canonical object ID without
its ticker occurrence.

## ResearchState interpretation

```text
answerable + strong_claim_allowed
  State the supported conclusion directly, then explain investor significance.

partial, conflict, or truncation
  Lead with the strongest supported conclusion. Bound its scope in ordinary
  investor language, name the closest alternative signal, and state the next
  monitor that could change the view.

not_answerable or supporting_context_only
  Keep only supported context or a conditional scenario. Do not manufacture a
  company conclusion merely to make the answer feel complete.
```

Do not expose `ResearchState`, coverage, omitted evidence, tools, retries, or
retrieval errors. Do not make a raw refusal the whole answer. Prefer a useful
investor boundary such as: the current evidence supports A; B is the condition
that would change the interpretation; C is the next observable signal.

## Quality check before final prose

```text
Every strong factual or calculated statement has a verified premise.
Every inference has a premise, an assumption, and a disconfirming signal.
Partial evidence has a bounded conclusion and a monitor, not a generic
"could not find" or "do not know" ending.
No conclusion says proven, confirmed, certain, or definitive when strong claims
are disallowed.
```

This is an evidence boundary, not a fixed answer template or a mandatory tool
sequence. Stop or continue research according to material evidence gaps.
