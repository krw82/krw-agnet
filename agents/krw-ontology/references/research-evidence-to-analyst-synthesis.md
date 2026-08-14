---
name: research-evidence-to-analyst-synthesis
description: "Evidence-to-analyst synthesis guardrail. Candidate route versus evidence, internal-to-user translation rules, and what to hide from normal answers."
when_to_use: "when turning admitted ontology evidence into analyst claims without exposing internal retrieval mechanics"
---

# Evidence to Analyst Synthesis

This reference preserves the old skill's core discipline while keeping normal web-chat answers clean.

## Principle

KRW ontology evidence is not the final answer. It is the research state the analyst uses to write the final answer.

```text
MCP output = evidence workbench
AI agent = analyst judgment and final Korean explanation
Final answer = business interpretation grounded in filing evidence
```

Do not copy ontology internals into user-facing prose.

Internal ontology knowledge may be used for evidence selection, confidence calibration, routing decisions, and quality checks. It must not become the user-facing explanation.

## Candidate route vs evidence

Projection and lookup tables are routing aids.

```text
metric_lookup
metric_dimension_lookup
company_dimension_catalog
exposure_lookup
agreement_lookup
event_lookup
factor_lookup
company_topic_index
object_search_text
```

These can identify likely objects, metrics, topics, or companies. They are not by themselves enough for strong claims.

Strong claims need at least one of:

```text
traced filing evidence
metric lineage
direct source object support
consistent cross-object support with caveat
```

## Translation rule

Convert internal evidence into analyst language.

```text
Internal: ExternalFactorExposure exists for input_cost_pressure.
User-facing: The company flags input-cost pressure as a margin risk.

Internal: directness = related_context.
User-facing: This supports a related pressure channel, not a directly quantified company impact.

Internal: specificity_score = 0.78.
User-facing: The disclosure is specific enough to support the direction of the risk, but not a precise quantified impact.

Internal: candidate route from company_topic_index.
User-facing: The company disclosures point to the topic as a recurring business issue.
```

## Explanatory text and notes first

When the user asks what a company action, product/platform strategy, cost change, cash-flow pattern, or investment program means, prioritize explanatory filing text and notes:

```text
MD&A / management discussion
financial statement notes
segment notes
revenue recognition notes
RPO / backlog commentary
SBC notes
business combinations or acquisition notes
share repurchase notes
debt and liquidity notes
risk-factor updates when they describe a specific current channel
business, product, platform, customer, or demand descriptions
```

Numbers are evidence, not the answer. Use them internally to verify magnitude, direction, period alignment, and accounting treatment. In the final answer, translate the numbers into business and investor meaning.

## Numeric evidence to investor language

Prefer:

```text
The company is funding AI/platform expansion while preserving high FCF conversion.
The cost structure is under pressure because subscription cost of revenue is rising faster than subscription revenue.
The buyback looks partly like dilution management rather than a pure capital-return signal.
The acquisition step-up began before the latest quarter, so the inflection should be dated to the earlier annual period and then reinforced by the latest quarter.
```

Avoid:

```text
long raw number tables when the question asks for meaning
mixing P&L expenses, FCF calculation items, investing cash flows, and financing cash flows as the same layer
adding R&D again as a post-FCF use of cash
treating every buyback dollar as pure shareholder return
attributing cost, revenue, or margin movement to AI/platform without explicit filing support
```

## Sector/global answers

For sector, macro, discovery, or multi-company basket questions, do not summarize object inventory.

Use this synthesis shape:

```text
1. conclusion
2. common macro signal
3. company-by-company evidence signal
4. financial channel
5. interpretation and caveat
```

Example:

```text
Bad:
COST has ExternalFactorExposure objects for trade_down and input cost pressure.

Good:
Costco's disclosures point to a value-oriented consumer and price investment strategy: demand can hold up, but gross margin can be pressured when the company absorbs part of input-cost inflation to preserve traffic.
```

## What to hide

Never expose these in normal answers:

```text
object type names
ontology layer names
specificity scores
directness labels
traceable/non-traceable labels
candidate route mechanics
lookup/index/table names
raw object IDs
tool routing
coverage diagnostics
internal English investment brief
schema quality
artifact contract
validation or rejected-object mechanics
pipeline, registry, or index internals
```

Use them only for debug, audit, raw evidence, structured handoff, or developer-facing output.
