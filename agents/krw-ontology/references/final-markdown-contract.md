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
- Never describe tool dispatch or model working state in the answer. Phrases
  such as "정밀 조회가 실행되지 않았다", "컨텍스트 바운드", "committed
  evidence", or a count of attempted queries are internal process details.
  If a fact is unavailable, say only the investor-facing reason (for example,
  that the company does not separately disclose the item) beside the affected
  conclusion.
- Start with the natural investor-facing conclusion, not a process disclaimer
  such as "partial answer" or a list of internal coverage gaps. Put a material
  evidence limitation next to the affected conclusion in ordinary language.
- Do not open with phrases such as "확보된 증거만으로", "이번 실행에서",
  "현재까지 확보된 자료", or "부분적으로 답변". Those describe the process,
  not the investment question. State the supported conclusion first; say
  "공시가 이 항목을 별도로 공시하지 않아 정확한 비중은 확인되지 않는다"
  only where that limitation matters.
- Use the company identity exactly as supplied by the company context. You may
  write the verified company name with its ticker in parentheses, or the ticker
  alone. Never guess, expand, translate, or substitute a company name from a
  ticker symbol. If the name is not present in the admitted context, use the
  ticker alone.
- For an open-ended research answer, you may end with one to three short
  numbered follow-up questions when they make the next decision materially
  easier. They must be useful without another explanation.
- A user who asks for a short, brief, concise, or one-paragraph answer has
  asked for an answer rather than a menu. Finish after the supported answer;
  do not append a follow-up section just to satisfy a format. Add one next
  question only when it is essential to explain a material uncertainty.

## Content checklist

Before completing, check privately:

1. Does the first section answer the user's question?
2. Are material numbers tied to the correct period and unit in the prose?
3. Is any causal or strong conclusion actually supported by observed evidence?
4. Is the most important limitation explicit when evidence is partial?
5. Does the answer explain the investment meaning instead of merely listing
   filing text?
6. Does every time comparison use the same frequency, or explicitly label
   annual and quarterly values as separate snapshots?
7. Does any peer, industry-average, market-share, or competitor comparison
   have an observed counterpart or benchmark? If not, remove the comparison.
8. If the user asks for the most important risk, is it presented as the
   analyst's first risk to watch with its mechanism, rather than an unsupported
   official ranking?
9. Is a product or customer named only in risk evidence being used as a growth
   driver? If so, relabel it as an estimate, move it to the risk discussion,
   or remove it.
10. If the user explicitly asked for brevity, did the answer stop after the
    direct conclusion and its minimum supporting facts instead of adding
    headings or follow-up prompts by template?
11. Is the company name exactly the verified name from the company context (or
    simply the ticker), with no guessed expansion of the ticker?

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
