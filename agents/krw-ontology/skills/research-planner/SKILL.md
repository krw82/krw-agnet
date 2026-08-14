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

5. **Treat a requested change as a number request.** Korean wording such as
`최근 매출 변화`, `현금흐름 추이`, `증가/감소`, or `얼마나 바뀌었나` needs a
separate required metric objective with reported observations. It is not
satisfied by a narrative that only says "improved" or "weakened". Use
`metric_time_series` when the user wants the recent path; use
`metric_change` only when the requested output is an explicit delta or growth
rate. Words such as `추세`, `흐름`, `트렌드`, or `path` mean a
`metric_time_series` even when the metric is not prefixed by `최근`.
If the user says only `현금흐름`, prefer `operating_cash_flow`; add
`free_cash_flow` only when the user asks for FCF or capex-adjusted cash flow.
Keep valuation implications, causes, and risks as separate qualitative
objectives rather than letting them displace the named metric.

6. **Keep the question broad without duplicating the plan.** A single user
question may correctly require several objectives; do not collapse unrelated
metrics, mechanisms, and risks into one vague objective. But keep closely
related dimensions of the same reported metric together, omit duplicate
wording variants, and mark only genuinely optional expansion as `deferred`.
The executable plan has room for at most 12 clauses, so leave room for a
later precise follow-up instead of spending it on overlapping first-pass
clauses.

Before emitting the proposal, make a short coverage checklist of every
measurement or breakdown the user actually named. An explicit item such as
`현금흐름`, `FCF`, `capex`, `R&D`, `판관비`, `iPhone`, or `Services` must have
its own required objective when it is needed to answer the question. Do not
spend those slots on inferred sub-breakdowns: a generic request such as
`지역별`, `제품별`, or `세그먼트별` is one grouped objective, not an instruction
to enumerate every company region or product. Only split a breakdown into
separate objectives when the user themselves lists its members. If the plan
would approach 12 clauses, collapse generic geography/product enumeration or
redundant margin variants first; never silently drop a directly named metric.
Do not add a generic catch-all `segment_revenue` goal after named product goals
or a grouped geography goal already cover the same requested mix. Likewise,
one reported margin objective is normally enough for a generic `마진` request;
add gross and operating margin separately only when their comparison is itself
asked for. This leaves retrieval capacity for the user's named costs and cash
metrics.

7. **Do not guess a hard filing scope.** If the user says `최근`, `최신`,
`current`, or `latest` without naming a year, quarter, or filing, use
`"document_types": []` and `"periods": []`. The runtime will select the
latest confirmed filing and keep the annual filing only as context. Use an
exact period/document filter only when the user explicitly names it. Never
convert an issuer fiscal label into an ontology routing label yourself: use a
returned source label.

8. **Make search wording executable.** Each alternative is one real filing
phrase. Put wording variants in separate alternatives. Each objective allows
**1–6 alternatives**; prefer 1–3 genuinely distinct filing phrases and keep
only the six most useful ones. Put the phrase most likely to appear in the
issuer's filing first: the runtime uses that order as the deterministic
first-pass preference. `predicates` holds one relationship wording
when needed (for example `"due to"`), not a list of synonyms that must all
appear in one sentence. The trusted company ticker is passed separately as
scope, so do not put a ticker symbol into `terms` as filler: filing quotes and
tables often contain the company name but not its market symbol.

9. **Rank a risk from both exposure and mechanism.** When the user asks to
prioritize a named geographic, product, customer, regulatory, or demand risk,
do not search only for a generic downside sentence. Create a separate baseline
objective for the current reported exposure or direction when it can affect
the ranking (for example, `Greater China net sales`), then create the
qualitative downside-mechanism objective (for example, demand, regulation, or
concentration pressure). A baseline can use `predicates: []`; do not put a
catch-all phrase such as `could adversely` into the baseline. This lets the
answer distinguish a large/current exposure from a merely hypothetical risk.

10. **Preserve every explicitly named breakdown item.** When the user lists
    two or more products, product categories, revenue line items, segments,
    regions, or customers and asks for their mix, contribution, or comparison,
    make one required metric objective for each named item. Give each objective
    its own literal filing phrase (for example, `iPhone net sales`, `Services
    net sales`, `Mac net sales`). Do not put those items into alternatives of
    one objective: alternatives are interchangeable search phrasings and the
    kernel may select one of them. A generic segment, geography, or business
    unit is not a substitute for the named list. For example, a question about
    instruments versus consumables/reagents must search those two product
    categories separately; do not substitute Life Sciences, Diagnostics, or a
    regional revenue line. Use `segment_revenue` with `"metric_dimensions": []`
    for named breakdown objectives unless a prior ontology result supplied the
    issuer's exact member label: the runtime resolves issuer-specific labels
    such as `I Phone` or `Service` from the literal filing phrase. If the only
    disclosed comparison is a broader bucket such as non-instrumentation,
    preserve that boundary in the final answer rather than treating it as a
    consumables-only figure. Use this rule only for items the user actually
    names, so an ordinary company overview remains compact. A bare request for
    `지역별` or `제품별` is not such a list: use one grouped objective with the
    relevant filing phrase (for example, `net sales by reportable segment`)
    unless the user supplied the individual names.

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
        "alternatives": [{ "terms": ["search phrase 1"] }, { "terms": ["search phrase 2"] }],
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
| `document_types` | Explicit user-requested filings only; otherwise leave empty | `["10-K"]`, `[]` |
| `periods` | Explicit user-requested ontology periods only; otherwise leave empty | `["FY2024"]`, `[]` |
| `objectives` | Evidence requests (1–12 items); keep only decision-relevant ones | see below |
| `priority` | `required` (essential) or `deferred` (optional) | |
| `alternatives[].terms` | Literal filing-language search phrases; 1–6 alternatives per objective; omit the trusted ticker | `["net sales", "revenue"]` |
| `directness` | How direct the evidence must be | `any`, `direct_preferred`, `direct_required` |
| `object_types` | Required array of ontology filters; use `[]` when none is needed | `MetricObservation`, `EvidenceQuote`, `ResearchClaim`, `BusinessFactor`, or `[]` |

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
  "metric_dimensions": []
}
```

For a named comparison, use one required objective per named member as in
rule 10; do not use a generic `"segment"` dimension filter.

### 5. qualitative_evidence — narrative explanation

Use when the question asks "why?" or "what caused?".

```json
"goal": {
  "kind": "qualitative_evidence",
  "concepts": ["gross margin", "product mix"],
  "predicates": ["due to"]
}
```

- `concepts`: topics to find (1–4 items)
- `predicates`: one relationship wording when 2+ concepts need a causal link

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

5. **Set periods.** Use exact periods only when the user mentioned them. For
   a recent/current question, leave periods and document types empty so the
   runtime can use the latest confirmed filing. For an explicit year-over-year
   comparison, include both confirmed period labels.

6. **Write alternatives.** Use the actual filing language (aliases from the
metric table) as search terms, not canonical IDs. Each alternative is one
phrase; do not put synonym alternatives together in a predicate list. Keep
each objective to 1–6 alternatives, preferring the most distinct 1–3 phrases.

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
        "alternatives": [{ "terms": ["net sales"] }, { "terms": ["revenue"] }],
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
        "alternatives": [{ "terms": ["net income"] }, { "terms": ["net earnings"] }],
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
        "alternatives": [{ "terms": ["gross margin"] }, { "terms": ["gross profit"] }],
        "directness": "direct_required",
        "object_types": ["MetricObservation"],
        "goal": { "kind": "metric_time_series", "metric": "gross_margin", "metric_dimensions": [] }
      },
      {
        "priority": "required",
        "alternatives": [{ "terms": ["gross margin due to product mix"] }],
        "directness": "direct_required",
        "object_types": ["BusinessFactor", "EvidenceQuote", "ResearchClaim"],
        "goal": {
          "kind": "qualitative_evidence",
          "concepts": ["gross margin", "product mix"],
          "predicates": ["due to"]
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
        "alternatives": [{ "terms": ["net sales year over year"] }],
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
