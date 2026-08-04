---
name: earnings_analysis
description: "Use when the user asks for a filing-grounded deep analysis of a covered company's latest reported quarter, including what changed, management commentary and note explanations, quarter-over-quarter and year-over-year direction, earnings quality, margin and cash-flow conversion, capital allocation, and whether the investment thesis strengthened or weakened. Includes the 5-stage causal chain (revenue composition, geographic drivers, cost trend, margin transmission, cash generation)."
when_to_use: "quarterly or annual earnings deep dive, margin quality, segment revenue, cash-flow conversion, thesis update"
---

# Earnings Deep Dive

## 1. Purpose

Analyze the latest reported quarter as new evidence that updates the
filing-supported operating thesis.

This is not a general company research pass and not an upside/base/downside
scenario stress test. The unit of analysis is the latest reported quarter:
what changed, why it changed, whether the explanation matches the numbers,
and how that evidence updates the investment view.

```text
latest quarter
-> operating thesis being updated
-> what changed this quarter
-> why management says it changed
-> whether numbers, commentary, and notes agree
-> whether the change is timing noise or thesis-relevant evidence
```

This skill is not a raw earnings table and not a market-reaction report.

Default visible output:

```text
Korean Markdown only.
```

## 2. Shared Research Contract

The planner, evidence-analyst, research-synthesis, and earnings-specific
policies are part of the immutable agent image. Follow them alongside this
skill:

```text
ResearchProposal v4 (not SearchPlan) is the model-authored planning ABI.
The kernel compiles it into the physical SearchPlan and MCP request.
Use the ontology catalog for canonical metric identifiers.
Read management discussion and explanatory notes before expanding metrics.
Apply the analyst estimation discipline when evidence is partial.
Write plain Korean investor language.
```

Do not load or apply:

```text
the buy/sell/hold report template
target-price logic
current market-price or consensus logic
```

## 3. Request Boundary

Use this skill for:

```text
이번 분기 실적을 자세히 분석해줘.
최신 실적에서 실제로 달라진 게 뭐야?
매출은 좋은데 현금흐름 품질은 어때?
이번 분기로 투자 가설이 강화됐어?
회사 코멘트와 숫자가 일치해?
마진 개선이 구조적인지 봐줘.
```

Do not use this skill for:

```text
pre-earnings preview
current-news or article discovery
consensus beat/miss without supplied consensus
options-implied move
stock-price reaction
full transcript Q&A when transcript evidence is unavailable
ordinary company overview
final target price, fair value, or definitive recommendation
```

If a current event after the filing is central, use news research. If the
user supplies external earnings-release, transcript, or estimate material,
treat it as an additional labeled input rather than ontology-confirmed fact.

## 4. Internal Earnings Brief

Before ontology calls, rewrite the request as a concise internal English
earnings brief.

Preserve:

```text
ticker and company
latest available reported quarter
requested metric, segment, product, or thesis
comparison period if supplied
growth, margin, cash-flow, cost, capex, debt, or capital-allocation focus
```

Add evidence axes:

```text
latest management discussion and filing notes
quarter-over-quarter direction
year-over-year direction
latest annual business baseline
company-specific operating drivers
earnings-quality and accounting effects
cash-flow conversion and investment burden
confirming and disconfirming thesis evidence
```

The brief must state the quarter update question in one sentence:

```text
This quarter tests whether <operating mechanism> is strengthening, intact, mixed, weakening, or needs re-underwriting.
```

Do not turn the brief into a broad company overview or a scenario stress-test.
Do not expose the internal English brief.

## 5. Period Map

The system retrieves both quarterly and annual filings automatically. Read
`references/earnings-period-map.md` for the full policy.

Default comparison:

```text
current quarter (latest 10-Q, the current driver)
prior quarter when available
same quarter one year earlier
latest annual 10-K baseline
```

Use CY labels in visible answers. Do not compare incompatible periods, scopes,
units, or accounting bases without explanation.

## 6. Default Workflow

Follow this order:

```text
1. Build the internal English earnings brief, then author one complete ResearchProposal v4 before any filing call. The kernel compiles it into the physical SearchPlan and MCP request.
2. Split headline metrics, guidance/expectations, operating drivers, margin/cash-flow paths, and risks into independently verifiable objectives. Keep qualitative relations in qualitative_evidence goals and exact metrics in metric goals.
3. Mark only answer-critical objectives as required; mark plausible but nonessential expansion as deferred.
4. Read the returned ResearchState: source_anchors, clause_coverage, evidence_units, computed_values, missing_parts, and recommended_actions.
5. Confirm the newest available 10-Q or latest 10-K when no newer quarter exists.
6. Define the operating thesis being updated. If the user supplied a thesis, use it; otherwise derive the narrow filing-supported operating thesis from the latest quarter evidence.
7. Read management discussion, explanatory notes, segment commentary, and business descriptions before following any missing metric.
8. Identify the material quarterly changes needed to update the thesis, not every reported line and not a fixed count.
9. Use representative metrics to verify direction, magnitude, and period alignment.
10. Reconcile company commentary with reported numbers and notes.
11. Decide whether each change is timing noise, accounting effect, operating proof, or thesis-relevant deterioration.
12. Check earnings quality, cash-flow conversion, capex, acquisitions, buybacks, debt, and dilution only when material to the quarter update.
13. Classify the thesis effect: strengthened, intact, mixed, weakened, or requires re-underwriting.
14. Follow only material missing_parts/recommended_actions and use selected trace/chain for load-bearing claims; do not impose a fixed follow-up count.
15. Write the Korean earnings update report.
```

## 7. Causal Chain Analysis

When the question turns on how revenue, cost, margin, and cash flow connect,
build the analysis as a five-layer causal chain. Each layer's numbers should
be traced to their source document (quarterly 10-Q vs annual 10-K) and the
chain should explain *why* a number moved, not just that it moved.

1. **Revenue composition**: Break total revenue into product/service segments
   when available (look for BusinessActivity evidence — iPhone Sales,
   Services Sales, etc.). Identify which segment drove growth and whether
   the mix is shifting toward higher- or lower-margin business.

2. **Geographic or segment growth drivers**: Identify which regions or
   segments grew fastest and what the filing says drove that growth. Quote
   the filing's own explanation (e.g., "price decreases", "product mix",
   "demand recovery").

3. **Cost trend**: Compare R&D and SG&A growth rates against revenue growth.
   If costs are growing faster than revenue, flag the margin pressure. If
   the filing explains the cost increase (e.g., "infrastructure-related
   costs"), quote it.

4. **Margin transmission**: Connect segment mix → gross margin → operating
   margin. Explain how the revenue mix shift (layer 1) flows through gross
   margin to operating margin, and where cost growth (layer 3) partially
   offsets the improvement.

5. **Cash generation**: Connect operating income → working capital changes
   → operating cash flow → capital expenditure → free cash flow. State
   whether earnings growth translated into cash growth, and whether capital
   return (buybacks, dividends) is accelerating.

Reconcile commentary with representative reported numbers across all five
layers. Separate core operations from accounting effects, working-capital
timing, investment burden, acquisitions, financing, and capital allocation.

## 8. Commentary And Notes Priority

Numbers show that something changed. Commentary and notes explain why.

Prioritize:

```text
MD&A and management discussion
segment notes
revenue-recognition and backlog/RPO notes
cost and margin explanation
inventory and working-capital notes
SBC, restructuring, tax, impairment, or litigation notes
business-combination and acquisition notes
debt, liquidity, commitment, and capital-allocation notes
specific risk updates that explain a current channel
```

Do not rank importance by the number of metrics or quotes retrieved.

## 9. Earnings Quality

Read `references/earnings-quality-policy.md`.

Separate:

```text
core operating change
accounting or below-the-line effect
working-capital timing
investment and capex burden
M&A cash use
financing and capital allocation
```

Do not treat headline EPS, net income, or company-defined FCF as clean
recurring performance without checking the relevant explanation.

## 10. Thesis Change

Read `references/thesis-change-policy.md`.

Classify:

```text
강화
유지
혼재
약화
재검토 필요
```

The classification must identify:

```text
which prior mechanism changed
what filing evidence supports the change
whether the change is current-quarter timing or structural
what next evidence would confirm or reverse the judgment
```

Do not infer the user's original thesis if none was supplied. In that case
assess the filing-supported operating thesis.

Do not build upside/base/downside cases unless the user explicitly asks for
a conditional scenario. For scenario stress tests, use the
scenario-sensitivity workflow instead.

## 11. Unsupported Market Inputs

Without user-provided or connected external evidence, do not claim:

```text
the company beat or missed consensus
the stock should rise or fall
the market already priced the quarter
estimate revisions will occur
options imply a specific move
current valuation is cheap or expensive
institutional positioning changed
```

Give a filing-supported earnings interpretation instead.

## 12. Stop Rules

Stop when:

```text
the latest period and comparison periods are correctly mapped
three to five material changes are supported
commentary, notes, and representative metrics explain the changes
earnings quality and cash conversion are addressed where material
the thesis effect and next proof points are clear
remaining gaps require transcript, consensus, price, or unsupported external data
```

## 13. Final Answer

Follow `references/output-contract.md`.

The answer should:

```text
start with one practical earnings judgment
explain what changed before listing numbers
prioritize management commentary and notes
connect the quarter to growth, margin, cash flow, and investment meaning
state the thesis effect and next-quarter proof points
end with exactly three immediately reusable same-company prompts under "이어서 볼 질문"
```

Keep internal tool, schema, object, pack, index, and routing language out of
the answer.
