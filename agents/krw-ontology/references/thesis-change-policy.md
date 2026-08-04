---
name: thesis_change_policy
description: "Use during earnings_deep_dive to classify the thesis effect (strengthened/intact/mixed/weakened/re-underwriting) and state the required explanation."
when_to_use: "when the relevant analysis context arises"
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
