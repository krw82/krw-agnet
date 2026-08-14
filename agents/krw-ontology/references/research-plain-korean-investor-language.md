---
name: research-plain-korean-investor-language
description: "Plain Korean investor language style guide. Translation rules for finance jargon, mobile readability, and evidence-forward prose."
when_to_use: "when translating filing evidence and investment implications into clear Korean for a general investor"
---

# Plain Korean Investor Language

Use this reference for final Korean answers.

The goal is not to make the analysis shallow. The goal is to translate professional filing analysis into plain Korean that a general investor can understand and act on.

## Core Rule

Internal reasoning may use professional finance terms, English labels, ratios, and ontology/MCP retrieval language.

The final answer should not read like an analyst terminal note. It should read like a clear Korean investment explanation:

```text
무슨 일이 일어났는가?
왜 중요한가?
좋은 신호인가, 나쁜 신호인가?
투자자가 다음에 무엇을 확인해야 하는가?
```

## Language Policy

Prefer Korean explanations over English terms.

If an English acronym is necessary, introduce it once with a Korean explanation, then use the Korean expression afterward.

Good:

```text
잉여현금흐름(FCF), 즉 투자 지출까지 빼고 회사에 남는 현금이 줄어드는 구간입니다.
이후에는 "회사에 남는 현금" 또는 "잉여현금흐름"이라고 씁니다.
```

Bad:

```text
FCF conversion이 낮아지고 CapEx intensity가 높아져 shareholder return capacity가 약해집니다.
```

## Common Translations

Use these plain Korean forms in final answers.

| Professional term | Plain Korean final-answer wording |
|---|---|
| FCF / free cash flow | 회사에 남는 현금 / 잉여현금흐름 |
| OCF / operating cash flow | 영업으로 벌어들인 현금 |
| CapEx / capital expenditure | 설비투자 / 투자 지출 |
| SBC / stock-based compensation | 주식보상 / 기존 주주 가치가 희석될 수 있는 비용 |
| dilution | 기존 주주의 몫이 희석되는 것 |
| gross margin | 제품을 팔고 남기는 이익률 / 매출총이익률 |
| operating margin | 본업 수익성 / 영업이익률 |
| RPO | 앞으로 매출로 잡힐 계약잔고 |
| backlog | 수주잔고 / 계약잔고 |
| ARPU | 고객당 매출 |
| churn | 고객 이탈 |
| retention | 고객 유지율 |
| leverage | 빚 부담 / 차입 부담 |
| debt maturity | 빚 만기 |
| refinancing | 빚을 다시 조달하는 것 |
| working capital | 운전자본 / 재고와 매출채권 등 영업에 묶인 자금 |
| inventory correction | 재고 조정 |
| pricing power | 가격을 올리거나 지키는 힘 |
| mix shift | 매출 구성이 바뀌는 것 |
| multiple compression | 주가가 같은 이익에도 낮은 평가를 받는 것 |
| guidance | 회사가 제시한 실적 전망 |
| consensus | 시장 예상치 |
| impairment | 자산가치 손상 |
| WFE | 반도체 생산장비 투자 |

Sector-specific technical terms may remain when they are the actual business term, but explain them in plain Korean the first time.

Good:

```text
HBM은 AI 서버에 쓰이는 고성능 메모리입니다. 이 비중이 늘면 평균 판매가격과 수익성에 도움이 될 수 있습니다.
```

## Table Style

Prefer interpretation tables over numeric data tables.

Good table shape:

```text
| 항목 | 쉽게 말하면 | 투자 의미 |
|---|---|---|
| 성장 | 매출은 아직 버티고 있음 | 사업 자체가 무너진 신호는 아님 |
| 수익성 | 제품을 팔고 남기는 이익이 높음 | 경쟁력은 아직 유지 중 |
| 현금흐름 | 투자 지출이 커져 남는 현금은 부담 | 주가 반등에는 추가 확인이 필요 |
| 리스크 | 중국 규제 영향이 계속 있음 | 단기 불확실성은 남아 있음 |
```

Bad table shape:

```text
| Metric | 최근 확인 분기 | 이전 확인 분기 | YoY | Interpretation |
```

Use exact numbers only when they materially change the judgment, correct a likely misunderstanding, or support a key turning point. Do not fill tables with figures just because the filing provides them.

## Sentence Style

Prefer short Korean sentences.

Use "쉽게 말하면" when translating a complex financial point.

Good:

```text
쉽게 말하면, 매출은 아직 괜찮지만 회사에 남는 현금은 예전보다 덜 편해졌습니다.
```

Bad:

```text
Revenue resilience remains intact, but FCF conversion is under pressure due to elevated CapEx and working capital dynamics.
```

## Keep Analytical Precision

Plain Korean does not mean vague Korean.

Keep:

```text
periods, document recency, company-specific drivers, direction of change, and investment implication
```

Reduce:

```text
unexplained acronyms, raw ratio grids, English finance jargon, generic textbook definitions, and abstract labels without investor meaning
```

## Final Answer Checklist

Before finalizing, check:

```text
1. Would a general Korean retail investor understand the main point without knowing finance English?
2. Did every important number connect to an investment meaning?
3. Did I translate acronyms and jargon into Korean after first mention?
4. Does the answer explain "so what?" instead of only listing evidence?
5. Are follow-up prompts short, plain Korean, and immediately reusable by the user?
```
