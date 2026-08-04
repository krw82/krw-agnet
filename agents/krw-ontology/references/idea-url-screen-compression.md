---
name: idea_url_screen_compression
description: "URL 기반 아이디어 스크린 압축. 기사/이벤트/외부 소스를 커버드 종목 연구 후보로 변환하는 규칙"
when_to_use: "idea_generation에서 사용자가 URL을 제공했을 때"
---

# URL Screen Compression

Use this reference only when the idea-generation workflow receives
user-provided URL context.

## Scope

URL support appears in more than one product surface. Do not apply this
compression policy everywhere.

```text
Discover / idea generation URL:
compress the article into one investable screen, then find covered-company
research candidates.

Company chat URL:
do not turn the article into a discovery screen. Treat the URL as an external
market/business narrative for the selected company and answer the company
question with filing evidence.

Market-move URL or price-move context:
do not turn the article into a discovery screen. Explain the market/news move
first, then offer filing-grounded follow-ups.

News discovery:
do not run filing discovery. Surface event candidates or short news context.
```

If the user is not in the idea-generation workflow, this reference is only a
boundary reminder.

## What Compression Means

Compression is not article summarization.

Compression means turning the article into one filing-testable investment
screen:

```text
article/event
-> economic path
-> company qualification rule
-> filing evidence required
-> reject condition
-> why this is worth researching now
```

The screen should be narrow enough that covered candidates can be tested
against the same evidence standard, but not so narrow that it names a single
company unless the article itself gives a ticker list.

## Required Screen Fields

Before tickerless discovery, define the screen internally:

```text
Screen:
what kind of covered company qualifies

Evidence required:
what filing evidence proves direct exposure

Financial path:
how the article's issue can reach revenue, margin, cash flow, capex, debt, or
balance sheet flexibility

Reject if:
what would make a candidate a thematic false positive

Why now:
why this article makes the screen worth researching now
```

Do not expose this internal screen as a tool report. Use it to guide search and
then explain the result in plain Korean.

## Choosing One Screen

If the URL contains multiple possible angles, choose one screen using this
priority:

```text
1. Most direct economic path in the article
2. Most filing-testable path
3. Most likely to map to covered companies
4. Most material financial path
```

Do not branch into several unrelated searches.

Bad branching from one article:

```text
GPU suppliers
memory suppliers
electronics manufacturing services
component distributors
gaming platforms
```

Good compression:

```text
companies with direct memory or semiconductor component cost exposure where
filings show pricing, supply constraint, customer commitment, or margin linkage
to hardware cost pressure
```

## Evidence-Driven Continuation

URL-based idea generation has no fixed tickerless call count. Before the first
filing call, compress the URL into one dominant screen and author a complete
SearchPlan v2 with `universe="covered"`, atomic qualification/rejection clauses,
and adaptive result limits. Call `query_context` with exactly `{search_plan}`.

Continue only when ResearchState shows a material required gap:

```text
missing direct-exposure proposition -> narrow that proposition
missing financial-path proposition -> add only the named channel/metric clause
unfinished candidate scope -> use continuation or a contract-sized ticker batch
covered clauses -> never repeat them with a wording variant
```

Independent open clauses may be researched concurrently when the runtime
supports it; dependent clauses must wait for the prior state. Every follow-up
must stay inside the same compressed screen. A new economic screen requires a
new user choice or a clearly dominant path from the source, not an arbitrary
extra search.

Stop when material candidates have comparable evidence coverage, when remaining
gaps cannot change classification, or when the evidence floor cannot be met.

## Stop And Ask

Stop before searching when:

```text
the article is mostly about an uncovered private company
the article has several plausible screens and no clearly dominant economic path
the screen would be generic, such as "AI beneficiary", "gaming stocks", or
"semiconductor stocks"
```

In that case, answer with 2-3 ready-to-send discovery screens the user can
choose from.

Stop after searching when:

```text
no covered candidates have direct exposure
results are only keyword matches
the strongest result is thematic rather than filing-supported
```

Then explain:

```text
the screen tested
why no strong covered candidates were found
better follow-up screens to try
```

## Examples

Article:
Valve Steam Machine pricing rose because component costs remain elevated.

Bad screen:

```text
gaming related stocks
```

Good screen:

```text
covered semiconductor or memory companies where filings show pricing, supply
constraint, customer commitments, or margin linkage from component cost pressure
that can flow into gaming/PC hardware prices
```

Article:
Apple explores restricted Chinese memory sourcing because memory costs are
rising.

Good screen:

```text
covered companies where memory cost inflation or China supply-chain constraints
can affect product margin, procurement flexibility, or supplier bargaining
power, with filing evidence for cost, supply, or margin channels
```

Article:
Data-center power shortage delays AI infrastructure expansion.

Good screen:

```text
covered power infrastructure or equipment companies where filings show
data-center power demand flowing into orders, backlog, capacity, pricing, or
cash-flow conversion
```

## Final Answer Shape

For successful URL idea generation:

```text
1. Start with the screen tested in plain Korean.
2. Show A / B / C / Reject candidates only when filing evidence supports the
   classification.
3. Explain why each candidate qualifies or fails.
4. End with follow-up prompts that move the user into deeper company research.
```

For unsuccessful URL idea generation:

```text
1. Say the article's direct listed-company angle was weak or uncovered.
2. State the screen tested.
3. Offer narrower screens the user can send next.
```
