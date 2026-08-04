---
name: final_markdown_contract
description: "Use when the composer role writes the final Markdown. The final state returns ordinary Korean Markdown, not JSON or AnswerIR."
when_to_use: "when the relevant analysis context arises"
---

# Final Markdown contract

The final state returns ordinary Korean Markdown, not JSON and not an
AnswerIR. The kernel separately stores the immutable EvidenceLedger collected
from actual capability results with the same atomic final commit.

## Required boundary

- Return only the finished Korean Markdown answer.
- Do not wrap it in a JSON object, code fence, `answer_ir`, or a status
  message.
- Do not call a tool in this state.
- Do not mention failed attempts, recovery feedback, internal contracts, or
  unavailable functions.
- End a normal research answer with exactly three short numbered follow-up
  questions. They must be useful without another explanation.

## Content checklist

Before completing, check privately:

1. Does the first section answer the user's question?
2. Are material numbers tied to the correct period and unit in the prose?
3. Is any causal or strong conclusion actually supported by observed evidence?
4. Is the most important limitation explicit when evidence is partial?
5. Does the answer explain the investment meaning instead of merely listing
   filing text?

## Correct completion

```markdown
## 결론

매출은 증가했지만, 현금흐름까지 같은 속도로 개선됐다고 말하기에는 추가 확인이
필요합니다.

## 왜 중요한가

매출 증가가 회사에 남는 현금으로 이어져야 성장의 질이 좋아졌다고 볼 수 있습니다.

## 확인이 더 필요한 부분

이번 자료는 매출 흐름은 보여 주지만, 투자 지출과 운전자본 영향을 함께 비교하지는
않습니다.

1. 잉여현금흐름도 함께 확인할까요?
2. 전년 대비 영업이익률 변화를 볼까요?
3. 가장 최근 분기에도 흐름이 이어졌는지 확인할까요?
```

## When a final response is cut off

If the kernel says `answer_output_truncated`, write the complete Markdown
answer again, more concisely. Preserve the supported conclusion and disclosed
limitation; do not invent a new fact to shorten the answer.
