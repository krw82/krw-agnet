---
name: thesis_change_policy
description: "분기 실적이 투자 가설에 미치는 영향 분류(강화/유지/혼재/약화/재검토 필요)와 필수 설명 항목"
when_to_use: "earnings_deep_dive에서 가설 효과를 판정할 때"
---

# Thesis Change Policy

## Status Labels

Use:

```text
강화:
the quarter provides new evidence that the core mechanism is working

유지:
the core mechanism remains intact without a decisive new proof point

혼재:
one important channel improves while another deteriorates or remains unproven

약화:
the quarter contradicts or delays a load-bearing part of the operating thesis

재검토 필요:
the original mechanism, financial capacity, or risk boundary materially changed
```

## Required Explanation

State:

```text
the load-bearing thesis mechanism
the new quarterly evidence
whether the effect is timing or structural
the strongest counter-signal
the next proof point
```

This is a quarter-update judgment, not a scenario tree. Explain how the latest
reported quarter changes the evidence base. Do not create upside/base/downside
cases unless the user explicitly asks for a conditional scenario.

## No Invented Prior Thesis

If the user did not provide an existing thesis:

```text
assess the filing-supported operating thesis
do not claim the user's conviction increased or decreased
do not infer position status, cost basis, or holding period
```

When deriving a filing-supported operating thesis, keep it narrow:

```text
which revenue, segment, margin, cash-flow, or capital-allocation mechanism the quarter is testing
what management says changed
what the representative numbers confirm or fail to confirm
what next quarterly evidence would change the status
```

## Capital Allocation

Treat M&A, buybacks, capex, debt, and strategic investments as thesis-relevant only when they change:

```text
growth capacity
cash-flow durability
financial flexibility
dilution
return on investment
risk absorption
```
