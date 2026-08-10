---
name: research_planner_skill
description: "Procedure for turning a user question into a minimal evidence request (ResearchProposal v4). Covers metric identifiers, goal types, required/deferred separation, and recovery."
when_to_use: "when the advertised role is planner or a corrected research decision is needed"
---

# KRW Research Planner

Use this skill only while the advertised role is a planner or the kernel asks
for a corrected research decision. It tells you how to turn a user's question
into a compact, executable evidence request without inventing physical search
details.

## Core rules (must follow)

1. **Canonical metric IDs only in the `metric` field.** Use the exact
   identifiers from the ontology catalog. Aliases (`sales`, `net_profit`,
   `fcf`, `capex`) are search terms for `alternatives[].terms` only — never
   put them in `goal.metric`.

2. **Numbers and explanations are separate objectives.** Never mix a metric
   goal and a qualitative goal in the same objective.

3. **Minimum objectives.** Include only what is needed to answer the question.
   Mark only answer-critical objectives as `required`; mark useful expansion
   as `deferred`.

4. **When rewriting a rejected proposal**, do not change the goal definition
   (metric + kind + dimensions) of an existing objective. If you must change
   the metric or kind, add a new objective instead.

## ResearchProposal v4 structure

```json
{
  "proposal": {
    "intent": "stable_lowercase_slug",
    "answer_scope": "direct",
    "uncertainty": "low",
    "document_types": ["10-K"],
    "periods": ["FY2024"],
    "objectives": [
      {
        "priority": "required",
        "alternatives": [{ "terms": ["search phrase 1", "search phrase 2"] }],
        "directness": "direct_required",
        "object_types": ["MetricObservation"],
        "goal": { "kind": "...", ... }
      }
    ]
  }
}
```

### Field reference

| Field | Description | Example |
|---|---|---|
| `intent` | A stable lowercase slug summarising the request | `"aapl_revenue_trend"` |
| `answer_scope` | Answer breadth, usually `direct` | `"direct"` |
| `uncertainty` | How uncertain the question is | `"low"` (single number), `"medium"` (analysis) |
| `document_types` | Which filings to search | `["10-K"]`, `["10-K", "10-Q"]` |
| `periods` | Which fiscal periods | `["FY2024"]`, `["FY2023", "FY2024"]` |
| `objectives` | Evidence requests (1–12 items); keep only decision-relevant ones | see below |
| `priority` | `required` (essential) or `deferred` (optional) | |
| `alternatives[].terms` | Literal filing-language search phrases | `["net sales", "revenue"]` |
| `directness` | How direct the evidence must be | `any`, `direct_preferred`, `direct_required` |
| `object_types` | Required array of ontology filters; use `[]` when none is needed | `MetricObservation`, `NarrativeEvidence`, or `[]` |

## Goal types (5 kinds)

### 1. metric_observation — a single number at one point in time

Use when the question asks "how much is X?".

```json
"goal": {
  "kind": "metric_observation",
  "metric": "revenue",
  "metric_dimensions": []
}
```

### 2. metric_time_series — numbers across multiple periods

Use when the question asks for a trend or multi-year comparison.

```json
"goal": {
  "kind": "metric_time_series",
  "metric": "gross_margin",
  "metric_dimensions": []
}
```

### 3. metric_change — a change or growth rate

Use when the question asks "how much did X grow/change?".

```json
"goal": {
  "kind": "metric_change",
  "metric": "revenue",
  "metric_dimensions": [],
  "change": "growth_rate",
  "window": "year_over_year"
}
```

- `change`: `"growth_rate"` (percentage) or `"absolute_change"` (raw delta)
- `window`: `"year_over_year"` or `"period_over_period"`

### 4. metric_difference — a comparison between segments

Use when the question asks "how does segment A compare to segment B?".

```json
"goal": {
  "kind": "metric_difference",
  "metric": "segment_revenue",
  "metric_dimensions": ["segment"]
}
```

### 5. qualitative_evidence — narrative explanation

Use when the question asks "why?" or "what caused?".

```json
"goal": {
  "kind": "qualitative_evidence",
  "concepts": ["gross margin", "product mix"],
  "predicates": ["driven by", "due to"]
}
```

- `concepts`: topics to find (1–4 items)
- `predicates`: relationships between concepts (1–3 items, required when 2+ concepts)

## Canonical metric identifiers

The `goal.metric` field MUST use one of these exact identifiers:

| Canonical ID | Display Name | Aliases (search terms only, NOT for `metric` field) |
|---|---|---|
| `revenue` | Revenue | total_revenue, net_sales, sales |
| `revenue_growth` | Revenue Growth | revenue_growth_rate, sales_growth |
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
| `research_and_development` | Research and Development | rd_expense |
| `selling_general_and_admin` | Selling, General and Administrative | sga |
| `operating_cash_flow` | Operating Cash Flow | cash_from_operations |
| `capital_expenditures` | Capital Expenditures | capex |
| `free_cash_flow` | Free Cash Flow | fcf |
| `fcf_margin` | FCF Margin | free_cash_flow_margin |
| `cash_and_equivalents` | Cash and Equivalents | cash, total_cash |
| `total_assets` | Total Assets | |
| `total_liabilities` | Total Liabilities | |
| `total_debt` | Total Debt | debt |
| `shareholders_equity` | Shareholders' Equity | total_equity |
| `roe` | Return on Equity | return_on_equity |
| `roa` | Return on Assets | return_on_assets |

### Common mistakes to avoid

The kernel rejects a proposal if ANY objective has a bad metric identifier.
The recovery feedback will name the offending field and value. Fix it on the
first retry — do not repeat the same mistake.

```
❌  "metric": "sales"             — alias, use "revenue"
❌  "metric": "net_profit"        — alias, use "net_income"
❌  "metric": "fcf"               — alias, use "free_cash_flow"
❌  "metric": "capex"             — alias, use "capital_expenditures"
❌  "metric": "gross_profit_margin" — alias, use "gross_margin"
❌  "metric": "ebitda"            — not in dictionary, use closest canonical metric
```

If the user asks about a metric not in the canonical list, use the closest
canonical metric and add `metric_dimensions` to narrow it, or use
`qualitative_evidence` for the unmeasurable part.

## Authoring procedure

1. **Analyse the question.** What does the user want — a number, a trend, a
   comparison, or an explanation?

2. **Select the metric.** Find the closest canonical metric from the table
   above. If the question says "sales", the metric is `revenue`. If it says
   "profit", disambiguate: `net_income` (bottom line), `operating_income`
   (core business), or `gross_profit` (after COGS).

3. **Choose the goal kind.**
   - "How much?" → `metric_observation`
   - "What is the trend?" → `metric_time_series`
   - "How much did it grow?" → `metric_change`
   - "How does A compare to B?" → `metric_difference`
   - "Why?" / "What caused?" → `qualitative_evidence`

4. **Split objectives.** If the question needs both numbers and explanations,
   create separate objectives. Example: "gross margin trend and its drivers"
   needs one `metric_time_series` objective (numbers) and one
   `qualitative_evidence` objective (causes).

5. **Set periods.** Use the periods mentioned in the question. For year-over-
   year comparisons, include both years.

6. **Write alternatives.** Use the actual filing language (aliases from the
   metric table) as search terms, not canonical IDs.

## Worked examples

### Example 1: single number

Question: "Apple FY2024 revenue?"

```json
{
  "proposal": {
    "intent": "aapl_fy2024_revenue",
    "answer_scope": "direct",
    "uncertainty": "low",
    "document_types": ["10-K"],
    "periods": ["FY2024"],
    "objectives": [
      {
        "priority": "required",
        "alternatives": [{ "terms": ["net sales", "revenue"] }],
        "directness": "direct_required",
        "object_types": ["MetricObservation"],
        "goal": { "kind": "metric_observation", "metric": "revenue", "metric_dimensions": [] }
      }
    ]
  }
}
```

### Example 2: two-year comparison

Question: "Apple FY2024 net income vs prior year?"

```json
{
  "proposal": {
    "intent": "aapl_net_income_yoy",
    "answer_scope": "direct",
    "uncertainty": "low",
    "document_types": ["10-K"],
    "periods": ["FY2023", "FY2024"],
    "objectives": [
      {
        "priority": "required",
        "alternatives": [{ "terms": ["net income", "net earnings"] }],
        "directness": "direct_required",
        "object_types": ["MetricObservation"],
        "goal": { "kind": "metric_time_series", "metric": "net_income", "metric_dimensions": [] }
      }
    ]
  }
}
```

### Example 3: numbers + explanation (two objectives)

Question: "Gross margin trend and what drove the change?"

```json
{
  "proposal": {
    "intent": "aapl_gross_margin_trend_and_drivers",
    "answer_scope": "direct",
    "uncertainty": "medium",
    "document_types": ["10-K", "10-Q"],
    "periods": ["FY2023", "FY2024"],
    "objectives": [
      {
        "priority": "required",
        "alternatives": [{ "terms": ["gross margin", "gross profit"] }],
        "directness": "direct_required",
        "object_types": ["MetricObservation"],
        "goal": { "kind": "metric_time_series", "metric": "gross_margin", "metric_dimensions": [] }
      },
      {
        "priority": "required",
        "alternatives": [{ "terms": ["gross margin", "due to", "product mix"] }],
        "directness": "direct_required",
        "object_types": ["NarrativeEvidence"],
        "goal": {
          "kind": "qualitative_evidence",
          "concepts": ["gross margin", "product mix"],
          "predicates": ["driven by", "due to"]
        }
      }
    ]
  }
}
```

### Example 4: growth rate

Question: "Revenue growth rate?"

```json
{
  "proposal": {
    "intent": "aapl_revenue_growth",
    "answer_scope": "direct",
    "uncertainty": "low",
    "document_types": ["10-K"],
    "periods": ["FY2023", "FY2024"],
    "objectives": [
      {
        "priority": "required",
        "alternatives": [{ "terms": ["net sales", "year over year"] }],
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

## Recovery guidance

When your proposal is rejected and you receive a recovery envelope:

1. **Read the `detail` field.** It tells you:
   - `detail.field`: the JSON pointer to the exact error location
   - `detail.offending_value`: the value that was wrong
   - `detail.valid_alternatives`: what you may use instead
   - `detail.hint`: a short instruction

2. **Fix only that field.** Do not change other objectives or fields.

3. **Avoid goal definition drift.** When rewriting a proposal after recovery,
   keep the goal definition (metric + kind + dimensions) of each existing
   objective unchanged. If you must change the metric or kind, add a new
   objective rather than modifying an existing one — otherwise the planner
   detects a definition change on the same goal ID and rejects it.

4. **Do not repeat the same mistake.** If the feedback is
   `proposal_metric_identity_invalid`, replace the offending metric with one
   of the `valid_alternatives`. Do not use another alias.
