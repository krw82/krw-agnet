# KRW Research Planner

Use this skill only while the advertised role is a planner or the kernel asks
for a corrected research decision. It tells you how to turn a user's question
into a compact, executable evidence request without inventing physical search
details.

## Working procedure

1. Read the ontology catalog to learn the exact metric identifiers, filing
   aliases, quote-type search hints, claim types, and searchable object types
   available in this release. Use these canonical names — never guess a metric
   identifier or object type that does not appear in the catalog.
2. Identify the smallest set of facts needed to answer the question.
3. Separate independent evidence needs. A number and its business explanation
   are two objectives, not one mixed objective.
4. Choose the document type, period, directness, and one tagged goal for each
   objective. Use the metric identifiers and filing-language aliases from the
   catalog so the retrieval index can match your request.
5. Mark only answer-critical objectives as `required`; mark useful but
   nonessential expansion as `deferred`.
6. Call the advertised filing-context function with exactly one top-level
   `proposal` field. Its value is the complete `ResearchProposal v4`.
7. After a result, use coverage and missing parts to decide whether a focused
   follow-up can change correctness. Otherwise compose a qualified answer.
8. After receiving evidence, if the question involves a causal or relational
   link (e.g., "how does X affect Y", "what drives X", "impact of X on Y"),
   request an `ontology.chain` trace on the most relevant evidence object_id.
   The chain returns connected objects: business activities, change events,
   external factors, and temporal links that explain cause and effect.
9. Use the chain result to compose an answer that explains the mechanism, not
   just the number. For example, "services revenue growth → product mix shift
   → overall margin improvement" rather than just "$109.2B".

The kernel, not you, owns authenticated tickers, user scope, SearchPlan
clauses, retrieval queries, candidate IDs, cost limits, calculation lowering,
and the physical MCP request. Do not recreate any of them in a proposal.

## Required contract references

- The ontology catalog lists every metric identifier, filing alias, search
  hint, and object type. Always use its exact names.
- Read `references/provider-proposal-contract.md` for the exact provider call
  shape and common invalid shapes.
- Read `references/research-proposal-examples.md` before choosing a goal type
  or splitting mixed evidence needs.
- Read `references/recovery-loop.md` whenever a capability returns
  `recovery_required`.

The references are part of the immutable planner prompt for this image. They
are not user-facing material and must never be quoted or described as an
internal system in the final answer.
