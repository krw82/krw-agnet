# 조각 1 — 자유 문 개통 (open_research) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 티커 없는(및 미커버 티커) 질문이 `question_only` 컨텍스트 실행(`open_research` 런카인드)으로 엔진 심층 루프를 타고, 온톨로지(커버드 유니버스 한정·필링급) + OpenBB/뉴스(전 시장·시장/뉴스급) 증거로 답변하게 한다.

**Architecture:** 스펙 `docs/superpowers/specs/2026-09-06-tickerless-open-research-vision.md`의 방법 2. 핵심은 3가지 엔진 변경 — (a) 신규 capability 스코프 바인딩 `market_plane`(회사 문=종전 멤버십 검증, 자유 문=정규 형식만 검증), (b) 권한 매트릭스에서 `question_only`가 covered-universe 도구와 market-plane 도구를 허가, (c) 워크플로에 없는 선행요구(구조적 도달불가) 스킵 — 와 데이터 저작(`agents/krw-ontology/agent.yaml`에 `open_research_v1` 워크플로), 게이트웨이 계약 확장(ticker 선택화), 에이전트 서비스의 LLM 디스패처(+커버리지 문 선택)다. protocol(`QuestionOnly {}`)·검증 5곳(admission/trusted_scope_payload/untrusted_task_payload/product_context_value/memory_tickers)·시장 사전필터(company 런카인드 게이트라 자동 스킵)는 **이미 처리돼 있어 무변경**임을 탐색으로 확인했다.

**Tech Stack:** Rust workspace(crates/agent-image, run-engine, research-planner), TypeScript host-ts(node:test), YAML agent spec + budgets, Python FastAPI(krw-agent-service, httpx, pytest).

## Global Constraints

- 원본 저장소(`~/krw-ontology`, `~/krw-agnet`)과 `~/krw-ontology-data/releases/prod` 절대 수정 금지. 모든 작업은 `~/krw-ontology-v2` 서브리포 + `releases/v2-dev`/`.local` 산출물만.
- 시크릿 값 절대 출력/로그 금지(환경변수 이름만 다룬다). 테스트에 자격증명 리터럴 금지.
- 테스트는 네트워크 프리(off-line). 라이브 E2E만 예외(Task 12).
- 어드바이저리 라벨·벤더 스크럽(fmp/fred/polygon 비노출) 교리 유지.
- Bash로 `*.py`/`*.sh`/`*.json` 소스 파일 직접 쓰기 금지(mimosa 훅) — Write/Edit 도구만.
- 커밋은 기능 단위로 자주. 브랜치: krw-agnet `feat/engine-v2-e1`, krw-agent-service 현행 브랜치 확인 후 사용.
- 스펙 결정사항: `open_research_en`(영문 이미지)은 **후속 조각**으로 연기(라틴 질문은 ko 자유 문으로 우회 처리 — 게이트웨이는 en 엔트리포인트 존재 시에만 en 라우팅). 회사 문 지표 관측 확대(§4.4 동료 스캔)도 1b로 연기; 본 플랜의 자유 문 프롬프트가 지표 관측 의무(미시=동료 필링 힌트, 거시=관측 계열)를 담는다.

---

### Task 1: agent-image — `market_plane` 스코프 바인딩

**Files:**
- Modify: `crates/agent-image/src/lib.rs` (`enum CapabilityScopeBinding`, 약 line 675; 형상 검증은 `validate_spec`/바인딩 검증부 — `grep -n "trusted_ticker_set\|TrustedTickerSet" crates/agent-image/src/lib.rs`로 위치 확인)
- Test: `crates/agent-image/src/lib.rs` (기존 테스트 모듈 내)

**Interfaces:**
- Produces: `CapabilityScopeBinding::MarketPlane { ticker_references: Vec<TickerReferenceSpec>, require_any_of: Vec<String> }` (serde tag `kind: market_plane`). 이후 Task 2/6이 사용.

- [ ] **Step 1: 실패 테스트 작성** — YAML 파싱이 `scope_binding: {kind: market_plane, ...}`을 `MarketPlane`로 읽고, `ticker_references`가 비어 있으면 스펙 검증 오류가 남을 확인하는 테스트 2개:

```rust
#[test]
fn market_plane_binding_parses_and_validates() {
    let yaml = r#"
api_version: krw.agent/spec-v3
kind: AgentSpec
metadata: { name: t, version: 1 }
entrypoints:
  - run_kind: t
    locale: ko-KR
    workflow: w
    required_budget_profile: t
    required_model_profile: m
    scope:
      allowed_context: company_ticker_set
      cardinality: { kind: exact, value: 1 }
      ticker_canonicalization: require_uppercase
    constants: {}
contracts: []
roles: []
prompt_segments: []
capabilities:
  - id: t.read
    execution: { kind: local, builtin: skill_load }
    permission: read
    input_contract: skill-load/v1
    output_contracts: [skill-content/v1, normalized-capability-result/v1]
    idempotency: canonical_args
    result_ingest: skill_content_v1
    scope_binding:
      kind: market_plane
      ticker_references:
        - { id: symbol, pointer_pattern: /symbol, value_kind: string }
      require_any_of: [symbol]
    parallel_safe: false
    prerequisites: []
workflows:
  - id: w
    initial: accepted
    states:
      - { id: accepted, kind: start, max_visits: 1 }
      - { id: succeeded, kind: terminal, terminal: succeeded, max_visits: 1 }
    transitions: []
planning_policy: { internal_brief_language: en }
evidence_policy: { directness_values: [direct] }
period_policy: {}
answer_policy: {}
security_policy: {}
validators: []
"#;
    // 기존 파싱 헬퍼(parse_spec + validate_spec)를 사용. MarketPlane로 디코딩되는지,
    // ticker_references 비어있으면 오류인지를 각각 assert.
}
```
(테스트는 저장소 기존 spec-fixture 헬퍼 방식에 맞춰 조정 — `parse_spec(&str)` / `validate_spec(&AgentSpec)` 시그니처는 lib.rs:4933 부근 테스트 참고.)

- [ ] **Step 2: 테스트 실패 확인** — `cargo test -p krw-agent-image --lib market_plane` → 컴파일/디코딩 실패.
- [ ] **Step 3: 구현** — enum에 변형 추가(문서 주석 포함):

```rust
/// Market/news-plane observation read (openbb.*, market.series,
/// news.*). In ticker-scoped runs it behaves exactly like
/// `TrustedTickerSet` (membership in the immutable run scope). In a
/// `question_only` run the ticker argument is checked for canonical
/// form only: market/news evidence covers the whole market — uncovered
/// issuers included — and is graded market/news evidence, never
/// filing-grade ontology evidence (open-research vision §6).
MarketPlane {
    ticker_references: Vec<TickerReferenceSpec>,
    #[serde(default)]
    require_any_of: Vec<String>,
},
```

`validate_spec`의 바인딩 형상 검증에 `MarketPlane` 추가: `ticker_references` 비어 있으면 오류(`reject_non_null_pointers`는 없음 — 이 바인딩은 유니버스 확장 포인터를 쓰지 않는다). 컴파일러가 지적하는 모든 exhaustive match에 MarketPlane 암 추가(바인딩 종류 나열/직렬화 테이블 등).
- [ ] **Step 4: `cargo test -p krw-agent-image --lib` 전체 통과 확인.**
- [ ] **Step 5: 커밋** `feat(agent-image): market_plane capability scope binding`

### Task 2: run-engine — 권한 매트릭스: question_only 허가 + market_plane 검증

**Files:**
- Modify: `crates/run-engine/src/validation.rs:387-482` (`validate_capability_run_scope`), `:618-674` (`validate_ticker_reference_values`, `validate_scope_ticker`)
- Test: `crates/run-engine/src/validation.rs` 테스트 모듈(또는 lib.rs 기존 스코프 테스트 옆)

**Interfaces:**
- Consumes: Task 1의 `MarketPlane` 변형.
- Produces: (a) `(CoveredUniverse 바인딩, QuestionOnly 컨텍스트)` 허가 — `validate_covered_universe_binding` 재사용; (b) `(MarketPlane 바인딩, Company/Notebook/SelectedFeedItems)` = 종전 TrustedTickerSet과 동일 멤버십 검증; (c) `(MarketPlane, QuestionOnly)` = 정규 형식만 검증(`validate_market_plane_binding`); (d) 무스코프 거부 암은 `RoutingRequest | ExistingAnswer`만 남김.

- [ ] **Step 1: 실패 테스트 4개 작성** (기존 매트릭스 테스트 스타일 참고):
  1. `question_only` + covered_universe 바인딩 capability + `/universe:"covered"`, `/limit_tickers:5`, 티커 인자 없음 + 엔트리포인트 cardinality Max(12) → Ok.
  2. `question_only` + market_plane 바인딩 + `/symbol:"PLTR"`(스코프 밖 티커) → Ok(형식만 검증).
  3. `company_ticker_set [SO]` + market_plane 바인딩 + `/symbol:"PLTR"` → `RunScopeViolation`(멤버십 위반 — 종전 행동 유지 회귀).
  4. `question_only` + covered_universe 바인딩 + `/tickers:["AVGO"]` → `RunScopeViolation`(명시 티커 금지 유지).
- [ ] **Step 2: 실패 확인** — `cargo test -p krw-agent-run-engine --no-default-features --lib market_plane question_only` (패키지명은 `cargo metadata`로 확인; 이하 동일).
- [ ] **Step 3: 구현**:

```rust
// validate_capability_run_scope 매칭에 암 추가/변경:
(
    CapabilityScopeBinding::MarketPlane {
        ticker_references,
        require_any_of,
        ..
    },
    RunContextV1::CompanyTickerSet { .. } | RunContextV1::ResearchNotebook { .. },
) => validate_trusted_ticker_binding(
    ticker_references,
    require_any_of,
    &[],
    arguments,
    context.trusted_tickers(),
),
(
    CapabilityScopeBinding::MarketPlane {
        ticker_references,
        require_any_of,
        ..
    },
    RunContextV1::SelectedFeedItems { .. },
) => {
    let scope = derived_ticker_scope.ok_or(EngineError::DerivedFeedScopeUnavailable)?;
    validate_trusted_ticker_binding(
        ticker_references,
        require_any_of,
        &[],
        arguments,
        &scope.tickers,
    )
}
(
    CapabilityScopeBinding::CoveredUniverse {
        ticker_references,
        required_string_values,
        bounded_integer_pointer,
    },
    RunContextV1::CoveredUniverse { .. } | RunContextV1::QuestionOnly {},
) => validate_covered_universe_binding(
    entrypoint,
    ticker_references,
    required_string_values,
    bounded_integer_pointer.as_deref(),
    arguments,
),
(
    CapabilityScopeBinding::MarketPlane {
        ticker_references,
        require_any_of,
        ..
    },
    RunContextV1::QuestionOnly {},
) => validate_market_plane_binding(ticker_references, require_any_of, arguments),
```

무스코프 암(`RunContextV1::QuestionOnly {} | RoutingRequest | ExistingAnswer → Err`)에서 `QuestionOnly` 제거. 그리고 멤버십 선택적화:

```rust
fn validate_ticker_reference_values(
    arguments: &Value,
    references: &[TickerReferenceSpec],
    trusted: Option<&BTreeSet<&str>>,
) -> Result<BTreeMap<String, usize>, EngineError> {
    // 본문 그대로, 단 validate_scope_ticker(ticker, trusted)? 호출로.
}
fn validate_scope_ticker(
    ticker: &str,
    trusted: Option<&BTreeSet<&str>>,
) -> Result<(), EngineError> {
    if !is_canonical_ticker(ticker) {
        return Err(EngineError::RunScopeViolation(
            "capability ticker is outside the immutable run scope",
        ));
    }
    if let Some(trusted) = trusted {
        if !trusted.contains(ticker) {
            return Err(EngineError::RunScopeViolation(
                "capability ticker is outside the immutable run scope",
            ));
        }
    }
    Ok(())
}

/// 자유 문의 시장 평면 규칙(비전 §6): openbb/뉴스 관측 도구는 정규 형식의
/// 어떤 티커(미커버 포함)도 받는다 — 증거 등급이 시장/뉴스급이기 때문.
/// 필링급 온톨로지 읽기는 covered-universe 바인딩이 여전히 지킨다.
fn validate_market_plane_binding(
    ticker_references: &[TickerReferenceSpec],
    require_any_of: &[String],
    arguments: &Value,
) -> Result<(), EngineError> {
    let observed = validate_ticker_reference_values(arguments, ticker_references, None)?;
    if !require_any_of
        .iter()
        .any(|reference_id| observed.get(reference_id).copied().unwrap_or_default() > 0)
    {
        return Err(EngineError::RunScopeViolation(
            "market-plane capability omitted its required ticker argument",
        ));
    }
    Ok(())
}
```

기존 `validate_covered_universe_binding`의 `validate_ticker_reference_values(..., &empty)` 호출은 `Some(&empty)`로 갱신(빈 신뢰집합 = 명시 티커 금지 의미 보존). `validate_trusted_ticker_binding` 내부 호출도 `Some(&trusted)`로.
- [ ] **Step 4: `scripts/dev-stack.sh test core` 전체 통과 확인** (기존 스코프 테스트 회귀 없음).
- [ ] **Step 5: 커밋** `feat(run-engine): question_only capability authorization — universe-bound + market-plane`

### Task 3: run-engine — 선행요구 구조적 도달불가 스킵

**Files:**
- Modify: `crates/run-engine/src/capability_dispatch.rs:1471-1484` (`prepare_calls` 내 선행요구 검사)
- Test: `crates/run-engine/src/lib.rs` (ScriptedProvider 기존 e2e-style 테스트 옆)

**Interfaces:**
- Produces: 선행요구는 `completed_capabilities` 포함 **또는** 현재 워크플로 프로그램이 그 선행요구 capability를 상태로 선언하지 않을 때(구조적으로 도달불가) 만족. `question_only` 실행에서 openbb.*/news.*(선행 `ontology.query_context`)가 열리는 근거.

- [ ] **Step 1: 실패 테스트** — QuestionOnly 컨텍스트 실행에서 `ontology.query_context` 상태가 없는 워크플로 상태에서 `market_plane` capability(prerequisites: [ontology.query_context]) 디스패치가 `CapabilityPrerequisiteMissing` 없이 준비되는지. 기존 FixtureEngine/ScriptedCapability 테스트 패턴(lib.rs:4947 부근)으로 mini 워크플로 fixture 작성.
- [ ] **Step 2: 실패 확인.**
- [ ] **Step 3: 구현**:

```rust
// prepare_calls 내, 기존 검사 교체:
let program_state_capabilities: BTreeSet<&str> = selected_entrypoint(input.image, input.request)
    .ok()
    .and_then(|entrypoint| {
        input
            .image
            .body
            .workflows
            .iter()
            .find(|workflow| workflow.id == entrypoint.workflow)
            .map(|workflow| {
                workflow
                    .states
                    .iter()
                    .filter_map(|state| state.capability_id.as_deref())
                    .collect::<BTreeSet<_>>()
            })
    })
    .unwrap_or_default();
if !capability.prerequisites.iter().all(|required| {
    state.completed_capabilities.contains(required)
        || !program_state_capabilities.contains(required.as_str())
}) {
    return Err(EngineError::CapabilityPrerequisiteMissing(capability.id.clone()));
}
```

(`EntrypointSpec`의 워크플로 필드명은 lib.rs:77-96에서 확인 — `workflow`. `selected_entrypoint`은 validation.rs pub(crate). 도달불가 스킵은 "워크플로가 의도적으로 그 선행요구를 두지 않은 체제"에서만 일어나고, 도달 가능하면 종전과 동일하게 완료 필요 — 주석으로 명시.)
- [ ] **Step 4: `scripts/dev-stack.sh test core` 통과 + 기존 선행요구 테스트 회귀 없음 확인.**
- [ ] **Step 5: 커밋** `feat(run-engine): skip structurally-unreachable capability prerequisites`

### Task 4: agent-image — QuestionOnly 카디널리티 Max 허용

**Files:**
- Modify: `crates/agent-image/src/lib.rs:3272-3276` (`validate_entrypoint_scope`의 QuestionOnly 정책) 및 `crates/runtime-config/src/lib.rs:2350-2365` 관련 테스트
- Test: `crates/agent-image/src/lib.rs`

**Interfaces:**
- Produces: `allowed_context: question_only`에서 cardinality가 `exact 0`(발견 없는 변형) 또는 `max 1..=20`(발견 상한 — covered_universe 선례) 허용. `scoped_value_count()`가 항상 0이라 admission은 자동 통과. planner(`max_discovery_tickers`)와 `validate_covered_universe_binding`의 limit 상한이 cardinality에서 흘러들어온다.

- [ ] **Step 1: 실패 테스트** — `question_only + cardinality max 12` 스펙이 현재 거부됨을 확인 → 허용으로 바뀌는지; `exact 1`은 여전히 거부; `require_uppercase` canonicalization은 여전히 거부.
- [ ] **Step 2: 실패 확인 → Step 3: 구현** (해당 라인의 실제 코드 형태에 맞춰 QuestionOnly 분기 분리):

```rust
RunContextKind::QuestionOnly => {
    // question_only는 질문 텍스트만 신뢰 입력이다. exact 0 = 발견 없음,
    // max n = 발견 상한(covered_universe 선례와 동일 의미 — 카디널리티는
    // 모델의 bounded discovery limit 상한으로 쓰인다).
    let cardinality_ok = matches!(
        cardinality,
        ScopeCardinality::Exact { value: 0 } | ScopeCardinality::Max { value: 1..=20 }
    );
    if !cardinality_ok || canonicalization != TickerCanonicalization::NotApplicable {
        return Err(ImageError::EntrypointScopePolicy(/* 기존 오류 타입 */));
    }
}
RunContextKind::RoutingRequest => { /* 종전 QuestionOnly 규칙 그대로: exact 0 + not_applicable */ }
```

- [ ] **Step 4: `cargo test -p krw-agent-image --lib` 통과.**
- [ ] **Step 5: 커밋** `feat(agent-image): allow bounded discovery cardinality for question_only`

### Task 5: research-planner — trusted_scope_values question_only 분기

**Files:**
- Modify: `crates/research-planner/src/initial_plan.rs:1692-1708` (`trusted_scope_values`)
- Test: `crates/research-planner/src/initial_plan.rs` (기존 `max_discovery_tickers: 1` 테스트들 옆)

**Interfaces:**
- Produces: QuestionOnly 실행의 최초 SearchPlan 스코프 = `(tickers: [], universe: "covered", limit=min(requested, cap))` — CoveredUniverse와 동일. 매크로 단독(티커 0개) 계획이 planner에서 성립하는 것이 이 분기로 보장된다(비전 §10 최우선 검증 항목).

- [ ] **Step 1: 실패 테스트 2개** — (1) `context: QuestionOnly{}`, `max_discovery_tickers: 12`, `requested_limit_tickers: 20` → `([], "covered", 12)`; (2) `max_discovery_tickers: 0`(exact 0 변형) → `UnsupportedScope`.
- [ ] **Step 2: 실패 확인 → Step 3: 구현**:

```rust
RunContextV1::CoveredUniverse { .. } | RunContextV1::QuestionOnly {} => {
    let cap = u64::from(scope.max_discovery_tickers);
    if cap == 0 {
        return Err(InitialPlanError::UnsupportedScope);
    }
    Ok((
        Vec::new(),
        Value::String("covered".into()),
        requested_limit_tickers.min(cap),
    ))
}
```

- [ ] **Step 4: `cargo test -p krw-research-planner --lib` 통과.**
- [ ] **Step 5: 커밋** `feat(research-planner): question_only initial-plan scope (universe discovery, macro-only plans)`

### Task 6: agent.yaml — market_plane 리태그 (14 capabilities)

**Files:**
- Modify: `agents/krw-ontology/agent.yaml` capabilities 섹션
- Test: `cargo test -p krw-agent-image --lib` + `cargo test -p krw-agent-run-engine --no-default-features --lib` (기존 테스트가 바인딩 종류를 assert하면 갱신)

**Interfaces:**
- Consumes: Task 1 `market_plane`.
- 리태그 목록(`kind: trusted_ticker_set` → `kind: market_plane`, `ticker_references`/`require_any_of` 그대로): `market.snapshot`, `market.series`, `openbb.price_history`, `openbb.quote`, `openbb.metrics`, `openbb.income_statement`, `openbb.balance_statement`, `openbb.cash_statement`, `openbb.consensus`, `openbb.peers`, `openbb.earnings_calendar`, `openbb.filings`, `news.feed_list`, `news.web_search`.
- 무변경: `ontology.*`(필링급, universe 변형이 별도 존재), `filing.*`, `quant.dcf`, unscoped들(`macro.series`, `openbb.macro_series`, `openbb.macro_cpi`, `openbb.yield_curve`, `openbb.macro_calendar`, `skill.load`).

- [ ] **Step 1: 리태그 적용(sed 아닌 Edit 도구로 14곳).**
- [ ] **Step 2: 테스트** — `cargo test -p krw-agent-image --lib && scripts/dev-stack.sh test core`; 바인딩 종류를 검사하는 테스트가 깨지면 의미에 맞게 갱신.
- [ ] **Step 3: 커밋** `refactor(image): retag market/news observation reads as market_plane scope`

### Task 7: agent.yaml — open_research 엔트리포인트/워크플로/롤/프롬프트/예산

**Files:**
- Modify: `agents/krw-ontology/agent.yaml` (entrypoints, roles, prompt_segments, workflows)
- Create: `agents/krw-ontology/prompts/open-research-analysis.md`, `agents/krw-ontology/references/open-research-output-contract.md`
- Modify: `agents/fixtures/entrypoints.tsv`, `deployments/local/budget-registry.yaml`, `deployments/prod/budget-registry.yaml`
- Test: `agents/check-all.sh`, `cargo test -p krw-agent-image --lib`, `scripts/dev-stack.sh test core`

**Interfaces:**
- Produces: 런카인드 `open_research`(ko-KR, workflow `open_research_v1`, scope `question_only`/`max 12`/`not_applicable`, budget `open_research`, model `glm_high`). roles `open_planner`/`open_analyst`/`open_composer`. 세그먼트 `open_research_analysis`/`open_research_output_contract`.

- [ ] **Step 1: 엔트리포인트 추가** (idea_generation 항목 바로 뒤, 동일 키 구조):

```yaml
  - run_kind: open_research
    locale: ko-KR
    workflow: open_research_v1
    required_budget_profile: open_research
    required_model_profile: glm_high
    scope:
      allowed_context: question_only
      cardinality: { kind: max, value: 12 }
      ticker_canonicalization: not_applicable
    constants: {}
```

(실제 키 순서/들여쓰기는 인접 항목과 정확히 일치시킬 것 — `sed -n '9,52p' agents/krw-ontology/agent.yaml` 참고.)

- [ ] **Step 2: roles 3종 추가** (roles 섹션 말미):

```yaml
  - id: open_planner
    prompt_segments: [security_boundary, kernel_runtime, ontology_catalog, wide_skill_catalog, research_planner_skill, research_scope]
    deterministic: false
    execution: { reasoning: direct, max_output_tokens: 8192 }
  - id: open_analyst
    prompt_segments: [security_boundary, kernel_runtime, ontology_catalog, wide_skill_catalog, evidence_analyst, open_research_analysis]
    deterministic: false
    execution: { reasoning: standard, max_output_tokens: 16384 }
  - id: open_composer
    prompt_segments: [security_boundary, kernel_runtime, research_synthesis, plain_korean_investor_language, final_markdown_contract, open_research_output_contract]
    deterministic: false
    execution: { reasoning: standard, max_output_tokens: 16384 }
```

(`wide_skill_catalog` 세그먼트 id가 실재하는지 먼저 확인 — `grep -n "wide_skill_catalog" agents/krw-ontology/agent.yaml`.)

- [ ] **Step 3: prompt_segments 2종 추가 + 프롬프트 파일 작성**:

```yaml
  - { id: open_research_analysis, path: prompts/open-research-analysis.md, stable_prefix: false, private: true }
  - { id: open_research_output_contract, path: references/open-research-output-contract.md, stable_prefix: false, private: true }
```

`prompts/open-research-analysis.md`(분석가 계약, 한국어) 필수 내용: (1) 자유 질문 체제 — 신뢰 스코프는 "커버드 유니버스(온톨로지 355社) 대상 발견"뿐; (2) 발견 사다리 — 1단계 `query_context_universe`(검색 계획, 명시 티커 금지) → 2단계 `query_universe`/`trace_universe` 후속 → 3단계 지표/매크로(티커 불필요); (3) 시장 평면 도구(openbb.*, 뉴스)는 미커버 티커 포함 전 시장 대상 — 단 그 증거는 시장/뉴스급이며 필링급 주장의 근거가 될 수 없음; (4) 지표 관측 의무 — 미시(업황·산업)는 발견된 동종 회사들의 온톨로지 객체에서, 거시는 관측 계열/시세를 시계열로; (5) 근거가 없으면 지어내지 않고 커버리지 한계를 답변에 명시; (6) 조사 중 데이터 갭은 답변 말미에 명시할 것(되묻기 1차 형태).
`references/open-research-output-contract.md`(작성 계약): 증거 등급 표기 규칙(필링급=온톨로지 인용/관측급=관측 계열/시장·뉴스급=openbb·뉴스, 각 주장에 인용), "온톨로지 커버리지 밖" 회사 명시 규칙, 어드바이저리 문구 유지, 확인 필요 갭+후속 질문 2-3개 제안, 표·헤딩 허용.
- [ ] **Step 4: 워크플로 추가** (workflows 섹션 말미; idea의 clarify 화면 + wide의 조사 루프 + company의 openbb/뉴스 frontier 결합):

```yaml
  - id: open_research_v1
    initial: accepted
    states:
      - { id: accepted, kind: start, max_visits: 1 }
      # assess 선행: 시작 직후 상태는 ModelDecision이어야 한다(idea 선례).
      - { id: classify_screen, kind: assess, role_id: open_analyst, max_visits: 1 }
      - { id: compose_clarification, kind: compose, role_id: open_composer, max_visits: 1 }
      - { id: author_plan, kind: plan, role_id: open_planner, max_visits: 2 }
      - { id: validate_plan, kind: validate, max_visits: 2 }
      - { id: repair_plan, kind: plan, role_id: open_planner, max_visits: 1 }
      - { id: discover_context, kind: capability, capability_id: ontology.query_context_universe, max_visits: 2 }
      - { id: ingest_evidence, kind: ingest, max_visits: 29 }
      - { id: assess_frontier, kind: assess, role_id: open_analyst, max_visits: 8 }
      - { id: targeted_query, kind: capability, capability_id: ontology.query_universe, max_visits: 3 }
      - { id: trace_selected, kind: capability, capability_id: ontology.trace_universe, max_visits: 2 }
      - { id: market_series_lookup, kind: capability, capability_id: market.series, max_visits: 1 }
      - { id: macro_series_lookup, kind: capability, capability_id: macro.series, max_visits: 1 }
      - { id: openbb_price_lookup, kind: capability, capability_id: openbb.price_history, max_visits: 1 }
      - { id: openbb_quote_lookup, kind: capability, capability_id: openbb.quote, max_visits: 1 }
      - { id: openbb_metrics_lookup, kind: capability, capability_id: openbb.metrics, max_visits: 1 }
      - { id: openbb_income_lookup, kind: capability, capability_id: openbb.income_statement, max_visits: 1 }
      - { id: openbb_balance_lookup, kind: capability, capability_id: openbb.balance_statement, max_visits: 1 }
      - { id: openbb_cash_lookup, kind: capability, capability_id: openbb.cash_statement, max_visits: 1 }
      - { id: openbb_consensus_lookup, kind: capability, capability_id: openbb.consensus, max_visits: 1 }
      - { id: openbb_peers_lookup, kind: capability, capability_id: openbb.peers, max_visits: 1 }
      - { id: openbb_earnings_calendar_lookup, kind: capability, capability_id: openbb.earnings_calendar, max_visits: 1 }
      - { id: openbb_filings_lookup, kind: capability, capability_id: openbb.filings, max_visits: 1 }
      - { id: openbb_macro_cpi_lookup, kind: capability, capability_id: openbb.macro_cpi, max_visits: 1 }
      - { id: openbb_yield_curve_lookup, kind: capability, capability_id: openbb.yield_curve, max_visits: 1 }
      - { id: openbb_macro_calendar_lookup, kind: capability, capability_id: openbb.macro_calendar, max_visits: 1 }
      - { id: news_web_search, kind: capability, capability_id: news.web_search, max_visits: 1 }
      - { id: compose_ir, kind: compose, role_id: open_composer, max_visits: 2 }
      - { id: verify_ir, kind: verify, max_visits: 2 }
      - { id: repair_ir, kind: compose, role_id: open_composer, max_visits: 1 }
      - { id: render_ko, kind: render, max_visits: 1 }
      - { id: commit, kind: commit, max_visits: 1 }
      - { id: succeeded, kind: terminal, terminal: succeeded, max_visits: 1 }
    transitions:
      - { from: accepted, on: begin, to: classify_screen }
      - { from: classify_screen, on: clarification_required, to: compose_clarification }
      - { from: classify_screen, on: screen_ready, to: author_plan }
      - { from: classify_screen, on: output_budget_reserved, to: compose_ir }
      - { from: compose_clarification, on: draft_ready, to: verify_ir }
      - { from: author_plan, on: plan_ready, to: validate_plan }
      - { from: author_plan, on: proposal_unrecoverable, to: compose_ir }
      - { from: author_plan, on: proposal_contract_invalid, to: repair_plan }
      - { from: validate_plan, on: plan_invalid, to: repair_plan }
      - { from: validate_plan, on: plan_valid, to: discover_context }
      - { from: repair_plan, on: plan_ready, to: validate_plan }
      - { from: discover_context, on: correction_required, to: repair_plan }
      - { from: discover_context, on: correction_unresolved, to: assess_frontier }
      - { from: discover_context, on: evidence_observed, to: ingest_evidence }
      - { from: ingest_evidence, on: ingested, to: assess_frontier }
      - { from: ingest_evidence, on: output_budget_reserved, to: compose_ir }
      - { from: assess_frontier, on: market_series_has_value, to: market_series_lookup }
      - { from: assess_frontier, on: macro_series_has_value, to: macro_series_lookup }
      - { from: assess_frontier, on: openbb_price_has_value, to: openbb_price_lookup }
      - { from: assess_frontier, on: openbb_quote_has_value, to: openbb_quote_lookup }
      - { from: assess_frontier, on: openbb_metrics_has_value, to: openbb_metrics_lookup }
      - { from: assess_frontier, on: openbb_income_has_value, to: openbb_income_lookup }
      - { from: assess_frontier, on: openbb_balance_has_value, to: openbb_balance_lookup }
      - { from: assess_frontier, on: openbb_cash_has_value, to: openbb_cash_lookup }
      - { from: assess_frontier, on: openbb_consensus_has_value, to: openbb_consensus_lookup }
      - { from: assess_frontier, on: openbb_peers_has_value, to: openbb_peers_lookup }
      - { from: assess_frontier, on: openbb_earnings_calendar_has_value, to: openbb_earnings_calendar_lookup }
      - { from: assess_frontier, on: openbb_filings_has_value, to: openbb_filings_lookup }
      - { from: assess_frontier, on: openbb_cpi_has_value, to: openbb_macro_cpi_lookup }
      - { from: assess_frontier, on: openbb_yield_curve_has_value, to: openbb_yield_curve_lookup }
      - { from: assess_frontier, on: openbb_macro_calendar_has_value, to: openbb_macro_calendar_lookup }
      - { from: assess_frontier, on: news_web_has_value, to: news_web_search }
      - { from: assess_frontier, on: precise_query_has_value, to: targeted_query }
      - { from: assess_frontier, on: selected_trace_has_value, to: trace_selected }
      - { from: assess_frontier, on: append_context_plan, to: discover_context }
      - { from: assess_frontier, on: proposal_rejected, to: assess_frontier }
      - { from: assess_frontier, on: evidence_sufficient, to: compose_ir }
      - { from: assess_frontier, on: no_positive_value_action, to: compose_ir }
      - { from: assess_frontier, on: output_budget_reserved, to: compose_ir }
      - { from: targeted_query, on: evidence_observed, to: ingest_evidence }
      - { from: trace_selected, on: evidence_observed, to: ingest_evidence }
      - { from: market_series_lookup, on: evidence_observed, to: ingest_evidence }
      - { from: macro_series_lookup, on: evidence_observed, to: ingest_evidence }
      - { from: openbb_price_lookup, on: evidence_observed, to: ingest_evidence }
      - { from: openbb_quote_lookup, on: evidence_observed, to: ingest_evidence }
      - { from: openbb_metrics_lookup, on: evidence_observed, to: ingest_evidence }
      - { from: openbb_income_lookup, on: evidence_observed, to: ingest_evidence }
      - { from: openbb_balance_lookup, on: evidence_observed, to: ingest_evidence }
      - { from: openbb_cash_lookup, on: evidence_observed, to: ingest_evidence }
      - { from: openbb_consensus_lookup, on: evidence_observed, to: ingest_evidence }
      - { from: openbb_peers_lookup, on: evidence_observed, to: ingest_evidence }
      - { from: openbb_earnings_calendar_lookup, on: evidence_observed, to: ingest_evidence }
      - { from: openbb_filings_lookup, on: evidence_observed, to: ingest_evidence }
      - { from: openbb_macro_cpi_lookup, on: evidence_observed, to: ingest_evidence }
      - { from: openbb_yield_curve_lookup, on: evidence_observed, to: ingest_evidence }
      - { from: openbb_macro_calendar_lookup, on: evidence_observed, to: ingest_evidence }
      - { from: news_web_search, on: evidence_observed, to: ingest_evidence }
      - { from: compose_ir, on: draft_ready, to: verify_ir }
      - { from: verify_ir, on: repair_required, to: repair_ir }
      - { from: verify_ir, on: verified, to: render_ko }
      - { from: repair_ir, on: draft_ready, to: verify_ir }
      - { from: render_ko, on: rendered, to: commit }
      - { from: commit, on: committed, to: succeeded }
```

(엔진 스펙 검증기가 지적하는 형식 오류 — 예: assess 상태의 전이 이벤트 집합, ingest 상한 대비 광고 가능 읽기 수 — 는 검증기 메시지에 맞춰 조정한다.)
- [ ] **Step 5: 예산 프로필** — local + prod 양쪽 `budget-registry.yaml`에 추가(wide_research 항목 뒤):

```yaml
  - profile_id: open_research
    limits:
      max_provider_turns: 24
      max_capability_calls: 24
      max_replans: 1
      max_repairs: 1
      max_input_tokens: 120000
      # Must exceed the compose retry reserve threshold (34816).
      max_output_tokens: 36864
      max_evidence_bytes: 8388608
      deadline_ms: 1500000
      capability_call_limits:
        ontology.query_context_universe: 4
        ontology.query_universe: 4
        ontology.trace_universe: 2
        openbb.quote: 3
        openbb.metrics: 3
        openbb.income_statement: 3
        openbb.filings: 3
        news.web_search: 2
```

- [ ] **Step 6: `agents/fixtures/entrypoints.tsv`에 open_research 행 추가**(기존 형식 준수) 후 `agents/check-all.sh` 실행 → 통과.
- [ ] **Step 7: 테스트** — `cargo test -p krw-agent-image --lib && scripts/dev-stack.sh test core` 전체 통과(엔트리포인트 수·워크플로 수를 assert하는 테스트가 있으면 갱신). 추가: open_research_v1의 capability 상태가 모두 실재 capability id를 참조하는지 검증하는 컴파일 테스트 1개(기존 `causal_chain_capability_is_reachable...` 스타일).
- [ ] **Step 8: 커밋** `feat(image): open_research entrypoint, workflow, roles, prompts, budget`

### Task 8: host-ts — 게이트웨이 오픈 리서치 계약

**Files:**
- Modify: `packages/host-ts/src/gateway.ts`, `packages/host-ts/src/contracts.ts`(필요 시), `packages/host-ts/src/local-gateway.ts`(createRun 라우팅)
- Test: `packages/host-ts/test/`(신규 `open-research.test.ts` 또는 기존 gateway 테스트 확장)

**Interfaces:**
- Produces:
  - `GatewayOpenResearchRequestV1 { schema_version: 1, question: string }`
  - `parseGatewayRunRequest(value): GatewayCompanyResearchRequestV1 | GatewayOpenResearchRequestV1` — body에 `ticker` 키가 있으면 회사 파싱(종전 규칙 그대로), 없으면 자유 문 파싱(`advisor_lens` 금지, `question` 검증 동일).
  - `prepareGatewayOpenResearch(input): Promise<PreparedEnqueueRunV1>` — run_kind `open_research`, locale은 한글→ko-KR, 라틴→descriptor에 `(open_research, en-US)` 엔트리가 있을 때만 en-US(지금은 없으므로 ko-KR 폴백).
  - POST /runs 라우트가 두 프렙 함수로 분기.

- [ ] **Step 1: 실패 테스트 작성** (`node --import tsx --test test/*.test.ts` 스타일):
  1. `{schema_version:1, question:"반도체 병목 관련 회사 있나?"}` 파싱 Ok + `prepareGatewayOpenResearch` 인텐트 `run_kind:"open_research"`, `context:{kind:"question_only"}`, locale ko-KR.
  2. `{schema_version:1, question:"PLTR earnings?", ticker:"PLTR"}` → 회사 파싱(종전과 동일).
  3. ticker 없이 `advisor_lens` → `ContractViolation`.
  4. 라틴 질문 + descriptor에 open_research_en 없음 → locale ko-KR(우회 폴백).
- [ ] **Step 2: 실패 확인 → Step 3: 구현** — gateway.ts에 위 인터페이스 그대로(로케일 헬퍼):

```ts
function releaseHasOpenResearchEntrypoint(descriptor: unknown, locale: string): boolean {
  if (!isPlainObject(descriptor) || !Array.isArray(descriptor.entries)) return false;
  return descriptor.entries.some(
    (entry) =>
      isPlainObject(entry) && entry.run_kind === "open_research" && entry.locale === locale,
  );
}

export function openResearchLocaleForQuestion(
  question: string,
  descriptor: unknown,
): "ko-KR" | "en-US" {
  if (/\p{Script=Hangul}/u.test(question)) return "ko-KR";
  if (
    /[A-Za-z]/.test(question) &&
    releaseHasOpenResearchEntrypoint(descriptor, "en-US")
  ) {
    return "en-US";
  }
  return "ko-KR";
}
```

local-gateway.ts createRun(305-365): 본문 파싱을 `parseGatewayRunRequest`로 갈라 `request`에 `ticker`가 있으면 기존 `prepareGatewayCompanyResearch`, 없으면 `prepareGatewayOpenResearch`. (실제 함수 시그니처는 파일에서 확인해 그대로 맞춘다.)
- [ ] **Step 4: `cd packages/host-ts && npm test && npm run typecheck` 통과.**
- [ ] **Step 5: 커밋** `feat(host-ts): ticker-less open-research gateway contract`

### Task 9: krw-agent CLI — ticker 선택화

**Files:**
- Modify: `bins/krw-agent/src/main.rs:94-124`(Run 인수), `bins/krw-agent/src/gateway.rs`(제출 body)
- Test: `bins/krw-agent/src/gateway.rs` 단위 테스트(있으면), 없으면 main.rs 테스트 모듈에 body 직렬화 테스트 추가

**Interfaces:**
- Produces: `krw-agent run --question ... [--ticker ...]` — ticker 생략 시 body에서 `ticker` 키를 완전히 생략(자유 문). 도움말 문구 갱신("One exact company ticker… omit for open research").

- [ ] **Step 1: 테스트(있다면) → Step 2: 구현** — `ticker: Option<String>`; body 빌드 시 `ticker`는 Some일 때만 삽입(`Object.hasOwn` 판별이 게이트웨이 분기 기준이므로 null 금지).
- [ ] **Step 3: `cargo build -p krw-agent`(bins 패키지명 확인) + 관련 테스트 통과.**
- [ ] **Step 4: 커밋** `feat(cli): ticker-optional gateway run submit (open research)`

### Task 10: krw-agent-service — LLM 디스패처 + 커버리지 문 선택

**Files:**
- Create: `krw_agent_service/dispatcher.py`, `krw_agent_service/coverage.py`
- Modify: `krw_agent_service/config.py`, `krw_agent_service/runners.py`, `krw_agent_service/flow.py`
- Test: `tests/test_dispatcher.py`, `tests/test_coverage.py`, `tests/test_gateway_runner.py`, `tests/test_query_stream.py` 갱신

**Interfaces:**
- Produces:
  - `dispatcher.dispatch_ticker(question: str) -> str | None` — 항상 폴백 안전(미설정·타임아웃·오류·파싱 실패 → None). 환경: `KRW_AGENT_DISPATCHER_BASE_URL`(기본 `https://open.bigmodel.cn/api/paas/v4`), `KRW_AGENT_DISPATCHER_API_KEY`(없으면 `GLM_API_KEY`), `KRW_AGENT_DISPATCHER_MODEL`(기본 `glm-4-flash`), `KRW_AGENT_DISPATCHER_TIMEOUT_SECONDS`(기본 6).
  - `coverage.CoverageCatalog.covers(ticker: str) -> bool | None` — MCP `krw_ontology_catalog`(환경 `KRW_AGENT_COVERAGE_URL`, 예: `http://127.0.0.1:8088/mcp`). 1시간 캐시, 실패/미설정 → None(개방 문 폴백).
  - `GatewayRunner.run`: 티커+커버드 → 회사 body `{schema_version, question, ticker}`; 티커 없음 또는 미커버/커버리지 불명 → 자유 body `{schema_version, question}`. `extract_ticker`/`_TICKER_RE`/`_TICKER_DENYLIST`/`TickerNotFound`/`NO_TICKER_MARKDOWN` 제거.

- [ ] **Step 1: 실패 테스트 작성** — (1) dispatcher: httpx MockTransport로 정상 JSON `{"ticker":"SO"}` → "SO"; 소문자 "pltr" → "PLTR"(대문elem 정규화 후 `^[A-Z][A-Z0-9.-]{0,15}$` 검사); 오류/타임아웃/불완전 → None; 키 미설정 → None(LLM 호출 없음). (2) coverage: MockTransport로 companies 응답 → covers("SO") True, covers("PLTR") False; 실패 → None. (3) runners: 디스패처 SO+커버드 → 회사 body; PLTR+미커버 → 자유 body; None → 자유 body. (4) flow: 티커 없는 질문이 더 이상 안내문으로 끝나지 않고 게이트웨이 제출까지 감(StubRunner 기반).
- [ ] **Step 2: 실패 확인 → Step 3: 구현.**

`dispatcher.py` 골격(전체 로직 포함, 한국어 주석):

```python
"""가벼운 LLM 디스패처 — 질문에서 티커 유무를 해석한다(정규식 폐지, 비전 §4.1).

모든 실패 경로(키 미설정·타임아웃·HTTP 오류·파싱 실패·형식 불일치)는
ticker=None, 즉 자유 문 폴백이다. 티커 없는 질문은 자유 문에서 어차피
해결되므로 폴백은 안전하다.
"""
from __future__ import annotations

import json
import re

import httpx

from .config import dispatcher_base_url, dispatcher_api_key, dispatcher_model, dispatcher_timeout_seconds

_TICKER_PATTERN = re.compile(r"^[A-Z][A-Z0-9.\-]{0,15}$")

_SYSTEM_PROMPT = (
    "당신은 투자 질문 분류기다. 사용자 질문에 명시된 주식 티커(심볼)를 추출한다.\n"
    "규칙: 1) 정확한 티커만 추출(예: SO, PLTR, NVDA). 2) 회사명(예: 세일즈포스)은\n"
    "티커로 바꾸지 말고 null. 3) 티커가 없거나 애매하면 null. 4) 출력은 오직\n"
    'JSON 한 줄: {"ticker": "XXXX"} 또는 {"ticker": null}'
)


async def dispatch_ticker(question: str) -> str | None:
    api_key = dispatcher_api_key()
    if not api_key:
        return None
    payload = {
        "model": dispatcher_model(),
        "messages": [
            {"role": "system", "content": _SYSTEM_PROMPT},
            {"role": "user", "content": question},
        ],
        "temperature": 0,
        "max_tokens": 512,
    }
    headers = {"Authorization": f"Bearer {api_key}"}
    try:
        async with httpx.AsyncClient(timeout=dispatcher_timeout_seconds()) as client:
            response = await client.post(
                f"{dispatcher_base_url().rstrip('/')}/chat/completions",
                json=payload,
                headers=headers,
            )
            response.raise_for_status()
            content = response.json()["choices"][0]["message"]["content"]
    except Exception:
        return None
    return _parse_ticker(content)


def _parse_ticker(content: str) -> str | None:
    start = content.find("{")
    end = content.rfind("}")
    if start < 0 or end <= start:
        return None
    try:
        value = json.loads(content[start : end + 1]).get("ticker")
    except Exception:
        return None
    if not isinstance(value, str):
        return None
    ticker = value.strip().upper()
    return ticker if _TICKER_PATTERN.fullmatch(ticker) else None
```

`coverage.py` 골격:

```python
"""커버드 유니버스(온톨로지 카탈로그) 집합 소속 판정 — 결정론적 문 선택(비전 §4.2).

KRW_AGENT_COVERAGE_URL이 가리키는 MCP 엔드포인트에서 krw_ontology_catalog의
companies 목록을 받아 1시간 캐시한다. 릴리스 단위로만 바뀌는 정적 집합이다.
실패/미설정은 None을 반환하고 호출자는 자유 문으로 폴백한다.
"""
from __future__ import annotations

import time

import httpx

from .config import coverage_url

_CACHE_TTL_SECONDS = 3600.0
_MCP_HEADERS = {
    "Accept": "application/json, text/event-stream",
    "Content-Type": "application/json",
}


class CoverageCatalog:
    def __init__(self) -> None:
        self._tickers: set[str] | None = None
        self._fetched_at: float = 0.0

    async def covers(self, ticker: str) -> bool | None:
        tickers = await self._universe()
        return None if tickers is None else ticker in tickers

    async def _universe(self) -> set[str] | None:
        now = time.monotonic()
        if self._tickers is not None and now - self._fetched_at < _CACHE_TTL_SECONDS:
            return self._tickers
        tickers = await self._fetch()
        if tickers is not None:
            self._tickers = tickers
            self._fetched_at = now
        return tickers

    async def _fetch(self) -> set[str] | None:
        url = coverage_url()
        if not url:
            return None
        base = url.rstrip("/")
        try:
            async with httpx.AsyncClient(timeout=10.0) as client:
                session_headers = dict(_MCP_HEADERS)
                initialize = await client.post(
                    base,
                    json={
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "initialize",
                        "params": {
                            "protocolVersion": "2025-03-26",
                            "capabilities": {},
                            "clientInfo": {"name": "krw-agent-service", "version": "0.1.0"},
                        },
                    },
                    headers=session_headers,
                )
                initialize.raise_for_status()
                session_id = initialize.headers.get("Mcp-Session-Id")
                if session_id:
                    session_headers["Mcp-Session-Id"] = session_id
                call = await client.post(
                    base,
                    json={
                        "jsonrpc": "2.0",
                        "id": 2,
                        "method": "tools/call",
                        "params": {"name": "krw_ontology_catalog", "arguments": {}},
                    },
                    headers=session_headers,
                )
                call.raise_for_status()
                return _extract_tickers(call.text)
        except Exception:
            return None


def _extract_tickers(payload: str) -> set[str] | None:
    # streamable-http SSE 프레임과 일반 JSON 모두 수용: 본문에서 JSON 객체를 찾아
    # companies 배열(문자열 또는 {ticker: ...})을 tolerant하게 파싱한다.
    import json as _json

    candidates: list[str] = []
    for line in payload.splitlines():
        text = line[5:].strip() if line.startswith("data:") else line.strip()
        if not text.startswith("{"):
            continue
        try:
            document = _json.loads(text)
        except Exception:
            continue
        stack = [document]
        while stack:
            node = stack.pop()
            if isinstance(node, dict):
                companies = node.get("companies")
                if isinstance(companies, list):
                    for company in companies:
                        if isinstance(company, str):
                            candidates.append(company)
                        elif isinstance(company, dict) and isinstance(
                            company.get("ticker"), str
                        ):
                            candidates.append(company["ticker"])
                stack.extend(value for value in node.values() if isinstance(value, (dict, list)))
            elif isinstance(node, list):
                stack.extend(node)
    return set(candidates) if candidates else None
```

`config.py`에 5개 env 게터 추가(기존 스타일: 호출 시점 `os.environ` 읽기, `tests/conftest.py` 미러 갱신). `runners.py`: `run()` 앞부분을 문 선택으로 교체(위 인터페이스 설명 그대로), 상태 전이 시 statusUpdate 텍스트 1회(queued→running 감지 시 "심층 조사를 진행 중입니다…"). `flow.py`: `TickerNotFound` import/branch와 `NO_TICKER_MARKDOWN` 삭제.
- [ ] **Step 4: `.venv/bin/pytest -q` 전체 통과.**
- [ ] **Step 5: 커밋** `feat(agent-service): LLM dispatcher + coverage-based door selection, retire regex ticker gate`

### Task 11: 전체 오프라인 스위트

- [ ] `cd ~/krw-ontology-v2/krw-agnet && cargo test -p krw-agent-protocol -p krw-agent-image -p krw-research-planner --lib && scripts/dev-stack.sh test core && scripts/dev-stack.sh test adapter`
- [ ] `cd packages/host-ts && npm test && npm run typecheck`
- [ ] `cd ../.. && agents/check-all.sh`
- [ ] `cd ../krw-agent-service && .venv/bin/pytest -q`
- [ ] 깨지는 기존 테스트는 의미 보존해 갱신(회귀가 아니라 계약 확장). 최종 커밋.

### Task 12: E2E 라이브 품질 테스트 + 결과 문서

**Files:**
- Create: `krw-agnet/docs/superpowers/e2e/2026-09-04-open-research-e2e.md`
- 산출물: 라이브 스택 로그/답변 캡처(시크릿 제외)

- [ ] **Step 1: 격리 스택 부팅** — `scripts/with_local_env.sh scripts/start_local_agent_gateway_stack.sh`를 오프셋 포트+별도 상태디렉터리로(`KRW_AGENT_GATEWAY_PORT=14518 KRW_AGENT_LOCAL_STATE_DIR=$PWD/.local/e2e-open` 등, `scripts/test_observation_e2e.sh:63-69` 패턴). 이미지 빌드가 새 엔트리포인트를 포함하는지 로그에서 확인. 온톨로지 사이드카는 기본 읽기전용 prod/current(불변 데이터, 쓰기 없음).
- [ ] **Step 2: 커버리지 소스 기동(에이전트서비스용)** — 별도 루프백 capabilityd 1개(`uv run krw-capabilityd`, 기본 :8088, 읽기전용) → `KRW_AGENT_COVERAGE_URL=http://127.0.0.1:8088/mcp`.
- [ ] **Step 3: 게이트웨이 직접 제출(엔진 경로)** — `target/debug/krw-agent run --gateway-url http://127.0.0.1:14518/v1/agent --question "..." --wait --wait-timeout-seconds 1500 --json` 형태. 질문은 성공기준 그대로: (1) "반도체 병목 관련된 회사 있나? 회사별 근거와 관련 지표도." (2) "인플레이션 오르면 어떤 섹터나 종목이 유리할까? 관련 거시 지표 흐름과 함께." 병렬 제출 후 순차 완료 대기(백그라운드).
- [ ] **Step 4: 에이전트 서비스 경로(전체 Copilot 회로)** — `KRW_AGENT_GATEWAY_URL=http://127.0.0.1:14518/v1/agent` (+토큰, 디스패처 키=GLM)로 `.venv/bin/uvicorn krw_agent_service.app:app --port 8391` 기동 후 (5) "PLTR 최근 실적은?" 자유 문 라우팅·(4) "SO 최근 실적은?" 회사 문 회귀를 SSE로 수집해 최종 마크다운·인용 저장.
- [ ] **Step 5: 판정 기준**(비전 §8): Q1 관련 회사 목록+필링 근거+커버리지 한계 명시; Q2 매크로 관측 증거+노출 회사+어드바이저리 라벨(티커 0개 성립 포함); Q5 openbb 기반 답변+"커버리지 밖" 명시; Q4 run state/trace에서 company_research 런카인드 확인(라우팅 무손상). 각 답변에서 벤더 토큰(fmp/fred/polygon) 부재 확인.
- [ ] **Step 6: 결과 문서 작성 + 커밋** `docs(e2e): open research slice-1 live quality results`.
- [ ] **Step 7: 스택 정리(오프셋 인스턴스만 종료 — launchd 프로덕트 agentd/게이트웨이 절대 건드리지 않는다).**

## Self-Review (2026-09-04)

- 스펙 커버리지: 비전 §6 항목 1(protocol — 무변경 확인됨: `QuestionOnly {}` validate Ok), 2(agent-image 정책 — Task 4), 3(매트릭스 6곳 중 5곳 기존 처리 확인, 심장부 `validate_capability_run_scope` — Task 2), 4(planner — Task 5), 5(시장 사전필터 — company 런카인드 게이트라 자동 스킵, 무변경), 6-7(host-ts/게이트웨이 — Task 8), 8(엔트리포인트+릴리스 재핀 — Task 7 + Task 12의 스택 재빌드가 재핀을 수행), 9(capability frontier — Task 6/7), 10(픽스처 — Task 7/8/10). 게이트웨이·서비스 공사: 디스패처/커버리지/NO_TICKER 폐기(Task 10), 티커 선택화(Task 8/9). §7의 "회사 문 지표 확대 1b"와 open_research_en은 전역 제약에 명시된 연기 사항.
- 오프라인 빌드가 Task 6 중간에 깨질 수 있다(바인딩 리태그 후 매트릭스 암 없음) — Task 순서가 1→2→6이므로 Task 6 시점에는 암이 이미 존재한다. Task 7 전에는 open_research 엔트리포인트가 없어 check-all은 통과해야 한다(tsv 행은 Task 7에서 추가).
- 타입 정합성: `MarketPlane` 필드명(ticker_references/require_any_of) Task 1↔2↔6 일치; `validate_ticker_reference_values` 시그니처 변경(Option<&BTreeSet>)의 호출처 3곳 갱신 Task 2에 포함; `selected_entrypoint(input.image, input.request)`는 validation.rs pub(crate) 그대로(Task 3).
- E2E 라이브 런은 ~10-25분/건, 4건 병렬/순차 혼합 — 세션 예산 내 수행, 진행 상황은 상태 요약으로 보고.
