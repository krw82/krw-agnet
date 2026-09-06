---
name: open_research_analysis
description: "Use when investigating a free-form question with no trusted ticker scope: discover candidate companies inside the covered universe, observe market-plane data (quotes, fundamentals, filings lists, news) for any canonical ticker including issuers outside the ontology coverage, and keep every claim grounded at the right evidence depth — without ever surfacing which engine layer produced a number."
when_to_use: "open research, tickerless question, uncovered ticker, free-form market question, concept-to-company discovery"
---

# Open research analysis (자유 질문 조사)

## Screening policy (classify_screen)

This workflow's flagship questions are exactly the broad ones: "X와 관련된
회사 있나?", "X가 오르면 누가 유리할까?", "X 업황은 어떤 지표를 봐야 하지?".
These MUST proceed to the discovery plan — that is what the ladder below is
for; broad is the expected input, not a defect. Choose clarification ONLY
when the question is unanswerable even in principle without more user input
(예: "이거 왜 이래?" 처럼 지칭이 전혀 없을 때). Never clarify merely because
the question is wide or lacks a ticker, and never refuse at the screen — the
discovery ladder plus honest coverage notes handle thin results.

The question text is the only trusted input. There is no trusted ticker scope,
so the investigation starts from concepts, not from a company.

## Hard tool rules (커널 위반 시 런 사망)

These are kernel-enforced. A violation burns a repair turn and can kill the
whole run, so treat them as absolute:

- **Universe tools never carry tickers.** `query_universe` and
  `trace_universe` must be called with topic/object arguments only — never
  `tickers`, never `ticker`, even for companies the run itself discovered.
  The ontology server resolves issuers; filing-grade citations come only from
  user- or server-vouched scope.
- **Observation reads may batch (≤4); universe queries never batch.**
  Observation-plane reads (`macro.series`, `market.series`, `openbb.*`,
  `news.web_search`) may be emitted together in one decision — at most FOUR
  calls per decision (a larger batch is rejected whole) — and the kernel
  drains them in order. Ontology universe tools (`query_context_universe`,
  `query_universe`, `trace_universe`) are planner-scored: emit each as its
  OWN single-call decision, and NEVER mix one into an observation batch —
  a mixed batch is rejected as a whole and costs a repair turn. If the
  kernel rejects a batch, re-emit the observation calls without the
  offending call instead of abandoning the door.
- **Discovery is one round.** Author the universe search plan once
  (`query_context_universe`, ≤12 clauses for the whole run). There is no
  second discovery round; follow-up drilling uses targeted topic queries,
  the market plane, macro series, and news.

1. **Discovery ladder.** Author the covered-universe search plan from the
   question (`query_context_universe`); never inject explicit tickers into a
   universe plan. Follow up on discovered candidates with `query_universe`
   and `trace_universe` (topic/object_id only) when the follow-up can change
   the conclusion.
2. **Market plane.** Quotes, fundamentals, statements, consensus, peers,
   earnings calendars, filing lists, web headlines, price history, and
   macro/market series (`openbb.*`, `news.web_search`, `market.series`) may
   name ANY canonical ticker — including issuers outside the ontology
   coverage. Use them freely when the question names a company the ontology
   does not cover. When the question's subject company itself is outside
   the covered universe, fetch the SUBJECT's own numbers from the market
   plane (income statement, metrics, quote) — never answer with only
   peers, and never answer with only a refusal. Peers are seasoning, not
   a substitute for the subject.
3. **Evidence depth stays honest — internal rule, never surfaced.**
   Business-fact claims (facts, outlook, risk) about a server-discovered
   company must cite ontology objects — the server found the issuer, so
   the citation is vouched. Market-plane reads are observations (numbers,
   dates, headlines) — never the sole basis for a business-fact claim.
   This vouching split decides WHAT you may claim; it is not a labeling
   scheme for the answer. Cite sources naturally inside the sentence
   (예: "실적발표 기준", "최근 시세 데이터 기준") and never mention
   ontology coverage, filing-ontology, evidence grades, or which engine
   layer produced a number. The reader cannot tell whether the ontology
   was used.
4. **Indicator observation duty.** A macro-framed question (inflation,
   rates, prices, 경기 국면) REQUIRES actually observing macro series
   (`macro.series`, or the openbb CPI / yield-curve / calendar lookups)
   before composing — reporting "미확보" without having looked is a
   contract failure. Whatever the question, observe the relevant
   indicators alongside the subject: micro/industry conditions from the
   ontology objects of related covered companies (their filings carry the
   industry-outlook hints). Look at time series, not just the latest
   value.
5. **Grounding, not refusal theater.** If neither the covered universe
   nor the market plane grounds a specific number, never fabricate it —
   follow the estimation discipline (a directional read from what IS
   grounded, labeled `제 판단`) and make the missing datum the first
   follow-up. Do not narrate coverage, ontology scope, or system limits
   anywhere in the answer; limitations ride beside the affected claim in
   ordinary words.

Keep the same three-layer separation as wide research: observed evidence with
its period, the causal interpretation, and remaining uncertainty. Advice-shaped
questions ("which stocks benefit?") are research questions, not
recommendations — answer with evidence, exposure, and counter-signals under
the advisory label.
