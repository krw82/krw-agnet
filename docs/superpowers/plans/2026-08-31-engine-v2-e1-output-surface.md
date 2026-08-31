# Engine v2 E1 — 출력 표면 확장 (Report Sections · Typed Caps · Charts) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 설계 문서 E1(최우선) 단계를 구현한다 — ① 작성기 어드미션 수학의 실제 제약을 진단·분리해 컴포지션 상한 상향이 연구 활주로를 잠식하지 않게 만들고, ② 섹션 단위 다중 compose→verify 사이클(`report-sections/v1`)로 16k 단일 턴 한계를 넘는 무손실 장문 리포트를 산출하며, ③ 타입 상한(섹션 64/클레임 256)·차트 상한(3→12)·팩 상한(16→32)을 확장하고, ④ 시맨틱 평가를 v3로 갱신한 뒤 GLM-5.3 게이트로 마감한다. 완료 기준(설계 §4): "장기 리서치가 16k를 넘는 리포트를 무손실 산출; 새니타이저 다운그레이드 0".

**Architecture:** 네 층위로 나뉜다. (a) **이미지 정책층**(krw-agent-image + `agents/krw-ontology/agent.yaml`): 명시적 `compose_retry_reserve_tokens`로 리저브를 상한에서 분리, compose 상태의 per-state `output_contract`(섹션 상태만 TypedJson) 도입, 워크플로우에 섹션 사이클 상태 추가. (b) **계약층**(krw-agent-contracts + `contracts/kernel/v1/schemas/`): `report-sections/v1`(섹션 배치, 모델 저작)·`answer-ir/v2`(조립된 최종 IR, 상한 64/256) 스키마 + 핀 등록. (c) **엔진층**(krw-agent-run-engine + krw-agent-evidence): TypedJson 배치 검증·누적(`ActiveRun.composed_sections`), 예산으로 종료되는 엔진 소유 루프, AnswerBundle v5 + 체크포인트 v15 + 회복 동치, 차트/팩 상한. (d) **평가층**(evals/krw-semantic/v3 + 매트릭스 러너): 리포트 가변 길이를 허용하는 계약 게이트 갱신 + 라이브 게이트.

**Tech Stack:** Rust workspace(cargo, `cargo test -p <crate>`) / JSON Schema 2020-12 + sha256 핀 / Python 3 검증 스크립트·러너

**스펙:** `docs/superpowers/specs/2026-08-31-engine-v2-autonomy-design.md` §2 R2, §4 E1, §5(E6 병행 주의), §7 리스크. 이 plan은 그 E1의 구현 계획이다. 모든 파일:라인 인용은 HEAD `81ea033` 실측.

## Global Constraints (불변 — 모든 태스크에서 위반 금지)

- 작업 폴더: `~/krw-ontology-v2/krw-agnet`, 브랜치 `feat/observation-v3-bundle` (base `81ea033`). 브랜치 생성/전환/삭제 금지, `main` 미접촉.
- **근거 교리 불변**: 강한 주장은 추적 가능한 직접 근거만; 관측/advisory는 `strong_claim_allowed: false`; `grade_never_upgrades_directness`. 새니타이저 claim↔ledger 바인딩(finalization.rs:384-419 `sanitize_answer` → evidence `validate_answer`) 로직 변경 금지 — 상한 수치만 갱신.
- **그라운딩 필터 불변**: `filter_grounded_visualizations`(crates/run-engine/src/finalization.rs:58-75)는 미변경. 차트 상한 12 확장 후에도 런 외부 근거 참조 아티팩트는 전량 탈락해야 한다.
- **final-markdown 계약 불변**: 결론 우선(`conclusion_first: true`), 한국어 투자자 문체, 후속질문 정확히 3개(`exact_follow_up_count: 3`), 가격 목표·확정 등급 금지(agent.yaml:1024-1040). 섹션 리포트에서도 이 계약은 렌더 결과에 그대로 적용된다.
- **v4 후방호환**: 기존 AnswerBundle v4 바이트는 신규 코드가 반드시 파싱해야 한다(`#[serde(default)]` 신규 필드). 구형 v4 체크포인트/번들이 깨지는 변경 금지.
- **E1 범위 밖**: 워크플로우 방문 상한·action_limits max_calls의 완화는 E4(연구 상태 `ingest_evidence`=14, `assess_obligations`=3, 각 capability 상태의 max_visits 일절 미변경). 4개 output-reserve 트리거의 "새 턴 시작 정지" 전환은 E3. 컨텍스트 봉투·컴팩션은 E3. 본 plan은 기존 상태의 max_visits를 하나도 바꾸지 않는다(신규 상태 추가만).
- 이미지 불변성: agent.yaml/프롬프트/계약 변경 = 이미지 해시 재계약. 배포·릴리스 갱신은 별도 작업(E1 게이트는 v2-dev 스택 기준).
- 테스트: 태스크마다 녹색, 네트워크 호출 금지(픽스처만). Rust는 `cargo test -p <crate>`. 커밋은 태스크 종료 시마다, 원자적으로.

---

### Task 1: 작성기 어드미션 수학 진단 + 명시적 재시도 리저브 분리

**진단(실측, agent.yaml:174-191 주석의 주장을 코드로 확정):**

| 제약 | 위치 | 내용 |
|---|---|---|
| 리저브 유도식 | `crates/agent-image/src/lib.rs:1822-1825` | `retry_reserve = max_composer_cap × 2`; 실효 리저브 = `max(base 8192, retry)` = **32,768** (composer cap 16,384 기준) |
| 컴파일 경계 | `crates/agent-image/src/lib.rs:3087-3096` | `cap × 2 > 64,000` → `InvalidSpec`(**"exceeds the 64,000-token two-attempt reserve"**) ⇒ cap ≤ 32,000 경성 상한. 그 위는 "image compile 실패" |
| 활주로 붕괴 | `crates/run-engine/src/provider_request.rs:601-607` + `crates/run-engine/src/active_run.rs:638-643` | 비답변 턴 가용 = `remaining − reserve`(미만이면 `FinalOutputReserveReached`), `remaining ≤ reserve + 2,048`이면 강제 compose. cap +1,000마다 리저브 +2,000 ⇒ 연구 활주로 −2,000. 출력 예산 36,000(피처 lib.rs:8707)에서 활주로는 36,000 − 34,816 = **1,184 토큰** |
| 외곽 상한 | `crates/runtime-config/src/lib.rs:43` | GLM 요청당 ceiling 131,072 (참고: per-request cap의 물리적 상한) |

즉 "더 큰 cap은 컴파일 실패(>32,000)하거나 활주로를 붕괴시킨다(2배 결합)"는 주석 주장이 사실로 확인됨. **수정**: 리저브를 cap에서 분리 — `answer_policy.compose_retry_reserve_tokens`(명시적, "가장 큰 작성기 턴 1회 전체 재시도" 이상)를 선언하면 유도식 `2×cap`을 대체한다. cap 상향이 리저브를 두 배로 불리지 않는다.

**Files:**
- Modify: `crates/agent-image/src/lib.rs:1153-1181` (`AnswerPolicySpec` 신규 필드), `:1780-1826` (`effective_final_output_reserve_tokens`), `:3035-3097` (`validate_final_output_reserve`), `:4679-4696` (기존 테스트 갱신)
- Modify: `agents/krw-ontology/agent.yaml:174-191` (composer 주석), `:1024-1027` (answer_policy 신규 키)
- Test: `crates/agent-image/src/lib.rs` tests 모듈(기존 파일에 케이스 추가)

**Interfaces:**
- Consumes: 없음(첫 태스크)
- Produces: `effective_final_output_reserve_tokens`가 `compose_retry_reserve_tokens` 선언 시 `max(base, explicit)` 반환 — 이후 모든 태스크(provider_request.rs:601-607, active_run.rs:630-644)는 기존 호출부 그대로 새 수치를 소비

- [ ] **Step 1: 실패 테스트 작성** — `crates/agent-image/src/lib.rs` tests 모듈(`mod tests`, :4481)에 추가. `parse_spec`/`validate_spec`은 같은 파일 내에서 직접 호출 가능하다:

```rust
    #[test]
    fn legacy_reserve_doubling_rejects_caps_above_the_two_attempt_ceiling() {
        // 실측 진단: cap > 32_000 이면 2 x cap > 64_000 이라 image compile 실패.
        let mut spec = parse_spec(&fs::read(agent_root().join("agent.yaml")).unwrap()).unwrap();
        for role in &mut spec.roles {
            if role.id == "composer" {
                role.execution.max_output_tokens = Some(40_000);
            }
        }
        let error = validate_spec(&spec).unwrap_err();
        assert!(format!("{error:?}").contains("64,000-token two-attempt reserve"));
    }

    #[test]
    fn explicit_retry_reserve_decouples_compose_cap_from_the_reserve() {
        let mut spec = parse_spec(&fs::read(agent_root().join("agent.yaml")).unwrap()).unwrap();
        spec.answer_policy.compose_retry_reserve_tokens = Some(16_384);
        for role in &mut spec.roles {
            if role.id == "composer" {
                role.execution.max_output_tokens = Some(40_000);
            }
        }
        // 명시적 리저브가 있으면 cap 40_000도 컴파일을 통과하고,
        // 리저브는 cap이 아니라 선언값을 따른다.
        validate_spec(&spec).expect("explicit retry reserve admits larger compose caps");
        for workflow in &spec.workflows {
            if workflow.id == "company_research_v2" {
                let max_cap = workflow
                    .states
                    .iter()
                    .filter(|state| state.kind == StateKind::Compose)
                    .filter_map(|state| state.role_id.as_deref())
                    .filter_map(|role_id| {
                        spec.roles
                            .iter()
                            .find(|role| role.id == role_id)
                            .and_then(|role| role.execution.max_output_tokens)
                    })
                    .max()
                    .unwrap_or_default();
                assert_eq!(max_cap, 40_000);
            }
        }
    }

    #[test]
    fn explicit_retry_reserve_must_cover_one_full_composer_retry() {
        let mut spec = parse_spec(&fs::read(agent_root().join("agent.yaml")).unwrap()).unwrap();
        // composer cap(16_384)보다 작은 리저브는 "1회 전체 재시도"를 보장 못 함.
        spec.answer_policy.compose_retry_reserve_tokens = Some(8_192);
        let error = validate_spec(&spec).unwrap_err();
        assert!(format!("{error:?}").contains("exceeds the declared retry reserve"));
    }
```

- [ ] **Step 2: 실패 확인** — `cargo test -p krw-agent-image explicit_retry_reserve legacy_reserve` → 컴파일 오류(`compose_retry_reserve_tokens` 필드 부재) 및 2번 케이스 실패로 시작됨을 기록.
- [ ] **Step 3: 구현** — `AnswerPolicySpec`(lib.rs:1159 `final_output_reserve_tokens` 뒤)에:

```rust
    /// E1: retry reserve for the terminal answer, declared instead of derived.
    /// When present it replaces the legacy implicit `2 x max compose cap`
    /// derivation, so raising a composer cap no longer doubles the tokens
    /// held back from research turns. Must cover one full retry of the
    /// workflow's largest composer turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compose_retry_reserve_tokens: Option<u32>,
```

`effective_final_output_reserve_tokens`(lib.rs:1822-1825)의 `let retry_reserve = …` 블록을 교체:

```rust
        let retry_reserve = match self.body.answer_policy.compose_retry_reserve_tokens {
            Some(explicit) => explicit,
            None => max_composer_cap.checked_mul(2).ok_or_else(|| {
                ImageError::InvalidSpec("compose retry reserve overflow".into())
            })?,
        };
        Ok(Some(base_reserve.max(retry_reserve)))
```

`validate_final_output_reserve`(lib.rs:3087-3096)의 per-cap 검사를 교체:

```rust
            let required_reserve = match spec.answer_policy.compose_retry_reserve_tokens {
                Some(explicit) => {
                    if !(256..=64_000).contains(&explicit) {
                        return Err(ImageError::InvalidSpec(format!(
                            "compose retry reserve must be between 256 and 64000 tokens (declared {explicit})"
                        )));
                    }
                    if cap > explicit {
                        return Err(ImageError::InvalidSpec(format!(
                            "compose role {role_id} output cap {cap} exceeds the declared retry reserve {explicit}"
                        )));
                    }
                    explicit
                }
                None => cap.checked_mul(2).ok_or_else(|| {
                    ImageError::InvalidSpec(format!(
                        "compose role {role_id} output cap is too large to reserve a full retry"
                    ))
                })?,
            };
            if required_reserve > 64_000 {
                return Err(ImageError::InvalidSpec(format!(
                    "compose role {role_id} output cap exceeds the 64,000-token two-attempt reserve"
                )));
            }
```

`agents/krw-ontology/agent.yaml` answer_policy(:1026 `final_output_reserve_tokens: 8192` 뒤)에:

```yaml
  # E1: the retry reserve is declared, not derived. One full retry of the
  # largest composer turn (16,384). Raising a compose cap no longer doubles
  # the held-back reserve (previously max(8192, 2x cap) = 32,768, which
  # capped compose caps at 32,000 and cut the research runway by 2x per
  # +1x cap). effective reserve: max(8192, 16384) = 16384.
  compose_retry_reserve_tokens: 16384
```

composer 역할 주석(agent.yaml:183-191)의 "The cap stays 16,384: the kernel's admission math holds at max(8192, 2x cap) = 32,768…" 문단을 새 수학으로 교체("the admission math now holds at the declared compose_retry_reserve_tokens (16,384); caps above the legacy 32,000 ceiling compile while the reserve stays decoupled"). 기존 테스트 `final_output_reserve_scales_to_each_workflows_composer`(lib.rs:4679-4696)의 기대값 `Some(32_768)` 2곳을 `Some(16_384)`로 갱신(주석도 "declared retry reserve tracks one full composer retry"로).

- [ ] **Step 4: 통과 확인** — `cargo test -p krw-agent-image` 전체 녹색. 활주로 효과 기록: 실효 리저브 32,768→16,384, 종료 임계 34,816→18,432(출력 예산 36,000에서 연구 활주로 1,184→17,568 토큰).
- [ ] **Step 5: 커밋** — `git add crates/agent-image/src/lib.rs agents/krw-ontology/agent.yaml && git commit -m "feat: explicit compose_retry_reserve decouples composer caps from the output reserve"`

### Task 2: report-sections/v1 섹션 배치 계약 + 핀 등록

**Files:**
- Create: `contracts/kernel/v1/schemas/report-sections-v1.json`
- Modify: `crates/krw-contracts/src/lib.rs:93-96` (BYTES), `:160-164` (id 상수), `:177-206` (핀), `:302-306` (`contract()` arm), `:348-369` (`descriptors()`)
- Test: `crates/krw-contracts/src/lib.rs` tests 모듈(기존 파일에 케이스 추가)

**Interfaces:**
- Consumes: Task 1 완료(독립 수행 가능하나 이미지 배선(Task 6)의 전제)
- Produces: `REPORT_SECTIONS_V1 = "report-sections/v1"` — Task 4(엔진 배치 검증)·Task 6(상태 output_contract)이 소비

- [ ] **Step 1: 실패 테스트 작성** — `crates/krw-contracts/src/lib.rs` tests 모듈에 추가:

```rust
    #[test]
    fn report_sections_v1_is_pinned_and_reachable() {
        let descriptor = contract(REPORT_SECTIONS_V1).expect("canonical descriptor");
        assert_eq!(
            descriptor.schema_sha256, REPORT_SECTIONS_V1_SCHEMA_SHA256,
            "pin must equal the canonical schema hash"
        );
        assert!(verify_pin(REPORT_SECTIONS_V1, &descriptor.schema_sha256).is_ok());
        assert!(descriptors()
            .iter()
            .any(|descriptor| descriptor.id == REPORT_SECTIONS_V1));
        let schema: serde_json::Value =
            serde_json::from_slice(descriptor.schema).expect("valid json schema");
        assert_eq!(schema["$id"], "krw-agent/kernel/report-sections/v1");
        assert_eq!(schema["properties"]["schema_version"]["const"], 1);
        assert_eq!(schema["properties"]["sections"]["maxItems"], 16);
        // v1과 동일 문형으로 배치 수준 claim/calculation을 운반한다.
        assert!(schema["$defs"]["claim"].is_object());
        assert!(schema["$defs"]["calculation"].is_object());
    }
```

- [ ] **Step 2: 실패 확인** — `cargo test -p krw-agent-contracts report_sections` → 컴파일 오류(상수 부재) 기록.
- [ ] **Step 3: 구현** — `contracts/kernel/v1/schemas/report-sections-v1.json` 생성. `sections`는 배치당 ≤16(엔진 루프 상한, Task 4), `claim`/`calculation` `$defs`는 `contracts/kernel/v1/schemas/answer-ir-v1.json`의 것을 그대로 복사한다(계약 재정의 금지, 스키마 단일 소스 유지):

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "krw-agent/kernel/report-sections/v1",
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "schema_version": {"const": 1},
    "batch_kind": {"enum": ["section_batch", "final_batch"]},
    "continuation": {"enum": ["more_sections", "report_done"]},
    "follow_up_questions": {
      "type": "array",
      "items": {"type": "string", "minLength": 1, "maxLength": 512},
      "maxItems": 3
    },
    "sections": {
      "type": "array",
      "minItems": 1,
      "maxItems": 16,
      "items": {"$ref": "#/$defs/section"}
    },
    "claims": {
      "type": "array",
      "maxItems": 256,
      "items": {"$ref": "#/$defs/claim"}
    },
    "calculations": {
      "type": "array",
      "maxItems": 256,
      "items": {"$ref": "#/$defs/calculation"}
    }
  },
  "required": ["schema_version", "batch_kind", "continuation", "sections", "claims", "calculations"],
  "$defs": {
    "section": {
      "type": "object",
      "additionalProperties": false,
      "properties": {
        "section_id": {"type": "string", "minLength": 1, "maxLength": 128},
        "order_hint": {"type": "integer", "minimum": 0, "maximum": 63},
        "heading": {"type": "string", "minLength": 1, "maxLength": 200},
        "body_markdown": {"type": "string", "minLength": 1, "maxLength": 20000},
        "claim_ids": {
          "type": "array",
          "items": {"type": "string", "minLength": 1, "maxLength": 128},
          "maxItems": 256
        }
      },
      "required": ["section_id", "order_hint", "heading", "body_markdown", "claim_ids"]
    },
    "claim": { },
    "calculation": { }
  }
}
```

(`claim`/`calculation`의 실제 본문은 answer-ir-v1.json `$defs`에서 복사 — `additionalProperties: false`와 `required` 목록 포함 전체.) 이어서 `crates/krw-contracts/src/lib.rs`에 ANSWER_IR(:93-96, :160, :179-180, :302-306, :363)와 동일한 5곳 배선:

```rust
const REPORT_SECTIONS_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/kernel/v1/schemas/report-sections-v1.json"
));
```

```rust
/// One model-authored section batch for the sectioned compose loop. The
/// engine accumulates validated batches into the assembled answer IR; this
/// contract is never the committed final output.
pub const REPORT_SECTIONS_V1: &str = "report-sections/v1";
pub const REPORT_SECTIONS_V1_SCHEMA_SHA256: &str = "sha256:<shasum -a 256 출력>";
```

`contract()`의 `ANSWER_IR_V1 => …` arm 뒤에 `REPORT_SECTIONS_V1 => Some(ContractDescriptor { id: REPORT_SECTIONS_V1, schema_sha256: REPORT_SECTIONS_V1_SCHEMA_SHA256, schema: REPORT_SECTIONS_BYTES }),` 추가, `descriptors()`(:363)에도 등록. 핀 값은 `shasum -a 256 contracts/kernel/v1/schemas/report-sections-v1.json` 실측치(기존 핀이 원시 파일 sha256임을 확인함 — answer-ir-v1.json = `618de032…` 일치).

- [ ] **Step 4: 통과 확인** — `cargo test -p krw-agent-contracts` 전체 녹색.
- [ ] **Step 5: 커밋** — `git add contracts/kernel/v1/schemas/report-sections-v1.json crates/krw-contracts/src/lib.rs && git commit -m "feat: report-sections/v1 canonical contract for sectioned compose"`

### Task 3: answer-ir/v2 + 타입 상한 64/256 (섹션 64·클레임 256·계산 256)

**Files:**
- Create: `contracts/kernel/v1/schemas/answer-ir-v2.json`
- Modify: `crates/evidence/src/lib.rs:18-20` (상수), `:512-520` (`AnswerIr` 문서화), `:540-590` (`validate_answer` 스키마 수락), `:940-960` (스키마 관련 이슈 코드)
- Modify: `crates/krw-contracts/src/lib.rs` (Task 2와 동일 5곳 패턴으로 ANSWER_IR_V2)
- Modify: `crates/run-engine/src/finalization.rs:1285-1300` (`ANSWER_IR_V1` 분기에 v2 병렬 처리)
- Test: `crates/evidence/src/lib.rs`, `crates/run-engine/src/lib.rs` tests (기존 파일에 케이스 추가)

**Interfaces:**
- Consumes: Task 2의 핀 패턴
- Produces: `MAX_ANSWER_SECTIONS = 64`, `MAX_ANSWER_CLAIMS = 256`, `MAX_ANSWER_CALCULATIONS = 256` — 새니타이저(finalization.rs가 import하는 pub 상수)가 자동으로 같은 상한으로 절단. Task 4 어셈블러·Task 8 평가가 소비

- [ ] **Step 1: 실패 테스트 작성** — `crates/evidence/src/lib.rs` tests에:

```rust
    #[test]
    fn v2_answer_admits_64_sections_and_256_claims() {
        let mut answer = base_answer_ir();
        answer.schema_version = 2;
        answer.sections = (0..64)
            .map(|index| AnswerSection {
                section_id: format!("s{index}"),
                heading: format!("섹션 {index}"),
                claim_ids: vec![format!("c{index}")],
                ..base_section()
            })
            .collect();
        answer.claims = (0..256).map(|index| base_claim(format!("c{index}"))).collect();
        let issues = validate_answer(&answer, &ledger_with_claims(&answer), &policy()).unwrap_err_or_ok();
        assert!(!issues.iter().any(|issue| issue.code == "too_many_sections"));
        assert!(!issues.iter().any(|issue| issue.code == "too_many_claims"));

        answer.sections.push(base_section_with_id("s64"));
        answer.claims.push(base_claim("c256".into()));
        let issues = validate_answer(&answer, &ledger_with_claims(&answer), &policy()).unwrap_err();
        assert!(issues.iter().any(|issue| issue.code == "too_many_sections"));
        assert!(issues.iter().any(|issue| issue.code == "too_many_claims"));
    }

    #[test]
    fn v1_answers_still_validate_after_the_v2_bump() {
        let mut answer = base_answer_ir();
        answer.schema_version = 1; // 과거 번들에서 읽은 v1 IR — 여전히 유효
        assert!(validate_answer(&answer, &ledger_with_claims(&answer), &policy()).is_ok());
    }
```

(헬퍼 `base_answer_ir`/`base_section`/`base_claim`/`ledger_with_claims`는 tests 모듈 기존 팩토리들 — lib.rs:1321 `claims: (0..=MAX_ANSWER_CLAIMS)` 패턴 — 에 맞춰 작성. `unwrap_err_or_ok` 대신 실제로는 `validate_answer` 반환값을 `match`로 분기.)

`crates/run-engine/src/lib.rs` tests에 새니타이저 정렬 회귀:

```rust
    #[test]
    fn sanitizer_truncates_claims_at_the_v2_cap_of_256() {
        let mut answer = fixture_answer_ir_v2();
        answer.claims = (0..300).map(|index| valid_claim(index)).collect();
        let (sanitized, completion) = sanitize_answer(&answer, &fixture_ledger(&answer), &policy())
            .expect("cardinality defects degrade, never fail");
        assert_eq!(sanitized.claims.len(), MAX_ANSWER_CLAIMS);
        assert_eq!(completion, ResearchCompletion::AcceptedWithWarnings);
    }
```

- [ ] **Step 2: 실패 확인** — `cargo test -p krw-agent-evidence v2_answer` / `cargo test -p krw-agent-run-engine sanitizer_truncates` → FAIL 기록.
- [ ] **Step 3: 구현** — `crates/evidence/src/lib.rs:18-20`:

```rust
pub const MAX_ANSWER_SECTIONS: usize = 64;
pub const MAX_ANSWER_CLAIMS: usize = 256;
pub const MAX_ANSWER_CALCULATIONS: usize = 256;
```

`validate_answer`(:546-555)의 스키마 검사를 v1/v2 이중 수락으로:

```rust
    if answer.schema_version != 1 && answer.schema_version != 2 {
        issues.push(ValidationIssue {
            code: "unsupported_answer_schema",
            claim_id: None,
            detail: format!(
                "expected AnswerIR schema 1 or 2, observed {}",
                answer.schema_version
            ),
        });
    }
```

`contracts/kernel/v1/schemas/answer-ir-v2.json`은 answer-ir-v1.json 전문에서 ① `"schema_version": {"const": 2}` ② 루트 `sections.maxItems: 64`·`claims.maxItems: 256`·`calculations.maxItems: 256` ③ `$id` → `krw-agent/kernel/answer-ir/v2`만 변경해 생성(문서화 코멘트로 "caps raised by E1; render contract unchanged" 명시). krw-contracts에 `ANSWER_IR_V2: &str = "answer-ir/v2"` + BYTES + SHA256 핀 + `contract()`/`descriptors()` 등록(Task 2 패턴, 핀은 `shasum -a 256` 실측). `finalization.rs:1285`의 `if contract.id == ANSWER_IR_V1`를 `if contract.id == ANSWER_IR_V1 || contract.id == ANSWER_IR_V2`로 확장(내부 AnswerIr 파싱·새니타이저 파이프라인 동일 — v2는 상한만 다르고 구조 동형). 렌더(`render_markdown`, evidence lib.rs:1045)·final-markdown 계약은 불변.

- [ ] **Step 4: 통과 확인** — `cargo test -p krw-agent-evidence && cargo test -p krw-agent-contracts && cargo test -p krw-agent-run-engine` 녹색.
- [ ] **Step 5: 커밋** — `git add contracts/kernel/v1/schemas/answer-ir-v2.json crates/evidence/src/lib.rs crates/krw-contracts/src/lib.rs crates/run-engine/src/finalization.rs && git commit -m "feat: answer-ir/v2 typed caps 64 sections / 256 claims with pinned schema"`

### Task 4: 엔진 섹션 누적 compose→verify 루프 (엔진 소유, 예산 종료)

**Files:**
- Modify: `crates/run-engine/src/active_run.rs` (`ActiveRun` 필드 + 보유 메서드; 구조체 초기화 :225-257, `retain_presentation_pack`(:2450-2487) 바로 뒤)
- Modify: `crates/run-engine/src/finalization.rs:1600-1700` (TypedJson 턴 처리 — REPORT_SECTIONS_V1 분기), `:1275-1300` (`validate_typed_output` 확장)
- Test: `crates/run-engine/src/lib.rs` tests (기존 파일에 케이스 추가)

**Interfaces:**
- Consumes: Task 2 `REPORT_SECTIONS_V1`, Task 3 `MAX_ANSWER_SECTIONS=64`
- Produces: `ActiveRun::composed_sections` + `retain_composed_section` + `assemble_answer_ir` — Task 5(번들/체크포인트)·Task 6(상태머신)이 소비. **루프 종료 조건은 (a) 남은 출력 예산이 `effective_final_output_reserve_tokens + minimum_research_turn_tokens` 이하로 내려가는 시점(엔진이 직접 점검)과 (b) 작성기 `continuation: "report_done"`의 2개뿐** — 4개 output-reserve 트리거(active_run.rs:620-674)가 섹션 루프 중간에 강제 종료하지 않는다: 트리거 2(:630-644)의 `finalize_for_output_reserve`는 현재 상태의 `output_budget_reserved` 에지(active_run.rs:1113)를 필요로 하는데 `compose_sections`는 그 에지를 갖지 않고(이미지 검증 agent-image lib.rs:3099-3121은 ingest/assess 상태에만 요구), 트리거 1(:624-628)·3(:656-658)은 `current_operation_emits_answer`가 true인 TypedJson 답변 턴을 제외한다.

- [ ] **Step 1: 실패 테스트 작성** — `crates/run-engine/src/lib.rs` tests에:

```rust
    #[test]
    fn composed_sections_accumulate_dedup_and_enforce_caps() {
        let mut state = fixture_active_run();
        let first = section_batch(vec![section("s0", 0), section("s1", 1)], "more_sections");
        let duplicate = section_batch(vec![section("s0", 0)], "more_sections");
        let second = section_batch(vec![section("s1", 1), section("s2", 2)], "report_done");
        assert!(state.retain_composed_section(&first).is_ok());
        // 같은 section_id 재발행은 거부 not 병합 — 작성기 반복을 루프 오류로 노출.
        assert!(state.retain_composed_section(&duplicate).is_err());
        assert!(state.retain_composed_section(&second).is_ok());
        assert_eq!(state.composed_sections_len(), 3);
    }

    #[test]
    fn section_loop_stops_when_budget_reaches_the_reserve_floor() {
        let mut state = fixture_active_run();
        state.limits.max_output_tokens = 36_000;
        state.usage.output_tokens = 36_000 - 16_384 - 2_048; // floor 도달
        assert!(!state.section_loop_may_continue(&fixture_image()).unwrap(),
            "engine ends the loop at the same floor the reserve protects, before dispatching another section turn");
        // 루프 종료는 answer-always: 이미 확보한 섹션으로 조립한다.
        state.retain_composed_section(&final_batch_with_follow_ups()).unwrap();
        let assembled = state.assemble_answer_ir(&policy()).unwrap();
        assert!(!assembled.sections.is_empty());
        assert_eq!(assembled.follow_up_questions.len(), 3);
    }

    #[test]
    fn oversized_batch_of_17_sections_is_rejected_and_repaired() {
        let mut state = fixture_active_run();
        let oversized = section_batch((0..17).map(|i| section(format!("s{i}"), i)).collect(), "more_sections");
        assert!(state.retain_composed_section(&oversized).is_err());
    }
```

(`fixture_active_run`/`section_batch`/`section` 헬퍼는 기존 테스트 픽스처 패턴(lib.rs:3996 예산 픽스처, evidence lib.rs:1321 claim 제너레이터)에 맞춰 작성.)

- [ ] **Step 2: 실키포인트 확인** — `cargo test -p krw-agent-run-engine composed_sections section_loop oversized_batch` → 컴파일 오류(메서드 부재) 기록.
- [ ] **Step 3: 구현** — `ActiveRun`(active_run.rs:42 `presentation_packs` 옆)에 `composed_sections: Vec<Value>` 추가, `new`(:225-257) 초기화. `retain_presentation_pack` 뒤에:

```rust
    /// Retain one validated report-sections/v1 batch. Bounded per batch by
    /// the contract (16 sections), per run by MAX_ANSWER_SECTIONS, and by
    /// bytes so a runaway composer cannot balloon the checkpoint. A repeated
    /// section_id is an error (repair lane), never a silent merge.
    pub(crate) fn retain_composed_section(&mut self, batch: &Value) -> Result<(), EngineError> {
        const MAX_BATCH_BYTES: usize = 256 * 1024;
        if batch.get("schema_version").and_then(Value::as_u64) != Some(1) {
            return Err(EngineError::AnswerValidation(vec!["unsupported_section_schema"
                .to_owned()]));
        }
        let sections = batch
            .get("sections")
            .and_then(Value::as_array)
            .ok_or_else(|| EngineError::AnswerValidation(vec!["missing_sections".into()]))?;
        if sections.len() > 16 {
            return Err(EngineError::AnswerValidation(vec!["too_many_sections_in_batch"
                .into()]));
        }
        let bytes = serde_jcs::to_vec(batch)?;
        if bytes.len() > MAX_BATCH_BYTES {
            return Err(EngineError::AnswerValidation(vec!["section_batch_too_large"
                .into()]));
        }
        for section in sections {
            let id = section.get("section_id").and_then(Value::as_str).unwrap_or_default();
            if id.trim().is_empty()
                || self
                    .composed_section_ids()
                    .iter()
                    .any(|existing| existing == id)
            {
                return Err(EngineError::AnswerValidation(vec!["duplicate_section_id".into()]));
            }
        }
        if self.composed_sections_len() + sections.len() > MAX_ANSWER_SECTIONS {
            return Err(EngineError::AnswerValidation(vec!["too_many_sections".into()]));
        }
        self.composed_sections.push(batch.clone());
        Ok(())
    }

    /// Engine-owned loop bound: one more section turn is dispatched only
    /// while the reserve floor (Task 1 effective reserve + minimum research
    /// turn) is still available. The four output-reserve triggers never fire
    /// mid-loop; this check IS the loop's budget termination.
    pub(crate) fn section_loop_may_continue(
        &self,
        image: &AgentImageManifest,
    ) -> Result<bool, EngineError> {
        let Some(reserve) =
            image.effective_final_output_reserve_tokens(&self.program.workflow.id)?
        else {
            return Ok(true);
        };
        let minimum = self
            .image_answer_policy_floor();
        Ok(self.remaining_output_tokens()? > reserve.saturating_add(minimum))
    }
```

`assemble_answer_ir`: 배치들을 `order_hint`·수신 순서로 병합 — 섹션(중복 id 제거된 상태), claims/calculation을 id 기준 dedupe 병합, `follow_up_questions`는 마지막 `batch_kind: "final_batch"`에서 취한다(정확히 3개 — policy 위반 시 새니타이저 기존 규칙(evidence lib.rs:955-965)이 절단/경고). 조립 결과는 `validate_answer`·`sanitize_answer`를 그대로 통과시킨다(근거 교리·claim↔ledger 바인딩 불변). `finalization.rs` TypedJson 분기(:1657 부근)에 `output_contract.id == REPORT_SECTIONS_V1` 이면: `validate_typed_output` 확장(계약 정합 + 배치 claim의 ledger 검증을 `sanitize_answer` 파츠 재사용) → `retain_composed_section` → 성공 시 `section_submitted` 전이, `continuation: "report_done"`이면 어셈블리 후 기존 verify→render 경로로. 배치 계약 위반은 기존 bounded repair 레인(:1633 `reserve_repair`, :1676 `apply_answer_repair`)을 탄다 — 신규 실패 경로 없음.

- [ ] **Step 4: 통과 확인** — `cargo test -p krw-agent-run-engine` 녹색. 회귀: `output_budget_reserved` 기존 테스트(active_run.rs:1015 경로) 전부 녹색인지 확인.
- [ ] **Step 5: 커밋** — `git add crates/run-engine/src/active_run.rs crates/run-engine/src/finalization.rs crates/run-engine/src/lib.rs && git commit -m "feat: engine-owned section accumulation compose-verify loop bounded by budget"`

### Task 5: AnswerBundle v5 + 체크포인트 v15 + 회복 동치 (v4 후방호환)

**Files:**
- Modify: `crates/run-engine/src/lib.rs:636-668` (`AnswerBundle`)
- Modify: `crates/run-engine/src/finalization.rs:1797-1821` (번들 조립 — schema_version 5 + sections)
- Modify: `crates/run-engine/src/active_run.rs:73` (체크포인트 상수 14→15), `:74-131` (스키마), `:139-162` (구조체), `:2517-2576` (`checkpoint_value`)
- Modify: `crates/run-engine/src/recovery.rs:262-307` (동치 검사)
- Test: `crates/run-engine/src/lib.rs`, `crates/run-engine/src/recovery.rs` tests (기존 파일에 케이스 추가)

**Interfaces:**
- Consumes: Task 4 `composed_sections`
- Produces: 번들 `schema_version: 5` + `sections: Vec<Value>`(`#[serde(default)]`), 체크포인트 `schema_version: 15` + `composed_sections_hash`

- [ ] **Step 1: 실패 테스트 작성**:

```rust
    #[test]
    fn answer_bundle_v4_still_parses_after_the_v5_bump() {
        // 81ea033 시점 실측 형상: schema_version 4, sections 필드 없음.
        let v4 = serde_json::json!({
            "schema_version": 4,
            "output_contract": fixture_contract_pin(),
            "output": {"answer": "결론 우선 답변"},
            "evidence_ledger_hash": fixture_content_hash(),
            "evidence_ids": ["ev1"],
            "answer_ir": null,
            "rendered_content": "결론 우선 답변",
            "rendered_markdown": "결론 우선 답변",
            "visualizations": [],
            "usage": fixture_budget_usage(),
            "agent_image_hash": fixture_content_hash()
        });
        let bundle: AnswerBundle = serde_json::from_value(v4).expect("v4 is backward compatible");
        assert_eq!(bundle.schema_version, 4);
        assert!(bundle.sections.is_empty(), "absent v4 sections default to empty");
    }

    #[test]
    fn v5_bundle_carries_accumulated_sections_and_round_trips() {
        let bundle = fixture_bundle_with_sections(vec![section("s0", 0), section("s1", 1)]);
        assert_eq!(bundle.schema_version, 5);
        assert_eq!(bundle.sections.len(), 2);
        let encoded = serde_json::to_value(&bundle).unwrap();
        assert_eq!(encoded["schema_version"], 5);
        let decoded: AnswerBundle = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, bundle);
    }

    #[test]
    fn checkpoint_declares_version_15_and_rebuilt_sections_match_on_recovery() {
        let mut state = fixture_active_run();
        state.retain_composed_section(&section_batch(vec![section("s0", 0)], "more_sections"))
            .unwrap();
        let checkpoint = state.checkpoint_value().unwrap();
        assert_eq!(checkpoint.schema_version, ACTIVE_RUN_CHECKPOINT_SCHEMA_VERSION);
        assert_eq!(checkpoint.schema_version, 15);
        // recovery.rs:289-307 동치 검사가 composed_sections_hash까지 비교한다.
        let rebuilt = state.checkpoint_value().unwrap();
        assert_eq!(checkpoint.composed_sections_hash, rebuilt.composed_sections_hash);
    }
```

- [ ] **Step 2: 실패 확인** — `cargo test -p krw-agent-run-engine answer_bundle_v4 v5_bundle checkpoint_declares` → FAIL 기록.
- [ ] **Step 3: 구현** — `AnswerBundle`(lib.rs:650 `answer_ir` 뒤)에:

```rust
    /// E1 sectioned-compose output: the validated report-sections/v1 batches
    /// accumulated across the compose→verify loop. `#[serde(default)]`
    /// keeps every v4 bundle (no sections field) parsing unchanged; v5 is
    /// written only by sectioned workflows.
    #[serde(default)]
    pub sections: Vec<Value>,
```

`finalization.rs:1804-1817` 번들 조립에 `schema_version: 5`와 `sections: state.composed_sections.clone()`. `active_run.rs:73` `ACTIVE_RUN_CHECKPOINT_SCHEMA_VERSION: u16 = 15`; 스키마 상수(:74-131)에 `"composed_sections_hash": {"type": "string"}` 속성·required 추가 및 `"schema_version": {"const": 15}` 갱신; `ActiveRunCheckpoint`(:139-162)에 `pub(crate) composed_sections_hash: ContentHash` 추가; `checkpoint_value()`(:2517)에서 `ContentHash::sha256(serde_jcs::to_vec(&self.composed_sections)?)`로 계산(누적 배치는 커밋된 섹션 아티팩트에서 재생 재구축 — `presentation_packs_hash`와 동일 문형, recovery.rs:296 참조). `recovery.rs:296` 비교 체인에 `|| declared.composed_sections_hash != rebuilt.composed_sections_hash` 추가.

- [ ] **Step 4: 통과 확인** — `cargo test -p krw-agent-run-engine` 녹색(기존 체크포인트 v14 픽스처가 있다면 v15로 갱신 — 회복 동치: 재생 경로가 섹션 아티팩트를 재구축해 해시 일치).
- [ ] **Step 5: 커밋** — `git add crates/run-engine/src/lib.rs crates/run-engine/src/finalization.rs crates/run-engine/src/active_run.rs crates/run-engine/src/recovery.rs && git commit -m "feat: AnswerBundle v5 + checkpoint v15 carry composed sections with v4 back-compat"`

### Task 6: 이미지 배선 — company_research_v2 섹션 상태머신 + 작성기 섹션 프로토콜 프롬프트

**Files:**
- Modify: `crates/agent-image/src/lib.rs:1030-1043` (`StateSpec.output_contract`), `:4146-4183` (`compile_model_output_mode`), `:4185-4265` (`state_output_contracts`), 검증(validate_spec 상태 검사 영역)
- Modify: `agents/krw-ontology/agent.yaml:705-707,744-762` (company_research_v2 상태·전이), `:180` (composer prompt_segments)
- Create: `agents/krw-ontology/references/composer-section-protocol.md`
- Test: `crates/agent-image/src/lib.rs` tests (기존 파일에 케이스 추가)

**Interfaces:**
- Consumes: Task 2 `REPORT_SECTIONS_V1`, Task 4 엔진 루프
- Produces: compose 상태 per-state `output_contract`(미선언 시 기존 `internal_format` 그대로 — **다른 4개 워크플로우는 일절 변경 없음**). 기존 `compose_ir`/`verify_ir`/`repair_ir`(max_visits 2/2/1)과 모든 `output_budget_reserved` 에지는 그대로 남는다(단발 폴백 경로 유지) — **기존 max_visits 변경 0개, 신규 상태만 추가(E4 불가침 준수)**

- [ ] **Step 1: 실패 테스트 작성** — `crates/agent-image/src/lib.rs` tests에:

```rust
    #[test]
    fn company_research_v2_compose_sections_is_a_typed_json_section_turn() {
        let image = compile_agent_dir(agent_root()).unwrap().manifest;
        let program = image.state_program("company_research_v2").unwrap();
        let section_compose = program
            .states
            .iter()
            .find(|state| state.id.ends_with("compose_sections"))
            .expect("sectioned compose state exists");
        match &section_compose.operation {
            StateOperation::ModelDecision { output_mode, output_contracts, .. } => {
                assert_eq!(*output_mode, ModelOutputMode::TypedJson);
                assert_eq!(output_contracts.as_slice(), [ContractPin::canonical(
                    REPORT_SECTIONS_V1_PLACEHOLDER,
                ).unwrap()]);
            }
            other => panic!("compose_sections must be a model decision, got {other:?}"),
        }
    }

    #[test]
    fn existing_compose_states_and_visit_limits_are_unchanged() {
        let image = compile_agent_dir(agent_root()).unwrap().manifest;
        let program = image.state_program("company_research_v2").unwrap();
        // 단발 폴백 경로는 그대로: compose_ir/verify_ir/repair_ir = 2/2/1.
        for (state_id, expected) in [("compose_ir", 2), ("verify_ir", 2), ("repair_ir", 1)] {
            let state = program.states.iter().find(|s| s.id.ends_with(state_id)).unwrap();
            assert_eq!(state.max_visits, expected, "{state_id} visit cap unchanged");
        }
        let ingest = program.states.iter().find(|s| s.id.ends_with("ingest_evidence")).unwrap();
        assert_eq!(ingest.max_visits, 14, "research visit caps are E4 territory");
    }
```

(`REPORT_SECTIONS_V1_PLACEHOLDER`는 krw-agent-contracts의 `REPORT_SECTIONS_V1` 상수 — agent-image가 이미 contracts를 의존하는지 확인 후, 없으면 문자열 `"report-sections/v1"` 리터럴과 핀 해시 비교로 대체.)

- [ ] **Step 2: 실패 확인** — `cargo test -p krw-agent-image compose_sections existing_compose_states` → FAIL 기록.
- [ ] **Step 3: 구현** — `StateSpec`(lib.rs:1030-1043)에:

```rust
    /// E1: explicit output contract for compose states. Absent = the image's
    /// `answer_policy.internal_format` (today's behavior, all other
    /// workflows unchanged). Present = that contract becomes the state's
    /// single model-output grammar (e.g. report-sections/v1 section turns).
    #[serde(default)]
    pub output_contract: Option<String>,
```

`state_output_contracts`(lib.rs:4263-4265)의 `StateKind::Compose` 분기를 `outputs.insert(state.output_contract.as_deref().map(|id| contract_by_id(contracts, id)).transpose()?.unwrap_or_else(|| internal()?)?);` 형태로 확장(Render/Commit은 기존대로 internal만). `compile_model_output_mode`(lib.rs:4171-4180)의 Compose 분기에서 명시 계약이 `FINAL_MARKDOWN_V1`이 아니면 `ModelOutputMode::TypedJson`(그 계약이 이미 유일 output contract로 흘러 :4172 검증 통과). `validate_spec`에: `output_contract`은 Compose 종류만 선언 가능, canonical 계약이어야 함, compose cap이 선언된 경우 Task 1 재시도 리저브 규칙이 그 상태의 role cap에도 동일 적용. `agents/krw-ontology/agent.yaml` company_research_v2에 상태·전이 추가(:705-707 뒤):

```yaml
      # E1 sectioned compose loop: the composer emits one report-sections/v1
      # batch per visit (<=16 sections); the engine accumulates and verifies.
      # New states only — no existing visit cap changes (E4 territory).
      - { id: compose_sections, kind: compose, role_id: composer, output_contract: report-sections/v1, max_visits: 5 }
      - { id: verify_sections, kind: verify, max_visits: 5 }
```

전이(:744-746의 compose_ir 진입 2개를 교체 — `evidence_sufficient`/`no_positive_value_action`는 섹션 경로로; `output_budget_reserved` 에지들은 기존대로 compose_ir 유지):

```yaml
      - { from: assess_obligations, on: evidence_sufficient, to: compose_sections }
      - { from: assess_obligations, on: no_positive_value_action, to: compose_sections }
      - { from: compose_sections, on: section_submitted, to: verify_sections }
      - { from: verify_sections, on: more_sections_required, to: compose_sections }
      - { from: verify_sections, on: sections_complete, to: render_ko }
```

composer 역할 prompt_segments(:180)에 `composer_section_protocol` 추가, 세그먼트 정의에 `- { id: composer_section_protocol, path: references/composer-section-protocol.md, stable_prefix: true, private: true }` 등록. 신규 프롬프트 파일 `agents/krw-ontology/references/composer-section-protocol.md`는 다음을 규정한다: 이 상태의 출력은 report-sections/v1 JSON 배치(섹션 ≤16, `order_hint` 오름차순, 첫 배치 첫 섹션은 결론), `continuation` 의미론(`more_sections` = 다음 배치 예약, `report_done` = 최종 배치이며 `follow_up_questions` 정확히 3개 포함), 배치별 claim는 이미 승인된 evidence만 참조(새 근거 생성 금지 — 근거 교리), 문체는 final_markdown_contract 상속(한국어 투자자 문체·가격 목표 금지), 같은 `section_id` 재발행 금지. 이미지 컴파일 후 `agents/krw-ontology` 해시 재계약 확인(`compile_agent_dir` 결정론 테스트 :4493-4498 자동 검증).

- [ ] **Step 4: 통과 확인** — `cargo test -p krw-agent-image` 녹색 + `cargo test -p krw-agent-run-engine` 녹색(상태머신 컴파일 결과로 엔진 통합 테스트 재확인).
- [ ] **Step 5: 커밋** — `git add crates/agent-image/src/lib.rs agents/krw-ontology/agent.yaml agents/krw-ontology/references/composer-section-protocol.md && git commit -m "feat: company_research_v2 sectioned compose state machine with section-protocol prompt"`

### Task 7: 차트 3→12 · 팩 16→32 · 차트 종류 확장 (그라운딩 필터 불변)

**Files:**
- Modify: `crates/run-engine/src/finalization.rs:2059` (`MAX_TOTAL_ARTIFACTS` 3→12)
- Modify: `crates/run-engine/src/active_run.rs:2451` (`MAX_PACKS_PER_RUN` 16→32)
- Modify: `crates/krw-presentation/src/lib.rs:179-202` (chart_kind 파생 확장), `:666-745` (`compile_group` match 확장)
- Test: `crates/run-engine/src/finalization.rs` tests, `crates/krw-presentation/src/lib.rs` tests (기존 파일에 케이스 추가)

**Interfaces:**
- Consumes: 없음(독립 수행 가능)
- Produces: 런당 최대 12개 그라운디드 아티팩트(팩당 상한 `MAX_ARTIFACTS_PER_PACK=3` 유지, lib.rs:27), 최대 32개 팩 보유. **`filter_grounded_visualizations`(finalization.rs:58-75)는 미변경** — 상한 인자만 12로

- [ ] **Step 1: 실패 테스트 작성** — `crates/run-engine/src/finalization.rs` tests에:

```rust
    #[test]
    fn compile_visualizations_admits_up_to_12_grounded_artifacts() {
        let packs = grounded_packs(8); // 팩당 2개 그라운디드 아티팩트 = 후보 16개
        let ledger = ledger_admitting_all(&packs);
        let artifacts = fixture_engine().compile_visualizations(&packs, &ledger);
        assert_eq!(artifacts.len(), 12, "E1 raises the run-level cap from 3 to 12");
    }

    #[test]
    fn ungrounded_artifacts_are_still_dropped_at_the_raised_cap() {
        let packs = mixed_grounded_and_foreign_packs(8);
        let ledger = ledger_admitting_grounded_only(&packs);
        let artifacts = fixture_engine().compile_visualizations(&packs, &ledger);
        assert!(artifacts.iter().all(|artifact| {
            let mut refs = BTreeSet::new();
            collect_visualization_evidence_refs(artifact, &mut refs);
            !refs.is_empty()
        }), "grounding filter unchanged: no run-external evidence survives");
    }

    #[test]
    fn thirty_two_packs_are_retained() {
        let mut state = fixture_active_run();
        for index in 0..32 {
            let pack = distinct_pack_v2(index);
            state.retain_presentation_pack(&pack);
        }
        assert_eq!(state.presentation_packs_len(), 32);
        state.retain_presentation_pack(&distinct_pack_v2(32));
        assert_eq!(state.presentation_packs_len(), 32, "cap raised 16 -> 32, still bounded");
    }
```

`crates/krw-presentation/src/lib.rs` tests에 신규 차트 종류(프론트 카드가 이미 지원하는 area/scatter/waterfall) 회귀:

```rust
    #[test]
    fn single_series_window_pack_compiles_an_area_artifact() {
        let pack = window_pack_single_series(6); // 계산창 있는 단일 시리즈, 추이 포인트 충분
        let artifacts = compile(&pack).expect("pack compiles");
        assert!(artifacts.iter().any(|a| a["views"][0]["chart_type"] == "area"));
    }

    #[test]
    fn two_aligned_series_pack_compiles_a_scatter_artifact() {
        let pack = comparison_pack_two_aligned_series();
        let artifacts = compile(&pack).expect("pack compiles");
        assert!(artifacts.iter().any(|a| a["views"][0]["chart_type"] == "scatter"));
    }

    #[test]
    fn period_deltas_pack_compiles_a_waterfall_growth_view() {
        let pack = trend_pack_with_period_deltas();
        let artifacts = compile(&pack).expect("pack compiles");
        assert!(artifacts.iter().any(|a| a["views"].as_array().unwrap().iter()
            .any(|v| v["chart_type"] == "waterfall")));
    }
```

- [ ] **Step 2: 실패 확인** — `cargo test -p krw-agent-run-engine compile_visualizations thirty_two_packs` / `cargo test -p krw-presentation area_artifact scatter_artifact waterfall` → FAIL 기록.
- [ ] **Step 3: 구현** — 상수 2곳: `finalization.rs:2059` `const MAX_TOTAL_ARTIFACTS: usize = 12;`, `active_run.rs:2451` `const MAX_PACKS_PER_RUN: usize = 32;` (팩 구조 검증 `schema_version==2`·`MAX_PACK_BYTES 64KiB`·`MAX_SERIES 8`·`MAX_POINTS 12`는 불변). krw-presentation: chart_kind 파생(:179-202)에 계산창+단일 시리즈 → `area`(기존 `trend` 조건과 구분: 시리즈 1개·포인트 ≥ `MIN_TREND_POINTS`), 비교 그룹에서 정렬된 2시리즈 → `scatter`, `period_over_period` 델타가 결정론 계산 가능한 그룹 → `waterfall` 파생 뷰. `compile_group`(:668-745)의 match에 세 종류 추가 — 각각 기존 데이터 충분성 검사(음수/불일치 시 `None` 반환, composition의 :704-745 문형 준수)를 그대로 두고, 그라운딩 참조(`evidence_ref`/`derived_from_evidence_refs`)는 기존 `view_json` 경유로 자동 부착. `filter_grounded_visualizations` 및 `semantic_fingerprint`(:1080) 로직은 미변경.
- [ ] **Step 4: 통과 확인** — `cargo test -p krw-presentation && cargo test -p krw-agent-run-engine` 녹색.
- [ ] **Step 5: 커밋** — `git add crates/run-engine/src/finalization.rs crates/run-engine/src/active_run.rs crates/krw-presentation/src/lib.rs && git commit -m "feat: raise chart cap to 12 and pack cap to 32 with area/scatter/waterfall kinds"`

### Task 8: 시맨틱 평가 v3 — 리포트 가변 길이 계약 게이트

**Files:**
- Create: `evals/krw-semantic/v3/cases.json`, `evals/krw-semantic/v3/manifest.json`, `evals/krw-semantic/v3/policy.json` (v2 사본 기준 갱신)
- Modify: `scripts/validate_semantic_evals.py` (v3 스위트 규칙 — 기본 경로/버전)
- Test: `scripts/validate_semantic_evals.py` 자체가 검증 대상(`python3 scripts/validate_semantic_evals.py --suite evals/krw-semantic/v3` 형태로 실행; 스크립트 인터페이스는 Step 3에서 실측 후 그대로 사용)

**Interfaces:**
- Consumes: Task 3(상한 64/256)·Task 5(번들 v5)·Task 7(차트 12)의 산출 규칙
- Produces: `suite_version: 3.0.0` 계약 게이트 — E1 산출물(섹션 리포트·v5 번들·12 차트)의 결정론 검증. v2는 디렉터리로 보존(히스토리)

- [ ] **Step 1: 실패 케이스 정의** — v2(`evals/krw-semantic/v2/`: manifest `suite_version: 2.0.0`, case_count 56, policy gates에 `answer.followups`/`answer.internal_terms`/`answer.decision_headings` 포함 — 실측)를 v3로 복사한 뒤: ① `suite_version`/`suite_id` 버전 갱신, ② 정책 gates에 신규 규칙 family 추가 — `report.sections.bounded`(배치 ≤16, 런 ≤64 — 음성 케이스: 17개 섹션 배치 거부, 65번째 섹션 거부), `report.followups_exact3`(유지 — 가변 길이 허용은 섹션 수이지 후속질문 수가 아님), `bundle.v4_backcompat`(v4 번들 파싱 픽스처), `viz.grounded_cap12`(12개 초과 아티팩트 절단 + 런 외부 근거 아티팩트 탈락), ③ 기존 `answer.*` 규칙의 긍정/부정 케이스 전부 유지(결론 우선·한국어·정확히 3개·가격 목표 금지는 v3에서도 동일). 각 신규 규칙에 긍정 1+음성 1 케이스(policy `require_positive_and_negative_per_rule` 충족).
- [ ] **Step 2: 실패 확인** — v3 케이스로 검증기 실행 → 신규 규칙 미등록/케이스 불충족으로 FAIL 기록(정확한 CLI 인자는 `python3 scripts/validate_semantic_evals.py --help` 실측 후 사용).
- [ ] **Step 3: 구현** — `scripts/validate_semantic_evals.py`의 검증 규칙 테이블에 v3 규칙 family 추가: `report.sections.bounded`(cases.json 음성 픽스처의 expected_codes 검증), `bundle.v4_backcompat`(픽스처 v4 JSON이 신규 필드 없이도 통과 — 검증기는 픽스처 계약만 검사하므로 "v4 필드 집합이 sections 없이 유효" 규칙), `viz.grounded_cap12`(아티팩트 배열 길이 ≤12 + 모든 evidence_ref가 허용 집합 내). 매니페스트 해시 재계산(스크립트가 요구하는 canonical-json sha256 방식 — `HARD_FILE_LIMIT`/`SHA256_RE` 규칙은 스크립트 헤더에서 이미 확인). 기본 스위트 경로를 v3로 지정(v2는 `--suite`로 여전히 검증 가능).
- [ ] **Step 4: 통과 확인** — `python3 scripts/validate_semantic_evals.py`(v3 기본) 녹색 + v2 회귀 `--suite evals/krw-semantic/v2` 녹색.
- [ ] **Step 5: 커밋** — `git add evals/krw-semantic/v3 scripts/validate_semantic_evals.py && git commit -m "test: semantic eval suite v3 for sectioned reports with retained v2 gates"`

### Task 9: GLM-5.3 최종 게이트 — 매트릭스 러너 서브셋 + 기록된 수동 콘텐츠 리뷰

**Files:**
- Modify: 없음(코드 변경 없음 — 실행·기록 태스크)
- Create: `docs/superpowers/plans/2026-08-31-engine-v2-e1-output-surface-gate-record.md` (게이트 기록 — 승인 문서, plan 아님)

**Interfaces:**
- Consumes: Task 1~8 전부(스택에 반영된 이미지·엔진·계약·평가)
- Produces: E1 완료 판정 근거 — 설계 §4 E1 완료 기준("장기 리서치가 16k를 넘는 리포트를 무손실 산출; 새니타이저 다운그레이드 0")에 대한 기록물. **정직 기록: E6의 자동 채점 3축(근거 무결성/근거 다양성/완결성)은 아직 구축되지 않았다(설계 §5, E6 범위). 본 게이트는 기존 매트릭스 러너 + 기록된 수동 콘텐츠 리뷰이며, "전 케이스 3축 녹색 2회 연속" 같은 정지 조건은 E6 이전에 성립하지 않는다.**

- [ ] **Step 1: 스택 준비** — `scripts/start_local_agent_gateway_stack.sh`로 v2-dev 스택 기동(러너 docstring이 요구하는 상시 스택 — 러너는 스택을 직접 올리지 않음). 이미지가 Task 1~7 변경을 포함해 컴파일되는지 `cargo build --workspace`로 선행 확인.
- [ ] **Step 2: 매트릭스 서브셋 실행** — `KRW_AGENT_GATEWAY_TOKEN=... ./scripts/run_live_quality_matrix.py --case <complex_*> ... --parallelism 1`로 심층(complex_) 버킷 위주 서브셋 실행(버킷 구조 short_/normal_/complex_, `--case` 선택·`manual_review_required` 보고는 러너 docstring 실측). 최소 구성: complex 3 + normal 2 + short 1. 산출 리포트(원문 질문·최종 Markdown·터미널 영수증·경과시간)를 게이트 기록에 첨부 경로로 남긴다.
- [ ] **Step 3: 수동 콘텐츠 리뷰 기록** — 게이트 기록 문서에 케이스별 체크리스트를 실측 결과로 작성: ① 결론 우선 ② 한국어 투자자 문체 ③ 후속질문 정확히 3개 ④ 가격 목표·확정 등급 부재 ⑤ 섹션 리포트가 16k 캐릭터를 넘어 무손실(잘림·`AcceptedWithWarnings` 다운그레이드 0 — `completion` 필드와 새니타이저 로그로 확인) ⑥ 차트 ≤12·그라운딩 통과 ⑦ 회복: 섹션 루프 진행 중 재시작 시나리오 1회(데몬 재시작 후 체크포인트 v15 재개·번들 커밋 완결 확인). 각 항목 PASS/FAIL + 증거(영수증 해시·리포트 발췌).
- [ ] **Step 4: 게이트 판정** — 전 항목 PASS 시 E1 완료로 기록하고 설계 §4의 다음 단계(E2 회복 재유도) 착수 조건 성립을 명시. FAIL 항목이 있으면 해당 Task로 환원하는 수정 루프를 기록에 명시하고 E1 미완료 상태로 둔다.
- [ ] **Step 5: 커밋** — `git add docs/superpowers/plans/2026-08-31-engine-v2-e1-output-surface-gate-record.md && git commit -m "docs: E1 GLM-5.3 gate record (matrix subset + manual content review)"`

---

## 리스크와 롤백

- Task 1 리저브 축소(32,768→16,384)는 연구 턴이 더 길어질 수 있음 — 종료 임계는 `reserve + 2,048`로 동일 문형 유지되므로 답변 보호는 약해지지 않음(재시도 1회 전체 보장). 이상 시 `compose_retry_reserve_tokens` 제거만으로 레거시 2×cap 유도로 즉시 복귀(serde optional).
- Task 6 상태 추가는 이미지 해시 재계약 — v2-dev 스택만 영향, prod `current` 심링크는 미변경(P1 운영 규칙 준수).
- Task 5 체크포인트 v15는 구버전(v14) 데몬이 새 체크포인트를 읽지 못하는 단방향 마이그레이션 — 배포 순서(엔진 먼저)를 게이트 기록에 명시.
- 워크플로우 방문 상한·reserve 트리거 전환·컨텍스트 봉투는 각각 E4·E3 범위로 본 plan에서 일절 미변경(설계 §4 단계 게이트 준수).
