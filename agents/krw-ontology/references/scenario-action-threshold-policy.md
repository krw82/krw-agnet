---
name: scenario_action_threshold_policy
description: "시나리오 가정을 관찰 가능한 증거 조건으로 번역. 판단 강화/유지/약화/가정 파기 분류 기준"
when_to_use: "scenario_sensitivity에서 행동 임계값을 정의할 때"
---

# Action Threshold Policy

## Purpose

Translate tested assumptions into observable evidence conditions. Do not create
personalized trading rules.

The threshold is not "what should the user trade?" It is "what evidence would
change the filing-grounded judgment?"

## Standard States

```text
판단 강화:
the financial conversion is better than the current baseline

판단 유지:
the central mechanism remains intact but still needs proof

판단 약화:
the burden rises, conversion is delayed, or management evidence deteriorates

가정 파기:
the load-bearing mechanism no longer holds
```

## Threshold Sources

Use this priority:

```text
1. Explicit company guidance, covenant, contract, or filing threshold
2. User-provided threshold
3. Qualitative evidence condition derived from filing mechanics, company commentary, notes, risk factors, or cash-flow path
```

Do not invent a number merely to make the answer look actionable.

## Evidence Conditions

Write conditions as evidence the user can check in later company materials:

```text
management commentary confirms the same demand path
orders, backlog, or usage convert into reported revenue
gross margin or operating margin absorbs the investment burden
working capital normalizes instead of consuming more cash
capex intensity begins to show operating proof
liquidity remains flexible after commitments, debt maturities, and capital allocation
risk-factor language becomes more concrete or starts matching reported results
```

Avoid thresholds that require the user to know market consensus, target prices,
technical chart levels, or portfolio loss tolerance.

## Good Conditions

```text
수주 증가가 실제 매출과 현금흐름 개선으로 이어질 때 판단이 강해집니다.
투자 지출은 늘지만 본업 수익성이 따라오지 않으면 판단이 약해집니다.
차입과 주식 발행 없이는 투자 계획을 유지하기 어려워지면 기존 가정을 다시 봐야 합니다.
회사 코멘트가 여전히 긍정적인데 현금흐름이 따라오지 않으면 판단은 유지보다 약화에 가깝습니다.
```

## Bad Conditions

```text
마진이 60% 아래면 매도
세 조건 중 두 개가 깨지면 손절
6개월 안에 반등하지 않으면 종료
```

Those thresholds are allowed only when supplied by the user or explicitly supported by company materials.
