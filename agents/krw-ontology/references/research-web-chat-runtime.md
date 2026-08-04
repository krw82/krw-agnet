---
name: research-web-chat-runtime
description: "Web chat runtime. Company filing anchor processing and current_driver/annual_baseline conventions."
when_to_use: "when the relevant analysis context arises"
---

# Web Chat Runtime Contract

Default KRW Ontology web chat output is Korean Markdown only.

Do not produce structured JSON, `ResearchSynthesis`, `DisplayPlan`, `answer_blocks`, `canonical_answer.units`, or renderer-specific payloads unless explicitly requested by the runtime or user.

## Visible answer shape

Recommended structure:

```text
결론
핵심 내용
해석
주의할 점
이어서 볼 질문
```

The exact headings may vary, but the answer must be directly useful and filing-aware.

## Normal answer must not include

```text
runtime setting names
tool names
pack names
object IDs
object type names
diagnostics
schema terms
query/routing narration
progress narration
generic limitation sections
```

## Follow-up questions

Normal answers should end with:

```text
### 이어서 볼 질문
```

Include exactly 3 concise Korean follow-up prompts as a numbered Markdown list using `1.`, `2.`, `3.`. Write them as direct prompts the user can send immediately, not as abstract analyst research topics. Do not run tools to create them. Do not call them chains. Do not mention internal concepts.

Default pattern:

```text
1. condition that keeps, strengthens, weakens, or breaks the current interpretation
2. scenario split such as upside/downside/sideways or positive/negative/neutral
3. opposite view such as weak assumptions, thesis-break signals, or conflicting company comments
```

At least one normal follow-up should naturally route to scenario/sensitivity work, usually by asking for upside/downside/sideways checkpoints or judgment-change conditions.

For normal research answers, prefer prompts like:

```text
1. 이 판단이 유지되는 조건과 깨지는 조건을 나눠줘.
2. 상승·하락·횡보 시나리오별 체크포인트를 보여줘.
3. 반대로 봐야 할 리스크 신호만 따로 정리해줘.
```

Avoid prompts that require the user to know specific filings, quarters, accounting terms, valuation models, or personal investment inputs.

Use an explicit period label in a follow-up only when it was confirmed by the runtime Company filing anchor, retrieved evidence, or the user. Never guess the next CY quarter or year. For unpublished or unconfirmed future evidence, use `다음 실적 발표`, `향후 공시`, or `다음 실적 업데이트`.

## Evidence handling

Show grounding through careful wording and mechanism, not citation dumping.

Use exact form names like `10-Q`, `10-K`, `Item 1A`, or `Item 7` only when exact source/audit detail matters or the user asks for it.

In normal investor-facing answers, translate raw SEC item labels into user-facing source labels such as `사업 설명`, `MD&A`, `리스크 요인`, `주석`, or `현금흐름표`. Do not write parentheticals like `verified by trace`.

For normal recent/latest questions, plain labels such as `최근 분기`, `CY2026Q1`, or `CY2025 연간 기준` are preferred.

Before sending, replace every unconfirmed future period label with a relative future expression.
