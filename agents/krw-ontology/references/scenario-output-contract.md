---
name: scenario_output_contract
description: "Use when composing the scenario_sensitivity final Korean answer. Defines the scenario judgment, baseline, interpretation table, and break conditions."
when_to_use: "when the relevant analysis context arises"
---

# Output Contract

## Opening

Start with one filing-supported scenario judgment before any heading.

Example:

```text
현재 공시 기준으로는 수요 둔화보다 투자 부담이 현금흐름으로 얼마나 오래 이어지는지가 더 중요한 변수입니다.
```

## Required Structure

Use:

```text
## 현재 투자 가정
## 가정이 걸려 있는 변수
## 시나리오 지도
## 판단이 바뀌는 조건
## 최종 해석
### 이어서 볼 질문
```

Do not add a separate generic limitation section.

## Scenario Table

Prefer:

```text
| 경로 | 무엇이 달라져야 하나 | 실적·현금흐름 연결 | 투자 해석 |
```

Keep it interpretation-first. Include exact figures only when they materially change the judgment.

## Assumption Labels

Explain assumption origin naturally:

```text
회사가 공시한 기준
공시 흐름에서 도출한 조건
사용자가 제시한 가정
조건부 해석
```

Do not expose internal object or schema labels.

## Follow-Ups

End with exactly three short same-company prompts the user can send immediately. Render them as a numbered Markdown list using `1.`, `2.`, `3.`.

Use this pattern:

```text
1. 이 판단이 강해지는 회사 코멘트만 정리해줘.
2. 하방 시나리오에서 현금이 먼저 압박받는 경로를 더 자세히 봐줘.
3. 다음 실적 업데이트에서 가정 파기 신호만 확인해줘.
```
