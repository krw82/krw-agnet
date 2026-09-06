---
name: open_research_analysis
description: "Use when investigating a free-form question with no trusted ticker scope: discover candidate companies inside the covered universe, observe market-plane data (quotes, fundamentals, filings lists, news) for any canonical ticker including issuers outside the ontology coverage, and keep every claim on its correct evidence grade."
when_to_use: "open research, tickerless question, uncovered ticker, free-form market question, concept-to-company discovery"
---

# Open research analysis (자유 질문 조사)

The question text is the only trusted input. There is no trusted ticker scope,
so the investigation starts from concepts, not from a company:

1. **Discovery ladder.** Author the covered-universe search plan from the
   question (`query_context_universe`); never inject explicit tickers into a
   universe plan. Follow up on discovered candidates with `query_universe` and
   `trace_universe` only when the follow-up can change the conclusion.
2. **Market plane.** Quotes, fundamentals, statements, consensus, peers,
   earnings calendars, filing lists, web headlines, price history, and
   macro/market series (`openbb.*`, `news.web_search`, `market.series`) may
   name ANY canonical ticker — including issuers outside the ontology
   coverage. Use them freely when the question names a company the ontology
   does not cover.
3. **Evidence grades stay honest.** Filing-grade claims (business facts,
   outlook, risk) must cite ontology objects. Market-plane reads are
   observation-grade: numbers, dates, and headlines — never the sole basis
   for a business-fact claim. State explicitly when a company is outside the
   filing-ontology coverage and only market data was available.
4. **Indicator observation duty.** Whatever the question, observe the
   relevant indicators alongside the subject: macro series (inflation,
   rates, yields, CPI, calendars) directly from the observation plane, and
   micro/industry conditions from the ontology objects of related covered
   companies (their filings carry the industry-outlook hints). Look at time
   series, not just the latest value.
5. **Honest refusals.** If neither the covered universe nor the market plane
   produces grounding, say so and describe what the current coverage can and
   cannot answer. Never fabricate. Name the gap in the answer so the user
   knows what to supply next.

Keep the same three-layer separation as wide research: observed evidence with
its period, the causal interpretation, and remaining uncertainty. Advice-shaped
questions ("which stocks benefit?") are research questions, not
recommendations — answer with evidence, exposure, and counter-signals under
the advisory label.
