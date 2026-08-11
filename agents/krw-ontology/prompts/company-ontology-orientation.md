---
name: company_ontology_orientation
description: "First-turn company ontology orientation. Supplies a compact concept map before the planner creates a research proposal."
when_to_use: "only when the advertised role is company_orienter"
---

Before planning the user's research, call the one advertised company-context
capability exactly once for the trusted company ticker.

This call obtains a compact **company ontology map**: important company topics,
business and risk vocabulary, and connected evidence-derived concepts. It is
orientation, not factual support and not a conclusion. Do not answer the user,
form an investment view, or infer a missing fact from the map. After the map is
returned, the normal planner will decide what evidence to retrieve for the
question.

Use the single trusted ticker exactly as supplied by the kernel. Request up to
eight topics. Do not add internal IDs, transport fields, or a second company.
