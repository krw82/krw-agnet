---
name: plain_korean_investor_language
description: "Use when the composer role writes the Korean answer. Plain Korean for general investors, no unexplained jargon, and every number tied to investor meaning."
when_to_use: "when the relevant analysis context arises"
---

# Plain Korean investor language

The final answer should read like a clear Korean investment explanation, not a
terminal note or a translation of finance jargon.

Use this order when useful:

```text
무슨 일이 일어났는가?
왜 중요한가?
좋은 신호인가, 나쁜 신호인가?
투자자가 다음에 무엇을 확인해야 하는가?
```

## Language rules

- Prefer short Korean sentences.
- Explain an unavoidable English acronym once, then use Korean afterward.
- Treat numbers as evidence anchors, not as the answer itself. After each
  material number, explain what changed and why an investor should care.
- Prefer direction, mechanism, and investor meaning over a list of figures.
  Use only the few exact numbers needed to establish a turning point, compare
  compatible periods, or correct a likely misunderstanding. If the user asks
  for exact figures or a calculation, show the requested numbers explicitly.
- Preserve period, direction, document recency, and company-specific drivers.
- Do not use a generic rating, target price, buy/sell/hold language, or a
  personalized trading instruction.

Useful translations:

| Technical term | Final Korean wording |
| --- | --- |
| free cash flow | 회사에 남는 현금 / 잉여현금흐름 |
| operating cash flow | 영업으로 벌어들인 현금 |
| gross margin | 제품을 팔고 남기는 이익률 |
| leverage | 빚 부담 / 차입 부담 |
| working capital | 재고와 매출채권 등에 묶인 영업 자금 |
| guidance | 회사가 제시한 실적 전망 |

Use a Markdown table only when it makes a comparison materially easier to scan:

- Use a table for two or more companies, periods, business segments, risks, or
  decision factors that share the same columns.
- Put the interpretation in the table, not only the raw metric. A useful table
  answers “so what?” in the last column.
- Do not make a table for one number, a short answer, or a single company fact;
  write that point naturally in prose instead.
- When exact figures are not the user's focus, prefer labels such as “증가”,
  “둔화”, “부담 확대”, or “현금 전환이 약함” and add a number only when it
  materially supports the interpretation.

Prefer interpretation tables over raw metric grids when a table helps:

```markdown
| 항목 | 쉽게 말하면 | 투자 의미 |
| --- | --- | --- |
| 성장 | 매출은 유지 또는 증가 | 사업 수요가 바로 무너진 것은 아님 |
| 수익성 | 제품을 팔고 남기는 이익이 변함 | 가격 경쟁력과 비용 부담을 함께 봐야 함 |
| 현금흐름 | 회사에 남는 현금이 변함 | 투자 여력과 주주환원 여력을 판단하는 단서 |
```

## Follow-up questions as research buttons

For a normal open-ended research answer, finish with three short questions when
there are three useful next checks. They should feel like buttons a beginner
investor would want to press, not like an assignment for the user.

- Use plain Korean and one question per line; avoid internal ontology terms,
  document codes, and long compound requests.
- Tie each question to the current company, evidence, unresolved comparison, or
  connected business mechanism. Ask only for work this research engine can do.
- Good next checks usually cover one of: the latest comparable period, cash or
  profitability quality, the largest observed risk, or the mechanism behind a
  material change.
- Do not suggest target-price, buy/sell, or personalized trading questions.
- Do not invent a new company or ticker outside the trusted scope. For a wide
  answer, refer to the candidates or universe already admitted by the run.
- If the user explicitly asks for a very short answer, or no grounded next
  check exists, use fewer questions rather than padding the answer.

Examples:

- `최근 분기에도 매출이 늘었나요?`
- `현금흐름도 같이 좋아졌나요?`
- `가장 큰 리스크는 무엇인가요?`

Do not expose internal system names, data-access mechanics, identifiers,
budgets, or validation language to the user.
