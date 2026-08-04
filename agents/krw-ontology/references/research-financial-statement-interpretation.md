---
name: research-financial-statement-interpretation
description: "재무제표 해석 (P&L/FCF/investing/financing 층 분리, R&D 중복 금지)"
when_to_use: "company_research 리서치에서 필요할 때"
---

# Financial Statement Interpretation

Use this reference to prevent accounting-layer mistakes in investor-facing synthesis.

## Separate the layers

Do not place P&L expenses, FCF calculation items, investing cash flows, and financing cash flows in the same bucket.

```text
P&L operating expense:
R&D, S&M, G&A, SBC by function when disclosed

OCF / FCF calculation:
OCF, capex, company-defined FCF adjustments, legal settlement adjustments, business-combination-related FCF addbacks

Investing cash flow:
capex, business combinations net of cash acquired, strategic investments, marketable securities

Financing cash flow:
share repurchases, debt issuance/repayment, dividends when applicable
```

## R&D and FCF

R&D is an operating expense. It is already reflected before OCF and FCF. Do not add R&D again as a post-FCF use of cash.

Better wording:

```text
The company generates high OCF/FCF after expensing a large R&D program, then allocates cash to capex, M&A, strategic investments, and buybacks.
```

Avoid:

```text
FCF uses include R&D plus capex plus M&A plus buybacks.
```

## M&A cash spending

For actual acquisition cash outflow, use cash-flow statement lines such as:

```text
business combinations, net of cash acquired
acquisitions, net of cash acquired
purchase of businesses, net of cash acquired
```

Do not treat company FCF addbacks such as `business combination and other related costs` as the acquisition purchase price. Those items are usually deal-related expenses, integration costs, compensation-related costs, or other adjustments, not the total M&A cash consideration.

## Company-defined FCF vs simple FCF

Distinguish:

```text
simple FCF = OCF - capex
company-defined FCF = company non-GAAP definition, which may add back or adjust selected items
```

If both are relevant, say which one is being used. Do not silently mix them across periods.

## Share repurchases

Share repurchases can be shareholder return, dilution management, or both.

When filings describe buybacks as offsetting stock-based compensation dilution, do not frame the full amount as pure shareholder return. Write the interpretation directly:

```text
The buyback supports capital return, but part of the program appears aimed at managing SBC dilution.
```

## Inflection timing

When judging a capital-allocation or cost-structure inflection, compare annual periods and the latest quarter together.

```text
If CY2025 already shows a step-up in M&A or buybacks, do not call CY2026Q1 the starting point merely because the latest quarter is large.
Say the transition began in CY2025 and became clearer or stronger in CY2026Q1.
```

## AI, platform, and product attribution

Do not attribute revenue growth, cost pressure, margin movement, capex, M&A, or FCF change to AI/platform/product expansion unless filings explicitly support the link.

If the link is inferential, label it as an inference:

```text
This is consistent with AI/platform investment pressure, but the filing does not isolate AI infrastructure cost as a separate line item.
```

## Output style

Use exact numbers when they are needed to correct or anchor the conclusion. Otherwise translate the financial statement mechanics into investor meaning.
