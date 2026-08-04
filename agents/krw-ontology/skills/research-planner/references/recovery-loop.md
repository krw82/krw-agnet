---
name: research_recovery_loop
description: "capability 오류/제안 오류를 private 피드백으로 처리하는 복구 루프 규칙. replace/narrow/split repair mode"
when_to_use: "capability가 recovery_required를 반환했을 때"
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
  "allowed_actions": ["revise_research", "narrow_scope", "answer_with_limits"]
}
```

The exact `reason_code`, `repair_mode`, and `allowed_actions` are authoritative
for that turn. The envelope contains no new investment fact.

## Correct response patterns

| Feedback | Correct next decision |
| --- | --- |
| `proposal_shape_invalid` or `provider_input_envelope_invalid` | Send one complete `{ "proposal": ... }` call with no extra root keys. |
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
