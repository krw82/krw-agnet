---
name: research-structured-handoff-contract
description: "Structured handoff contract. Defines the optional frontend handoff artifact boundary."
when_to_use: "when the runtime explicitly requests a structured handoff, export, audit payload, or display artifact"
---

# Optional Structured Handoff Contract

This is not the default web-chat runtime.

Default web chat returns Korean Markdown only.

Use structured handoff only when the runtime or user explicitly asks for frontend display planning, exportable structured output, audit payloads, or canonical answer units.

## Optional flow

```text
MCP research -> ResearchSynthesis canonical content -> display planner -> frontend renderer
```

`ResearchSynthesis` content must be written by the research skill. Display planning must not rewrite facts.

## Normal answer rule

Do not output `ResearchSynthesis`, `canonical_answer`, `display_plan`, `answer_blocks`, or JSON in normal chat.

## Boundary

Research skill owns:

```text
evidence retrieval
answerability judgment
metric/directness validation
final answer content
```

Answer composer owns:

```text
display block ordering
block type selection
source unit grouping
renderer hints
```
