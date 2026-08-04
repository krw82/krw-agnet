---
name: idea_screen
description: "투자 테마를 커버드 종목 리서치 큐로 전환. 화면 정의→후보 발견→동일 기준 검증→A/B/C/Reject 우선순위. false positive 거절 포함"
when_to_use: "아이디어 스크리닝, 테마 기반 종목 발굴, 후보 검증, PM 스타일 트리지가 필요할 때"
---

# Idea Generation

## 1. Purpose

Use this skill to turn a broad investment question into a bounded,
filing-grounded research queue.

```text
investment theme or screen
-> explicit screen definition
-> covered-company candidate discovery
-> filing evidence validation, not keyword promotion
-> financial pathway and rejection risk
-> why this deserves research attention now
-> A / B / C / Reject research priority
-> next deep-research question
```

This skill finds what deserves deeper research. It does not decide what the
user should buy or sell.

```text
Idea Generation = candidate discovery and research prioritization
Company Research = deep analysis of a selected company or comparison
```

Default visible output:

```text
Korean Markdown only.
```

## 2. Shared Research Contract

The planner, evidence-analyst, research-synthesis, and idea-specific policies
are part of the immutable agent image. Follow them alongside this skill.
ResearchProposal v4 (not SearchPlan) is the model-authored planning ABI; the
kernel compiles it into the physical SearchPlan and MCP request.

Do not load or apply the buy/sell/hold report template, target-price or final
recommendation logic, or the normal exactly-three follow-up requirement.

## 3. Internal Idea-Screen Brief

Before calling ontology tools, convert the user's request into a concise
internal English idea-screen brief.

If the user did not provide a usable screen, do not invent one silently. A
usable screen needs at least one concrete axis:

```text
business driver, product/channel exposure, financial condition, risk condition,
event type, company type, named ticker list, or explicit exclusion
```

For conditionless requests such as "good stocks", "companies to enter now",
or "what should I buy", first ask the user to choose a screen by offering
3-5 ready-to-send examples. Do not run broad discovery just to fill the
answer.

The brief must preserve:

```text
theme or requested condition
named companies or ticker list
requested beneficiary or risk pathway
time context
financial channels
desired exclusions
```

Add evidence axes that help distinguish real exposure from a thematic false
positive:

```text
direct business activity or customer demand exposure
latest management commentary
orders, backlog, pricing, volume, or contract evidence
revenue, margin, cash-flow, or balance-sheet pathway
capex, cost, financing, or execution burden
recent strengthening or weakening
first rejection evidence
```

The brief must also define the screen before discovery:

```text
Screen:
what kind of company should qualify

Evidence required:
what filing evidence would prove direct exposure

Financial path:
how the driver could reach revenue, margin, cash flow, capex, or balance sheet

Reject if:
what evidence would make the candidate a false positive

Why now:
what recent filing evidence, change, pressure, or catalyst makes the candidate worth prioritizing now
```

Do not treat "why now" as stock timing. It means research timing: the latest
filing made the exposure, pathway, burden, or risk more important to
investigate. Do not expose the internal English brief.

## 4. Request Types

Classify the request before research:

```text
A. Theme or beneficiary discovery
B. Financial or business-condition screen
C. User-provided candidate-list triage
D. False-positive or weak-exposure filtering
E. User-provided URL idea-screen extraction
```

This skill is not for deep analysis of one already-selected company, ordinary
company overview, buy/sell/hold timing, target price, current market-price
screening, consensus estimate revisions, institutional positioning, or
portfolio sizing.

## 5. Candidate Discovery

Before discovery, read and apply `references/idea-search-order.md`.

Use a three-phase workflow:

```text
1. Screen definition:
   Translate the user's broad question into qualification, evidence, financial path, rejection, and why-now criteria.

2. Candidate discovery:
   Author a ResearchProposal v4 with universe scope to find enough covered candidates for the requested screen. The kernel compiles it into the physical SearchPlan.
   Do not finalize ranking in this phase.

3. Candidate validation:
   Verify each material candidate on the same axes before assigning A / B / C / Reject.
```

### Theme or condition without a ticker list

Start with one complete ResearchProposal. Treat resolved tickers as routes to
verify, not as final ranked ideas. Use clause_coverage and evidence_units to
determine which candidates actually satisfy the screen.

Do not:

```text
run whole-catalog scans
rank more companies than the available evidence can compare consistently
repeat the same broad query with slightly different wording
advance a candidate from keyword relevance alone
```

### User-provided ticker list

Use only the named, covered tickers. Apply identical evaluation clauses to
every candidate. Exclude unavailable tickers silently unless their absence
materially changes the requested screen.

### Candidate validation

Validate material candidates with a complete but focused evidence path:

```text
1. Author a validation ResearchProposal whose explicit ticker scope and atomic objectives apply the same screen to every selected candidate.
2. Read the returned ResearchState clause_coverage/evidence_units.
3. Verify direct exposure, financial pathway, why-now evidence, strongest burden, and first rejection risk.
4. Follow missing_parts/recommended_actions only for a material required axis.
5. Use selected trace/chain only when a material candidate claim needs stronger lineage support.
6. Stop when every material candidate can be classified consistently or disclose the remaining evidence boundary.
```

## 6. PM-Style Triage

Read and apply:

```text
references/idea-search-order.md
references/idea-candidate-funnel.md
references/idea-candidate-evidence-policy.md
references/idea-rejection-policy.md
references/idea-output-contract.md
```

Evaluate every material candidate on the same axes:

```text
Exposure:
Does the company directly participate in the requested business or financial pathway?

Evidence:
Is the connection supported by recent company filing commentary, notes, contracts, or metrics?

Financial path:
How could the driver affect revenue, margin, cash flow, capital intensity, or balance-sheet risk?

Recent change:
Did the latest available filing strengthen, weaken, or leave the idea unchanged?

Why now:
Why should this company be researched before other candidates in this screen?

Burden:
What cost, capex, financing, execution, concentration, or cycle risk offsets the potential benefit?

First rejection:
What is the earliest filing-supported reason this candidate may be a false positive?

Next research:
What exact same-company question should be investigated next?
```

## 7. Priority Buckets

Use these buckets only as research priority:

```text
A - immediate deep-research candidate
B - watchlist candidate; one material condition remains
C - thematic screen flag; exposure or financial linkage remains weak
Reject - filing evidence does not support advancing the idea
```

Hard interpretation:

```text
A means research first.
A does not mean buy now.
B does not mean hold.
Reject means reject from this screen, not permanently reject the company.
```

Do not convert these buckets into ratings, target prices, expected returns,
or portfolio actions.

## 8. Evidence Floor

A candidate cannot reach A from thematic relevance alone.

For A, require:

```text
credible direct or operational exposure
recent filing evidence
a plausible revenue, margin, cash-flow, or balance-sheet pathway
at least one explicit offset or first-rejection risk
clear reason to research this candidate first within the screen
```

B may have a credible pathway with one important evidence gap.

C is appropriate when:

```text
the theme is mentioned but economic linkage is not demonstrated
evidence is old, generic, or mostly risk-factor boilerplate
direct exposure is unclear
the financial pathway is speculative
```

Reject when the evidence fails the rules in
`references/idea-rejection-policy.md`.

Do not fill evidence gaps with model memory, market folklore, or unsupported
peer assumptions.

## 9. Answer Contract

Follow `references/idea-output-contract.md`.

Default answer shape:

```text
one-sentence research-priority conclusion
candidate funnel table
A candidates
B/C candidates
rejected or deprioritized false positives
next same-company research prompt for each important candidate
```

Use interpretation-oriented tables:

```text
등급 | 기업 | 후보로 잡힌 이유 | 실적 연결 | 첫 반증 위험
```

Do not overload the answer with raw numbers. Use exact figures only when
they materially change candidate classification.

Do not describe ontology objects, candidate scores, ranking internals,
search diagnostics, tools, packs, schemas, or index structure.

## 10. Handoff

End with actionable same-company deep-research prompts, not generic follow-up
questions. Render them as a numbered Markdown list so the user can pick the
next investigation directly.

Good:

```text
1. VST의 데이터센터 전력 수요가 실제 매출과 현금흐름으로 연결되는지 자세히 분석해줘.
2. ETN의 최근 수주 신호가 매출총이익률 개선으로 이어지는지 확인해줘.
3. XYL이 B 후보에 머문 핵심 근거 공백만 자세히 봐줘.
```

Do not introduce new peer companies in the handoff unless the user requested
a comparison.

## 11. Final Guardrails

Before sending the answer, verify:

```text
only covered companies are presented as researched candidates
the latest available filing drives current judgments
every A candidate has exposure proof, financial pathway, and first rejection
keyword matches and generic risk language were not promoted into strong ideas
no market price, consensus, positioning, short-interest, or portfolio facts were invented
the buckets are clearly research priorities, not recommendations
internal English briefs and implementation terms are absent
the Korean answer is concise, readable, and investment-focused
```
