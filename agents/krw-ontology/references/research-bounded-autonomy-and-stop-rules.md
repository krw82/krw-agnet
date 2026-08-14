---
name: research-bounded-autonomy-and-stop-rules
description: "Bounded autonomy and stop rules. Prevents looping, enforces evidence sufficiency before answering, and defines overflow recovery."
when_to_use: "when evidence is partial, a continuation may help, or the run needs a bounded stop decision"
---

# Bounded Autonomy and Stop Rules

Preserve analytical judgment while making continuation evidence-state driven.

```text
Preserve:
interpretation
comparison judgment
caveats
business mechanism explanation
final Korean prose

Prevent:
repeated broad plans with cosmetic wording changes
unbounded fallback retrieval
tracing every candidate
turning related evidence into direct proof
shrinking evidence budgets before logs show that quality is preserved
```

The model owns the research strategy. The runtime owns the boundary between a
supported fact, a valid calculation, an investment inference, and a scenario.
Do not turn this boundary into a fixed tool sequence or a fixed number of
searches.

## Default workflow

```text
1. Have the active DeepSeek agent author a complete SearchPlan.
2. Call query_context with {search_plan}.
3. Read answerability, clause_coverage, calculation_coverage, missing_parts,
   recommended_actions, continuation, and warnings.
4. Continue only for a specific gap that could change correctness.
5. Use selected trace/chain only when materially useful.
6. Write the final Korean answer.
```

Before final prose, keep factual and calculated statements tied to verified
evidence. For an inference, retain the premise, material assumption, and a
signal that would weaken it. See `grounded-investment-inference.md`.

`plan_query` is optional validation/debug and performs no retrieval.

## Adaptive budgets

Do not expose limits, modes, or internal failures in normal answers.

Choose `limit_results` and `limit_tickers` for the actual number of required
clauses, tickers, metric periods/pairs, and possible conflicts. Do not impose a
fixed top-5, limit-3, one-follow-up, or other hard tool-call rule. Reduce
budgets later only when production logs and quality evaluation support it.

## Stop conditions

Stop searching when:

```text
answerability.status is answerable and every load-bearing required clause is covered
the requested numeric axes have covered calculation_coverage
answerability permits only a narrow partial answer and remaining gaps cannot change it
direct exposure is unsupported and related context is already separated
the remaining gap requires market price, valuation, or unavailable external data
the next call would only add volume rather than improve correctness
```

Continue when a `missing_parts` or `recommended_actions` entry identifies a
material, answerable gap. Target that clause, ticker, metric, period, or object;
do not repeat the full broad plan.

## Tickerless discovery

For a concrete tickerless factor, channel, product, business model, metric,
event type, or company type, set `universe="covered"` and leave `tickers`
empty. For explicit companies, set `tickers` and omit `universe`.

Let the agent choose a discovery budget large enough for the user's requested
breadth and evidence needs. Treat routed companies as candidates until their
required clauses are covered. Avoid whole-catalog inventory and unsupported
companies.
