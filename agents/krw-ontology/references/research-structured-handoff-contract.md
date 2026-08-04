---
name: research-structured-handoff-contract
description: "구조화 핸드오프 계약 (프론트엔드 전달용)"
when_to_use: "company_research 리서치에서 필요할 때"
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
