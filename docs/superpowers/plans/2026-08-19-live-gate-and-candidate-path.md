# Live-Gate 녹색화 + 후보 경로 일관성 + 레거시 정리 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 라이브 GLM 품질 게이트를 녹색화하고(원인 특정 → 관대화), 검토에서 발견한 버그 4건을 수리하며, 후보 베껴쓰기 신호를 측정 가능하게 만들고, 미사용/레거시 코드를 정리한다.

**Architecture:** 커널(Rust) 중심 수정 — 예산 소진의 ledger-fallback 우회, 캐시 히트 복구 불가, 후보 매핑 불일치 2건은 run-engine/research-planner 안의 국소 수정. 플래너 관대화는 12절 초과 시 auto-Narrow. 어휘 주입과 귀속 카운터는 adapter/capability_dispatch의 투영·계측 확장. 스키마(계약 해시) 불변.

**Tech Stack:** Rust workspace (cargo 1.97.1), krw-ontology MCP runtime (Python, 건드리지 않음), YAML AgentSpec.

## Global Constraints

- **빌드 환경**: 모든 cargo 명령 전에 `export PATH="$HOME/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin:$PATH"` + `export KRW_ONTOLOGY_ROOT="$HOME/krw-ontology"`. rustfmt/clippy는 rust-toolchain.toml 핀.
- **계약 불변**: `contracts/kernel/v4/schemas/*.json`, `contracts/krw-ontology/v2/schemas/*.json`의 바이트와 SHA-256 핀(`krw-contracts/src/lib.rs:158-159` 등), `agents/*/agent.yaml`의 `content_hash` 핀을 **어떤 작업도 변경하지 않는다** (Task 8의 프롬프트 본문 수정은 핀 밖 영역; `spec check`로 확인).
- **테스트 게이트**: 각 작업 종료 시 해당 패키지 테스트 + `cargo check --workspace --all-targets --locked` 녹색. 최종 작업에서 `cargo test --workspace --no-fail-fast`.
- **커밋 스타일**: 기존 관행 따름 (`fix(run-engine): ...` 형태, 한 줄).
- **한국어 최종 답변 형식**(결론우선, 각주 출처)은 어떤 작업도 변경하지 않는다.
- **라이브 키**: GLM 라이브가 필요한 작업은 `set -a; source .env.local; set +a`로 키를 올린다. 라이브 런 비용이 드므로 승인된 작업만 실행.
- 작업 순서: Task 1 → (2,3,4 독립) → Task 5는 Task 1 결과 의존 → 6 → 7 → 8 → 9(마지막).

---

### Task 1: 라이브 거절 원인 특정 (live episode audit)

**Files:**
- Read only: `crates/runtime-persistence/tests/live_provider_episode_audit.rs`, `bins/krw-agent/src/main.rs:470-506`
- Output: 진단 결과를 작업 보고서에 기록 (코드 수정 없음)

**Interfaces:**
- Produces: 실제 거절 에러 코드(예: `model_research_proposal_plan_too_large` vs `proposal_search_plan_contract_invalid` vs 기타) — Task 5의 분기 결정 입력.

- [ ] **Step 1**: `live_provider_episode_audit.rs`를 읽어 활성화 조건(env 게이트, ignored 여부)과 진단 코드 매핑을 확인한다.
- [ ] **Step 2**: 로컬 스택의 capability 사이드카가 필요한지 확인(`scripts/start_local_agent_gateway_stack.sh` 또는 최소 사이드카). 필요하면 기동한다.
- [ ] **Step 3**: `set -a; source .env.local; set +a` 후, 감사 게이트를 켜고 `cargo run --bin krw-agent --features dev-tools -- quality run --case <다중-claim 케이스: company-research-dual-claim 계열>` 1~2회 실행. 녹화된 에피소드에서 InitialPlanError→진단코드 매핑 결과를 추출.
- [ ] **Step 4**: 결과를 보고서에 기록: 실제 reason_code, 몇 차례 재시도, 최종 런 상태. 스택이든 런이든 실패 시 **원인과 필요한 것**을 BLOCKED 보고 (추측 금지).
- [ ] **Step 5**: 커밋 없음 (코드 불변). 진단 로그가 `.local/`에 남으면 그 경로만 기록.

### Task 2: Critical — 전역 예산 소진이 ledger fallback을 우회하지 않게 수정

**Files:**
- Modify: `crates/run-engine/src/finalization.rs:1043-1052` (`error_allows_ledger_fallback`)
- Modify: `crates/run-engine/src/active_run.rs:566-571` (`remaining_output_tokens`)
- Test: `crates/run-engine/src/lib.rs` (기존 fallback 자격 테스트 근처에 추가)

**Interfaces:**
- Consumes: `EngineError::Contract(ContractError::BudgetExceeded { resource, .. })` (protocol), `EngineError::CounterOverflow`
- Produces: `error_allows_ledger_fallback`이 `BudgetExceeded`(모든 resource)에 대해 true 반환; `remaining_output_tokens`이 초과분에서 포화(0) 반환.

- [ ] **Step 1: 실패 테스트** — `error_allows_ledger_fallback(&EngineError::Contract(ContractError::BudgetExceeded { resource: "provider_turns".into(), .. }))`가 true임을 주장하는 단위 테스트 (기존 테스트 패턴 참조, resource 3종 이상).
- [ ] **Step 2**: `cargo test -p krw-run-engine fallback` 로 실패 확인.
- [ ] **Step 3**: 구현 — `error_allows_ledger_fallback` 매치 추가. `remaining_output_tokens`는 `usage > limit`일 때 `CounterOverflow` 대신 0(→`NoRemainingOutputBudget`) 반환; 다른 `CounterOverflow` 경로는 유지.
- [ ] **Step 4**: 테스트 통과 + `cargo check --workspace --all-targets --locked`.
- [ ] **Step 5**: `git commit -m "fix(run-engine): let exhausted global budgets reach the ledger fallback"` + 두 번째 커밋으로 포화 변경 분리 가능.

### Task 3: Important — 캐시 히트 에피소드의 복구 재생 가능화

**Files:**
- Modify: `crates/run-engine/src/recovery.rs:669-676` (`replay_committed_episode`)
- Test: `crates/run-engine/src/lib.rs` (recovery 테스트 근처)

**Interfaces:**
- Consumes: 라이브 캐시 분기(`orchestrator.rs:687-734`)의 동일 조건 — `state.action_cache`에 해당 action key 존재 + `state.logical_action_keys` 포함.
- Produces: 캐시 히트로 체크포인트된 에피소드가 action 영수증 없이도 재생 통과.

- [ ] **Step 1: 실패 테스트** — 캐시 히트 에피소드(영수증 없음)를 포함한 체크포인트 복원이 `InvalidRecoverySnapshot` 없이 성공하는 테스트 (기존 recovery 테스트의 체크포인트 구성 패턴 재사용).
- [ ] **Step 2**: 실패 확인.
- [ ] **Step 3**: 구현 — `replay_committed_episode`에서 영수증 조회 전에 라이브 캐시 분기와 동일한 키 계산으로 캐시 존재 시 스킵(동일 key 산출 로직 재사용, 중복 구현 금지).
- [ ] **Step 4**: 테스트 통과 + workspace check.
- [ ] **Step 5**: `git commit -m "fix(recovery): replay cache-hit episodes without an action receipt"`.

### Task 4: 후보 경로 일관성 — focused 후보 매핑 + 체크포인트 검증

**Files:**
- Modify: `crates/run-engine/src/capability_dispatch.rs:1667-1734` (`exact_required_gap_arguments`)
- Modify: `crates/research-planner/src/lib.rs:1500` (`validate_projection`)
- Test: `crates/run-engine/src/lib.rs` (기존 `paraphrased_required_gap_query_dispatches_its_canonical_full_read` 근처), `crates/research-planner/src/lib.rs` 테스트

**Interfaces:**
- Consumes: 후보 `topic`(focused 어구, adapter `lib.rs:659-685`)와 절 바인딩 `clause.retrieval_query`의 차이.
- Produces: (a) 후보 발포 시 물리 인자의 `topic`이 **절의 `retrieval_query`로 정규화**되어 `map_goals`(문자열 완전일치) 통과 — 광고된 후보의 친화적 어구는 모델용 그대로 유지. (b) `validate_projection`이 `response_detail == "compact"` 후보를 수용.

- [ ] **Step 1: 실패 테스트 A** — focused 후보(`research and development` 계열)를 "그대로 복사"한 dispatch가 `proposal_unmapped` 없이 성공하는 테스트.
- [ ] **Step 2: 실패 테스트 B** — compact 후보를 포함한 투영의 체크포인트 검증 통과 테스트.
- [ ] **Step 3**: 구현 A — `exact_required_gap_arguments`가 후보의 topic 대신 대응 절의 `retrieval_query`를 인자로 쓰도록 (후보→절 매핑은 이미 이 함수의 입력에 있음; planner `map_goals` 불변).
- [ ] **Step 4**: 구현 B — `validate_projection`의 후보 `response_detail` 허용값에 compact 추가.
- [ ] **Step 5**: 테스트 통과 + workspace check.
- [ ] **Step 6**: `git commit -m "fix(capability): canonicalize focused candidate topics to their clause binding"` (+ 두 번째 커밋으로 B 분리 가능).

### Task 5: 라이브 게이트 녹색화 — 빈 document_types의 기본값 강등 (개정: Task 1 실측 반영)

> 개정 사유: Task 1 실측(2회 라이브) 결과 **어떤 제안/컴파일 거절도 발생하지 않는다**(다중 objective 3~5개가 무수리·무재계획으로 정상 컴파일, engine_error None). 실제 실패 사슬: 제안의 `document_types` 공란 허용(`krw-contracts/src/lib.rs:852`) → 강등된 SearchPlan에 document_types 없음(`initial_plan.rs:624` 통과) → fixture 게이트 `required_document_type_present`(10-K, `research-quality/src/lib.rs:1638`) 실패 + 증거 원장 공란(`retrieval_empty` 폴백 답변). 원래의 auto-Narrow(5a)·컴파일 상세(5b)는 원인이 아니므로 폐기(YAGNI).

**Files:**
- Modify: `crates/research-planner/src/initial_plan.rs` (~:624, `lower_research_proposal`의 document_types 통과 지점)
- Modify: `docs/IMPLEMENTATION_STATUS.md` (not-green 단락을 실측 원인으로 갱신)
- Test: `crates/research-planner/src/initial_plan.rs` 테스트(1683-2412 근처)

**Interfaces:**
- Consumes: Task 1 실측 — 원인 코드 `quality_fixture_plan_rejected`/`required_document_type_present=false` + `retrieval_empty`.
- Produces: 제안의 `document_types`가 **공란일 때만** 커널이 모드 표준 공시 세트로 기본값 적용(모델이 명시한 값은 그대로). 우선 소스: 기존 기간 정책(`agent.yaml:834-839` `latest_confirmed_10q/10k`)이 플래너에 전달되면 그 값을 재사용; 전달되지 않으면 문서화된 상수 `["10-K","10-Q"]`(기간 정책과의 정합 주석 포함). 계약/스키마/해시 불변 — 이것은 강등 시점의 커널 기본값이며 제안 계약 변경이 아님.

- [ ] **Step 1: 실패 테스트** — `document_types` 공란 제안이 강등 후 비어 있지 않은 document_types(10-K 포함)를 가지는지 주장. 모델 명시값은 수정 없이 통과하는 대응 테스트도 추가.
- [ ] **Step 2**: 실패 확인 (`cargo test -p krw-research-planner initial_plan`).
- [ ] **Step 3**: 구현 — 기본값 적용 지점과 근거 주석. 기본값이 intent receipt/계획 어디에 기록되는지 확인(투명성).
- [ ] **Step 4**: 테스트 통과 + workspace check + `cargo run --bin krw-agent --features dev-tools -- quality replay --suite evals/krw-research-quality/v4` 3케이스 통과.
- [ ] **Step 5**: 라이브 확인 최대 2회(`set -a; source .env.local; set +a` 후 `quality run --case company-research-independent-cash-and-debt`) — `fixture_plan_accepted`가 통과로 바뀌는지, 증거 커밋(`minimum_evidence`, `required_evidence_is_committed`)이 회복되는지 검증. document_types 수정 후에도 `retrieval_empty`가 남으면 그 증거를 보고 (추측 없이).
- [ ] **Step 6**: `docs/IMPLEMENTATION_STATUS.md`의 not-green 기술을 새 원인·해결으로 갱신.
- [ ] **Step 7**: `git commit -m "fix(planner): default empty document_types to the canonical filing set"`.

### Task 6: company_context 어휘의 플래닝 투영 주입

**Files:**
- Modify: `crates/krw-ontology-adapter/src/lib.rs` (`derive_research_planning_projection` ~:533-647; company_context 매핑 :1885-1947)
- Modify: `crates/research-planner/src/lib.rs` (`validate_projection` — 신규 선택 필드 수용)
- Test: `crates/krw-ontology-adapter/src/lib.rs` 테스트, `crates/research-planner/src/lib.rs` 체크포인트 테스트

**Interfaces:**
- Consumes: orientation `EvidenceRecord`(predicate `company_topic_orientation`, provider_content `company-context-orientation/v1`).
- Produces: `ResearchPlanningProjection`에 선택 필드 `orientation_vocabulary: Vec<OrientationTerm>` (term=topic_label, document_type, period; serde default로 역호환). advisory_only 의미 유지 — "정식 용어 후보"로만 제시.
- 컴팩션 경계 통과 보장: 기존 후보 필드와 동일한 방식으로 투영에 실리면 role view가 자동 반영(플래너는 전체 정규 뷰).

- [ ] **Step 1: 실패 테스트** — company_context 섭취 후 투영에 orientation_vocabulary가 존재하고 라벨이 일치; 체크포인트 왕복(직렬화→validate_projection) 통과.
- [ ] **Step 2**: 실패 확인.
- [ ] **Step 3**: 구현 — 어댑터에서 orientation 팩트를 투영으로 승격(중복 제거, 상한 8~12항), planner 검증기에 선택 필드 수용.
- [ ] **Step 4**: 테스트 통과 + workspace check.
- [ ] **Step 5**: `git commit -m "feat(adapter): surface company orientation vocabulary in the planning projection"`.

### Task 7: 베껴쓰기 귀속 카운터 (측정 신호)

**Files:**
- Modify: `crates/run-engine/src/capability_dispatch.rs` (`canonicalize_required_gap_targeted_query` :1569-1632 — 반환값에 귀속 결과 추가)
- Modify: `crates/persistence/src/metrics.rs` (신규 카운터)
- Test: `crates/run-engine/src/lib.rs`, `crates/persistence` 메트릭 테스트 패턴

**Interfaces:**
- Produces: `krw_targeted_query_attribution_total{outcome="verbatim|canonicalized|unmatched"}` 카운터. DB 스키마/ActionIntent 불변(라벨만).
- 귀속 판정: 모델 topic == 후보 topic → `verbatim`; 토큰 부분집합으로 재작성됨 → `canonicalized`; 후보 있으나 불일치 → `unmatched`; 후보 없음 → 계수 안 함.

- [ ] **Step 1: 실패 테스트** — 세 경로 각각 카운터 증가 주장 (기존 메트릭 테스트 패턴).
- [ ] **Step 2**: 실패 확인.
- [ ] **Step 3**: 구현 — 함수 반환값 확장 + 호출부에서 카운터 증가.
- [ ] **Step 4**: 테스트 통과 + workspace check.
- [ ] **Step 5**: `git commit -m "feat(metrics): attribute targeted queries to their exact candidates"`.

### Task 8: 프롬프트 정비 — EN 애널리스트 포팅 + 유령 참조 제거

**Files:**
- Modify: `agents/krw-ontology-en/prompts/evidence-analyst.md` (후보 지시 포팅 — ko `evidence-analyst.md:73-89`의 영문 등가)
- Modify: `agents/krw-ontology/prompts/evidence-analyst.md:99-103` (`required_evidence_gap_remains` 유령 참조를 실제 메커니즘(`not_dispatched` 결과 + 갭 힌트)으로 재작성)
- Test/게이트: `spec check` + 이미지 빌드/검증

**Interfaces:**
- Consumes: ko 프롬프트의 문단 구조, Task 4/7의 동작(표현은 실제 메커니즘과 일치시킨다).
- Produces: EN/ko 애널리스트 프롬프트가 동일한 후보 규칙을 가르침; 유령 심볼 참조 소멸.

- [ ] **Step 1**: EN 프롬프트에 ko 73-89의 규칙을 영문으로 이식 (문단 위치 대응).
- [ ] **Step 2**: ko 99-103을 실제 커널 결과 코드에 맞게 재작성 (`grep -rn "required_evidence_gap_remains"`로 잔여 참조 0 확인).
- [ ] **Step 3**: `export PATH=...; export KRW_ONTOLOGY_ROOT=...; cargo run --bin krw-agent --features dev-tools -- spec check` (대상 에이전트) 통과.
- [ ] **Step 4**: 이미지 빌드+검증 (`krw-agent image build` / `image verify` — §7 절차; 기존 배포 스크립트 사용 가능하면 그것으로).
- [ ] **Step 5**: `git commit -m "feat(prompts): parity analyst candidate rules; drop phantom gap-remains reference"`.

### Task 9: 레거시/미사용 정리 + 전체 검증 (마지막)

**Files:**
- Remove/정리: 증거 기반으로 결정 (사전 승인된 삭제 목록 없이는 삭제 금지)
- Test: 전체 워크스페이스

**Interfaces:**
- Consumes: Tasks 1-8 완료 상태.
- Produces: 미사용 코드 제거 + 전체 스위트 녹색.

- [ ] **Step 1**: `cargo clippy --workspace --all-targets --locked 2>&1 | grep -E "warning|dead_code"`로 경고 목록 수집.
- [ ] **Step 2**: 후보군 조사 — (a) dead_code 경고 대상, (b) `agents/`, `scripts/`, `prompts/`에서 코드/YAML/핀/manifest 어디에서도 참조되지 않는 파일 (`grep -rn <이름>` 검증), (c) 본 플랜으로 무효화된 주석. **참조 0이 확인된 것만** 삭제 후보.
- [ ] **Step 3**: 삭제 실행 + `cargo check --workspace --all-targets --locked` + 대상 테스트 + `spec check`.
- [ ] **Step 4**: `cargo test --workspace --no-fail-fast` 전체 녹색 확인.
- [ ] **Step 5**: `git commit -m "chore: remove verified-unused code and stale comments"` (성격별 분리 가능).

---

## Self-Review

- 스펙 커버: 4 버그(Task 2,3,4×2) + 원인특정(1) + 관대화(5) + 어휘(6) + 신호(7) + 프롬프트(8) + 정리(9) — 검토 보고의 전 항목 대응. 8-K는 사용자 취소로 제외.
- 자리표시자: 각 단계에 구체적 파일/라인/명령 명시. Task 5의 5a는 Task 1 결과에 대한 조건 분기로 명시.
- 타입 일관성: Task 4의 정규화 지점(`exact_required_gap_arguments`)과 Task 7의 귀속 판정(`canonicalize_required_gap_targeted_query`)은 서로 다른 함수 — 7이 4의 변경 후 기준(verbatim = 최종 발포 인자 기준이 아닌 모델 원안 vs 후보 비교)으로 판정함을 명시.
