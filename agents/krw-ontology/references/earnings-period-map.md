---
name: earnings_period_map
description: "Use during earnings_deep_dive to set comparison periods (current quarter, prior quarter, year-ago quarter, annual baseline) with safe reporting labels and comparability checks."
when_to_use: "when comparing the latest quarter with the prior quarter, year-ago quarter, and annual baseline"
---

# Earnings Period Map

## Default Comparison

Use:

```text
Current:
latest available reported quarter

Sequential:
immediately preceding quarter when useful and available

Year over year:
same quarter one year earlier

Annual baseline:
latest 10-K for business mix, structural cost base, capital allocation, and historical context
```

Lead with the newest available filing. A newer 10-Q is the current driver even when the latest 10-K contains more detail.

## Use safe reporting labels

Good:

```text
latest reported quarter (or its observed end date and basis)
immediately preceding reported quarter
matching prior-year quarter
latest 10-K annual baseline
```

Never expose a raw `CY...` routing code. Use an issuer fiscal label only when
the source explicitly provides it; otherwise use the observed date/basis or a
neutral filing label.

## Comparability Checks

Before comparing, confirm:

```text
same period length
same currency and unit
same consolidated or segment basis
same GAAP/non-GAAP basis
same reported or constant-currency basis
same company-defined metric
```

If comparability is weak, explain direction qualitatively.

## Inflection Timing

Check the annual period and prior quarters before calling the latest quarter the starting point.

Use:

```text
the change began earlier and became clearer this quarter
the latest quarter is the first reported evidence of the change
the quarter looks temporary against the annual baseline
```
