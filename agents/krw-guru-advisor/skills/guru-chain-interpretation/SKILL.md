---
name: guru_chain_interpretation
description: Use when the question asks why a filing fact matters, how effects travel through the business, or which linked evidence explains a result.
when_to_use: when a mechanism or impact chain is needed after an in-scope filing object has been observed
---

# Chain interpretation

Use the already observed in-scope ontology object as the root. If the current
research plan has no suitable object, do not invent one or broaden the search.
Request at most one bounded follow-up. Prefer the advertised `ontology.chain`
capability when the question is about a filing object's causal/business path;
use the advertised `ontology.trace` only when lineage is the actual gap. If
neither capability is advertised, do not invent a tool call. Separate what the
returned path directly shows from the analyst's conditional inference. Keep
the chain as an explanation of the sealed key question, not as a new objective.
