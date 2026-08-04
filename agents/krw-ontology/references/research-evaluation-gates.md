---
name: research-evaluation-gates
description: "평가 게이트 (답변 품질 검증 기준)"
when_to_use: "company_research 리서치에서 필요할 때"
---

# Evaluation Gates

Use these gates for web-chat quality checks and E2E harnesses.

## Normal answer gates

```text
Korean Markdown only
no internal terms
no mode/tool/pack names
no budget/fallback/error labels
no generic limitation heading
no FY as primary user-facing label
answers start from the most recent available filing unless the user asks for a historical period or specific filing
a newer 10-Q beats an older 10-K for current drivers; a latest 10-K is primary only when no newer 10-Q exists
three short follow-up prompts present under 이어서 볼 질문
strong claims traceable or metric-lineage grounded
direct vs related context separated
investment inferences name the premise, material assumption, and disconfirming signal
partial, conflicting, or truncated evidence yields a bounded investor conclusion and monitor, not a bare refusal
strong-claim-disallowed states never become proven, confirmed, or certain conclusions
normal answers are interpretation-first, not raw-number dumps
tables use interpretation columns unless exact numbers are requested or required for the conclusion
exact numbers are limited to the few figures needed to support, qualify, or correct the conclusion
material numbers are translated into business/investor meaning
final answers do not expose the internal English brief
```

## Tool behavior gates

```text
model-authored SearchPlan v2 exists before the first query_context call
query_context arguments are exactly {search_plan}; legacy top-level arguments = 0
SearchPlan preserves user intent, tickers/universe, periods, metrics, dimensions, predicates, comparison axes, and calculation windows in atomic clauses
ResearchState required clause/calculation coverage is checked before synthesis
structurally invalid SearchPlan input is repaired before execution; legacy fallback arguments = 0
retrieve is a targeted recall extension for a named missing clause only; do not use after sufficient ResearchState coverage
trace/chain selected roots only; never brute-force every candidate
no repeated equivalent query
no broad full-response first pass
open tool result missing completed runs = 0
tool budget / fallback / retry wording visible to user = 0
overflow or oversized tool result becomes split-and-continue behavior, not a final-answer failure
```

## Financial statement interpretation gates

```text
P&L expenses, OCF/FCF calculation items, investing cash-flow items, and financing cash-flow items are separated
R&D is not added again as a post-FCF cash use
actual M&A cash spending uses acquisition cash-flow lines, not business-combination-related FCF addbacks
company-defined FCF and simple OCF minus capex are distinguished
share repurchases are not framed as pure shareholder return when filings indicate dilution management
capital-allocation inflection timing checks annual periods and latest quarters together
AI/platform/product attribution requires explicit filing support or is framed as an inference
```

## Latency and quality targets

```text
internal leak cases = 0
runtime setting leak cases = 0
budget/error/fallback/overflow wording cases = 0
B-grade cases <= 1 in 30Q smoke
```
