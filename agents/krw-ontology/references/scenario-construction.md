---
name: scenario_construction
description: "Use during scenario_sensitivity to build base/upside/downside cases from the same load-bearing variables. Different states of one assumption, not unrelated narratives."
when_to_use: "when the relevant analysis context arises"
---

# Scenario Construction

## Core Rule

Build scenarios from the same small set of load-bearing variables. Do not create unrelated narratives for upside, base, and downside.

Do not start by inventing scenario narratives. Start by identifying the
investment assumption already implied by the filing evidence, then test that
assumption under better, continuing, and worse evidence paths.

This workflow is narrower than general research. Use company evidence only to
answer the assumption stress-test question. Do not summarize the whole company,
the whole latest quarter, or every risk factor.

## Baseline

Anchor the baseline in:

```text
latest available 10-Q current drivers
latest 10-K annual mix and structural baseline
latest management commentary and filing notes
segment discussion, MD&A, commitments, risk factors, and liquidity disclosures
current reported revenue, margin, cash-flow, capex, debt, and liquidity direction
```

The base case is not "nothing changes." It is the continuation path most consistent with currently reported evidence.

## Assumption-First Research

Before writing any case, name one to three load-bearing assumptions:

```text
what the company evidence currently implies
which business activity carries the assumption
which financial channel must confirm it
what would make the assumption stronger
what would make it weaker or broken
```

Good assumptions:

```text
AI infrastructure demand must convert into reported revenue and cash flow.
Pricing recovery must outpace cost and inventory pressure.
Capex must create capacity without exhausting liquidity flexibility.
Backlog must convert on schedule rather than only extending the revenue tail.
```

Bad assumptions:

```text
The stock will go up if demand is good.
Management is strong.
The company is cheap.
AI is a long-term trend.
```

If the user did not name an explicit assumption, infer the narrowest assumption
from the user's stress-test wording:

```text
"수요 둔화되면?" -> demand conversion and margin/cash-flow resilience
"설비투자 늘면?" -> capex burden and operating proof
"가정이 깨지는 조건?" -> the current load-bearing business mechanism
"증자 필요해?" -> liquidity flexibility and financing dependence
```

## Case Design

Use:

```text
Base:
current evidence continues without a material new acceleration or deterioration

Upside:
one or more supported drivers improve and the benefit converts into financial results

Downside:
the key driver weakens, conversion is delayed, or the burden grows faster than the benefit
```

For each case identify:

```text
tested assumption
trigger
business mechanism
first affected financial channel
offsetting factor
observable evidence
investment interpretation
```

## Same-Variable Discipline

Good:

```text
Demand -> pricing/mix -> margin -> cash conversion
```

Bad:

```text
Upside from demand, base from regulation, downside from management turnover
```

If a new risk is important but does not belong to the same assumption set,
state it separately as an outside risk instead of forcing it into the scenario
table.

## Scenario Skew

Describe skew qualitatively unless supported numerical outputs exist:

```text
upside requires several conditions while downside needs only one failure
operating upside is plausible but cash-flow conversion is delayed
downside is limited by contracted revenue or liquidity
the balance sheet makes a delayed payoff more costly
```

Do not infer probabilities from evidence volume.
