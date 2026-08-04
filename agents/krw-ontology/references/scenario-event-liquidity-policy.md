---
name: scenario_event_liquidity_policy
description: "Use during scenario_sensitivity for event-path scenarios (contracts, projects, M&A, litigation, regulation) and capital/liquidity downside stress."
when_to_use: "when the relevant analysis context arises"
---

# Event And Liquidity Policy

## Event Paths

Use event paths for contracts, projects, products, acquisitions, litigation, regulation, and milestones.

An event path should test the investment assumption created by the event. Do
not summarize the event as news. Identify the business mechanism that must
convert after the event, then follow the financial path.

Default tree:

```text
success or on-time
delay or partial conversion
failure, cancellation, or adverse outcome
```

For each branch state:

```text
tested assumption
what has to happen
timing evidence
affected business activity
revenue/cost/cash-flow channel
balance-sheet consequence
next observable signpost
```

Do not assign probabilities unless the user supplies them or an explicit sourced basis exists.

## Liquidity Downside

Test the sequence:

```text
operating cash generation
-> capex and commitments
-> acquisition or strategic investment needs
-> debt maturity and refinancing
-> buyback/dividend flexibility
-> external financing or dilution risk
```

Start with the company's current cash-flow and liquidity evidence before
describing financing risk. A high-investment period is only a financing problem
when operating cash conversion, commitments, debt maturities, or capital
allocation pressure make flexibility visibly weaker.

Separate:

```text
liquidity pressure
solvency risk
temporary working-capital use
growth investment
shareholder distribution
```

Do not describe every high-capex period as a financing crisis.

Tie liquidity language to evidence:

```text
operating cash flow does or does not fund the investment path
commitments and lease obligations reduce future flexibility
debt maturities or refinancing windows create timing pressure
buybacks, dividends, M&A, or strategic investments compete with reinvestment
external financing becomes a conditional path, not an assumed outcome
```

## Dilution

Discuss dilution only when:

```text
equity issuance is part of the user scenario
filings disclose share issuance, convertibles, SBC pressure, or financing need
cash-flow and balance-sheet evidence make external equity a plausible conditional path
```

Use conditional wording unless issuance has been announced.
