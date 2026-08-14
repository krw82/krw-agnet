# Adaptive MCP Detail and Context Budget Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 일반 리서치는 answer-ready `compact` MCP 응답을 기본으로 사용하되 모델이 현재 질문과 핵심 objective의 중요도를 보고 `full`을 선택하며, 누적 입력 144,000·출력 60,000 토큰 안에서 정상 추론 품질과 최종 사용자 답변을 보존한다.

**Architecture:** 새 MCP, 새 저장소, 새 모델 턴을 만들지 않는다. 기존 `ontology.query(response_detail)` 한 경로의 compact 표현을 보강하고, 모델이 capability call에서 `compact|full`을 선택하도록 한다. Rust는 모델이 선택한 detail을 보존하면서 ticker/topic/filter/limit과 trusted scope만 canonicalize한다. 기존 EvidenceLedger와 `CompactedProviderContext`는 그대로 사용하고 역할별 dynamic view만 줄이며, 60k 출력 예산의 기존 final-reserve/tail recovery를 회귀 테스트로 고정한다.

**Tech Stack:** Rust 1.97+, serde/serde_jcs, Python 3.12+, pytest, Anthropic Messages-compatible GLM/DeepSeek wire, MCP Streamable HTTP, YAML AgentImage and deployment registries.

## Global Constraints

- 수정 범위는 `~/krw-agnet`뿐이다.
- `~/krw-ontology`와 온톨로지 스키마는 변경하지 않는다.
- 새 MCP tool, 새 evidence store, 새 DB table, 새 자동 LLM turn을 만들지 않는다.
- 모델 호출 상한, thinking 수준, capability allowlist, trace/chain 허용 횟수를 줄이지 않는다.
- `response_detail`의 공개 enum `ids_only | compact | ticker_summary | full`은 유지한다.
- compact 결과가 잘렸다는 사실은 `없음`으로 해석하지 않는다.
- MCP 오류를 회사 비공시나 데이터 부재로 바꾸지 않는다.
- 답변 품질을 hard reject하는 새 final-answer validator를 만들지 않는다.
- `agents/krw-ontology/skills/**`, `agents/krw-ontology/references/**`, Guru skill 문서, `prompts/research-analysis.md`는 변경하지 않는다.
- 모델 detail 선택과 직접 충돌하는 `prompts/evidence-analyst.md`의 targeted-query 두 문장만 role prompt로서 최소 수정한다. 새로운 규칙 문서를 만들거나 skill에 복사하지 않는다.
- 일반 실행에서는 GLM과 DeepSeek이 같은 AgentImage·workflow·budget 계약을 사용한다.
- `company_research_glm`의 `max_input_tokens=144000`, `max_output_tokens=60000`을 유지하며 합계 204,000이 GLM 204,800 context ceiling을 넘지 않게 한다.
- 기존 dirty worktree의 persistence/runtime-persistence/migration 변경을 되돌리거나 덮어쓰지 않는다.

## Decisions Locked Before Implementation

1. 일반 회사 지도, 사업 설명, 리스크 탐색, 일반 실적 흐름은 compact가 기본이다.
2. detail 결정은 모델이 한다. 질문의 핵심 결론, 사용자가 요구한 깊이, 정확한 원문·수치·기간·scope·lineage 필요성을 종합해 같은 capability call에서 `compact|full`을 고른다.
3. Rust는 모델이 고른 유효 detail을 덮어쓰지 않는다. 다만 model scope, ticker, canonical topic, period/document/object filters, `answer_candidate_only`, limit은 현재 trusted candidate로 고정한다.
4. compact를 먼저 부른 뒤 항상 full을 한 번 더 부르는 2단계 프로토콜은 만들지 않는다. 모델이 중요하다고 판단하면 첫 targeted read부터 full을 선택할 수 있다.
5. compact 결과가 실제로 잘렸고 필수 gap이 남은 경우에만 기존 workflow의 다음 analyst turn이 기존 targeted capability를 선택할 수 있다. 자동 재호출은 추가하지 않는다.
6. EvidenceLedger와 durable action result는 그대로 사용한다. 별도 “전체 근거 저장소”나 “모델용 근거 저장소”는 만들지 않는다.
7. provider에게 보내는 역할별 view만 줄인다. durable receipt는 계속 full canonical context를 pin한다.
8. 기존 skill과 research-analysis 문서는 그대로 정적 삽입한다. `skill.load`로 옮겨 모델이 로드를 빠뜨리는 실패지점을 만들지 않는다.

## File Map

- Modify: `services/krw-ontology-runtime/src/krw_capability_runtime/mcp_server/tools.py` — answer-ready compact projection과 중복 없는 query kernel envelope.
- Modify: `services/krw-ontology-runtime/src/krw_capability_runtime/transport/mcp/descriptors.py` — 모델에게 compact/full 선택 의미를 도구-local 설명으로 제공.
- Modify: `services/krw-ontology-runtime/tests/test_descriptor_registry.py` — compact/full wire contract 회귀 테스트.
- Modify: `agents/krw-ontology/prompts/evidence-analyst.md` — 모델이 candidate의 detail만 자율 선택하도록 targeted-query 두 문장 수정.
- Modify: `crates/krw-ontology-adapter/src/lib.rs` — compact result ingestion/status와 model-selectable candidate default.
- Modify: `crates/run-engine/src/lib.rs` — model-selected detail 보존, canonical query dispatch, request footprint telemetry, final reserve 회귀 고정.
- Modify: `crates/context-compaction/src/lib.rs` — 역할별 evidence/fact 동기 필터와 best-effort byte bound.
- Modify: `crates/runtime-config/src/lib.rs` — 144k/60k shared budget contract 회귀 테스트.
- Create: `fixtures/live-quality/v1/adaptive-detail-v1.json` — compact/full 선택과 최종 답변 품질 canary 질문.

---

### Task 1: Make `compact` Answer-Ready Without Turning It Into `full`

**Files:**

- Modify: `services/krw-ontology-runtime/src/krw_capability_runtime/mcp_server/tools.py:539-555,923-1000,4053-4152`
- Modify: `services/krw-ontology-runtime/tests/test_descriptor_registry.py:112-225`

**Interfaces:**

- Preserve `query_tool(response_detail=ResponseDetail.COMPACT)` as the default.
- Add two private projection helpers:

```python
def _compact_object_summary(item: Mapping[str, Any]) -> dict[str, Any]:
    obj = item.get("object")
    if not isinstance(obj, Mapping):
        return {}
    summary = {
        key: obj[key]
        for key in _COMPACT_OBJECT_FIELDS
        if key != "dimensions" and obj.get(key) is not None
    }
    dimensions = obj.get("dimensions")
    if isinstance(dimensions, Mapping):
        summary["dimensions"] = {
            _short_text(str(key), 64): _short_text(str(value), 128)
            for key, value in list(dimensions.items())[:8]
            if isinstance(value, (str, int, float, bool))
        }
    return summary


def _compact_evidence_summary(evidence: Mapping[str, Any]) -> dict[str, Any]:
    counts = {
        "claim_count": len(evidence.get("claims") or []),
        "quote_count": len(evidence.get("quotes") or []),
        "span_count": len(evidence.get("spans") or []),
        "related_object_count": len(evidence.get("related_objects") or []),
    }
    counts["truncated"] = (
        counts["claim_count"] > 2
        or counts["quote_count"] > 2
        or counts["span_count"] > 1
        or counts["related_object_count"] > 2
    )
    return counts
```

- `_compact_object_summary` exposes only these answer fields when present:

```python
_COMPACT_OBJECT_FIELDS = (
    "id", "type", "metric_name", "canonical_metric", "value", "unit",
    "currency", "formatted_value", "period_type", "start_date", "end_date",
    "metric_scope", "dimensions",
)
```

- `dimensions` is capped at eight keys; key text is capped at 64 characters and scalar value text at 128 characters. Nested arbitrary objects are omitted.
- Compact evidence retains at most two claims, two quotes, one span, two related objects, and the existing compact metric-lineage fields.
- Add `evidence_summary` with original counts and `truncated: bool`; this is control metadata, not evidence.
- Top-level `results` remains authoritative. `kernel.research_pack.query_results` keeps only `{id,type,ticker,document_id}` references instead of duplicating the full result bodies.

- [ ] **Step 1: Write failing compact metric and narrative tests**

Add `test_compact_query_keeps_answer_ready_metric_basis` with a `MetricObservation` containing `value=15400`, `unit=USD_millions`, `currency=USD`, `period_type=duration`, company-total dimensions, and complete metric lineage. Assert that compact retains those fields while excluding an injected private field.

Add `test_compact_query_discloses_evidence_truncation` with four quotes and four related objects. Assert that only the configured bounded subset is present and `evidence_summary.truncated is True` with the original counts.

- [ ] **Step 2: Write a failing duplicate-envelope test**

Assert that:

```python
assert payload["results"][0]["evidence"]["quotes"]
assert payload["kernel"]["research_pack"]["query_results"] == [
    {"id": "metric_1", "type": "MetricObservation", "ticker": "AVGO", "document_id": None}
]
assert "evidence" not in payload["kernel"]["research_pack"]["query_results"][0]
```

- [ ] **Step 3: Run the focused Python tests and confirm pre-change failures**

Run:

```bash
python3 -m pytest -q \
  services/krw-ontology-runtime/tests/test_descriptor_registry.py \
  -k 'compact_query or duplicate_envelope'
```

Expected: compact numeric object fields and truncation metadata are absent, and the kernel envelope duplicates result bodies.

- [ ] **Step 4: Implement the bounded compact projection**

Implement `_compact_object_summary` and `_compact_evidence_summary`; call both from `_compact_bundle`. Do not change `query_with_diagnostics` or the full branch. Keep source excerpts in their existing order because the store already ranks evidence.

- [ ] **Step 5: Replace only the duplicated kernel copy with references**

In `_query_kernel_envelope` call construction, use `_ids_only_bundle(result)` for `query_results`. Keep result count, answerability, diagnostics, pagination, and top-level results unchanged.

- [ ] **Step 6: Run descriptor and MCP boundary verification**

Run:

```bash
python3 -m pytest -q services/krw-ontology-runtime/tests/test_descriptor_registry.py
```

Expected: all tests pass; the descriptor still reports `compact` as default and the four original enum values.

- [ ] **Step 7: Commit the independently reviewable MCP projection**

```bash
git add \
  services/krw-ontology-runtime/src/krw_capability_runtime/mcp_server/tools.py \
  services/krw-ontology-runtime/tests/test_descriptor_registry.py
git commit -m "perf: make compact ontology results answer ready"
```

### Task 2: Let the Model Select `compact|full` While the Kernel Keeps Scope Canonical

**Files:**

- Modify: `services/krw-ontology-runtime/src/krw_capability_runtime/transport/mcp/descriptors.py:508-531`
- Modify: `services/krw-ontology-runtime/tests/test_descriptor_registry.py:112-145`
- Modify: `agents/krw-ontology/prompts/evidence-analyst.md:58-80`
- Modify: `crates/krw-ontology-adapter/src/lib.rs:407-430,643-710,3060-3185`
- Modify: `crates/run-engine/src/lib.rs:7850-7980,9040-9205,12560-12770`

**Interfaces:**

- Keep `ExactTargetedQueryCandidate.response_detail: String` for checkpoint compatibility, but change newly derived candidates from `"full"` to the safe default `"compact"`.
- Add one model-choice parser at the run-engine boundary:

```rust
fn selected_targeted_response_detail(arguments: &Value) -> &'static str {
    match arguments
        .get("response_detail")
        .and_then(Value::as_str)
    {
        Some("full") => "full",
        Some("compact") | None => "compact",
        Some(_) => "compact",
    }
}
```

- Change the canonical argument builder to accept the model choice while keeping every other candidate field trusted:

```rust
fn exact_required_gap_arguments(
    candidate: &ExactTargetedQueryCandidate,
    selected_response_detail: &str,
) -> Value;
```

- Preserve `answer_candidate_only=true`, canonical ticker/topic/filter values, and the existing bounded limit. The model controls only `response_detail`.

- [ ] **Step 1: Write descriptor tests for model-owned detail selection**

Assert the `ontology.query` tool description communicates all four facts without adding a skill segment:

1. compact is the default;
2. the model chooses full when the user's main question or a primary objective materially benefits from exact quote, numeric basis, period/scope, or lineage;
3. full should be one precise bounded read rather than a broad dump;
4. choosing full does not widen ticker, period, document, or object scope.

- [ ] **Step 2: Write run-engine preservation tests**

For the same exact candidate, assert:

```rust
assert_eq!(physical_args(model_args("compact"))["response_detail"], "compact");
assert_eq!(physical_args(model_args("full"))["response_detail"], "full");
assert_eq!(physical_args(model_args_without_detail())["response_detail"], "compact");
```

In all three cases assert ticker/topic/document/period/object filters, `answer_candidate_only`, and limit equal the trusted candidate rather than model variants.

- [ ] **Step 3: Add an autonomy regression test**

Script one analyst turn that selects full for a high-importance exact-source objective and one that selects compact for supporting context. Assert both calls are admitted without adding a provider turn, repair turn, or alternative capability.

- [ ] **Step 4: Run focused tests and confirm current hard-coded-full behavior**

```bash
python3 -m pytest -q \
  services/krw-ontology-runtime/tests/test_descriptor_registry.py \
  -k 'detail or targeted_query'
cargo test -p krw-ontology-adapter planning_projection_keeps_exact
cargo test -p krw-agent-run-engine targeted_query
```

Expected before implementation: the candidate and canonicalization force full even when the model requests compact.

- [ ] **Step 5: Update only the tool-local guidance and conflicting role-prompt lines**

Change the MCP description to:

```text
Compact is the default. Choose full in this same precise bounded call when the
user's main question or a primary research objective materially benefits from
exact source text, numeric basis, period/scope detail, or lineage. The choice
does not widen authenticated ticker, document, period, or object scope.
```

In `evidence-analyst.md`, replace only the unconditional “request full detail” and “copy response_detail exactly” wording. State that candidate scope/filter/limit fields are copied exactly while `response_detail` is the analyst's research-depth choice. Do not modify any file under `skills/**` or `references/**`.

- [ ] **Step 6: Preserve the model choice during canonicalization**

Read `response_detail` before replacing model arguments with the exact trusted candidate. Pass only that value to `exact_required_gap_arguments`; invalid or missing detail falls back to compact. Keep server-side full cap and all capability budgets unchanged.

- [ ] **Step 7: Verify no workflow or skill expansion**

Assert provider-turn limits, capability-call limits, analyst reasoning, and `ontology.query/trace/chain` allowlists are unchanged. Assert no new prompt segment and no new `skill.load` requirement appears in the compiled image.

- [ ] **Step 8: Run and commit the model-choice boundary**

```bash
python3 -m pytest -q services/krw-ontology-runtime/tests/test_descriptor_registry.py
cargo test -p krw-ontology-adapter
cargo test -p krw-agent-run-engine targeted_query
git add \
  services/krw-ontology-runtime/src/krw_capability_runtime/transport/mcp/descriptors.py \
  services/krw-ontology-runtime/tests/test_descriptor_registry.py \
  agents/krw-ontology/prompts/evidence-analyst.md \
  crates/krw-ontology-adapter/src/lib.rs \
  crates/run-engine/src/lib.rs
git commit -m "feat: preserve model-selected ontology detail"
```

### Task 3: Preserve Compact Completeness Signals Through Rust Ingestion

**Files:**

- Modify: `crates/krw-ontology-adapter/src/lib.rs:1810-1935,2470-2795,3260-3355`
- Test: `crates/krw-ontology-adapter/src/lib.rs` test module

**Interfaces:**

- Reuse `SupplementalReadStatus.warning_codes`; do not add a new workflow state or result contract.
- Add the closed warning `compact_evidence_truncated` when any compact result reports `evidence_summary.truncated=true`.
- `targeted_result_object` continues to merge the compact `object` summary with envelope ticker, period, document type, section, and text.

- [ ] **Step 1: Add a compact metric ingestion test**

Use the exact Task 1 compact result shape and assert the EvidenceRecord retains:

- ticker;
- economic period;
- metric predicate;
- numeric value;
- unit and currency context;
- metric scope/dimensions;
- source object ID;
- metric-lineage directness when lineage is present.

- [ ] **Step 2: Add a compact narrative truncation test**

Assert that an included direct quote becomes a source excerpt, while `compact_evidence_truncated` remains a status warning and never becomes a fact or citation.

- [ ] **Step 3: Run focused tests and verify the missing warning**

```bash
cargo test -p krw-ontology-adapter compact_targeted
```

- [ ] **Step 4: Extend supplemental status derivation**

Read only the boolean `evidence_summary.truncated`. Never copy raw warning text. Deduplicate and sort warning codes using the existing bounded vocabulary semantics.

- [ ] **Step 5: Run the complete adapter suite and commit**

```bash
cargo test -p krw-ontology-adapter
git add crates/krw-ontology-adapter/src/lib.rs
git commit -m "fix: retain compact evidence completeness signals"
```

### Task 4: Make Existing Context Compaction Role-Coherent and Best-Effort Bounded

**Files:**

- Modify: `crates/context-compaction/src/lib.rs:31,103-335,640-830,1150-1565`

**Interfaces:**

```rust
const COMPANY_ANALYST_VIEW_MAX_BYTES: usize = 64 * 1024;
const COMPANY_COMPOSER_VIEW_MAX_BYTES: usize = 64 * 1024;
const SPECIALIZED_COMPOSER_VIEW_MAX_BYTES: usize = 96 * 1024;
const MAX_FACTS_PER_VIEW_EVIDENCE: usize = 6;

fn retain_facts_for_evidence(context: &mut CompactedProviderContext);
fn protected_goal_evidence_ids(context: &CompactedProviderContext) -> BTreeSet<String>;
fn shrink_role_view_best_effort(
    context: &mut CompactedProviderContext,
    max_bytes: usize,
    protected: &BTreeSet<String>,
) -> Result<(), CompactionError>;
```

`shrink_role_view_best_effort` is prompt-only. It must never return `EssentialContextExceedsLimit`; if protected essential fields alone exceed the target, it returns a valid over-target view and emits a size diagnostic. Existing hard durable-compaction validation remains unchanged.

- [ ] **Step 1: Add the analyst orphan-fact regression test**

Build 20 evidence records with facts. After `view_for_role("analyst")`, assert there are at most 16 evidence entries and every retained fact's `evidence_id` exists in that filtered evidence index.

- [ ] **Step 2: Add goal-witness protection tests**

Build three required goals, each pointing to a different evidence ID, plus many higher-volume unrelated records. Assert that the role view retains at least one evidence/fact witness for every required goal before retaining unrelated extras.

- [ ] **Step 3: Add role byte-bound and non-failure tests**

Assert ordinary analyst and composer fixtures serialize under 64 KiB and specialized composer under 96 KiB. Add a synthetic essential-overflow fixture and assert `view_for_role` still returns a valid view rather than a new runtime error.

- [ ] **Step 4: Filter analyst state duplication**

For `ROLE_ANALYST`:

1. keep `research_projection` and `retrieval_status`;
2. set the raw `state.payload` to `Null` while retaining artifact hash/contract/payload hash;
3. clear calculations;
4. trim evidence to 16;
5. remove facts whose evidence was trimmed;
6. retain at most six highest-priority facts per evidence;
7. clear `omitted_fact_refs` from the prompt-only view because the durable receipt already pins them.

- [ ] **Step 5: Bound composer view without losing required witnesses**

Before removing `research_projection`, compute protected required-goal evidence IDs. Then:

1. keep citations, admitted facts, calculations, answerability, and safe retrieval status;
2. remove execution handles and raw state as today;
3. retain protected evidence first, then direct/metric-lineage evidence, then related evidence;
4. retain at most six facts per evidence;
5. progressively remove lowest-priority unprotected facts and orphan evidence until the role target is met;
6. never remove the only protected witness solely to satisfy the soft target.

- [ ] **Step 6: Run compaction tests**

```bash
cargo test -p krw-context-compaction
```

Expected: durable receipt verification remains unchanged; only prompt views become smaller.

- [ ] **Step 7: Commit role-view compaction**

```bash
git add crates/context-compaction/src/lib.rs
git commit -m "perf: bound role-specific research context"
```

### Task 5: Measure Static Prompt Cost Without Editing Skill or Research Documents

**Files:**

- Modify: `crates/run-engine/src/lib.rs:10070-10460,10901-11170`
- Test: `crates/run-engine/src/lib.rs` test module

**Interfaces:**

```rust
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PromptFootprintBreakdown {
    static_prompt_bytes: u64,
    compacted_view_bytes: u64,
    session_memory_bytes: u64,
    tool_schema_bytes: u64,
    request_bytes: u64,
}
```

This task records sizes only. It does not edit, split, dynamically parse, omit, or move any skill/reference/research-analysis document.

- [ ] **Step 1: Add a component-accounting test**

Build one ordinary analyst request with compacted context and session memory. Assert each component is non-zero where expected and changing only the compacted role view changes `compacted_view_bytes` without changing `static_prompt_bytes`.

- [ ] **Step 2: Add a privacy test**

Format the tracing fields and assert they contain only role ID and integer sizes; they must not contain prompt text, user question text, evidence values, tool arguments, or session-memory content.

- [ ] **Step 3: Implement byte accounting at existing assembly points**

Sum verified `context.static_segments[].byte_len` for static prompt bytes, use `CompactedContextView::byte_len` for compacted view bytes, `memory.canonical.len()` for session memory, and serialized advertised tool definitions for tool-schema bytes. Attach the breakdown to `BuiltProviderRequest` and emit it with the existing request-footprint tracing event.

- [ ] **Step 4: Verify all skill and research documents are unchanged**

Run:

```bash
git diff --exit-code -- \
  agents/krw-ontology/skills \
  agents/krw-ontology/references \
  agents/krw-ontology/prompts/research-analysis.md
```

Expected: no diff.

- [ ] **Step 5: Run and commit telemetry**

```bash
cargo test -p krw-agent-run-engine prompt_footprint
git add crates/run-engine/src/lib.rs
git commit -m "perf: measure research prompt components"
```

### Task 6: Lock the 144k/60k Budget and Preserve the Final Answer Tail

**Files:**

- Modify: `crates/runtime-config/src/lib.rs` test module
- Modify: `crates/run-engine/src/lib.rs:5320-5410,5760-5895,10240-10680,17250-17565`

**Interfaces:**

- No production budget value changes in this task.
- Preserve:

```text
max_input_tokens  = 144000
max_output_tokens = 60000
provider ceiling  = 204800
effective ordinary-company composer retry reserve = 32768
```

- Normal analyst/composer thinking remains enabled.
- Only an already-entered final answer turn with fewer than 1,025 output tokens may disable thinking to avoid a terminal provider-contract failure.

- [ ] **Step 1: Add local/prod shared-budget regression tests**

Load both budget registries and assert the `company_research_glm` limits are identical, equal 144,000/60,000, and sum to at most the bound `glm_max` context limit.

- [ ] **Step 2: Keep and tighten tail-recovery tests**

Verify the existing tests cover:

- evidence-poor thinking floor routes to the image-declared composer;
- input usage at 80% routes to composition before the hard cap;
- admitted evidence reaches the composer without another research turn;
- a tiny final tail disables thinking only for that final answer request;
- non-answer roles never silently switch to direct mode.

- [ ] **Step 3: Add an effective composer-reserve assertion**

Load the ordinary company workflow and assert `effective_final_output_reserve_tokens("company_research_v2") == Some(32_768)`: two complete 16,384-token composer attempts remain protected even though the image-wide floor is 8,192.

- [ ] **Step 4: Verify detail autonomy does not change reasoning budgets**

Build otherwise identical analyst requests with compact and full targeted arguments. Assert both retain the same thinking mode, reasoning effort, provider-turn counter, and 16,384 per-turn cap; only the capability arguments differ.

- [ ] **Step 5: Run budget and engine tests**

```bash
cargo test -p krw-agent-runtime-config
cargo test -p krw-agent-run-engine thinking_floor
cargo test -p krw-agent-run-engine input_budget_reserve
python3 scripts/check_active_budget_parity.py
```

- [ ] **Step 6: Commit budget invariants and tail protection**

```bash
git add crates/runtime-config/src/lib.rs crates/run-engine/src/lib.rs
git commit -m "fix: preserve the final research answer budget"
```

### Task 7: Verify Quality, Provider Parity, and Release Behavior

**Files:**

- Create: `fixtures/live-quality/v1/adaptive-detail-v1.json`

**Interfaces:**

The new corpus contains these exact questions and expected trace properties:

| Case | Question | Expected detail behavior |
|---|---|---|
| `choice_cat_overview` | `Caterpillar는 무슨 기업이야? 투자자가 알아야 할 핵심도 같이 알려줘.` | 모델 선택과 physical detail이 일치하는지 확인; 일반적으로 compact가 경제적 |
| `choice_aapl_revenue_risk` | `Apple의 최근 매출 흐름과 핵심 리스크를 공시 근거 중심으로 간단히 정리해줘.` | 모델이 답변 중요도와 근거 깊이로 detail을 선택 |
| `choice_avgo_metric_basis` | `Broadcom의 최근 연간 매출 수치와 증감률을 정확한 기간·단위·계산 근거와 함께 알려줘.` | 정확한 수치·계산 근거 요구를 모델이 full 필요성으로 인식하는지 평가 |
| `choice_aapl_quote` | `Apple 최신 10-K의 핵심 리스크를 공시 원문 근거와 함께 설명해줘.` | 원문 중심 요구를 모델이 full 필요성으로 인식하는지 평가 |

- [ ] **Step 1: Run all focused offline suites**

```bash
python3 -m pytest -q services/krw-ontology-runtime/tests/test_descriptor_registry.py
cargo test -p krw-ontology-adapter
cargo test -p krw-context-compaction
cargo test -p krw-agent-runtime-config
cargo test -p krw-agent-run-engine targeted_query
cargo test -p krw-agent-run-engine thinking_floor
cargo test -p krw-agent-run-engine input_budget_reserve
```

- [ ] **Step 2: Run formatting and static parity checks**

```bash
cargo fmt --all -- --check
python3 scripts/check_active_budget_parity.py
python3 scripts/run_live_quality_matrix.py \
  --corpus fixtures/live-quality/v1/adaptive-detail-v1.json \
  --provider glm --dry-run
python3 scripts/run_live_quality_matrix.py \
  --corpus fixtures/live-quality/v1/adaptive-detail-v1.json \
  --provider deepseek --dry-run
```

- [ ] **Step 3: Build one dual-provider candidate after focused tests pass**

```bash
scripts/build_dual_provider_release.sh \
  --output-root "$HOME/.local/share/krw-agent/releases/adaptive-detail-candidate"
python3 scripts/test_dual_provider_release.py \
  --staging-root "$HOME/.local/share/krw-agent/releases/adaptive-detail-candidate" \
  --expect-sealed true
```

Expected: both GLM and DeepSeek candidates seal against the same AgentImage and 144k/60k budget contract.

- [ ] **Step 4: Reuse one long-lived local stack for GLM canary**

With the already-running Gateway and local token:

```bash
KRW_AGENT_GATEWAY_TOKEN="$KRW_AGENT_GATEWAY_TOKEN" \
python3 scripts/run_live_quality_matrix.py \
  --corpus fixtures/live-quality/v1/adaptive-detail-v1.json \
  --provider glm --parallelism 1 --timeout-seconds 900
```

Inspect original answer, sanitized action trace, provider-turn count, detail choices, prompt tokens, and terminal receipt. Do not treat transport completion alone as quality success.

- [ ] **Step 5: Repeat the same bounded canary for DeepSeek**

```bash
KRW_AGENT_GATEWAY_TOKEN="$KRW_AGENT_GATEWAY_TOKEN" \
python3 scripts/run_live_quality_matrix.py \
  --corpus fixtures/live-quality/v1/adaptive-detail-v1.json \
  --provider deepseek --parallelism 1 --timeout-seconds 900
```

- [ ] **Step 6: Apply acceptance criteria**

Accept only when all are true:

1. all four cases produce a terminal user-facing answer;
2. model-authored `response_detail` and physical MCP `response_detail` match for every targeted query;
3. Rust contains no question-kind or missing-code table that decides compact/full on the model's behalf;
4. no answer says evidence is absent when status says truncated, application error, or has-more;
5. GLM and DeepSeek keep the same workflow/capability/reasoning policies;
6. skill/reference/research-analysis documents are unchanged;
7. ordinary analyst/composer compacted dynamic role views are normally at most 64 KiB;
8. no new MCP call, provider turn, database object, or store exists solely for compaction;
9. 60k output remains cumulative and a final answer is still produced at the thinking/input tail boundaries.

- [ ] **Step 7: Commit the corpus after evidence review**

```bash
git add fixtures/live-quality/v1/adaptive-detail-v1.json
git commit -m "test: cover adaptive ontology detail across providers"
```

## Rollout Order

1. Merge Tasks 1 and 3 first: compact becomes answer-ready before full usage is reduced.
2. Merge Task 2 second: only after compact ingestion tests prove value/period/source survival.
3. Merge Tasks 4 and 5 third: dynamic role-view compaction and footprint measurement are independent of retrieval correctness.
4. Merge Task 6 fourth: lock existing 60k/tail behavior and add measurement.
5. Run Task 7 once, build one release candidate, then deploy that sealed candidate.

This order prevents the unsafe intermediate state in which full is removed before compact can carry the answer basis. Each commit is independently revertible; rollback never requires changing `~/krw-ontology` or the ontology schema.

## Self-Review Result

- Spec coverage: compact default, selective full, no extra MCP/storage/model turn, context compression, 60k final-answer protection, GLM/DeepSeek parity, and quality verification each have a dedicated task.
- Placeholder scan: no deferred implementation placeholders remain.
- Type consistency: `selected_targeted_response_detail`, `exact_required_gap_arguments`, compact summary fields, and role byte constants use one name throughout the plan.
- Failure-point audit: all new size limits are prompt-view best effort or test-time assertions; no new runtime hard-failure gate is introduced.
