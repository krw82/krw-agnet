---
name: earnings_output_contract
description: "earnings_deep_dive 최종 답변 구조. 실적 한 줄 판단→가설→달라진 것→코멘트→품질→영향→다음 확인→이어서 볼 질문 고정 헤딩"
when_to_use: "earnings_deep_dive 최종 한국어 답변을 작성할 때"
---

# Output Contract

## Opening

Start with one practical earnings judgment before any heading.

Example:

```text
이번 분기는 매출 성장보다 수익성과 현금흐름이 함께 따라오기 시작했는지가 핵심인데, 현재 공시에서는 개선 신호와 투자 부담이 함께 보입니다.
```

## Required Structure

Use:

```text
## 실적 한 줄 판단
## 이번 분기가 검증한 투자 가설
## 이번 분기에 달라진 것
## 숫자보다 중요한 회사 코멘트
## 성장·마진·현금흐름 품질
## 투자 가설에 미친 영향
## 다음 분기에 확인할 것
### 이어서 볼 질문
```

The opening sentence comes before `실적 한 줄 판단`.

## Interpretation Table

When useful, prefer:

```text
| 변화 | 쉽게 말하면 | 투자 의미 |
```

Do not default to a wide quarter-by-quarter raw-number table.
Do not use an upside/base/downside scenario table unless the user explicitly
asked for conditional scenario analysis.

## Number Use

Use exact numbers only when they:

```text
prove materiality
show an inflection
correct a likely misunderstanding
distinguish operating improvement from accounting noise
change the thesis status
```

## Follow-Ups

End with exactly three same-company prompts the user can send immediately, rendered as a numbered Markdown list using `1.`, `2.`, `3.`.

Prefer this pattern:

```text
1. 이 분기 판단이 유지되는 조건과 깨지는 조건을 나눠줘.
2. 상승·하락·횡보 시나리오별 체크포인트를 보여줘.
3. 다음 실적 업데이트에서 먼저 확인할 회사 코멘트만 뽑아줘.
```
