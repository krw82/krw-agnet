# Chat Session Continuity and Runtime Performance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 여러 회원이 여러 채팅방에서 동시에 리서치하더라도 각 채팅방의 후속 질문이 정확히 이어지고 서로 섞이지 않게 하면서, 모델 호출 횟수·추론 수준·capability 개수를 줄이지 않고 하네스 내부의 대기시간·CPU·메모리 낭비를 줄인다.

**Architecture:** 제품 계층인 `~/krw-ontology-front`가 회원, 채팅방, 메시지 목록, 제목, 삭제, pagination, reconnect의 source of truth다. 실행 계층인 `krw-agent`는 프런트가 인증한 `tenant_id + principal_id + session_id + run_id`를 그대로 받아 실행 queue, checkpoint, evidence, session-memory만 소유한다. `principal_id`는 회원 UUID, `session_id`는 선택한 `chat_sessions.id`, `run_id`는 제품 `agent_runs.id`와 동일하게 고정한다. 같은 채팅방은 한 번에 하나만 실행하고 서로 다른 채팅방은 병렬 처리하되 principal fair queue로 한 회원이 전체 worker를 독점하지 못하게 한다. Provider에는 해당 채팅방의 최신 연속 대화 꼬리를 항상 시간순으로 제공하고, 더 오래된 턴은 같은 채팅방 안에서만 현재 질문과 trusted ticker에 따라 보조 선택한다. 성능 개선은 provider permit 범위, immutable plan 재사용, 요청 직렬화, 세션 delta 적용, connection/process 수명주기를 최적화한다.

**Tech Stack:** Rust 1.97.1, Tokio, serde/serde_jcs, reqwest, PostgreSQL, TypeScript/Node.js, Anthropic Messages-compatible GLM provider.

## Global Constraints

- 수정 범위는 `~/krw-agnet`뿐이다.
- `~/krw-ontology-front`는 제품 계약 확인을 위한 read-only reference다. 이 계획은 그 저장소를 수정하지 않는다.
- `~/krw-ontology`는 읽거나 수정하지 않는다.
- 온톨로지 스키마, ontology capability 입출력 계약, MCP 도구 계약을 변경하지 않는다.
- 모델 호출 횟수, GLM 추론 수준, 모델별 output budget, capability 개수와 허용 capability 목록을 줄이지 않는다.
- 세션 컨텍스트는 같은 `tenant_id + principal_id + session_id` 안에서만 복원한다. 다른 세션을 자동 검색하거나 섞는 개인 메모리를 만들지 않는다.
- 채팅방 생성·목록·제목·soft delete·메시지 pagination·SSE reconnect는 프런트와 제품 DB가 담당한다. `krw-agent`에 두 번째 제품용 채팅방 저장소나 목록 API를 만들지 않는다.
- 과거 채팅 재개는 프런트에서 사용자가 선택한 정확한 `session_id`로만 한다. 하네스는 `latest`, `아까 채팅`, 사용자 전체 검색 같은 모호한 별칭을 해석하지 않는다.
- 과거 답변은 `context_only`다. 현재 실행의 evidence 권한으로 승격하지 않으며, 현재 사실 주장은 기존 evidence gate에서 다시 검증한다.
- 일반 회사 리서치의 Markdown 최종 출력 계약을 이번 계획에서 AnswerIR로 바꾸지 않는다. Markdown도 `RecentTurnV3`에 질문과 답변이 저장되므로 일반 후속 대화에는 사용할 수 있다.
- GLM live 품질 검사는 마지막 수동 검증 단계에서만 수행한다. DeepSeek live 호출은 하지 않는다.
- 기존 dirty worktree의 사용자 변경을 덮어쓰거나 되돌리지 않는다.

---

## Corrected Session Semantics

한 회원은 여러 채팅방을 가질 수 있고 여러 회원의 채팅방이 동시에 실행될 수 있다. 여기서 `session`은 개인 전체 메모리가 아니라 제품의 채팅방 한 개다. 현재 구현은 같은 세션의 질문과 답변을 보관하지만, Provider에 세션 전체를 그대로 재생하지는 않는다.

- 저장 카탈로그: 최근 최대 16턴
- Provider view: 그중 최대 6턴
- 현재 문제: 6턴 모두 관련도 점수로 재정렬되므로 바로 직전 대화가 빠지거나 시간 순서가 흐트러질 수 있음
- 이번 목표: 최신 4턴을 연속된 대화 꼬리로 우선 보존하고, 남는 2자리만 같은 세션의 오래된 관련 턴으로 보충
- byte limit이 빡빡할 때: 보충 턴 → 가장 오래된 비필수 tail 순으로 제거하고, 가장 최근 2턴은 항상 유지

`claims`와 `unresolved_goals`는 장기 구조화 리서치용 보조 데이터다. 일반적인 “그럼 왜?”, “그 회사 리스크는?” 같은 후속 질문은 `RecentTurnV3`의 질문·답변 연속성이 핵심이므로, 이번 작업에서 개인 리서치 메모리나 ResearchCapsule을 추가하지 않는다.

### Product and harness ownership boundary

| Concern | Source of truth | `krw-agent` responsibility |
|---|---|---|
| 회원 인증·채팅방 소유권 | front/Supabase | 인증된 UUID tuple을 바꾸지 않고 검증 |
| 채팅방 목록·제목·preview·soft delete | front/Supabase | 저장하지 않음 |
| 원문 메시지 이력·pagination | front/Supabase | 제품 목록 API를 제공하지 않음 |
| 실행 queue·lease·checkpoint | `agent_v1` | 소유 |
| 후속 질문용 압축 context | session-memory | 같은 principal+session 안에서만 복원 |
| evidence·tool trace | `krw-agent` | 현재 실행 권한과 provenance 유지 |
| 최종 답변의 제품 반영·reconnect | front outbox projection/SSE | immutable terminal event를 중복 없이 방출 |

이미 존재하는 `runs_one_active_per_session_idx`는 같은 채팅방의 두 답변이 순서를 뒤집는 것을 막고, `principal_fair_queue`는 다수 채팅방을 연 한 회원이 다른 회원을 굶기지 않게 한다. 이 두 축을 합치되 “회원당 한 번에 한 실행”으로 축소하지 않는다. 같은 회원의 서로 다른 채팅방도 전역 자원 한도 안에서는 병렬 실행할 수 있어야 한다.

### Multi-user resource policy

- queued run마다 Tokio task, DB connection, provider client, MCP process를 미리 만들지 않는다. durable queue row는 싸게 유지하고 claim된 run만 bounded worker task를 가진다.
- DB/provider/MCP transport와 immutable release/image/compiled-plan cache는 process 단위로 공유한다. mutable checkpoint, prompt assembly, session-memory catalog는 run/room 단위로만 소유한다.
- provider semaphore는 전체 연구 run이 아니라 실제 GLM episode 동안만 잡는다. 한 사용자의 MCP 조회·checkpoint 시간이 다른 사용자의 GLM slot을 막지 않는다.
- cache key는 content hash + release/deployment binding처럼 immutable한 값만 사용한다. principal/session/question이 들어간 결과를 전역 cache에 넣지 않는다.
- overload 때 연구 단계를 줄이거나 reasoning을 낮추지 않는다. run을 durable queue에 둔 채 공정하게 기다리게 하고, queue wait와 active execution time을 분리해 측정한다.
- pool 크기는 회원 수가 아니라 configured worker/provider concurrency와 background worker 수를 기준으로 둔다. 20명에서 2,000명으로 늘어도 idle 회원당 상주 메모리나 connection이 생기지 않아야 한다.

### External compatibility prerequisite

현재 front의 `20260802230000_agent_v1_product_projection.sql`은 제품 enqueue에서 모델 이름을 `^deepseek-...$`로 제한하지만, 이 저장소의 host/runtime 계약은 `glm-5.2`만 허용한다. `~/krw-agnet`만 수정해서는 이 SQL 거부를 해결할 수 없다. 따라서 실제 제품 연결 전 front 측 projection이 release descriptor에서 pin된 `glm-5.2`를 허용하는지 별도 확인되어야 한다. 이 계획에서는 현재 저장소에 호환성 fixture와 preflight 검사를 두어 불일치를 조용히 통과시키지 않는다.

또한 front에는 Rust product host/projection/outbox 코드가 준비되어 있지만, 현재 live `/api/chat/run`은 여전히 `executorType: "claude_agent"`로 기존 queue를 호출한다. 따라서 아래 구조는 목표 production boundary이며, 실제 UI cutover에는 front의 별도 변경과 검증이 필요하다. 기존 Claude 채팅방을 그대로 이어갈 경우에도 현재 host는 browser/session-memory 주입을 금지하므로 자동 승계되지 않는다. 권장 cutover는 제품 DB가 완료된 user/assistant pair의 최근 bounded history를 읽고, agent가 `context_only`로만 받아들이는 일회성 trusted bootstrap 계약을 별도 설계하는 것이다. 이 bootstrap이 준비되지 않은 상태에서는 기존 방을 조용히 빈 문맥으로 이어가지 말고 새 방 시작을 명시해야 한다.

### Durable retention issue to fix before multi-user scale

`migrations/0008_row_retention.sql`은 session-memory를 남긴 채 오래된 terminal run을 지우려 하지만, 현재 frontier/delta/snapshot의 run foreign key는 `ON DELETE RESTRICT`다. 따라서 메모리 delta를 남긴 성공 run은 reaper의 마지막 `DELETE FROM runs`를 막아 transaction 전체를 rollback시킬 수 있다. 다회원 운영에서는 실행 graph와 DB 용량이 계속 증가할 수 있다.

해결 원칙은 채팅 연속성을 지우는 것이 아니다. 최종 답변에서 검증된 작은 source identity/hash tombstone을 session-memory와 함께 남기고, provider episode/action/checkpoint/immutable run graph는 기존 retention 기간 뒤 정리할 수 있게 분리한다. session-memory 자체는 front의 soft delete만으로 삭제하지 않는다. 제품이 채팅방을 영구 purge했다는 trusted lifecycle 요청이 오고 해당 방에 active/queued run이 없을 때만 별도 idempotent procedure로 제거한다.

## File Map

- Create `crates/session-memory/src/selection.rs`: 세션 내부 턴 선택·정규화 점수·시간순 조립만 담당한다.
- Modify `crates/session-memory/src/lib.rs`: `SessionViewQuery`를 받고 continuity tail과 보조 턴을 view에 투영한다.
- Modify `crates/runtime-persistence/src/memory.rs`: 질문 문자열 대신 세션 선택 입력을 받아 carrier를 만든다.
- Modify `crates/runtime-persistence/src/executor.rs`: 현재 요청의 trusted ticker를 세션 선택에 전달하고 provider permit의 범위를 모델 episode로 좁힌다.
- Create `crates/runtime-persistence/src/episode_provider.rs`: 모델 episode마다 semaphore permit을 획득·해제하는 Provider wrapper다.
- Modify `crates/run-engine/src/lib.rs`: 사전 컴파일 실행 계획과 준비된 provider 요청을 사용하고 세부 stage 시간을 기록한다.
- Create `crates/run-engine/src/timings.rs`: executor/provider/engine이 공유하는 bounded atomic stage timing accumulator다.
- Create `crates/run-engine/src/compiled_plan.rs`: 이미지별 immutable workflow/context plan을 한 번만 컴파일한다.
- Create `crates/provider-wire/src/prepared.rs`: canonical provider bytes, hash, footprint를 한 번만 생성한다.
- Modify `crates/provider-wire/src/lib.rs`: retry마다 동일 canonical bytes를 전송한다.
- Modify `crates/protocol/src/lib.rs`: 비과금용 세부 duration counter를 backward-compatible하게 추가한다.
- Modify `packages/host-ts/test/integration.test.ts`: 제품 회원/채팅방 UUID가 agent claim에 정확히 고정되는 boundary 테스트를 추가한다.
- Modify `crates/persistence/src/agent_v1.rs`: 같은 채팅방 직렬화, 다른 채팅방 병렬성, principal fairness 계약 테스트를 보강한다.
- Modify `packages/host-ts/INTEGRATION.md`: front가 제품 원장이고 local Gateway는 개발용 adapter임을 명시한다.
- Modify `packages/host-ts/src/local-gateway.ts`: 두 번째 제품 session store로 오용되지 않도록 개발 전용 경계를 주석과 startup log에 명시한다.
- Create `fixtures/product-chat/v1/multi-user-chat-bindings.json`: 다회원·다채팅방 identity/idempotency 호환성 fixture다.
- Create `scripts/check_product_projection_compat.py`: 외부 product projection SQL을 읽기 전용으로 검사하는 optional preflight다.
- Create `migrations/0016_session_memory_source_retention.sql`: memory provenance를 작은 tombstone으로 분리하고 영구 삭제된 방의 bounded retirement ABI를 추가한다.
- Modify `crates/persistence/src/agent_v1.rs`: retention/retirement strict request·response와 migration 계약 테스트를 추가한다.
- Modify `packages/host-ts/src/client.ts`: trusted product lifecycle용 `retireSessionMemory` client를 추가한다.
- Modify `packages/host-ts/src/contracts.ts`: exact-key retirement request/response 타입을 추가한다.
- Modify `migrations/README.md`: run retention과 chat-room memory retirement의 서로 다른 수명주기를 문서화한다.
- Modify `scripts/dev-stack.sh`: canonical singleton stack을 재사용하고 stale supervisor를 정리한다.
- Modify `scripts/start_local_agent_gateway_stack.sh`: ephemeral state의 PostgreSQL만 종료한다.
- Create `scripts/test_dev_stack_lifecycle.sh`: 실제 서비스를 띄우지 않는 supervisor/lease 수명주기 테스트다.
- Create `scripts/run_session_followup_quality.py`: 기존 Gateway를 재사용하는 multi-turn GLM 품질 runner다.
- Create `fixtures/live-quality/v1/session-followup-v1.json`: 같은 세션/다른 세션/재개 체인을 고정한다.
- Create `fixtures/performance/v1/multi-user-chat-load-v1.json`: 다회원·다채팅방 공정성/격리 부하 corpus다.
- Modify `crates/perf-harness/src/main.rs`: 모델 호출을 줄이지 않는 deterministic 다중 세션 부하 시나리오를 추가한다.
- Modify `docs/LOCAL_FEEDBACK_LOOP.md`: 변경 종류별 재시작 경계와 성능 검증 절차를 고정한다.

---

### Task 1: Lock the Chat-Session Continuity Contract

**Files:**
- Create: `crates/session-memory/src/selection.rs`
- Modify: `crates/session-memory/src/lib.rs:24-43,1075-1200,2235-2293`
- Test: `crates/session-memory/src/lib.rs` test module

**Interfaces:**
- Produces: `pub struct SessionViewQuery<'a> { pub question: &'a str, pub trusted_tickers: &'a [String] }`
- Produces: `SessionMemoryCatalogV3::select_view(&SessionViewQuery<'_>, usize) -> Result<SessionMemoryViewV3, MemoryError>`
- Preserves: `SessionMemoryViewV3` wire schema version 3 and `context_only` authority

- [ ] **Step 1: Add failing tests for contiguous recent context**

Add fixture helpers that append six direct-Markdown turns with unique `run_id`s. Add this test shape:

```rust
#[test]
fn view_keeps_latest_four_turns_in_chronological_order() {
    let catalog = six_markdown_turn_catalog();
    let tickers = vec![String::from("AAPL")];
    let query = SessionViewQuery {
        question: "그중 가장 중요한 원인은?",
        trusted_tickers: &tickers,
    };
    let view = catalog.select_view(&query, 256 * 1024).unwrap();
    let ids = view.recent_turns
        .iter()
        .map(|turn| turn.source_run_id.as_str())
        .collect::<Vec<_>>();
    assert!(ids.ends_with(&["run:3", "run:4", "run:5", "run:6"]));
    assert!(view.recent_turns.windows(2).all(|pair| {
        view.sources.get(&pair[0].source_run_id).unwrap().revision
            < view.sources.get(&pair[1].source_run_id).unwrap().revision
    }));
}
```

- [ ] **Step 2: Add failing tests for ticker-assisted older-turn selection**

Create a same-session sequence containing AAPL, MSFT, and NVDA answers. The newest four remain pinned. When one supplemental slot is available, a query with trusted ticker AAPL must choose an older AAPL turn over a zero-overlap MSFT turn. Do not filter all non-AAPL turns because comparison questions may legitimately refer to multiple companies.

```rust
#[test]
fn trusted_ticker_breaks_ties_only_inside_the_same_session() {
    let catalog = mixed_ticker_markdown_catalog();
    let tickers = vec![String::from("AAPL")];
    let query = SessionViewQuery {
        question: "이전 수익성과 비교해줘",
        trusted_tickers: &tickers,
    };
    let view = catalog.select_view(&query, 256 * 1024).unwrap();
    assert!(view.recent_turns.iter().any(|turn| turn.source_run_id == "run:aapl-old"));
    assert_eq!(view.session_id_hash, ContentHash::sha256(SESSION_ID));
}
```

- [ ] **Step 3: Run the focused tests and verify they fail for the expected reason**

Run:

```bash
cargo test -p krw-session-memory view_keeps_latest_four_turns_in_chronological_order
cargo test -p krw-session-memory trusted_ticker_breaks_ties_only_inside_the_same_session
```

Expected: the existing score-only selector either omits a recent turn or returns relevance order instead of chronological order.

- [ ] **Step 4: Define the closed selector input and deterministic ranking**

In `selection.rs`, add:

```rust
pub const TARGET_CONTINUITY_TURNS: usize = 4;
pub const MIN_CONTINUITY_TURNS: usize = 2;

pub struct SessionViewQuery<'a> {
    pub question: &'a str,
    pub trusted_tickers: &'a [String],
}

pub(crate) struct RankedCandidate {
    pub index: usize,
    pub score: u32,
}
```

Use the existing Korean/English token and 2–3 character n-gram normalization. For non-tail candidates use this bounded score:

```rust
score = lexical_overlap * 64 + trusted_ticker_hits * 256 + normalized_recency
```

where `normalized_recency` is `0..=31`. Reject a supplemental candidate when both lexical overlap and ticker hits are zero. Do not add embeddings, a vector database, or BM25 indexing: the candidate set is bounded to 16 turns, so those systems add more cost and failure modes than they remove.

- [ ] **Step 5: Assemble the view in continuity-first order**

Update `select_view` to:

1. Pin the newest `min(4, turn_count)` turns.
2. Rank only earlier turns for the remaining `MAX_VIEW_TURNS - pinned_count` slots.
3. Union pinned and supplemental indices.
4. Sort the final turns by source revision ascending before serializing.
5. Normalize claim/goal recency to the same `0..=31` range rather than mixing source revision and array index.
6. During byte shrinking, remove supplemental turns first, then the oldest pinned turns down to two. Never remove the newest turn before claims/goals/constraints.

Keep `SessionMemoryViewV3`, `RecentTurnV3`, snapshot version, and ontology schemas unchanged.

- [ ] **Step 6: Add bounded-size and cross-session regression tests**

Assert all of the following:

```rust
assert!(view.canonical_context().unwrap().len() <= MAX_SESSION_MEMORY_VIEW_BYTES);
assert_eq!(view.authority, MemoryAuthority::ContextOnly);
assert_eq!(view.session_id_hash, ContentHash::sha256(SESSION_ID));
assert!(other_session.apply_delta(&delta_from_first_session).is_err());
```

Also test a 32KiB user excerpt plus 32KiB answer excerpt so byte shrinking retains the newest two turns.

- [ ] **Step 7: Run the crate tests and commit**

```bash
cargo test -p krw-session-memory
cargo fmt --check
git add crates/session-memory/src/lib.rs crates/session-memory/src/selection.rs
git commit -m "fix: preserve contiguous chat session context"
```

---

### Task 2: Thread Trusted Session Scope Through Reconstruction

**Files:**
- Modify: `crates/runtime-persistence/src/memory.rs:267-340`
- Modify: `crates/runtime-persistence/src/executor.rs:541-590`
- Test: `crates/runtime-persistence/src/memory.rs` test module

**Interfaces:**
- Consumes: `SessionViewQuery<'_>` from Task 1
- Produces: `SessionMemoryPageAccumulator::finish(self, &SessionViewQuery<'_>)`

- [ ] **Step 1: Write a failing reconstruction test**

Build two completed pages in the same session and use a pronoun-only question. Assert the carrier contains the last two turns and validates its source frontier/hash.

```rust
let tickers = vec![String::from("AAPL")];
let query = SessionViewQuery {
    question: "그럼 가장 큰 위험은?",
    trusted_tickers: &tickers,
};
let resolved = accumulator.finish(&query).unwrap();
let carrier = resolved.carrier.unwrap();
carrier.validate_carrier().unwrap();
let view: SessionMemoryViewV3 = serde_json::from_value(carrier.canonical_view).unwrap();
let ids = view.recent_turns.iter()
    .map(|turn| turn.source_run_id.as_str())
    .collect::<Vec<_>>();
assert!(ids.ends_with(&["run:source:1", "run:source:2"]));
```

- [ ] **Step 2: Change `finish` to accept the selector input**

Replace `finish(self, question: &str)` with `finish(self, query: &SessionViewQuery<'_>)` and call `provider_catalog.select_view(query, MAX_SESSION_MEMORY_VIEW_BYTES)`.

- [ ] **Step 3: Pass only authenticated request scope from the executor**

At claim execution, construct:

```rust
let session_query = SessionViewQuery {
    question: &validated.request().question,
    trusted_tickers: validated.request().context.trusted_tickers(),
};
let resolved_memory = memory.finish(&session_query)?;
```

Do not accept ticker or session-memory data from model output, search results, or a caller-supplied memory carrier. The fenced worker remains the only reconstruction authority.

- [ ] **Step 4: Verify empty and tampered sessions still fail closed**

Run:

```bash
cargo test -p krw-agent-runtime-persistence memory::tests
```

Expected: empty session produces `carrier=None`; cross-session/tampered delta tests remain green.

- [ ] **Step 5: Commit**

```bash
git add crates/runtime-persistence/src/memory.rs crates/runtime-persistence/src/executor.rs
git commit -m "fix: bind follow-up context to trusted chat scope"
```

---

### Task 3: Lock the Multi-User Product Chat Boundary

**Files:**
- Modify: `packages/host-ts/test/integration.test.ts`
- Modify: `crates/persistence/src/agent_v1.rs`
- Modify: `packages/host-ts/INTEGRATION.md`
- Modify: `packages/host-ts/src/local-gateway.ts`
- Create: `fixtures/product-chat/v1/multi-user-chat-bindings.json`
- Create: `scripts/check_product_projection_compat.py`

**Interfaces:**
- Preserves: `AuthenticatedRunOwnershipV1` and the existing `agent_v1.enqueue_run` ABI
- Pins: `principal_id = authenticated user UUID`
- Pins: `session_id = selected chat_sessions.id`
- Pins: `run_id = product agent_runs.id = daemon run_id`
- Preserves: host enqueue carries `session_memory = null`; only the fenced worker reconstructs it

- [ ] **Step 1: Document one source of truth instead of creating another session service**

Add this deployment mapping to `INTEGRATION.md`:

```text
front auth.users.id      -> agent principal_id
front chat_sessions.id   -> agent session_id
front agent_runs.id      -> agent run_id
deployment/product realm -> agent tenant_id
```

The front remains responsible for session creation/listing, membership/ownership checks, title, preview, soft deletion, message pagination, and reconnect. `krw-agent` remains responsible for execution state, evidence, checkpoints, and context-only session memory. Explicitly state that `krw_gateway_local.sessions` is an isolated development adapter, not a production replica and not a source for the product sidebar.

- [ ] **Step 2: Add a strict product-binding fixture**

Create a closed JSON corpus with at least these cases:

1. one user, same room, two runs: identical tenant/principal/session and different run IDs;
2. one user, two rooms: identical principal and distinct session IDs;
3. two users, one tenant: distinct principal/session pairs;
4. exact retry: all four IDs and the preparation hash are unchanged;
5. tampered retry: changing principal, session, or run identity must fail closed.

Use UUID-shaped values matching the product schema, but no real user ID, question, answer, message body, evidence, or secret. Reject unknown fixture keys.

- [ ] **Step 3: Add host integration tests for the identity tuple**

For every fixture case, build `PreparedEnqueueRunV1` and assert the exact tuple appears unchanged in all three places:

```text
prepared.ownership
prepared.agent_request top-level
prepared.agent_request.immutable_snapshot.request
```

Also assert host materialization never accepts browser-supplied session memory, product run retry is byte-for-byte stable, and changing any identity field under the same run ID produces an identity/preparation conflict rather than a second logical run.

- [ ] **Step 4: Lock room serialization and user fairness at the runtime DB boundary**

Extend the migration contract tests to assert:

- `runs_one_active_per_session_idx` is keyed by `(tenant_id, session_id)`;
- the fair queue tail is keyed by `(tenant_id, principal_id)`;
- the session-memory frontier is keyed by `(tenant_id, principal_id, session_id)`;
- a second run in the same room remains queued while the first is active;
- runs in different rooms are claimable independently;
- repeated work from one principal does not jump ahead of already queued work from another principal.

Do not add a global “one active run per user” constraint. Product concurrency limits and provider capacity remain separate from per-room ordering.

- [ ] **Step 5: Make the local Gateway boundary unmistakable**

Keep its existing new/continue endpoints for local CLI and quality work. Add a startup message and documentation that it is `development_only`, contains no product messages/titles, and must not be deployed as the front's chat history service. Do not add session-list, `resume-latest`, title, preview, or soft-delete APIs here.

- [ ] **Step 6: Add a read-only product-projection compatibility preflight**

`scripts/check_product_projection_compat.py` accepts an explicit SQL file/directory plus an optional explicit read-only front root and checks that:

- `principal_id`, `session_id`, and `run_id` are mapped without rewriting;
- enqueue is service-owned/idempotent;
- the pinned model `glm-5.2` is accepted;
- terminal outbox projection is deduplicated.
- the product live route is wired to `rust_agent` rather than the legacy Claude queue;
- an explicit cutover policy exists for legacy rooms: trusted bounded bootstrap or forced new room.

It returns nonzero on the currently observed DeepSeek-only model regex and prints a concise incompatibility report. It never edits the supplied repository and is not invoked during normal runtime startup.

- [ ] **Step 7: Run the focused boundary checks**

```bash
npm --prefix packages/host-ts test
npm --prefix packages/host-ts run typecheck
cargo test -p krw-agent-persistence
python3 scripts/check_product_projection_compat.py --front-root ~/krw-ontology-front
```

The first three commands must pass. Until the front projection accepts `glm-5.2`, the last command is expected to fail and is a product-deployment blocker, not a harness test failure to suppress.

- [ ] **Step 8: Commit**

```bash
git add packages/host-ts/test/integration.test.ts packages/host-ts/INTEGRATION.md packages/host-ts/src/local-gateway.ts crates/persistence/src/agent_v1.rs fixtures/product-chat/v1/multi-user-chat-bindings.json scripts/check_product_projection_compat.py
git commit -m "test: lock multi-user chat room boundary"
```

---

### Task 4: Measure Local Runtime Time Separately from GLM Time

**Files:**
- Create: `crates/run-engine/src/timings.rs`
- Modify: `crates/protocol/src/lib.rs:153-178`
- Modify: `crates/runtime-persistence/src/executor.rs`
- Modify: `crates/run-engine/src/lib.rs`
- Modify: `packages/host-ts/src/atomic-final.ts`
- Test: protocol serde tests and run-engine usage tests

**Interfaces:**
- Produces backward-compatible `BudgetUsage` fields:
  `provider_queue_wait_ms`, `session_memory_total_ms`, `market_preflight_ms`, `prompt_build_total_ms`, `checkpoint_total_ms`
- Produces: `RuntimeStageTimings` and `RuntimeStageTimingSnapshot`

- [ ] **Step 1: Add serde compatibility tests before fields**

Test that old JSON without new fields deserializes all new counters to zero and new JSON round-trips exactly.

- [ ] **Step 2: Add duration counters with `#[serde(default)]`**

```rust
#[serde(default)]
pub provider_queue_wait_ms: u64,
#[serde(default)]
pub session_memory_total_ms: u64,
#[serde(default)]
pub market_preflight_ms: u64,
#[serde(default)]
pub prompt_build_total_ms: u64,
#[serde(default)]
pub checkpoint_total_ms: u64,
```

These are diagnostic counters, not billable token/credit fields.

- [ ] **Step 3: Add one shared, bounded timing accumulator**

Define a non-serializable runtime object backed by `AtomicU64`:

```rust
#[derive(Default)]
pub struct RuntimeStageTimings {
    provider_queue_wait_ms: AtomicU64,
    session_memory_total_ms: AtomicU64,
    market_preflight_ms: AtomicU64,
    prompt_build_total_ms: AtomicU64,
    checkpoint_total_ms: AtomicU64,
}

#[derive(Clone, Copy, Default)]
pub struct RuntimeStageTimingSnapshot {
    pub provider_queue_wait_ms: u64,
    pub session_memory_total_ms: u64,
    pub market_preflight_ms: u64,
    pub prompt_build_total_ms: u64,
    pub checkpoint_total_ms: u64,
}
```

Each `add_*` method converts elapsed milliseconds with saturation and uses `fetch_update`/`saturating_add` semantics. `snapshot()` performs relaxed loads because the counters are diagnostics, not synchronization authority.

The executor creates one `Arc<RuntimeStageTimings>` per run. It passes the same object to `PermitBoundProvider` and `RunInput`. `ActiveRun` stores a `durable_timing_base` copied from recovered `BudgetUsage` (zero for a new run). Before every durable checkpoint and final bundle it writes `durable_timing_base + current_atomic_snapshot` with saturating addition. This prevents recovery from resetting old timing or repeatedly adding the same current-process snapshot.

- [ ] **Step 4: Measure exact boundaries**

- `session_memory_total_ms`: first fenced memory read through carrier validation
- `provider_queue_wait_ms`: waiting for the model semaphore only
- `market_preflight_ms`: optional preflight call only
- `prompt_build_total_ms`: context plan, tool schema, trusted prompt, and canonical provider request preparation
- `checkpoint_total_ms`: durable episode/run-state checkpoint awaits

Use `Instant::elapsed`, saturating `u64` conversion, and recovery semantics identical to existing `provider_total_ms` handling.

Do not attach raw `principal_id`, `session_id`, question, ticker, or run ID as unbounded metrics labels. Aggregate by workload class, run kind, terminal state, and bounded timing bucket; individual run inspection continues through the authenticated trace path.

Enqueue-to-claim time starts before a worker owns the run, so do not force it into `BudgetUsage` or expand the strict claim ABI just for diagnostics. Task 11 measures it from durable enqueue/active timestamps in the performance harness and the product's existing run projection.

Until Task 5 moves the semaphore into `PermitBoundProvider`, measure queue wait around the existing acquisition point. Task 5 relocates the same `add_provider_queue_wait` call into the wrapper and removes the old call so time is never double-counted.

- [ ] **Step 5: Project counters through the existing safe usage response**

Add optional TypeScript fields and strict validation. Do not expose prompts, request bodies, provider reasoning, tool arguments, or database diagnostics.

- [ ] **Step 6: Test and commit**

```bash
cargo test -p krw-agent-protocol
cargo test -p krw-agent-run-engine usage
npm --prefix packages/host-ts test
git add crates/protocol/src/lib.rs crates/runtime-persistence/src/executor.rs crates/run-engine/src/lib.rs crates/run-engine/src/timings.rs packages/host-ts/src/atomic-final.ts
git commit -m "perf: expose bounded runtime stage timings"
```

---

### Task 5: Hold Provider Permits Only During Provider Episodes

**Files:**
- Create: `crates/runtime-persistence/src/episode_provider.rs`
- Modify: `crates/runtime-persistence/src/lib.rs`
- Modify: `crates/runtime-persistence/src/executor.rs:210-260,519-529,635-660`
- Test: `crates/runtime-persistence/src/episode_provider.rs`

**Interfaces:**
- Produces: `PermitBoundProvider<P>` implementing `krw_agent_run_engine::Provider`
- Preserves: descriptor `max_in_flight` as the exact concurrent provider-episode limit
- Consumes: `Arc<RuntimeStageTimings>` from Task 4

- [ ] **Step 1: Write concurrency tests with a fake provider**

Use a semaphore size of one and a fake provider blocked by `Notify`. Assert:

1. the first `complete` owns the permit;
2. a second `complete` waits;
3. releasing the first provider episode allows the second;
4. no permit is held before or after `complete`;
5. cancellation releases the permit.

- [ ] **Step 2: Implement the wrapper**

```rust
pub struct PermitBoundProvider<P> {
    inner: Arc<P>,
    permits: Arc<Semaphore>,
    timings: Arc<RuntimeStageTimings>,
}

#[async_trait]
impl<P: Provider> Provider for PermitBoundProvider<P> {
    async fn complete(
        &self,
        request: &MessagesRequest,
        context: &EpisodeContext,
    ) -> Result<ProviderEpisodeV1, DependencyFailure> {
        let started = Instant::now();
        let permit = self.permits.clone().acquire_owned().await.map_err(|_| {
            DependencyFailure::redacted(
                "provider_in_flight_closed",
                "provider semaphore was closed",
                true,
                DeliveryCertainty::NotDispatched,
            )
        })?;
        self.timings.add_provider_queue_wait(started.elapsed());
        let result = self.inner.complete(request, context).await;
        drop(permit);
        result
    }
}
```

The wrapper stores the per-run `Arc<RuntimeStageTimings>` and calls `add_provider_queue_wait(started.elapsed())`. Task 7 later changes only the request argument from `MessagesRequest` to `PreparedMessagesRequest`; permit behavior remains identical.

- [ ] **Step 3: Return a permit-bound provider from the catalog**

Replace the executor-level `acquire_permit` call with a catalog method accepting the run timing accumulator and returning an `Arc<PermitBoundProvider<ProviderClient>>` for the exact model. Remove the run-wide permit variable and its temporary Task 4 timing call.

- [ ] **Step 4: Verify capability and memory work do not consume provider slots**

Add an executor test with two runs where run A is waiting in a fake capability and run B reaches the provider. B must be admitted because A is not inside a provider episode.

- [ ] **Step 5: Test and commit**

```bash
cargo test -p krw-agent-runtime-persistence episode_provider
cargo test -p krw-agent-runtime-persistence executor
git add crates/runtime-persistence/src/episode_provider.rs crates/runtime-persistence/src/lib.rs crates/runtime-persistence/src/executor.rs
git commit -m "perf: scope concurrency permits to provider episodes"
```

---

### Task 6: Reuse Image-Compiled Workflow and Context Plans

**Files:**
- Create: `crates/run-engine/src/compiled_plan.rs`
- Modify: `crates/run-engine/src/lib.rs:1717-1740,1932-1965,4274-4317,4490-4515`
- Modify: `crates/runtime-persistence/src/executor.rs:280-333,635-665`
- Test: run-engine compiled plan tests and release-catalog tests

**Interfaces:**
- Produces: `pub struct CompiledExecutionPlan`
- Produces: `CompiledExecutionPlan::compile(image, run_kind, locale)`
- Consumes: exact image hash, run kind, and locale

- [ ] **Step 1: Write hash/scope tests**

Compile a plan for `company_research/ko-KR`. Assert it accepts an exact matching request and rejects a changed image hash, run kind, or locale before the first Provider call.

- [ ] **Step 2: Define the immutable plan**

```rust
pub struct CompiledExecutionPlan {
    image_hash: ContentHash,
    run_kind: String,
    locale: String,
    program: Arc<ProgramRuntime>,
    context_planner: Arc<ContextPlanner>,
}
```

Move the current `ProgramRuntime` implementation into `compiled_plan.rs` as `pub(crate) struct ProgramRuntime`; its fields remain private. `ProgramRuntime` and `ContextPlanner` are immutable during a run. Change `ActiveRun.program` and `ActiveRun.context_planner` to `Arc` references rather than rebuilding owned copies.

- [ ] **Step 3: Compile every entrypoint once in `ProductionReleaseEntry`**

At release catalog startup, build a map keyed by `(run_kind, locale)`. Reject duplicate entrypoints and any plan whose image hash differs from the release image. Replace the currently stored-but-unused standalone `context_planner` field with this map.

- [ ] **Step 4: Require the exact compiled plan in production `RunInput`**

Add `execution_plan: Arc<CompiledExecutionPlan>` to `RunInput`. `run_inner` validates the plan, clones two `Arc`s into `ActiveRun`, and no longer calls `ProgramRuntime::compile` or `ContextPlanner::compile` per run.

- [ ] **Step 5: Update fixture constructors explicitly**

Every test fixture compiles its plan once when constructing the fixture. Do not add an implicit production fallback that could silently reintroduce per-run compilation.

- [ ] **Step 6: Test and commit**

```bash
cargo test -p krw-agent-run-engine compiled_plan
cargo test -p krw-agent-runtime-persistence release
git add crates/run-engine/src/compiled_plan.rs crates/run-engine/src/lib.rs crates/runtime-persistence/src/executor.rs
git commit -m "perf: reuse immutable execution plans"
```

---

### Task 7: Canonicalize Each Provider Request Once

**Files:**
- Create: `crates/provider-wire/src/prepared.rs`
- Modify: `crates/provider-wire/src/lib.rs:942-959,1403-1525`
- Modify: `crates/run-engine/src/lib.rs` Provider trait, request builder, live/recovery call sites, and mocks
- Modify: `crates/runtime-persistence/src/episode_provider.rs`
- Modify: `crates/provider-wire/Cargo.toml`
- Test: provider-wire canonical request and retry tests

**Interfaces:**
- Produces: `PreparedMessagesRequest`
- Changes: `Provider::complete(&PreparedMessagesRequest, &EpisodeContext)`

- [ ] **Step 1: Add a one-serialization test hook**

Construct a request, prepare it once, retry it three times against a local fake HTTP server, and assert all request bodies are byte-identical and the hash equals `sha256(body)`.

- [ ] **Step 2: Define the prepared request**

```rust
pub struct PreparedMessagesRequest {
    request: MessagesRequest,
    canonical_bytes: bytes::Bytes,
    request_hash: ContentHash,
    footprint: ProviderRequestFootprint,
}

impl PreparedMessagesRequest {
    pub fn new(request: MessagesRequest) -> Result<Self, WireError> {
        request.validate()?;
        let canonical_bytes = bytes::Bytes::from(serde_jcs::to_vec(&request)?);
        let request_hash = ContentHash::sha256(canonical_bytes.as_ref());
        let footprint = ProviderRequestFootprint::from_len(canonical_bytes.len())?;
        Ok(Self { request, canonical_bytes, request_hash, footprint })
    }
}

impl ProviderRequestFootprint {
    fn from_len(canonical_bytes: usize) -> Result<Self, WireError> {
        Ok(Self {
            canonical_bytes,
            input_tokens_upper_bound: u64::try_from(canonical_bytes)
                .map_err(|_| WireError::RequestFootprintOverflow)?,
        })
    }
}
```

Expose read-only accessors for model, messages, tools, hash, bytes, and footprint. Keep the original `MessagesRequest` for tests and episode assembly metadata.

- [ ] **Step 3: Prepare at the run-engine boundary**

`build_provider_request` constructs `MessagesRequest` and immediately converts it to `PreparedMessagesRequest`. Reuse its hash for durable request receipts, recovery comparison, debug output, and episode verification. Remove redundant `serde_jcs::to_vec` calls at the live call site.

- [ ] **Step 4: Send exact canonical bytes on every retry**

Replace reqwest `.json(request)` with:

```rust
.header(reqwest::header::CONTENT_TYPE, "application/json")
.body(prepared.canonical_bytes().clone())
```

The SSE assembler receives `prepared.request_hash().clone()`. Retry logic, status handling, and call count remain unchanged.

- [ ] **Step 5: Update fake providers and recovery tests**

Mocks inspect `prepared.request()` and return episodes bound to `prepared.request_hash()`. No test may recompute a different JSON serialization.

- [ ] **Step 6: Test and commit**

```bash
cargo test -p krw-agent-provider-wire
cargo test -p krw-agent-run-engine provider
git add crates/provider-wire/src/prepared.rs crates/provider-wire/src/lib.rs crates/provider-wire/Cargo.toml crates/run-engine/src/lib.rs crates/runtime-persistence/src/episode_provider.rs
git commit -m "perf: serialize provider requests once"
```

---

### Task 8: Remove Full-Catalog Cloning from Session Delta Application

**Files:**
- Modify: `crates/session-memory/src/lib.rs:737-865`
- Test: `crates/session-memory/src/lib.rs` test module

**Interfaces:**
- Produces internal `ValidatedDeltaPlan`
- Preserves exact frontier hash, source lineage hash, snapshots, and all fail-closed behavior

- [ ] **Step 1: Add equivalence and failure-atomicity tests**

Keep a test-only reference function that applies a delta to a cloned catalog using the current implementation. For typed and Markdown deltas, assert the new implementation produces identical canonical snapshots/frontier/source lineage. For every invalid delta case, serialize the catalog before and after and assert it is unchanged.

- [ ] **Step 2: Split validation from mutation**

Add:

```rust
struct ValidatedDeltaPlan {
    next_frontier_hash: ContentHash,
    next_source_lineage_hash: ContentHash,
    source: MemorySourceV3,
    // only validated inserts/replacements for touched collections
}

fn validate_delta_transition(
    catalog: &SessionMemoryCatalogV3,
    delta: &SessionMemoryDeltaV3,
) -> Result<ValidatedDeltaPlan, MemoryError>;
```

All fallible canonicalization, lineage, duplicate, supersession, resolution, and bound checks happen before mutation.

- [ ] **Step 3: Commit the validated plan without cloning the catalog**

After validation succeeds, mutate only touched maps/sets, append the recent turn, drop oldest turns above `MAX_RECENT_TURNS`, and assign the precomputed revision/frontier/lineage. `commit_validated_delta` must contain no validation branches that can return an error after the first mutation.

- [ ] **Step 4: Verify long-session behavior**

Apply 64 deltas, create snapshots at the existing threshold, restore, and compare the final view/hash with the reference implementation.

- [ ] **Step 5: Test and commit**

```bash
cargo test -p krw-session-memory
cargo test -p krw-agent-runtime-persistence memory
git add crates/session-memory/src/lib.rs
git commit -m "perf: apply session deltas without catalog clones"
```

---

### Task 9: Decouple Run Retention from Chat-Room Continuity

**Files:**
- Create: `migrations/0016_session_memory_source_retention.sql`
- Modify: `crates/persistence/src/agent_v1.rs`
- Modify: `packages/host-ts/src/contracts.ts`
- Modify: `packages/host-ts/src/client.ts`
- Modify: `packages/host-ts/src/procedure.ts`
- Modify: `packages/host-ts/test/integration.test.ts`
- Modify: `migrations/README.md`

**Interfaces:**
- Produces: compact `agent_store.session_memory_sources` provenance rows
- Produces: `agent_v1.retire_session_memory(jsonb)` for trusted permanent product purge only
- Preserves: normal session-memory snapshot-tail read and `context_only` authority
- Preserves: front soft deletion without agent-memory deletion

- [ ] **Step 1: Reproduce the retention conflict in a PostgreSQL integration test**

Commit one final run with a valid session-memory delta, age the terminal run past the retention cutoff, and invoke `reap_retained_runs`. The pre-migration fixture must demonstrate that the run-side `ON DELETE RESTRICT` relationship prevents the intended cleanup. Also include a terminal run without session memory to prove the test distinguishes the two cases.

- [ ] **Step 2: Add compact, immutable source tombstones**

Create `session_memory_sources` keyed by `(run_id, tenant_id, principal_id, session_id)` and containing only bounded source hashes already validated during final commit: answer bundle, final output, final-commit intent, and commit timestamp. Backfill it from existing append-only deltas.

Add an internal trigger on session-memory delta insertion that independently verifies the source run owner and the hashes in the delta, then inserts the tombstone in the same final-commit transaction. Repoint frontier/delta source ownership to the tombstone. A snapshot is already owner-, revision-, frontier-, lineage-, and content-hash-bound, so remove only its `checkpoint_run_id -> runs` retention FK while retaining the checkpoint ID as audit metadata. Do not weaken any write privilege or append-only trigger.

- [ ] **Step 3: Prove heavy run graphs can be reaped without losing follow-up context**

After migration, reap the old terminal run and assert provider episodes, actions, checkpoints, answer bundle, mutations, and the full run row are deleted, while the compact source, frontier, snapshot/delta, and canonical memory view remain valid. Enqueue a new run for the same principal/session and verify snapshot-tail reconstruction returns the previous question/answer context.

- [ ] **Step 4: Add explicit, idempotent permanent-room retirement**

Define an exact request containing:

```text
abi_version, mutation_id, mutation_hash,
tenant_id, principal_id, session_id,
lifecycle_receipt_hash, reason_code=product_hard_purge
```

The SECURITY DEFINER procedure locks the session row, rejects retirement while any queued/deferred/active run exists, verifies owner scope, records a bounded idempotency receipt, and deletes only that room's snapshots, deltas, frontier, and source tombstones in FK order. The existing append-only delete guards may be bypassed only inside this procedure through a transaction-local maintenance flag; runtime roles still have no direct table delete permission.

Soft delete, logout, account switching, a known session UUID, or inactivity alone must never invoke this path. Until the product sends a trusted hard-purge lifecycle request, memory remains available for explicit room resume.

- [ ] **Step 5: Add a strict host client, without making the browser authoritative**

Expose `retireSessionMemory` only from the server-side host client. Validate exact keys, bounded identifiers, canonical mutation hash, and response identity. Do not expose it through local browser CORS routes or accept a raw chat-history payload. The product service must first authenticate the user/session lifecycle event; the agent only consumes the resulting trusted receipt.

- [ ] **Step 6: Add isolation, retry, and capacity tests**

Assert:

- owner A cannot retire owner B's room;
- retiring room A does not touch room B for the same user;
- exact retry returns the stored result and conflicting retry fails;
- normal run reaping and room retirement can execute concurrently without deadlock;
- normal snapshot-tail reads remain bounded after 64 turns;
- report tombstone, delta, and snapshot bytes per 1,000 rooms so DB growth is visible.

- [ ] **Step 7: Test and commit**

```bash
cargo test -p krw-agent-persistence
npm --prefix packages/host-ts test
npm --prefix packages/host-ts run typecheck
git add migrations/0016_session_memory_source_retention.sql migrations/README.md crates/persistence/src/agent_v1.rs packages/host-ts/src/contracts.ts packages/host-ts/src/client.ts packages/host-ts/src/procedure.ts packages/host-ts/test/integration.test.ts
git commit -m "fix: retain chat context without retaining full runs"
```

---

### Task 10: Enforce One Reusable Local Stack

**Files:**
- Modify: `scripts/dev-stack.sh`
- Modify: `scripts/start_local_agent_gateway_stack.sh`
- Create: `scripts/test_dev_stack_lifecycle.sh`
- Modify: `docs/LOCAL_FEEDBACK_LOOP.md`
- Test: shell syntax and lifecycle smoke commands

**Interfaces:**
- Produces: canonical persistent stack lease under the existing dev state directory
- Produces: explicit `persistent` versus `ephemeral` PostgreSQL ownership marker

- [ ] **Step 1: Add a shell-level lifecycle fixture**

Use a temporary state directory and stub `pg_ctl`/service commands. Assert:

- a healthy canonical supervisor is reused;
- a stale PID file is removed only after `kill -0` fails;
- an ephemeral stack cleanup requests PostgreSQL shutdown;
- a persistent canonical stack cleanup leaves PostgreSQL running;
- two concurrent `up` commands cannot start two supervisors.

`scripts/test_dev_stack_lifecycle.sh` creates its temporary directory with `mktemp -d`, installs command stubs only beneath that directory, validates the resolved path is beneath the temporary root, and removes only that exact directory in its exit trap.

- [ ] **Step 2: Add an atomic singleton lease**

Use an atomic `mkdir` lock directory containing PID, start time, state path, and heartbeat timestamp. Validate the resolved state path before any cleanup. Never use an unresolved environment variable or a broad directory as a delete target.

- [ ] **Step 3: Mark PostgreSQL ownership explicitly**

Create a small marker in the exact state directory:

```text
postgres-lifecycle=persistent
```

for `dev-stack.sh`, and `postgres-lifecycle=ephemeral` for temporary acceptance stacks. The cleanup function reads only this validated marker; ephemeral mode calls `pg_ctl -D <exact-path> stop -m fast`, persistent mode retains the current behavior.

- [ ] **Step 4: Preserve hot-reload boundaries**

- Rust binary changed: prepare binary, restart agentd/Gateway, keep PostgreSQL/MCP when compatible.
- TypeScript changed: restart Gateway only.
- Python capability adapter changed: restart capabilityd only.
- AgentSpec/prompt changed: rebuild fingerprinted image, restart agentd/Gateway, keep PostgreSQL.
- Question/quality run only: reuse all services; no compile or image build.

- [ ] **Step 5: Validate and commit**

```bash
bash -n scripts/dev-stack.sh
bash -n scripts/start_local_agent_gateway_stack.sh
bash -n scripts/test_dev_stack_lifecycle.sh
./scripts/test_dev_stack_lifecycle.sh
./scripts/dev-stack.sh status
git add scripts/dev-stack.sh scripts/start_local_agent_gateway_stack.sh scripts/test_dev_stack_lifecycle.sh docs/LOCAL_FEEDBACK_LOOP.md
git commit -m "perf: enforce reusable local stack lifecycle"
```

---

### Task 11: Regression, Performance, and GLM Quality Gate

**Files:**
- Modify: `docs/LOCAL_FEEDBACK_LOOP.md`
- Create: `fixtures/live-quality/v1/session-followup-v1.json`
- Create: `fixtures/performance/v1/multi-user-chat-load-v1.json`
- Create: `scripts/run_session_followup_quality.py`
- Modify: `crates/perf-harness/src/main.rs`

**Interfaces:**
- Consumes all previous tasks
- Produces a repeatable report comparing calls, tool use, timing, memory, and follow-up quality

- [ ] **Step 1: Add deterministic follow-up cases**

Use this closed corpus shape:

```json
{
  "schema_version": 1,
  "suite_id": "chat-session-followup-v1",
  "suite_version": 1,
  "chains": [
    {
      "chain_id": "aapl_pronoun_risk",
      "steps": [
        {"step_id": "initial", "session_action": "new", "ticker": "AAPL", "question": "애플의 성장 동력과 핵심 리스크를 조사해줘.", "evaluation_focus": ["initial_grounding"]},
        {"step_id": "followup", "session_action": "continue", "ticker": "AAPL", "question": "그중 가장 큰 리스크는?", "evaluation_focus": ["pronoun_resolution", "same_session"]}
      ]
    },
    {
      "chain_id": "aapl_year_comparison",
      "steps": [
        {"step_id": "initial", "session_action": "new", "ticker": "AAPL", "question": "애플의 최근 매출과 영업이익 흐름을 조사해줘.", "evaluation_focus": ["period_grounding"]},
        {"step_id": "followup", "session_action": "continue", "ticker": "AAPL", "question": "그 수치가 전년보다 좋아진 거야?", "evaluation_focus": ["numeric_referent", "period_continuity"]}
      ]
    },
    {
      "chain_id": "aapl_msft_comparison",
      "steps": [
        {"step_id": "aapl", "session_action": "new", "ticker": "AAPL", "question": "애플의 현금흐름 안정성을 조사해줘.", "evaluation_focus": ["aapl_cash_flow"]},
        {"step_id": "msft", "session_action": "continue", "ticker": "MSFT", "question": "마이크로소프트의 현금흐름 안정성도 같은 기준으로 조사해줘.", "evaluation_focus": ["msft_cash_flow", "same_session"]},
        {"step_id": "compare", "session_action": "continue", "ticker": "MSFT", "question": "두 회사 중 현금흐름은 어디가 더 안정적이야?", "evaluation_focus": ["multi_company_referent", "comparison"]}
      ]
    },
    {
      "chain_id": "cross_session_isolation",
      "steps": [
        {"step_id": "aapl_session", "session_action": "new", "ticker": "AAPL", "question": "애플의 공급망 리스크를 조사해줘.", "evaluation_focus": ["session_a"]},
        {"step_id": "nvda_new_session", "session_action": "new", "ticker": "NVDA", "question": "그럼 가장 큰 위험은?", "evaluation_focus": ["session_b", "no_cross_session_leak"]}
      ]
    },
    {
      "chain_id": "resume_initial_session",
      "steps": [
        {"step_id": "aapl_initial", "session_action": "new", "ticker": "AAPL", "question": "애플의 핵심 위험과 밸류에이션 부담을 조사해줘.", "evaluation_focus": ["initial_session"]},
        {"step_id": "msft_new", "session_action": "new", "ticker": "MSFT", "question": "마이크로소프트의 성장 동력을 조사해줘.", "evaluation_focus": ["intervening_session"]},
        {"step_id": "aapl_resume", "session_action": "resume_initial", "ticker": "AAPL", "question": "아까 말한 위험이 밸류에이션에 어떤 영향을 줘?", "evaluation_focus": ["explicit_resume", "referent_recovery"]}
      ]
    },
    {
      "chain_id": "six_turn_continuity",
      "steps": [
        {"step_id": "business", "session_action": "new", "ticker": "AAPL", "question": "애플의 사업 구조를 간단히 조사해줘.", "evaluation_focus": ["business_model"]},
        {"step_id": "services", "session_action": "continue", "ticker": "AAPL", "question": "그중 서비스 사업은 왜 중요한데?", "evaluation_focus": ["services_referent"]},
        {"step_id": "margin", "session_action": "continue", "ticker": "AAPL", "question": "그게 전체 마진에는 어떤 영향을 줘?", "evaluation_focus": ["margin_causality"]},
        {"step_id": "risk", "session_action": "continue", "ticker": "AAPL", "question": "그 논리를 깨뜨릴 수 있는 위험은 뭐야?", "evaluation_focus": ["countercase"]},
        {"step_id": "valuation", "session_action": "continue", "ticker": "AAPL", "question": "그 위험까지 고려하면 현재 밸류에이션은 어떻게 봐야 해?", "evaluation_focus": ["valuation_impact"]},
        {"step_id": "synthesis", "session_action": "continue", "ticker": "AAPL", "question": "앞의 논리를 종합해서 투자자가 확인할 지표를 우선순위로 정리해줘.", "evaluation_focus": ["four_turn_tail", "synthesis"]}
      ]
    }
  ]
}
```

The loader rejects unknown keys and accepts only `session_action` values `new`, `continue`, and `resume_initial`. A chain's first step must be `new`; `continue` requires a current session; `resume_initial` requires a previously completed initial session.

- [ ] **Step 2: Add a deterministic multi-user, multi-room load matrix**

Create a fake-provider/fake-capability workload that represents 20 principals, three rooms per principal, and two turns per room. Each logical run uses the same provider-turn and capability-call policy as its baseline; the fake transports remove network variance but do not skip workflow steps.

Add a separate queue-only case with 2,000 principals and one queued run each. It must not create 2,000 Tokio run tasks, provider clients, MCP processes, or checked-out DB connections; only the bounded claim loop may promote work to active execution.

The harness must check all of these separately:

- two enqueued runs for the same room never become active together and finish in turn order;
- different rooms for the same principal can make progress concurrently subject to global limits;
- a principal with many queued rooms does not starve another principal with one room;
- no reconstructed prompt or session-memory source crosses principal or room boundaries;
- retrying the same product run ID creates no duplicate logical run or terminal event;
- RSS growth is bounded as active rooms rise from 1 to 4 to 16;
- PostgreSQL/provider/MCP pools are shared by the process, while mutable run/session state is never shared across rooms.

Record p50/p95/p99 for enqueue-to-claim, session-memory reconstruction, provider-permit wait, local non-provider wall time, and end-to-end deterministic wall time. Fairness is evaluated from normalized wait ranks; metrics must not emit principal or session IDs as labels.

- [ ] **Step 3: Implement a dedicated multi-turn runner**

`scripts/run_session_followup_quality.py` reuses validation, bounded response reading, polling, and private report conventions from `run_live_quality_matrix.py`, but executes each chain sequentially:

1. `new` POSTs to `/v1/agent/runs`; the first returned ID is retained as `initial_session_id` and every later `new` replaces only `current_session_id`;
2. `continue` POSTs to `/v1/agent/sessions/{current_session_id}/runs`;
3. `resume_initial` POSTs to `/v1/agent/sessions/{initial_session_id}/runs` and verifies the response returns that exact ID;
4. the cross-session case automatically verifies that the second response has a different session ID; answer-level contextual leakage remains an explicit manual quality judgment rather than an unreliable substring heuristic;
5. independent chains may run with `--parallelism 1..=4`, but steps within one chain are always serial;
6. `--dry-run` validates corpus and prints the request sequence without a Gateway call;
7. reports are written only to a new absolute private directory and include original questions, raw Markdown, bounded trace, usage, timings, and manual-review fields.

- [ ] **Step 4: Pin non-regression invariants**

For recorded deterministic replay assert:

```text
provider_turns_after == provider_turns_before
capability_calls_after == capability_calls_before
thinking_mode_after == thinking_mode_before
advertised_capabilities_after == advertised_capabilities_before
```

Memory selection is the only expected prompt-content difference.

- [ ] **Step 5: Capture local performance before and after**

Measure warm-stack p50/p95 for:

- enqueue-to-claim scheduler wait
- session reconstruction
- provider semaphore wait
- prompt build and canonical serialization
- checkpoint work
- total provider time
- total capability time
- peak RSS for 1, 4, and 16 active rooms

Acceptance criteria:

- no increase in provider/capability call counts;
- no output/reasoning budget reduction;
- zero second full-stack startup for a question-only run;
- provider slot is not held during MCP or session-memory work;
- request hash and canonical bytes are identical across retry;
- warm local overhead does not regress by more than 5%;
- 4-room and 16-room peak RSS do not regress by more than 5%; absolute memory per additional active room is reported.

- [ ] **Step 6: Run the complete deterministic gate**

```bash
cargo test -p krw-session-memory
cargo test -p krw-agent-runtime-persistence
cargo test -p krw-agent-provider-wire
cargo test -p krw-agent-run-engine
cargo test -p krw-agent-perf-harness
npm --prefix packages/host-ts test
npm --prefix packages/host-ts run typecheck
cargo fmt --check
cargo clippy -p krw-session-memory -p krw-agent-runtime-persistence -p krw-agent-provider-wire -p krw-agent-run-engine --all-targets -- -D warnings
```

- [ ] **Step 7: Validate the runner without GLM, then run GLM live quality only after deterministic gates pass**

```bash
python3 scripts/run_session_followup_quality.py --dry-run
```

Use the already-running Gateway and the exact same `session_id` for each follow-up chain. Run steps within a room sequentially; independent rooms may run concurrently. Save question original, answer original, bounded trace, usage, and stage timings. Do not invoke DeepSeek and do not rebuild or restart the stack between questions. The live GLM smoke is intentionally small because the deterministic harness already supplies concurrency pressure; it must still preserve the full configured model/tool workflow for every live run.

- [ ] **Step 8: Manual quality judgment**

For each answer mark:

- `context_correct`: company, period, and referents resolve to the same chat
- `no_cross_session_leak`: no other session content appears
- `research_depth_preserved`: impact/countercase/uncertainty remain comparable to baseline
- `evidence_regrounded`: old answer is not presented as fresh evidence without current support
- `call_budget_unchanged`: model/tool counts match baseline policy
- `room_isolation`: another member or room's company, period, or claim never appears
- `resume_after_interleaving`: a room still resolves references after other users/rooms complete work in between

- [ ] **Step 9: Commit the fixtures, runner, and documentation**

```bash
git add fixtures/live-quality/v1/session-followup-v1.json fixtures/performance/v1/multi-user-chat-load-v1.json scripts/run_session_followup_quality.py crates/perf-harness/src/main.rs docs/LOCAL_FEEDBACK_LOOP.md
git commit -m "test: gate chat continuation and runtime performance"
```

---

## Explicit Non-Goals for This Plan

- Cross-session personal research memory
- Replacing the front/Supabase chat session, message, session-list, soft-delete, or reconnect model
- Copying product chat titles, previews, or full message history into `krw_gateway_local`
- A separate daemon/process, provider client, MCP client, or cache per member
- Vector database, embeddings, or semantic-search service
- Automatic merging of two chat sessions
- Converting ordinary Markdown answers to AnswerIR
- Reducing planner/analyst/composer turns
- Lowering GLM reasoning or output budgets
- Removing ontology query/trace/chain capabilities
- Changing Guru philosophy, skills, or ontology contracts
- Rewriting the typed workflow as an unstructured Claw-style transcript loop
- Merkle checkpoint redesign or `Arc<CapabilityResult>` conversion before stage timings show they are material

The last two optimizations remain candidates only if Task 4 measurements show checkpoint hashing or capability-result duplication exceeds either 5% of non-provider wall time or 100MiB RSS per active run. This threshold prevents a broad runtime rewrite without measured benefit.

## Final Architecture After This Plan

```text
Product plane (front/Supabase)
  authenticated member UUID + selected chat-room UUID + product run UUID
  → ownership/idempotency validation
  → exact identity tuple sent to krw-agent

Execution plane (krw-agent)
  → principal-fair queue across members
  → one active run per chat room, other rooms remain concurrent
  → fenced snapshot + same-member/same-room delta reconstruction
  → newest 4 chronological turns pinned
  → up to 2 older same-room relevant turns
  → existing typed research workflow, evidence gate, and tool autonomy
  → final answer + next session turn committed atomically
  → deduplicated terminal outbox projected back to the product plane

Runtime internals
  → immutable image plans reused
  → provider permit held only during GLM episode
  → provider request canonicalized once
  → session delta mutates touched data only
  → shared bounded DB/provider/MCP pools; no per-user processes
  → persistent local stack reused
```

This keeps the current financial-research safety architecture while making every product chat room behave like one continuous conversation, allows many members and rooms to progress fairly, and removes local work that does not improve answer quality. The front remains the product history system; the harness remains the execution and research-continuity system.
