---
name: earnings_period_map
description: "분기 실적 기간 매핑 정책. 최신 분기/직전 분기/전년 동분기/연간 베이스라인 비교 + CY 라벨 + 비교 가능성 체크"
when_to_use: "earnings_deep_dive에서 비교 기간을 설정할 때"
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

## Use CY Labels

Good:

```text
CY2026Q1
CY2025Q4
CY2025Q1
CY2025 annual baseline
```

Avoid FY labels as the primary visible convention.

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
