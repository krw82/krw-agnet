---
name: idea_search_order
description: "Use during idea_generation to author ResearchProposal v4 for the 3-phase workflow (screen definition, candidate discovery, candidate validation)."
when_to_use: "when planning the three-phase idea-screen workflow: define the screen, discover candidates, then validate candidates"
---

# Idea Generation Search Order

## Core Rule

Do not search every ontology layer equally. Idea generation is candidate
discovery and research prioritization, not deep single-company research.

Use ontology layers in this order:

```text
screen definition
-> global-spine candidate route
-> semantic exposure validation
-> recent change / why-now evidence
-> financial pathway and burden
-> quote, support, trace, or chain verification
```

For idea generation, search semantic exposure before numeric evidence. Use the
global spine to find candidate routes, company semantic objects to validate
exposure, recent change objects to establish why-now, and numeric/evidence
trace only to classify or verify top candidates.

## 1. Screen Definition

Before broad discovery, define:

```text
who qualifies
what filing evidence proves direct or operational exposure
what financial path must exist
what would make a candidate a false positive
what could make a candidate worth researching now
```

Do not use a theme label alone as the screen.

Minimum viable screen:

```text
At least one concrete business driver, product/channel exposure, financial
condition, risk condition, event type, company type, named ticker list, or
explicit exclusion.
```

If the user provides no usable screen, do not run broad discovery. Offer a
short choice set of ontology-friendly prompts instead:

```text
AI data-center power demand beneficiaries
companies with improving cash-flow conversion but manageable capex burden
companies where margin pressure is easing rather than worsening
companies with direct memory-price upside and visible demand commentary
companies with lower tariff/regulatory exposure than peers
```

Bad:

```text
AI beneficiary
memory winner
defensive consumer company
good stocks
companies to enter now
what should I buy
```

Good:

```text
companies with direct operating exposure to AI data-center power demand where
filings show demand, orders, backlog, capacity, pricing, or cash-flow linkage
```

## 2. Candidate Route

For tickerless theme or condition questions, start with one complete
model-authored global-spine SearchPlan v2:

```text
search_plan={
  question: original user screen,
  intent: idea_screen,
  universe: "covered",
  clauses: atomic qualification, direct-exposure, financial-path, rejection, and why-now propositions,
  limit_tickers: adaptive candidate breadth,
  limit_results: enough to cover all required clauses and candidate comparisons
}
query_context(search_plan=search_plan)
```

Call `query_context` with exactly `{search_plan}`. Use the result to identify
candidate routes and their evidence coverage, not to finalize ranking.

Inspect:

```text
resolved_scope.resolved_tickers
answerability
clause_coverage
evidence_units and supported clause IDs
missing_parts / recommended_actions / continuation
```

Do not infer:

```text
high score = strong idea
many keyword hits = direct exposure
top reason = investment conclusion
candidate route = final ranking
```

## 3. URL-Based Candidate Route

When the discovery input is a user-provided URL, follow
`url-screen-compression.md` before using this search order.

That reference owns URL screen compression, multi-surface URL boundaries, and
evidence-driven continuation. This search-order document only defines
which ontology layers to inspect after a URL screen has been selected.

## 4. Semantic Exposure Layer

After candidates are routed, validate direct exposure before metrics.

Prefer these object types:

```text
BusinessActivity
CompanyBusinessProfile
ExternalFactorExposure
BusinessFactor
```

Use this layer to answer:

```text
What does the company actually do that qualifies it for this screen?
Which product, service, customer, capacity, cost, demand, or business activity creates exposure?
Is the exposure direct, operationally related, or only thematic?
```

If exposure is only thematic, downgrade to C or Reject.

## 5. Recent Change And Why-Now Layer

After exposure is real, check whether the latest filing makes the candidate
more or less important to research now.

Prefer:

```text
ChangeEvent
TrendObservation
BusinessEvent
BusinessFactor
```

Use this layer to answer:

```text
Did the latest filing strengthen the exposure?
Did it weaken the pathway?
Did management become more specific?
Did the burden or risk become more important?
Is this just stale annual business-description language?
```

Why-now does not mean stock timing. It means research timing: the filing made
the exposure, conversion path, burden, or rejection risk important enough to
prioritize.

## 6. Financial Pathway And Burden Layer

Only after semantic exposure is real, check whether the driver can reach a
financial line item.

Prefer:

```text
MetricObservation
XBRLFact
Calculation
AgreementTerm
```

Use this layer to answer:

```text
Can the driver reach revenue, margin, operating cash flow, capex, debt, or balance sheet flexibility?
Is there a contract, backlog, commitment, covenant, or financing term that changes the path?
Does the benefit have a cost, capex, working-capital, concentration, or financing burden?
```

Do not use raw numbers to decorate the answer. Use numbers only when they
change the A / B / C / Reject classification.

## 7. Evidence Verification Layer

Use these only after a candidate or rejection path is selected:

```text
EvidenceQuote
SupportLink
trace
chain
```

Use this layer when:

```text
promoting a candidate to A
making a strong direct-exposure claim
making a strong financial-pathway claim
rejecting a tempting false positive
resolving a conflict between candidate route and actual filing evidence
```

Do not start broad discovery from quotes. Quotes verify selected routes; they
do not define the candidate universe.

## 8. Stop Rule

Stop once each material candidate can be classified:

```text
A - direct exposure + recent evidence + financial path + why-now + rejection risk
B - credible path with one material evidence gap
C - thematic or weakly connected candidate
Reject - false positive for this screen
```

Do not continue into full company research. The handoff for one selected
candidate is `krw-ontology-research`.
