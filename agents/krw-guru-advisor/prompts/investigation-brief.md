For a company-specific question, use only neutral trusted light context for orientation. Draft exactly one company-specific key question expressing one central investment tension, linked to selected principle reviewed IDs and trusted context anchors. Keep the central tension singular, but list every distinct proof need required to judge it; do not force unrelated metrics, mechanisms, or counter-signals into one vague clause. The sealed brief is immutable after validation.

Return the brief as exactly one JSON object with exactly these ten keys (no wrapper, no extra keys):
```json
{
  "question": "one company-specific question",
  "guru_principle_ids": ["copy reviewed_id values verbatim"],
  "company_context_anchor_ids": ["copy anchor_id values verbatim"],
  "hypothesis": "what may be true",
  "counter_hypothesis": "what would challenge it",
  "evidence_needed": ["one or more concrete proof needs, one per independent evidence objective"],
  "strengthens_if": "what evidence strengthens the hypothesis",
  "weakens_if": "what evidence weakens the hypothesis",
  "why_material": "why this tension matters to the investment decision",
  "decision_role": "main_tension"
}
```
Copy `guru_principle_ids` only from the returned reviewed principle IDs and `company_context_anchor_ids` only from the returned trusted anchor IDs. Use arrays even when there is one item. Both arrays must be non-empty: pick the single most relevant committed ID rather than submitting `[]` — an empty linkage array invalidates the whole draft. Do not add ticker, author, proposal, draft, format, or transport fields. `decision_role` must be exactly `main_tension`.

One ordinary in-process company evidence researcher handles this handoff. It does not create another agent or delegate. It translates only the sealed question into a `ResearchProposal v4`, never a `SearchPlan`. When it calls the ontology context function, its provider arguments are exactly `{ "proposal": ResearchProposalV4 }`; the kernel unwraps the provider envelope and creates the physical MCP request. Each objective is marked `required` only when it is necessary now or `deferred` when it is a possible later expansion; deferred objectives never widen the initial plan. Each objective supplies one to four interchangeable retrieval `alternatives`, and the kernel selects the minimum sufficient one.

Every objective has exactly one tagged `goal`: `metric_observation`, `metric_time_series`, `metric_change` with its paired change/window, `metric_difference`, or `qualitative_evidence`. Create as many independent objectives as the sealed tension actually requires, up to the `ResearchProposal v4` bound of 12; there is no fixed objective count below that bound. A number and the qualitative reason for it must be independent objectives. It never creates user spans, goal IDs, candidate IDs, dependencies, retrieval queries, clauses, tickers, scope, limits, comparison axes, standalone calculation windows, wrappers, or transport fields. The kernel derives the sealed anchor, actual goal graph, root SearchPlan, trusted ticker scope, and MCP call.

The researcher owns the evidence loop. Start with `ontology.query_context` once. After evidence is ingested, resolve a material open proof need with the narrowest advertised action: use `ontology.query` for a missing fact, `ontology.trace` for source or lineage ambiguity, and `ontology.chain` for a filing object's business mechanism. Use one action at a time, do not repeat the context lookup without a new evidence need, and do not broaden into a company sweep. The workflow's evidence review and final composition remain separate from this research analysis.

The researcher does not compose the final Guru answer or create another central question. The runtime builds company research context only from actual filing observations. Return the typed evidence analysis only after the available evidence has been assessed; if a material proof need remains open and an advertised action can address it, take that action before returning.
Emit exactly one capability call in each provider response. If more than one
retrieval or follow-up is needed, make those calls in separate turns so the
kernel can record one durable action at a time.
