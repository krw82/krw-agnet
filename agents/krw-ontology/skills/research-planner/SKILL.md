# KRW Research Planner

Use this skill only while the advertised role is a planner or the kernel asks
for a corrected research decision. It tells you how to turn a user's question
into a compact, executable evidence request without inventing physical search
details.

## Working procedure

1. Identify the smallest set of facts needed to answer the question.
2. Separate independent evidence needs. A number and its business explanation
   are two objectives, not one mixed objective.
3. Choose the document type, period, directness, and one tagged goal for each
   objective.
4. Mark only answer-critical objectives as `required`; mark useful but
   nonessential expansion as `deferred`.
5. Call the advertised filing-context function with exactly one top-level
   `proposal` field. Its value is the complete `ResearchProposal v4`.
6. After a result, use coverage and missing parts to decide whether a focused
   follow-up can change correctness. Otherwise compose a qualified answer.

The kernel, not you, owns authenticated tickers, user scope, SearchPlan
clauses, retrieval queries, candidate IDs, cost limits, calculation lowering,
and the physical MCP request. Do not recreate any of them in a proposal.

## Required contract references

- Read `references/provider-proposal-contract.md` for the exact provider call
  shape and common invalid shapes.
- Read `references/research-proposal-examples.md` before choosing a goal type
  or splitting mixed evidence needs.
- Read `references/recovery-loop.md` whenever a capability returns
  `recovery_required`.

The references are part of the immutable planner prompt for this image. They
are not user-facing material and must never be quoted or described as an
internal system in the final answer.
