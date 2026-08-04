---
name: research-artifact-contract
description: "아티팩트 계약 (ResearchSynthesis/canonical_answer/display_plan)"
when_to_use: "company_research 리서치에서 필요할 때"
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
