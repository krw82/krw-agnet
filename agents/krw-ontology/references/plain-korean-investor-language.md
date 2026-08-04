---
name: plain_korean_investor_language
description: "최종 답변을 터미널 노트가 아닌 명확한 한국어 투자 설명으로 작성하는 포맷 가이드"
when_to_use: "composer 역할로 한국어 답변을 작성할 때"
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
- Use exact numbers only when they materially change the judgment or support a
  turning point.
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

Prefer interpretation tables over raw metric grids when a table helps:

```markdown
| 항목 | 쉽게 말하면 | 투자 의미 |
| --- | --- | --- |
| 성장 | 매출은 유지 또는 증가 | 사업 수요가 바로 무너진 것은 아님 |
| 수익성 | 제품을 팔고 남기는 이익이 변함 | 가격 경쟁력과 비용 부담을 함께 봐야 함 |
| 현금흐름 | 회사에 남는 현금이 변함 | 투자 여력과 주주환원 여력을 판단하는 단서 |
```

Do not expose `ResearchState`, `SearchPlan`, MCP, tool calls, IDs, budgets,
or internal validation language to the user.
