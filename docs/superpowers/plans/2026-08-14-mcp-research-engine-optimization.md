# MCP Research Engine Optimization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 모든 리서치 MCP가 실제 조회 결과, 빈 결과, 입력 문제, 페이지 잘림, 외부 장애를 구분해 하네스에 전달하도록 만들고, 모델 호출 수·추론 수준·허용 도구 수를 줄이지 않은 채 속도와 답변 신뢰도를 개선한다.

**Architecture:** 개선의 중심은 모델 출력 검사 강화가 아니라 MCP 결과 전달 경로다. Rust 하네스가 각 supplemental read의 안전한 상태 sidecar를 보존하고, compaction 뒤에도 작성자에게 필요한 제한 사유만 전달한다. 제품 계층은 이미 존재하는 run.failed outbox 응답을 정확한 장애 분류와 함께 표시하고, 연결 재사용은 stateless가 암호적으로 증명된 MCP에만 단계적으로 켠다.

**Tech Stack:** Rust 1.97+, Tokio, serde/serde_jcs, MCP Streamable HTTP, TypeScript/Node.js, Supabase/PostgreSQL, GLM live quality lane, sealed DeepSeek/GLM release packaging.

## Global Constraints

- 수정 대상은 ~/krw-agnet 및, Feed·Filings MCP를 소유한 ~/krw-ontology-front 뿐이다.
- ~/krw-ontology 는 수정하지 않는다.
- 온톨로지 스키마와 현재 공개 성공 응답 MCP 계약을 바꾸지 않는다.
- 모델 호출 횟수, reasoning 수준, output budget, capability 수, capability allowlist를 줄이지 않는다.
- 자동 재조회, 자동 재계획, 추가 LLM turn을 새로 만들지 않는다. 기존 workflow가 다음 턴을 사용할 때만 모델이 기존 capability를 선택할 수 있다.
- MCP 오류를 회사의 비공시 또는 데이터 부재로 바꾸지 않는다.
- “근거가 부족한 답변”을 hard failure로 막는 새 final-answer gate를 만들지 않는다. 대신 불완전성의 원인을 보존해 모델과 사용자에게 정확히 전달한다.
- 외부 MCP/provider 장애 시 raw 오류, 파일 경로, 비밀값을 사용자에게 노출하지 않는다.
- 데이터베이스·프런트 자체가 내려간 경우까지 답변 저장을 보장할 수는 없다. 이 계획의 보장은 정상 제품 저장소가 살아 있는 상황에서 MCP/provider 장애가 나도 사용자에게 상태 응답을 남기는 것이다.
- 기존 dirty worktree 변경을 되돌리거나 덮어쓰지 않는다.
- 라이브 답변 품질 평가는 GLM만 사용한다. DeepSeek은 release 계약·배포 검증만 수행하고, 별도 승인 없이는 live quality 호출을 하지 않는다.

## Decisions Locked Before Implementation

1. 일반 회사 리서치의 Markdown 최종 형식을 이번 작업에서 AnswerIR로 강제 전환하지 않는다.
2. MCP 상태는 evidence가 아니라 control metadata다. 상태 자체가 사실 주장이나 인용의 근거가 되면 안 된다.
3. application error가 담긴 정상 JSON payload는 empty result와 다르게 기록하되, workflow를 막거나 무한 재시도하지 않는다.
4. retryable transport failure는 기존 durable retry 정책을 유지한다. 재시도 예산이 끝난 terminal failure는 기존 front outbox 메시지를 정확한 안전 문구로 투영한다.
5. Guru는 현재 문서/게이트웨이와 production release binding의 재사용 정책이 일치해야 한다. Feed·Filings는 stateless 증명을 통과하기 전까지 run-scoped를 유지한다.
6. Feed 성능 개선은 새 DB cache/RPC를 먼저 만들지 않는다. 먼저 현재 output에 필요한 컬럼만 읽도록 좁혀 네트워크·메모리 낭비만 제거한다.

## File Map

### krw-agnet

- Modify: crates/krw-ontology-adapter/src/lib.rs — supplemental query/trace 결과의 bounded status projection을 만든다.
- Modify: crates/run-engine/src/lib.rs — status에 따라 action cache, research planner, compacted context를 갱신한다.
- Modify: crates/context-compaction/src/lib.rs — supplemental status를 bounded control context로 유지하고 composer-safe view로 줄인다.
- Modify: crates/runtime-persistence/src/executor.rs — terminal dependency failure에 closed failure class를 outbox로 전달한다.
- Modify: crates/runtime-persistence/src/bridge.rs — failure class가 durable run/outbox event에 손실 없이 실리도록 한다.
- Modify: crates/tool-mcp/src/lib.rs — reuse telemetry를 bounded tracing field로 노출한다. 연결 정책은 이 task에서 바꾸지 않는다.
- Modify: deployments/local/deployment-binding.example.yaml — attested stateless 정책의 source of truth를 정렬한다.
- Modify: deployments/prod/deployment-binding.example.yaml — attested stateless 정책의 source of truth를 정렬한다.
- Modify: docs/MCP_SESSION_ISOLATION.md — 실제 release policy와 승격 조건을 문서화한다.
- Modify: scripts/test_dual_provider_release.py — sealed release의 session reuse matrix와 provider parity를 검사한다.

### krw-ontology-front

- Modify: src/worker/krw-feed-mcp-server.ts — stateless attestation 후보 검증을 위한 readiness metadata를 제공한다. 검증 전 정책은 바꾸지 않는다.
- Modify: src/worker/filings-mcp-server.ts — public tool path와 personal tool path의 session-state 경계를 명시한다.
- Modify: scripts/install-krw-agent-local-mcp-gateways.mjs — gateway manifest/reuse matrix를 source binding과 대조한다.
- Modify: src/krw-feed-mcp/feed-reader.ts — select(*)를 output-contract에 필요한 명시 컬럼으로 바꾼다.
- Modify: src/krw-feed-mcp/server.test.ts — feed output contract와 tool-error 분류 회귀를 검증한다.
- Modify: src/filings-mcp/server.test.ts — public filings와 personal filings session isolation을 검증한다.
- Create: supabase/migrations/20260814120000_agent_v1_failed_research_notice.sql — terminal dependency failure를 사용자용 상태 메시지로 안정적으로 투영한다.
- Modify: supabase/migrations/20260812090200_agent_v1_product_outbox_projection.sql — 새 migration이 덮어쓰지 않도록 현재 projection의 failure semantics를 참조한다.
- Modify: src/lib/api/chat/immediate-response.ts — readiness fallback과 terminal run failure를 UI에서 구분할 수 있는 bounded metadata shape를 정렬한다.

---

### Task 1: Preserve Supplemental Read Meaning Instead of Treating Every Non-Record Result as Empty

**Files:**

- Modify: crates/krw-ontology-adapter/src/lib.rs:312-347,1794-1880,2445
- Modify: crates/run-engine/src/lib.rs:6400-6455,7148-7160
- Test: crates/krw-ontology-adapter/src/lib.rs test module
- Test: crates/run-engine/src/lib.rs test module

**Interfaces:**

- Produces a bounded, non-evidence status:

~~~rust
pub enum SupplementalReadKind {
    Retrieved,
    Empty,
    NotFound,
    InputRejected,
    Ambiguous,
    ApplicationError,
}

pub struct SupplementalReadStatus {
    pub kind: SupplementalReadKind,
    pub result_count: u16,
    pub has_more: bool,
    pub next_offset: Option<u32>,
    pub warning_codes: Vec<String>,
}
~~~

- Consumes existing successful ontology.query and ontology.trace payloads. It does not change their public contracts or the CapabilityResult checkpoint ABI.

- [x] **Step 1: Add failing adapter tests for the five outcomes**

Add fixtures for:

1. results with one numeric MetricObservation and pagination.has_more=true;
2. empty results with no error;
3. error.code=not_found;
4. error.code=input_invalid;
5. error.code=ambiguous;
6. an unrecognized application error.

Assert that all cases produce bounded status, that only the first has evidence, and that error payloads never become status Empty.

~~~rust
#[test]
fn targeted_input_error_is_not_normalized_as_empty_evidence() {
    let delta = map_targeted_query(&json!({
        "error": { "code": "input_invalid" }
    }), &mapping_context()).unwrap();
    assert_eq!(delta.status.kind, SupplementalReadKind::InputRejected);
    assert!(delta.records.is_empty());
}
~~~

- [x] **Step 2: Run focused adapter tests and verify the current failure**

Run:

~~~bash
cargo test -p krw-ontology-adapter targeted_input_error_is_not_normalized_as_empty_evidence
~~~

Expected: the pre-change adapter returns the same empty delta for error and empty result, and there is no status field.

- [x] **Step 3: Add SupplementalReadStatus and populate it deterministically**

Extend SupplementalEvidenceDelta with status. Map only closed, safe codes:

| Payload condition | Stored kind |
| --- | --- |
| non-empty results | Retrieved |
| results=[] without error | Empty |
| code contains not_found | NotFound |
| code contains invalid, input, validation | InputRejected |
| code contains ambiguous | Ambiguous |
| any other application error | ApplicationError with warning code supplemental_unclassified_error |

Read pagination only from typed, bounded fields. Clamp result_count to MAX_SUPPLEMENTAL_RECORDS, next_offset to a non-negative u32, and warning_codes to a small fixed vocabulary (at most four codes per read). Do not copy raw message text.

- [x] **Step 4: Expose one bounded status derivation helper**

Export a helper from krw-ontology-adapter that derives SupplementalReadStatus from a targeted/trace provider payload. The run engine calls it only after the existing successful adapter mapping. Do not add a field to CapabilityResult, do not change checkpoint JSON, and do not expose the status as a new MCP response field.

- [x] **Step 5: Change cache behavior without creating retry loops**

In the run engine:

1. Record every dispatched supplemental action in the durable action receipt.
2. Insert into action_cache only for Retrieved, Empty, and NotFound.
3. Do not cache InputRejected, Ambiguous, or ApplicationError.
4. Mark InputRejected, Ambiguous, and ApplicationError as an observed non-evidence outcome so the workflow can continue to its already-existing compose path.
5. Do not automatically rerun or automatically create an alternative query.

This lets a normal next model turn see the state, but does not consume an additional turn by itself.

- [x] **Step 6: Add run-engine regression tests**

Add one test proving an invalid targeted query:

- leaves no evidence record;
- leaves a visible non-absence status;
- does not create an action-cache hit;
- allows the existing terminal path to complete rather than returning a workflow error.

Add a second test proving a real empty query is cached as Empty and is distinguishable from InputRejected.

- [x] **Step 7: Run focused verification**

Run:

~~~bash
cargo test -p krw-ontology-adapter
cargo test -p krw-agent-capability-runtime
cargo test -p krw-agent-run-engine supplemental
~~~

- [ ] **Step 8: Commit**

~~~bash
git add crates/krw-ontology-adapter/src/lib.rs crates/run-engine/src/lib.rs
git commit -m "fix: preserve supplemental MCP result status"
~~~

### Task 2: Keep Pagination, Effective Retrieval Status, and Object Handles Across Compaction

**Files:**

- Modify: crates/krw-ontology-adapter/src/lib.rs:312-333,408-421,746-780
- Modify: crates/run-engine/src/lib.rs:6400-6455
- Modify: crates/context-compaction/src/lib.rs:98-130,250-385,700-735
- Test: crates/context-compaction/src/lib.rs test module
- Test: crates/run-engine/src/lib.rs test module

**Interfaces:**

- Extends ResearchRetrievalStatus without changing the ontology ResearchState v2 schema:

~~~rust
#[serde(default)]
pub supplemental_reads: Vec<SupplementalReadStatus>,
~~~

- Produces a max-four-entry history, sorted by action receipt order, not a raw tool transcript.

- [x] **Step 1: Add failing compaction tests**

Build a compacted context with:

- initial continuation.has_more=true;
- one targeted result with result_count=20, has_more=true, next_offset=20;
- one ambiguous trace result;
- source_object_ids on a targeted record.

Assert that the analyst view retains all bounded control facts, while the composer view sees only the bounded read kind, result count, and has_more flag:

~~~json
{"kind":"retrieved","result_count":20,"has_more":true}
~~~

The composer view must not expose object IDs, raw error codes, or server routing labels.

- [x] **Step 2: Run focused compaction tests and verify the current failure**

Run:

~~~bash
cargo test -p krw-context-compaction compaction_keeps_supplemental_status
~~~

Expected: the test is absent and supplemental pagination/error state does not survive the settled boundary.

- [x] **Step 3: Add bounded append semantics in the research planner**

Add one method that appends a SupplementalReadStatus to the existing ResearchRetrievalStatus:

~~~rust
pub fn record_supplemental_read_status(
    &mut self,
    status: SupplementalReadStatus,
) -> Result<(), ResearchPlannerError>;
~~~

Deduplicate adjacent identical statuses by action key at the caller; retain at most four latest statuses. The state is diagnostic control context only and must not change goal coverage or answerability.

- [x] **Step 4: Project safe status for each provider role**

Retain complete bounded status for planner and analyst. In composer_retrieval_status:

1. preserve only the bounded read kind, result count, and has_more/omitted-evidence signal;
2. remove next_offset, object identifiers, raw warning code, and exact server reason;
3. keep the existing source anchor period neutralization behavior.

- [x] **Step 5: Add regression tests for false absence**

Create a synthetic compacted composer context where a targeted search was truncated. Assert that the generated trusted context preserves the bounded read kind and has_more flag and does not contain any phrase such as company did not disclose.

This test asserts data availability, not model prose. No LLM invocation is required.

- [x] **Step 6: Run focused verification**

Run:

~~~bash
cargo test -p krw-context-compaction
cargo test -p krw-agent-run-engine retrieval_status
~~~

- [ ] **Step 7: Commit**

~~~bash
git add crates/krw-ontology-adapter/src/lib.rs crates/run-engine/src/lib.rs crates/context-compaction/src/lib.rs
git commit -m "fix: retain MCP pagination and non-absence status"
~~~

### Task 3: Make the Existing Failure Reply Accurate and Idempotent

**Files:**

- Modify: ~/krw-ontology-front/supabase/migrations/20260814130000_agent_v1_failure_projection.sql
- Test: existing product projection and runtime-persistence suites

**Interface:**

The existing `run.failed` outbox event and retry policy remain unchanged. A
small SECURITY DEFINER projection trigger reads only the private, closed
reason vocabulary after the existing projection has marked the run failed and
adds a public `research_failure` metadata marker. Raw provider/MCP text never
crosses the product boundary.

- [x] **Step 1: Keep the existing failure terminal path**

The durable reason code, defer cap, and `run.failed` event are already bounded
and idempotent. No retry, state, or model-turn behavior is changed.

- [x] **Step 2: Keep retry policy unchanged**

Do not change the existing retryable/non-retryable decision, delay, or retry cap. Only attach the closed class when the outcome becomes terminal.

This avoids a new retry policy, extra MCP calls, and additional model turns.

- [x] **Step 3: Add a monotonic front projection migration**

The migration must modify the product outbox projection so a terminal run.failed:

1. releases existing reservations exactly as it does now;
2. writes one Korean status response into the already-created assistant message;
3. keeps message status failed, not completed;
4. writes bounded metadata:

~~~json
{
  "kind": "research_failure",
  "research_completed": false,
  "failure_class": "dependency_unavailable"
}
~~~

5. never overwrites an already completed answer;
6. is idempotent under outbox replay.

Suggested copy for DependencyUnavailable:

> 리서치에 필요한 데이터 연결이 일시적으로 준비되지 않았습니다. 질문은 저장되어 있으며, 잠시 후 같은 채팅방에서 다시 시도해 주세요.

Do not say that the company did not disclose information. Do not expose MCP, provider, database, endpoint, or internal error names.

- [x] **Step 4: Add front migration contract tests**

Use the existing product projection test harness to prove:

- duplicate run.failed event produces one message;
- completed answer followed by late run.failed remains completed;
- dependency failure response is visibly distinct from a research answer;
- no analysis credit is settled on the failure path.

- [x] **Step 5: Run focused verification**

Run:

~~~bash
cargo test -p krw-agent-runtime-persistence
npm exec vitest run src/lib/agent-v1/migration-contract.test.ts
~~~

Run the TypeScript command from ~/krw-ontology-front.

- [ ] **Step 6: Commit each repository separately**

~~~bash
git add crates/runtime-persistence/src/executor.rs crates/runtime-persistence/src/bridge.rs
git commit -m "fix: classify terminal research dependency failures"
~~~

Then commit the front migration and tests in the front repository:

~~~bash
git add supabase/migrations src/lib/api/chat/immediate-response.ts src/lib/agent-v1
git commit -m "fix: show safe terminal research outage notice"
~~~

### Task 4: Remove Only Proven MCP Session-Initialization Overhead

**Files:**

- Modify: docs/MCP_SESSION_ISOLATION.md
- Modify: scripts/test_dual_provider_release.py
- Modify: deployments/local/deployment-binding.example.yaml
- Modify: deployments/prod/deployment-binding.example.yaml
- Modify: ~/krw-ontology-front/scripts/install-krw-agent-local-mcp-gateways.mjs
- Modify: ~/krw-ontology-front/src/worker/krw-feed-mcp-server.ts
- Modify: ~/krw-ontology-front/src/worker/filings-mcp-server.ts
- Test: crates/tool-mcp/src/lib.rs test module
- Test: ~/krw-ontology-front/src/krw-feed-mcp/server.test.ts
- Test: ~/krw-ontology-front/src/filings-mcp/server.test.ts

**Interfaces:**

- Consumes the existing stateless contract:

~~~text
krw-agent/mcp-tool-session-stateless/v1
~~~

- A capability can use attested-stateless-v1 only when both readiness and initialize emit the existing pinned attestation and the service behavior passes the isolation test.

- [ ] **Step 1: Add a release-matrix test before changing any reuse mode**

Extend scripts/test_dual_provider_release.py to read the final sealed deployment binding and assert:

1. each active capability has one explicit reuse policy;
2. Guru policy matches the gateway manifest and MCP_SESSION_ISOLATION.md;
3. any attested stateless binding has an expected attestation entry;
4. Feed/Filings cannot silently become attested merely because a YAML string changed.

This detects the current source/document/release-policy drift before installation.

- [ ] **Step 2: Certify Guru first**

Guru is already documented as pure read-only and the local gateway materializer declares it attested-stateless. Verify the generated release actually carries that policy.

If readiness and initialize attestation match, switch only the generated Guru bindings to attested-stateless-v1. If either attestation differs, leave it run-scoped and fail the release verification with a configuration error; do not bypass the check.

- [ ] **Step 3: Add public Feed stateless proof**

The test must initialize two independent sessions, issue different public feed calls, and prove that:

- tool list is identical;
- result is determined only by the request;
- no previous issue ID, cursor, or auth-derived user state affects the next call;
- server creates no session identifier.

Only the public Feed path is in scope. Any future personalized Feed tool remains run-scoped.

- [ ] **Step 4: Add public Filings stateless proof**

Repeat the proof for public filing tools. Add a negative test showing a personal-filings authorization header changes the available tool surface, and therefore personal tools remain excluded from attested reuse.

Do not enable stateless reuse for a mixed public/personal binding unless the public and personal endpoints are split or the gateway proves the public route has no user identity.

- [ ] **Step 5: Promote one family at a time**

Promotion order:

1. Guru;
2. public Feed, only after Task 3 tests pass;
3. public Filings, only after the personal tool boundary test passes.

Each promotion is one configuration-only release with rollback to run-scoped. Do not combine it with a workflow/prompt change.

- [ ] **Step 6: Add bounded connection observability**

Add only these tracing fields at MCP acquire/initialize:

~~~text
capability_id
pool_reused
initialize_performed
tool_session_reuse
elapsed_ms
~~~

Do not log question text, arguments, response bodies, user identifiers, credentials, or raw errors.

- [ ] **Step 7: Run focused verification**

Run:

~~~bash
cargo test -p krw-agent-tool-mcp
python3 scripts/test_dual_provider_release.py
~~~

Then, from the front repository:

~~~bash
npm exec vitest run src/krw-feed-mcp/server.test.ts src/filings-mcp/server.test.ts
~~~

- [ ] **Step 8: Commit**

Commit the policy checker and agent binding changes together. Commit the front attestation/isolation changes separately so either release can be rolled back without modifying research behavior.

### Task 5: Reduce Feed MCP Data Transfer Without New Caches or New Database Infrastructure

**Files:**

- Modify: ~/krw-ontology-front/src/krw-feed-mcp/feed-reader.ts
- Modify: ~/krw-ontology-front/src/krw-feed-mcp/server.test.ts
- Test: ~/krw-ontology-front/src/krw-feed-mcp/worker-bundle.test.ts

**Interfaces:**

- Public Feed result schemas remain byte-for-byte compatible for the same fixture data.
- Adds local SELECT column constants, not a new RPC, materialized view, cache layer, or database migration.

- [ ] **Step 1: Write output-equivalence fixtures**

Capture fixtures for listFeedItems, getFeedItems, and getFeedContext containing:

- one issue with two entities;
- one source post;
- one source material with media;
- one research packet;
- one missing issue ID.

Assert the serialized output retains the same fields, ordering, context_hash behavior, and original-text truncation semantics.

- [x] **Step 2: Run focused tests and record current query shape**

Run:

~~~bash
npm exec vitest run src/krw-feed-mcp/server.test.ts
~~~

Record the current select(*) locations in feed-reader.ts. Do not benchmark via a live production database.

- [x] **Step 3: Replace select(*) with named projections**

Define one column list per table based on the properties actually read by serializeIssue and loadResearchArtifacts. Keep all joins as the current bounded calls:

- market_issues;
- market_issue_entities;
- market_issue_posts;
- market_issue_items;
- market_source_items;
- market_issue_sources;
- market_source_media;
- market_issue_research_packets.

Keep the existing MAX_ISSUES, MAX_POSTS_PER_ISSUE, and source-text budget exactly unchanged.

- [x] **Step 4: Remove only local repeated scans**

Build maps for links by issue ID and source items by ID before iterating issue bundles. Do not redesign the Supabase access layer or introduce an unbounded cross-request cache.

- [x] **Step 5: Run output and bundle checks**

Run:

~~~bash
npm exec vitest run src/krw-feed-mcp/server.test.ts src/krw-feed-mcp/worker-bundle.test.ts
~~~

- [ ] **Step 6: Commit**

~~~bash
git add src/krw-feed-mcp/feed-reader.ts src/krw-feed-mcp/server.test.ts src/krw-feed-mcp/worker-bundle.test.ts
git commit -m "perf: narrow feed MCP database projections"
~~~

### Task 6: Verify Quality, Release the Two Providers, and Roll Back Safely

**Files:**

- Modify: scripts/test_dual_provider_release.py
- Modify: docs/MCP_SESSION_ISOLATION.md
- Modify: docs/release/dual-provider-cutover-file-inventory.md
- Reuse: fixtures/live-quality/v1/* and scripts/run_live_quality_matrix.py

**Interfaces:**

- Input fixture has three fixed cases:

~~~json
{
  "schema_version": 1,
  "cases": [
    {"id": "company-basic", "ticker": "CAT", "question": "무슨 기업이야?"},
    {"id": "period-explicit", "ticker": "AAPL", "question": "FY2024 10-K 기준 매출 흐름을 정리해줘."},
    {"id": "follow-up", "ticker": "AVGO", "question": "그 리스크가 현금흐름에는 어떤 영향을 줄 수 있어?"}
  ]
}
~~~

- Output records only bounded run facts: final/non-final, capability names, supplemental status kinds, evidence count, elapsed stage timings, and rendered answer hash. It must not retain prompt text beyond the fixture itself or provider reasoning.

- [x] **Step 1: Add deterministic negative fixtures**

Add synthetic capability payload fixtures for:

- targeted input rejection;
- targeted truncation;
- trace ambiguity;
- temporary MCP dependency failure.

These run in Rust/TypeScript unit tests and do not require an LLM or external service.

- [x] **Step 2: Add a GLM-only live quality runner**

The existing runner reuses the current daemon/release and executes the selected
corpus cases. It reports:

- response received;
- no statement of company non-disclosure when a retained status says incomplete/non-absence;
- session follow-up uses the same session ID;
- current tool set, model turn count, and reasoning configuration are unchanged from the baseline release.

The runner is review-oriented: it must mark a factual judgement as manual_review_required rather than inventing a false automated fact score.

- [x] **Step 3: Build both sealed providers without live DeepSeek quality calls**

Run the existing dual-provider release build. Verify:

- GLM and DeepSeek manifests contain the same MCP capability matrix;
- only provider endpoint/model settings differ;
- each sealed binding passes the reuse-policy matrix test;
- release authorization and TLS fingerprints remain pinned.

- [x] **Step 4: Release in reversible order**

1. Release Tasks 1–3 with all Feed/Filings still run-scoped.
2. Confirm deterministic tests and GLM quality review.
3. Release Guru stateless reuse if its attestation gate passes.
4. Release Feed and Filings reuse one family at a time only after their isolation tests pass.
5. Release Feed query projections last; they are independent of research correctness.

- [x] **Step 5: Define rollback**

| Change | Rollback |
| --- | --- |
| supplemental status sidecar | deploy preceding sealed Rust release |
| failed response projection | apply paired down migration only if no production receipts depend on it; otherwise deploy a forward corrective migration |
| stateless reuse | set only the affected binding back to run-scoped and rebuild/reseal |
| feed query projection | deploy preceding front commit |

Never delete run data, messages, evidence, session memory, Docker images, or release roots as part of rollback.

- [x] **Step 6: Final verification**

Run focused unit/contract tests first. Run GLM live quality last. Record only:

~~~text
release_id
provider
case_id
final_status
supplemental_statuses
pool_reused_count
initialize_count
elapsed_ms
answer_hash
manual_review_required
~~~

- [ ] **Step 7: Commit**

Commit fixtures, runner, and release checklist separately from runtime behavior changes.

## Acceptance Criteria

1. A targeted or trace MCP application error can never be interpreted as a normal empty result in compacted context.
2. Pagination/truncation from a supplemental MCP read survives compaction in bounded form.
3. A normal empty result remains usable and cacheable; input-invalid/ambiguous results are visible but not cached as successful evidence.
4. No new LLM turn, automatic retry, capability reduction, reasoning downgrade, or final-answer hard gate is introduced.
5. A terminal MCP/provider failure produces exactly one safe product response when the product database is available, and it never claims company non-disclosure.
6. Stateless connection reuse is enabled only after readiness + initialize attestation and behavior-isolation tests pass.
7. Public Feed output remains contract-compatible after its query narrowing.
8. GLM quality runs show the same workflow breadth as before, with additional retained status rather than additional control prompts.
9. Sealed GLM and DeepSeek releases have matching MCP policy matrices.

## Self-Review

- Scope coverage: Tasks 1–2 address false absence, targeted query/trace status loss, pagination loss, and compaction loss. Task 3 addresses the user-visible failure path. Task 4 addresses initialization overhead without weakening isolation. Task 5 addresses Feed transfer/memory cost. Task 6 covers GLM quality, dual-provider packaging, rollout, and rollback.
- Failure-mode check: the plan adds metadata and bounded classifications, not extra model rules, retries, model turns, or blocking final validators.
- Contract check: public success MCP schemas and the ontology schema remain unchanged. All newly introduced shapes are internal Rust sidecars, outbox payload metadata, or readiness attestation already required by the transport contract.
- Deployment check: every potentially risky session-reuse change is independently reversible by configuration and requires an explicit proof before promotion.

## Current implementation update (2026-08-14)

The model-free audit found one additional retrieval defect in the serving
runtime: metric projections were filtering their derived observation period,
but not always the joined source filing object. An explicit `periods` or
`document_types` read could therefore return a metric attached to an older or
different source filing.

- `services/krw-ontology-runtime/.../agent_index/store.py` now applies the
  caller's source-object scope to normal, dimensioned, and planned metric
  joins. Metric observation periods remain separate from source filing
  periods, so a comparative value carried by a newer filing is not discarded.
  No public MCP or ontology schema changed.
- `tests/test_query_plan_normalization.py` contains a SQLite regression fixture
  for stale-source and wrong-document rows.
- `scripts/run_direct_mcp_audit.py` performs bounded, provider-free readiness,
  tool-list, ticker, pagination, error-vs-empty, chain-shape, and explicit
  filing-scope checks. It never calls a model or prints secret values.
- `scripts/test_direct_mcp_audit.py` covers the audit helpers offline.

Source/runtime verification: 55 Python tests and 10 direct-audit unit tests
pass. The model-free audit is now runnable against the active gateway without
provider calls; it uses the current frontend Feed/Filings credentials when
requested so an operator can distinguish an actual MCP failure from a stale
operator-env token.

A sealed GLM/DeepSeek candidate for commit `061a6d7` is already present under
`krw-agnet-prod/releases/20260814140241-39590-061a6d7ce622`; both provider
bundles and the dual-release index verify successfully. It does not yet contain
the uncommitted source-period join fix above, and it has not been promoted to
`releases/current`.

The existing candidate was re-verified without starting either provider: both
standalone manifests pass (`glm-5.2` and `deepseek-v4-flash`, 261 files each),
the dual-provider release contract tests pass, and the GLM live matrix dry-run
enumerates 18 cases without making a model request. Provider acceptance dry-runs
also pass for both sealed lanes (9 cases each). The candidate is still not
installed or promoted.

An additional temporary clean snapshot including the source-period fix built
both provider candidates successfully (`krw-agentd` production build in 1m38s,
all image hashes verified). Its GLM and DeepSeek bundles each passed the
standalone verifier and dual-provider matrix. A separate loopback capabilityd
using the current source then returned HTTP 200 for an explicit `AAPL/FY2022/
10-K` `krw_ontology_query_context` call; the returned metric evidence stayed
inside the FY2022/CY2022 annual source filing scope. That daemon was stopped
after the probe and the operator gateway was untouched.

The same model-free probe found an operator configuration mismatch on the
local Feed gateway: the token in `krw-agent-deploy.env` is not the token
accepted by the currently running Feed worker (the front worker token succeeds
with the same HTTPS gateway). The audit never prints either value. This must be
re-synchronised during the next local/prod gateway install; it is not a reason
to weaken Feed authentication or make the Feed endpoint stateless.

The frontend worker-env generator now copies `KRW_FEED_MCP_TOKEN` (or the
legacy `FEED_MCP_AUTH_TOKEN` alias), Feed endpoint, and enabled state into the
generated local runtime envelope. It also accepts `export KEY=value` source
files. A secret-free hash comparison confirmed that the generated value matches
the source `.env`; the existing external `krw-agent-deploy.env` remains
unchanged until the operator regenerates or deliberately updates that file.

The frontend `prod:deploy:full` wrapper now closes that gap without modifying
the operator file: when the current front `.env` has a Feed credential, it
creates a temporary mode-600 runtime overlay, passes it to sealed-release
creation and the Rust daemon activation, then removes the overlay on success,
failure, or signal. The credential is never copied into the release bundle,
GCP archive, or public descriptor.

The analyst/composer guidance now also distinguishes “related evidence” from
“no evidence”: a covered clause with related support is rendered as a
conditional, investor-useful insight instead of being rewritten as company
non-disclosure. This is prompt guidance only; it adds no model turn, tool call,
hard final gate, or new runtime failure branch.

Live model-free verification against the current local gateway now separates
the two cases: with `--front-env-file` pointing at the current front env,
Ontology, Feed, Filings, and Guru all pass readiness, tool-shape, ticker,
pagination, chain, explicit source-filing scope, and error-vs-empty checks
(`provider_calls=0`) for AAPL, CAT, and AVGO; using the older credential from
the operator runtime env reproduces only Feed HTTP 401. This is an operational
credential mismatch, not a retrieval-empty result.
The audit now reads nested source-filing periods for metric projections, so
comparative FY periods are not mistaken for an out-of-scope filing. The source
capabilityd was also run on an isolated loopback port and returned the same
explicit-scope `krw_ontology_query` successfully. No production daemon or
gateway was changed by this probe.

The existing long-lived GLM quality stack was reused for a bounded one-case
per-bucket live pass (no stack restart and no provider configuration change).
The short case completed with a final answer after 8 provider turns, 2 MCP
calls, and one repair; its content still requires human grounding review. The
normal case ended with the existing safe `model_response` retry response after
one `ontology.company_context` action, which is a provider/protocol recovery
failure rather than an MCP empty result. The complex case progressed through
six provider episodes and three accepted MCP actions but exceeded the 600
second runner bound; the test run was then explicitly cancelled so it could
not remain active. This confirms the current stack's operational behavior,
but it is not a promotion gate for the uncommitted source tree.
