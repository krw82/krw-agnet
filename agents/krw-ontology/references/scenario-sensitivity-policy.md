---
name: scenario_sensitivity_policy
description: "1-3개 바인딩 변수 선정과 민감도 분석 규칙. 정성적 vs 정량적 민감도 사용 기준"
when_to_use: "scenario_sensitivity에서 바인딩 변수와 민감도를 분석할 때"
---

# Sensitivity Policy

## Select The Binding Variables

Choose one to three variables that can materially change the conclusion:

```text
demand or volume
pricing or mix
gross or operating margin
cost inflation
capex and investment intensity
working capital
contract or backlog conversion
debt cost or refinancing
share issuance or dilution
project timing
```

Select variables from company evidence first, not from generic valuation
templates. Prefer variables that management commentary, filing notes, segment
discussion, risk factors, commitments, or cash-flow disclosures already make
important.

Do not produce a broad matrix merely because many metrics are available.

The useful sensitivity question is:

```text
If this assumption changes, which business mechanism moves first, and which
financial channel proves or disproves it?
```

## Qualitative Default

Without explicit numerical assumptions, use an interpretation table. The
default should be qualitative because most user questions are about judgment,
not model precision:

```text
| 변수 | 좋아질 때 | 나빠질 때 | 투자적으로 중요한 이유 |
```

Use directional language. Do not fabricate percentage bands.

Preferred qualitative paths:

```text
demand converts / demand stalls
pricing or mix absorbs cost pressure / margin fails to absorb it
cash flow catches up with growth / cash conversion remains delayed
capex creates operating proof / capex burden rises without proof
liquidity stays flexible / financing dependence rises
management commentary confirms the path / commentary conflicts with reported results
```

## Numerical Sensitivity

Numerical sensitivity is allowed only from:

```text
user-provided assumptions
explicit company guidance
reported contractual ranges
direct arithmetic from sourced values
```

Show:

```text
input source
formula or relationship
result
what the result does and does not prove
```

Never imply a complete valuation model when only one operating sensitivity was calculated.
Do not back-solve target prices, fair values, or required stock returns from a
single filing sensitivity.

## Comparability

Keep:

```text
same period basis
same currency and unit
same GAAP/non-GAAP basis
same company-defined metric definition
```

If those conditions are not met, use directional interpretation instead of false precision.
