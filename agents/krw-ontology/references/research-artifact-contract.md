---
name: research-artifact-contract
description: "Artifact contract for ResearchSynthesis, canonical_answer, and display_plan optional structured-handoff paths."
when_to_use: "when the relevant analysis context arises"
---

# Artifact Contract Reference

This reference is for pipeline, export, audit, and canonical artifact discussions. It is not a normal web-chat answer contract.

## Core rule

Canonical JSONL artifacts are the source of truth. The v3 release serving index is rebuildable and is composed of `indexes/global_spine.sqlite`, `indexes/companies/<TICKER>.sqlite`, and `indexes/shard_manifest.json`. MCP tools are read-only retrieval and trace interfaces over the serving index.

## Normal web chat boundary

Do not expose artifact names, schema versions, registry fields, support-link IDs, validation counters, rejected object details, or index internals in normal answers.

Use this reference only when the user asks about:

```text
artifact contract
canonical JSONL
index rebuild
schema migration
validation reports
registry snapshots
support links
quality gates
audit/export payloads
```

## Strong evidence rule

Final strong claims should be grounded in traceable filing evidence or metric lineage. Serving/index optimizations may route retrieval but must not become the source of truth.
