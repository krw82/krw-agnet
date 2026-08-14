---
name: research_recovery_loop
description: "Use when a capability returns recovery_required. Defines replace/narrow/split repair modes for model-correctable errors."
when_to_use: "when the previous proposal or capability result requests a model-correctable replacement, narrowing, or split"
---

# Recovery loop

Most tool and proposal errors are private feedback, not run-ending failures.
When the kernel returns a model-correctable recovery result, read its allowed
actions and make the next legal decision. Do not describe the error to the
user and do not treat it as evidence.

## Common envelope

```json
{
  "status": "recovery_required",
  "class": "model_correctable",
  "reason_code": "proposal_shape_invalid",
  "repair_mode": "replace",
  "allowed_actions": ["revise_research"]
}
```

The exact `reason_code`, `repair_mode`, and `allowed_actions` are authoritative
for that turn; do not invent another action name. The envelope contains no new
investment fact.

### Diagnostic detail (when present)

For some violations the envelope includes a `detail` object that tells you
exactly what went wrong and how to fix it:

```json
{
  "status": "recovery_required",
  "reason_code": "proposal_metric_identity_invalid",
  "repair_mode": "replace",
  "detail": {
    "schema_version": 1,
    "field": "/objectives/2/goal/metric",
    "offending_value": "net_profit",
    "valid_alternatives": ["revenue", "net_income", "operating_income", ...],
    "hint": "Replace the offending metric identifier with one of the valid_alternatives..."
  }
}
```

- **`detail.field`** — JSON pointer to the exact location of the error in your
  proposal (e.g. `/objectives/2/goal/metric` means the third objective's goal
  metric field).
- **`detail.offending_value`** — the value you used that was rejected (e.g.
  `"net_profit"` — this is an alias, not a canonical identifier).
- **`detail.valid_alternatives`** — the complete list of canonical identifiers
  you may use instead. Pick the one that matches your intent.
- **`detail.hint`** — a short instruction on how to apply the fix.

When you see a `detail`, fix that exact field in your next proposal. Do not
change anything else — only the field identified by `detail.field`.

## Correct response patterns

| Feedback | Correct next decision |
| --- | --- |
| `proposal_shape_invalid` or `provider_input_envelope_invalid` | Send one complete `{ "proposal": ... }` call with no extra root keys. |
| `proposal_metric_identity_invalid` (with `detail`) | Replace the offending metric at `detail.field` with one of `detail.valid_alternatives`. Use canonical IDs only, not aliases. |
| `mixed_metric_and_qualitative_goal` and `repair_mode: split` | Keep the evidence need, but emit separate metric and qualitative objectives. |
| `proposal_too_broad` and `repair_mode: narrow` | Remove only nonessential/deferred expansion or reduce alternatives; retain answer-critical objectives. |
| `capability_not_available` | Choose an advertised capability, legal transition, or a qualified answer. Never retry an unavailable function. |
| `capability_prerequisite_pending` | Use the advertised prerequisite or wait for its result; do not fabricate a result. |
| evidence gap after a successful result | Propose a focused follow-up only if it can change correctness; otherwise answer with the observed limitation. |

## What not to do

Do not repeat the same rejected proposal unchanged. Do not send a SearchPlan,
`search_plan` wrapper, ticker, user question, or physical MCP field to repair
a provider proposal. Do not add fictional periods, concepts, or facts merely
to satisfy formatting. If the admitted release cannot answer a material part,
either request the smallest decision-changing clarification or give a clearly
limited answer supported by the actual evidence.
