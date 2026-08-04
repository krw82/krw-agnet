---
name: research_scope
description: "Use when the idea_planner role designs the research scope. Defines ResearchProposal v4 authoring rules: objective/alternative/goal types and required/deferred policy."
when_to_use: "when the relevant analysis context arises"
---

Create a compact English internal research brief and one `ResearchProposal v4`, never a `SearchPlan`. The kernel owns the authenticated question, ticker scope, universe marker, goal graph, candidate IDs, retrieval queries, limits, comparison axes, calculation lowering, and physical MCP encoding.

Use one objective for each genuinely distinct evidence need. Mark it `required` only when its absence would materially leave the user's answer incomplete; mark a plausible but nonessential expansion `deferred`, so it does not widen the initial plan. At least one objective must be `required`. Each objective has one to four interchangeable `alternatives`, where every alternative contains focused literal `terms`. The kernel chooses the least sufficient alternative rather than treating every alternative as extra research.

Every objective has exactly one tagged `goal`:

- `metric_observation`: one reported metric value.
- `metric_time_series`: a sequence or trend of reported metric observations. Use this for “trend” or “trajectory”; do not invent a growth-rate calculation.
- `metric_change`: an explicit `absolute_change` or `growth_rate`, with its paired `period_over_period` or `year_over_year` window.
- `metric_difference`: a same-period difference between comparable metric observations.
- `qualitative_evidence`: one or more concepts and, when several concepts must be established together, their predicate/relationship.

Metric goals contain only `metric` and optional dimensions. Qualitative goals contain only concepts and predicates. If the answer needs both a number and its explanation, mechanism, risk, or business implication, create independent objectives. Do not create a global comparison axis or a free-floating calculation window: the tagged goal makes that choice once and the kernel derives the physical fields.

Keep alternatives literal and focused on the same evidence need. Do not broaden a simple question into generic risk, valuation, moat, management, growth, or comparison research. Do not create or restate goal IDs, candidate IDs, dependencies, user spans, `retrieval_query`, clauses, tickers, universe, limits, wrappers, cost estimates, or confidence values.

At the initial planning state invoke the available filing-context function before emitting a workflow event or drafting an answer. Its provider arguments are exactly the named `proposal` envelope shown by the tool schema: `{ "proposal": ResearchProposalV4 }`. Inside that one field, author the complete `ResearchProposal v4`; never send a `SearchPlan`, `{ "search_plan": ... }`, ticker scope, or any extra top-level field. The kernel alone unwraps the proposal and compiles the physical MCP request. After evidence ingestion, request only a precise open-clause query, observed lineage trace, an `ontology.chain` trace on a relevant `object_id` when the question turns on a causal or relational link, or a genuinely new focused proposal when an evidence gap can change the conclusion. Repeating canonical work is forbidden.
