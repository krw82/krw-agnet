---
name: research_proposal_examples
description: "Use before the planner role chooses a goal type or splits mixed evidence needs. Five semantic ResearchProposal v4 examples."
when_to_use: "when deciding whether a question is numeric, explanatory, multi-objective, scenario, or wide research"
---

# ResearchProposal v4 examples

These are semantic examples for the Flash planner. They are not physical MCP
requests. Each example is a complete provider function argument.

## 1. One direct numeric trend

Question meaning: “Show the last three annual revenue values and trend.”

```json
{
  "proposal": {
    "intent": "revenue_trend_10k",
    "answer_scope": "direct",
    "uncertainty": "low",
    "document_types": ["10-K"],
    "periods": ["FY2022", "FY2023", "FY2024"],
    "objectives": [
      {
        "priority": "required",
        "alternatives": [
          { "terms": ["net sales"] },
          { "terms": ["revenue"] },
          { "terms": ["total net sales"] },
          { "terms": ["annual revenue"] }
        ],
        "directness": "direct_required",
        "object_types": ["MetricObservation"],
        "goal": {
          "kind": "metric_time_series",
          "metric": "revenue",
          "metric_dimensions": []
        }
      }
    ]
  }
}
```

Use `metric_time_series` for a sequence or trend. Do not invent a growth-rate
calculation merely because the user asks whether a trend rose or fell.

## 2. A requested year-over-year calculation

Question meaning: “What was annual revenue growth?”

```json
{
  "proposal": {
    "intent": "revenue_growth_10k",
    "answer_scope": "direct",
    "uncertainty": "low",
    "document_types": ["10-K"],
    "periods": ["FY2023", "FY2024"],
    "objectives": [
      {
        "priority": "required",
        "alternatives": [
          { "terms": ["net sales year over year"] },
          { "terms": ["revenue annual growth"] }
        ],
        "directness": "direct_required",
        "object_types": ["MetricObservation"],
        "goal": {
          "kind": "metric_change",
          "metric": "revenue",
          "metric_dimensions": [],
          "change": "growth_rate",
          "window": "year_over_year"
        }
      }
    ]
  }
}
```

Use `metric_change` only when the change itself is requested. The kernel
derives the pair of observations and calculation lineage.

## 3. One direct qualitative claim

Question meaning: “Does the filing directly link weaker customer demand to
revenue pressure?”

```json
{
  "proposal": {
    "intent": "customer_demand_revenue_pressure",
    "answer_scope": "direct",
    "uncertainty": "medium",
    "document_types": ["10-K", "10-Q"],
    "periods": ["FY2024"],
    "objectives": [
      {
        "priority": "required",
        "alternatives": [
          { "terms": ["customer demand revenue"] },
          { "terms": ["demand net sales"] }
        ],
        "directness": "direct_required",
        "object_types": ["BusinessFactor", "EvidenceQuote", "ResearchClaim"],
        "goal": {
          "kind": "qualitative_evidence",
          "concepts": ["customer demand", "revenue"],
          "predicates": ["pressures"]
        }
      }
    ]
  }
}
```

The goal asks for direct evidence. Related market commentary is not a
substitute for the requested causal link.

## 4. A number and its explanation must be separate objectives

Question meaning: “How did revenue change, and what explanation did the
company give?”

```json
{
  "proposal": {
    "intent": "revenue_change_and_company_explanation",
    "answer_scope": "direct",
    "uncertainty": "medium",
    "document_types": ["10-K"],
    "periods": ["FY2023", "FY2024"],
    "objectives": [
      {
        "priority": "required",
        "alternatives": [
          { "terms": ["net sales year over year"] },
          { "terms": ["revenue annual growth"] }
        ],
        "directness": "direct_required",
        "object_types": ["MetricObservation"],
        "goal": {
          "kind": "metric_change",
          "metric": "revenue",
          "metric_dimensions": [],
          "change": "growth_rate",
          "window": "year_over_year"
        }
      },
      {
        "priority": "required",
        "alternatives": [
          { "terms": ["revenue increase due to"] },
          { "terms": ["net sales primarily driven by"] }
        ],
        "directness": "direct_required",
        "object_types": ["BusinessFactor", "EvidenceQuote", "ResearchClaim"],
        "goal": {
          "kind": "qualitative_evidence",
          "concepts": ["revenue", "company explanation"],
          "predicates": ["driven by"]
        }
      }
    ]
  }
}
```

Never put a metric and qualitative concepts/predicates inside one objective.
They need different evidence and are independently verified.

Every multi-concept qualitative objective in these examples carries a
non-empty `predicates` entry — that is mandatory, not stylistic: the proposal
compiler rejects a qualitative objective whose `concepts` list has two or
more items while `predicates` is empty.

## 5. Preserve focus with a deferred extension

Question meaning: “Give the revenue trend; discuss geographic mix only if it
materially explains the result.”

```json
{
  "proposal": {
    "intent": "revenue_trend_with_optional_geography",
    "answer_scope": "direct",
    "uncertainty": "medium",
    "document_types": ["10-K"],
    "periods": ["FY2022", "FY2023", "FY2024"],
    "objectives": [
      {
        "priority": "required",
        "alternatives": [
          { "terms": ["net sales"] },
          { "terms": ["revenue"] }
        ],
        "directness": "direct_required",
        "object_types": ["MetricObservation"],
        "goal": {
          "kind": "metric_time_series",
          "metric": "revenue",
          "metric_dimensions": []
        }
      },
      {
        "priority": "deferred",
        "alternatives": [
          { "terms": ["geographic net sales"] },
          { "terms": ["geographic revenue"] }
        ],
        "directness": "direct_preferred",
        "object_types": ["MetricObservation"],
        "goal": {
          "kind": "metric_time_series",
          "metric": "segment_revenue",
          "metric_dimensions": ["geographic"]
        }
      }
    ]
  }
}
```

At least one objective must be `required`. A deferred objective must not turn
a narrow question into broad exploratory research before the evidence shows it
is material.

## 5. Earnings chain: revenue mix → cost → margin → cash

Question meaning: "How do revenue composition, costs, and margins connect?"

```json
{
  "proposal": {
    "intent": "earnings_margin_chain",
    "answer_scope": "direct",
    "uncertainty": "medium",
    "document_types": ["10-K", "10-Q"],
    "periods": ["FY2024", "FY2025"],
    "objectives": [
      {
        "priority": "required",
        "alternatives": [
          { "terms": ["net sales"] },
          { "terms": ["revenue"] },
          { "terms": ["total net sales"] },
          { "terms": ["annual revenue"] }
        ],
        "directness": "direct_required",
        "object_types": ["MetricObservation"],
        "goal": {
          "kind": "metric_observation",
          "metric": "revenue",
          "metric_dimensions": []
        }
      },
      {
        "priority": "required",
        "alternatives": [
          { "terms": ["gross margin"] },
          { "terms": ["gross profit"] }
        ],
        "directness": "direct_required",
        "object_types": ["MetricObservation"],
        "goal": {
          "kind": "metric_observation",
          "metric": "gross_margin",
          "metric_dimensions": []
        }
      },
      {
        "priority": "required",
        "alternatives": [
          { "terms": ["research and development"] },
          { "terms": ["operating expense"] },
          { "terms": ["R&D"] },
          { "terms": ["operating costs"] }
        ],
        "directness": "direct_required",
        "object_types": ["MetricObservation"],
        "goal": {
          "kind": "metric_observation",
          "metric": "research_and_development",
          "metric_dimensions": []
        }
      },
      {
        "priority": "deferred",
        "alternatives": [
          { "terms": ["iphone services products"] },
          { "terms": ["segment revenue"] },
          { "terms": ["product revenue"] }
        ],
        "directness": "any",
        "object_types": ["BusinessActivity", "MetricObservation", "Calculation"],
        "goal": {
          "kind": "qualitative_evidence",
          "concepts": ["product mix", "segment revenue"],
          "predicates": ["due to"]
        }
      }
    ]
  }
}
```

This example shows the earnings chain pattern: revenue and margin metrics are
required, R&D/operating costs are required for the cost layer, and segment
breakdown (BusinessActivity + MetricObservation/Calculation) is deferred but included so the
model can attempt product-mix decomposition. Both 10-K and 10-Q document types
are requested so quarterly and annual data are both available.

## Canonical metric identifiers

The `goal.metric` field in every metric goal (`metric_observation`,
`metric_time_series`, `metric_change`, `metric_difference`) MUST use one of
these exact canonical identifiers. Aliases are search terms only — never put
an alias in the `metric` field.

| Canonical ID | Display Name | Aliases (search terms only, NOT for `metric` field) |
|---|---|---|
| `revenue` | Revenue | total_revenue, net_sales, sales |
| `revenue_growth` | Revenue Growth | revenue_growth_rate, sales_growth, net_sales_growth |
| `segment_revenue` | Segment Revenue | segment_sales |
| `gross_margin` | Gross Margin | gross_profit_margin |
| `gross_profit` | Gross Profit | |
| `operating_margin` | Operating Margin | |
| `operating_income` | Operating Income | |
| `net_income` | Net Income | net_profit |
| `net_margin` | Net Margin | |
| `eps` | Earnings Per Share | earnings_per_share |
| `cost_of_revenue` | Cost of Revenue | cost_of_goods_sold, cogs |
| `operating_expense` | Operating Expense | total_operating_expenses, sg_and_a |
| `research_and_development` | Research and Development | rd_expense, research_development_expense |
| `selling_general_and_admin` | Selling, General and Administrative | sga, selling_general_administrative |
| `operating_cash_flow` | Operating Cash Flow | cash_from_operations |
| `capital_expenditures` | Capital Expenditures | capex, pp_and_e |
| `free_cash_flow` | Free Cash Flow | fcf |
| `fcf_margin` | FCF Margin | free_cash_flow_margin |
| `cash_and_equivalents` | Cash and Equivalents | cash, total_cash |
| `total_assets` | Total Assets | |
| `total_liabilities` | Total Liabilities | |
| `total_debt` | Total Debt | debt |
| `shareholders_equity` | Shareholders' Equity | total_equity, stockholders_equity |
| `roe` | Return on Equity | return_on_equity |
| `roa` | Return on Assets | return_on_assets |

### Common mistakes to avoid

The kernel rejects a proposal if ANY objective has a bad metric identifier.
The recovery feedback will name the offending field and value. Fix it on the
first retry — do not repeat the same mistake.

❌ **Using an alias as the metric identifier:**
```json
"metric": "sales"       // WRONG — "sales" is an alias
"metric": "net_profit"  // WRONG — "net_profit" is an alias
"metric": "fcf"         // WRONG — "fcf" is an alias
"metric": "capex"       // WRONG — "capex" is an alias
```

✅ **Using the canonical identifier:**
```json
"metric": "revenue"             // correct
"metric": "net_income"          // correct
"metric": "free_cash_flow"      // correct
"metric": "capital_expenditures" // correct
```

❌ **Inventing a metric not in the dictionary:**
```json
"metric": "ebitda"              // WRONG — not canonical
"metric": "enterprise_value"    // WRONG — not canonical
"metric": "dividend_yield"      // WRONG — not canonical
```

If the user asks about a metric that is not in the canonical list, use the
closest canonical metric with `qualitative_evidence` for the rest. For a
named product or geography breakdown, use `segment_revenue` with an empty
`metric_dimensions` array and place the member's literal filing phrase in its
own required objective. Only use `metric_dimensions` when a prior ontology
result has already supplied that issuer's exact member label; do not invent a
generic filter such as `"geographic"`.
