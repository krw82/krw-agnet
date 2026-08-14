---
name: research-forbidden-user-facing-language
description: "Forbidden user-facing language. Lists internal terms, limitation phrases, tool names, and diagnostic labels that must never appear in normal answers."
when_to_use: "before composing any normal user-facing answer to remove internal tool, failure, and implementation wording"
---

# Forbidden User-Facing Language

Normal answers must not expose implementation, retrieval limits, or generic limitation boilerplate.

## Forbidden headings and phrases

Do not write:

```text
공시자료 기반 한계
공시자료 기준 한계
분석 한계
참고: 위 분석은
수치 기반의 정량적 매크로 지표보다는
질적 리스크 요인을 중심으로
이번 분석은 내부 실행 설정에 따라 진행되어
현재 조회 범위에서는
현재 도구 조회 범위에서는
근거 탐색 범위
조회 범위
세부 매출 수치가 충분히 추출되지 않았습니다
추가 확인이 가능합니다
더 자세한 분석이 필요하면
tool_budget_exceeded
budget exceeded
budget stop
tool budget
composer fallback
fallback
Now let me
Let me verify
I will query
I'll check
verified by trace
verified from trace
```

Do not make any of these a whole-answer ending when evidence is only partial:

```text
찾지 못했습니다
모르겠습니다
알 수 없습니다
could not find it
I do not know
not enough information
```

These phrases may appear only when they accurately bound one specific claim,
and then pair them with the strongest supported conclusion, the closest
alternative signal, and the next investor monitor.

## Forbidden internal terms

Do not write:

```text
MCP
plugin
skill
agent_index
SQLite
JSONL
cache
schema
ontology quality
artifact contract
validation
rejected object
rejected_object
index internals
pipeline
registry
diagnostics
object_id
quote_id
span_id
support_link_id
edge_id
trace_id
topic_id
query_context
retrieve
trace
chain
research_pack
metric_series_pack
business_profile_pack
risk_mechanism_pack
comparison_view
direct_exposure_pack
scope_guard_pack
agent_autonomy
allowed_next_tools
do_not_call
ResearchKernel
kernel
runtime setting
execution mode
EvidenceQuote
ResearchClaim
MetricObservation
XBRLFact
ExternalFactorExposure
BusinessFactor
AgreementTerm
BusinessEvent
traceable_direct
traceable_related
direct_answerable
negative_answer_supported
internal English investment brief
ontology/MCP-aware
retrieval surface
evidence retrieval
스키마
온톨로지 품질
아티팩트 계약
검증 실패
거절 객체
인덱스 내부
파이프라인
```

## Replacement style

Use filing-facing language:

```text
공시자료에서 확인되는 내용
공시된 수치 기준
직접 확인되는 내용은 아닙니다
관련 비용/공급망/마진 맥락은 있습니다
공시자료의 기간별 수치
사업/리스크 요인
```

## Rule

If evidence is incomplete, narrow the answer. Do not add a limitation section explaining internal retrieval limits.
