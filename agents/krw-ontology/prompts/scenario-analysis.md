---
name: scenario_analysis
description: "Use when the user asks to stress-test filing-grounded investment assumptions through upside/base/downside scenarios, operating-driver sensitivity, macro or business-factor sensitivity, liquidity or dilution downside, event-path consequences, breakpoints, thesis-break conditions, or evidence-based action thresholds for a covered public company."
when_to_use: "scenario analysis, sensitivity, downside stress, thesis-break conditions, action thresholds"
---

# Scenario And Sensitivity

## 1. Purpose

Stress-test a specific investment assumption for a covered company.

This is not a general company research pass and not a latest-quarter earnings
update. Use only enough filing context to define the current assumption, then
test how that assumption strengthens, holds, weakens, or breaks.

Do not begin by inventing scenario narratives. Begin by stating the assumption
being tested.

The research flow is:

```text
user scenario or thesis-break question
-> current assumption being tested
-> narrow filing evidence that supports or pressures that assumption
-> one to three binding variables
-> business and financial transmission path
-> evidence that strengthens, maintains, weakens, or breaks the assumption
```

This skill analyzes scenario mechanics. It does not invent a valuation model,
market probability, target price, position size, stop-loss level, or trade
instruction.

Default visible output:

```text
Korean Markdown only.
```

## 2. Shared Research Contract

The planner, evidence-analyst, research-synthesis, and scenario-specific
policies are part of the immutable agent image. Follow them alongside this
skill. ResearchProposal v4 (not SearchPlan) is the model-authored planning
ABI; the kernel compiles it into the physical SearchPlan and MCP request.

Do not load or apply the buy/sell/hold report template, target-price logic,
or personalized portfolio or position-sizing logic.

## 3. Request Boundary

Use this skill for:

```text
상승·기준·하락 시나리오를 만들어줘.
수요가 둔화되면 현금흐름이 어떻게 달라져?
설비투자 확대에 얼마나 민감해?
어떤 조건에서 투자 가설이 깨져?
증자나 추가 차입이 필요해지는 하방 경로를 봐줘.
이 계약이 지연되거나 취소되면 어떤 영향이 생겨?
```

Do not use this skill for:

```text
ordinary company overview
simple latest-quarter summary
generic risk-factor explanation without a scenario request
current-news discovery or article selection
candidate screening across companies
final target price or fair value
definitive buy/sell/hold advice
portfolio sizing, hedging, borrow, options, or execution
```

## 4. Internal Investment-Assumption Brief

Before calling ontology tools, rewrite the request as a concise internal
English investment-assumption brief.

Preserve:

```text
ticker and company
latest reported period
scenario variable, thesis variable, or event
direction of change
time horizon if supplied
financial channels
named user assumptions
requested downside or thesis-break focus
```

Add evidence axes:

```text
latest management commentary, MD&A, and segment discussion
filing notes, commitments, risk factors, and liquidity disclosures
current business and financial baseline
direct exposure and transmission mechanism
revenue, margin, working-capital, cash-flow, capex, debt, and liquidity path
offsetting factors
first observable confirmers and falsifiers
```

The brief must state the investment assumption being tested in one sentence:

```text
The assumption being tested is that <driver/event> can or cannot convert into <business/financial outcome>.
```

Do not turn the brief into a company overview, latest-quarter recap, or broad
risk memo. Do not expose the internal English brief.

## 5. Scenario Modes

Classify the request before research:

```text
A. Operating-driver scenario
   demand, price, volume, mix, capacity, customer, backlog, or cost

B. External-factor sensitivity
   commodity, FX, rates, regulation, geography, supply chain, or macro demand

C. Capital and liquidity downside
   capex burden, cash burn, refinancing, debt, dilution, buybacks, or M&A

D. Event-path scenario
   contract, project, product, acquisition, litigation, regulatory, or milestone path

E. User-assumption sensitivity
   user-provided growth, margin, capex, financing, timing, or valuation inputs
```

Read only the references needed for the selected mode:

```text
references/scenario-construction.md
references/scenario-sensitivity-policy.md
references/scenario-event-liquidity-policy.md
references/scenario-action-threshold-policy.md
references/scenario-output-contract.md
```

## 6. Default Workflow

Follow this order:

```text
1. Build the internal English investment-assumption brief, then author one complete ResearchProposal v4 before any filing call. The kernel compiles it into the physical SearchPlan and MCP request.
2. Classify the request: downside, upside, thesis-break, sensitivity, event path, or liquidity stress.
3. Confirm the covered company and newest available filing boundary without doing a broad company overview.
4. Put the assumption, transmission mechanism, financial channel, falsifier, and any exact metrics in atomic objectives. Use qualitative_evidence goals for relations and metric goals for arithmetic.
5. Read the returned ResearchState: clause_coverage, evidence_units, computed_values, missing_parts, and recommended_actions.
6. Define the current tested assumption in one sentence.
7. Collect only evidence relevant to that assumption: management commentary, notes, segment discussion, risk factors, commitments, liquidity, capex, debt, and cash-flow path.
8. Identify the binding variables supported by the evidence; do not impose a fixed count or model every possible input.
9. Map the assumption through company evidence -> business mechanism -> revenue/margin/working-capital/cash-flow/liquidity path -> investor meaning.
10. Separate reported facts, filing-derived assumptions, user assumptions, and analyst conditional inference.
11. Build base, upside, and downside cases only as different states of the same tested assumption.
12. Define observable evidence that strengthens, maintains, weakens, or breaks the assumption.
13. Follow only material missing_parts/recommended_actions; do not impose a fixed follow-up count.
14. Use selected trace/chain only when a strong mechanism, amount, date, term, or direct-exposure claim needs support.
15. Write the Korean assumption stress-test report.
```

## 7. Assumption Provenance

Every material scenario input must belong to one category:

```text
Reported fact:
explicit filing number, term, date, guidance, or management statement

Filing-derived assumption:
conditional interpretation supported by filing evidence

User assumption:
number, probability, time horizon, price, or condition supplied by the user

Analyst conditional inference:
directional scenario logic not explicitly stated by the company
```

Never present an analyst assumption as company guidance or filing fact.

## 8. Numeric And Probability Policy

When the user does not provide numerical assumptions, use qualitative
sensitivity:

```text
strengthens / remains stable / weakens
improves / stays constrained / deteriorates
lower / similar / higher burden
earlier / unchanged / delayed conversion
```

Use numerical scenario tables only when:

```text
the user supplied the numbers
the filing or company guidance explicitly supplied the values
the arithmetic follows directly from sourced values with a visible formula
```

Do not invent growth rates, margin thresholds, probabilities, terminal
values, valuation multiples, target prices, time horizons, stop-loss levels,
or N-out-of-M decision rules.

## 9. Financial Path Discipline

Keep these layers separate:

```text
operating change
-> revenue or cost effect
-> margin effect
-> operating cash-flow effect
-> capex and investing cash flow
-> financing need, debt, dilution, or capital allocation
```

Do not count R&D again as a post-FCF use of cash. Do not treat M&A-related
FCF addbacks as acquisition purchase price. Do not treat all buybacks as pure
shareholder return.

## 10. Action Threshold Meaning

Action thresholds are evidence conditions, not personalized trade orders.

Use:

```text
판단 강화
판단 유지
판단 약화
가정 파기
```

Prefer observable company evidence:

```text
orders convert into reported revenue
margin improves despite investment
cash conversion catches up with growth
capex burden rises without operating proof
liquidity weakens or financing dependence increases
management commentary conflicts with reported direction
```

Without user position context, use `추가 확인`, `기다림`, `재검토`, or
`가정 파기` language. Do not imply add, trim, exit, hedge, or stop-loss
execution.

## 11. Coverage And Stop Rules

Use covered companies only unless the user explicitly names another company
and accepts the evidence limitation.

Stop when:

```text
the current baseline and one to three scenario variables are supported
each scenario has a mechanism, offset, and observable signpost
the remaining gap requires market price, consensus, portfolio, or unsupported valuation data
another tool call would add volume rather than change the scenario judgment
```

## 12. Final Answer

Follow `references/scenario-output-contract.md`.

The answer should:

```text
start with one practical scenario judgment
show the current filing baseline
use one interpretation-first scenario table
identify the binding sensitivity and downside path
state what strengthens, weakens, or breaks the view
end with exactly three immediately reusable same-company prompts under "이어서 볼 질문"
```

Keep internal tool, schema, object, pack, index, and routing language out of
the answer.
