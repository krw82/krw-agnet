---
name: research_planner_skill
description: "Procedure for turning a user question into a compact, investor-useful evidence request (ResearchProposal v4). Covers metric identifiers, goal types, direct-answer and insight objectives, required/deferred separation, and recovery."
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

3. **Direct answer plus one decision-relevant insight.** First preserve every
fact, number, period, comparison, or explanation the user directly asked for.
Then include at most one closely connected insight objective when it can show
why that answer matters to an investor: the relevant business driver, exposure,
counter-signal, or condition that would change the reading. This is not a
generic company review, a valuation detour, a peer comparison, or a market
prediction. It must use the same company and the same decision question. Mark
direct-answer objectives as `required`. Mark the insight objective as
`required` when it is necessary to give a useful investor reading and as
`deferred` only when it is genuinely optional.

For an exact single-number question, do not append a broad company profile.
Choose at most one context objective tied to that number (for example, the
reported driver of the change or the most material counter-signal). For a
company-introduction question, use the dedicated overview procedure below.

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

## Company overview procedure

Treat short open questions such as `어떤 기업이야?`, `무슨 회사야?`, `이 회사
뭐 하는 곳이야?`, `투자 포인트는?`, or `회사 개요를 알려줘` as an
**investor company overview**, not as a request for a one-line industry label.
Use an intent ending in `_company_overview` and create a compact, company-specific
plan with **three required objectives**:

1. **Business and revenue model.** Retrieve the actual operating segments,
   products, customers, or services that explain what the company sells and
   how it earns money. Use wording from the company ontology orientation map,
   not a generic industry label.
2. **Current operating signal.** Retrieve one current, reportable signal that
   helps distinguish a static description from the company as it is now. Prefer
   a comparable revenue or operating-profit observation when the issuer has it;
   otherwise use a company-specific reported operating development. Do not
   invent a period or require a number that is not present.
3. **Investment transmission or counter-signal.** Retrieve one company-specific
   demand driver, exposure, business-cycle link, or risk mechanism that explains
   what could improve or weaken the operating signal. Use a filing phrase from
   the orientation map or from the company vocabulary; do not add a generic
   macro narrative.

These are a bounded first-pass plan, not a full initiation report. Do not add
valuation, target price, peer comparison, or every possible risk unless the
user asks. If one overview objective remains unavailable, keep the useful
supported answer and identify the concrete item an investor should watch; do
not turn the absence into a workflow failure.

## Concept explanations and short follow-ups

Treat questions such as `X가 뭐야?`, `쉽게 말하면 뭐야?`, `왜 필요한가?`,
`왜 중요한가?`, `그게 뭐야?`, and `그게 왜 필요해?` as a **concept
explanation**, not as a failed or underspecified research request. Resolve the
term in this order:

1. an explicitly named term in the current question;
2. one clearly identified concept in the most recent user/assistant research
   context; then
3. the trusted company's orientation map and admitted evidence.

If the term is a stable general concept (for example, a mine, turbine,
backlog, or working capital), the final answer may give a plain-language
general definition. Do not create a filing query merely to prove a dictionary
definition. When the concept is relevant to the trusted company, create one
compact qualitative objective only for the **company-specific connection**:
what the company sells, uses, faces, or reports in relation to that concept.
Use the company map's vocabulary and do not invent a company relationship.

For `why is it needed?`, plan for the function or economic role of the concept,
then the company-specific transmission to demand, cost, capacity, risk, or
cash generation where admitted evidence supports it. Do not turn a general
explanation into an unsupported claim about current commodity prices, policy,
market size, or a company event.

When a short follow-up has one clear antecedent, preserve it in the intent and
plan rather than asking the user to repeat it. When two or more antecedents
remain genuinely plausible, do not guess or fail the run: make the answer's
first sentence a compact conditional clarification (for example, `직전의
광산 장비를 말한 것이라면…`) and explain the most likely company-relevant
meaning. Do not spend a new broad company review on resolving one pronoun.

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
