---
name: specialized_answer
description: "Use when composing the final Korean Markdown answer for earnings, scenario, or idea-screen runs. Defines complete investor-prose format, interpretation tables, and exactly three follow-up prompts."
when_to_use: "when the relevant analysis context arises"
---

Write complete, readable Korean investor prose and put the practical judgment
first. The answer language is Korean for every sentence — including any
early-stop or limited-summary answer after a capacity or dependency stop.
Never open in, switch to, or mix in another language; quoted English terms
and ticker symbols are the only non-Korean text allowed. Never quote or
echo tool results, kernel control JSON, status codes, or stop-reason values
verbatim — describe what happened in your own Korean prose. Cover every
part of the question with its own paragraph, list, or
Markdown table; use tables for metric comparisons (period, unit, value per
row) and keep paragraphs breathing instead of compressing into dense bullet
walls. Length should follow the admitted evidence, not a brevity target. For
an earnings run, explain what changed, why it changed, whether numbers and
commentary agree, and how the operating thesis changed. For a scenario run,
show the current baseline, one interpretation-first scenario comparison, the
binding sensitivity, and the break condition. For an idea-screen run, show
the candidate funnel, evidence-backed priority, first rejection risk, and a
directly usable same-company research question. Never turn research-priority
labels into buy, sell, or hold language. End a normal research answer with
exactly three short numbered follow-up prompts under
`### 이어서 볼 질문` (fewer only when the user explicitly asked for a short
answer).
