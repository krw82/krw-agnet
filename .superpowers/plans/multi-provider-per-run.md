# Multi-Provider (Per-Run Selection) — GLM-5.2 추가

## Goal
z.ai GLM-5.2를 두 번째 프로바이더로 추가하여 run마다 DeepSeek 또는 GLM을 선택할 수 있게 한다. 동시성 부하 시 DeepSeek 단일 프로바이더 병목을 해소한다.

## Context
- 현재 DeepSeek-only 하드코드가 11곳에 걸쳐 있음 (protocol, runtime-config, deepseek-wire, run-engine, executor)
- Wire(네트워크) 레벨은 OpenAI 호환이므로 DeepSeek 클라이언트를 재사용 가능
- run마다 snapshot에 model이 pinned되는 보안 모델 유지
- 결정론적 재현(replay_hash) 보존
- **제약: krw-agnet만 수정. krw-ontology, krw-ontology-data 건드리지 않음**

## Global Constraints
1. 기존 DeepSeek 동작 완전 보존 — 기존 테스트 전부 통과
2. `DEEPSEEK_MODEL_ID` 하드코드를 허용 모델 집합으로 일반화
3. 새로운 실패 지점(검증)을 만들지 않음 — 기존 검증을 일반화만 함
4. KRW_ONTOLOGY_ROOT=$HOME/krw-ontology 환경에서 빌드/테스트
5. clippy `-D warnings`, fmt clean 필수
6. Cargo.lock 업데이트 금지 (z.ai SDK 추가하지 않음, HTTP 클라이언트만)

## Tasks

### Task 1: 허용 모델 집합 일반화 (protocol crate)

**파일:** `crates/protocol/src/lib.rs`

DeepSeek-only 상수/검증을 복수 모델 허용 구조로 일반화:

1. `DEEPSEEK_MODEL_ID` 상수는 유지하되, 새 상수 `GLM_MODEL_ID = "glm-5.2"` 추가
2. `ALLOWED_MODEL_IDS: &[&str] = &["deepseek-v4-flash", "glm-5.2"]` 상수 추가
3. `ProviderWireCapabilities::deepseek_v4_flash()` 유지 + `ProviderWireCapabilities::glm_5_2()` 추가
   - GLM capability matrix: thinking/non_thinking 모두 tools 지원, tool_choice 지원
4. `ModelRegistry::resolve_profile_exact`의 `requested != DEEPSEEK_MODEL_ID` 거부를 `!ALLOWED_MODEL_IDS.contains(&requested)`로 변경
5. `ModelDescriptor`에 provider 구분을 위한 필드는 추가하지 않음 (model_id 자체가 식별자)
6. 기존 DeepSeek 관련 상수/함수 전부 유지 (backwards compatible)

**테스트:** 기존 테스트 전부 통과 + GLM model_id로 resolve_profile 호출 테스트 추가

### Task 2: 모델 검증 일반화 (runtime-config crate)

**파일:** `crates/runtime-config/src/lib.rs`

`validate_model` 함수(~line 1074)의 DeepSeek-only 하드코드를 provider별 분기로 변경:

1. 현재: `api_base == "https://api.deepseek.com"` AND `model_id == DEEPSEEK_MODEL_ID` AND ...
2. 변경: model_id 또는 api_base 기반으로 provider 식별 후 해당 provider 검증 적용
3. GLM 검증: `api_base == "https://open.bigmodel.cn/api/paas/v4"`, `model_id == "glm-5.2"`, capability matrix 검증
4. `validate_model_profile`에 GLM profile 추가 (예: `glm_high`, `glm_direct`)
5. DeepSeek 검증 로직은 기존과 동일하게 유지

**테스트:** 기존 DeepSeek 검증 테스트 통과 + GLM 검증 성공/실패 테스트 추가

### Task 3: DeepSeek wire 클라이언트 일반화

**파일:** `crates/deepseek-wire/src/lib.rs`

`DeepSeekClient`를 복수 모델을 허용하도록 일반화 (crate 이름은 유지):

1. `DeepSeekClient::new`(~line 1080)의 `allowed_models.len() == 1 && contains(DEEPSEEK_MODEL_ID)` 제거
   - 허용 모델 집합(`ALLOWED_MODEL_IDS`) 중 하나면 통과
2. `complete_stream`(~line 1117)의 `request.model != DEEPSEEK_MODEL_ID` 거부를 `!allowed_models.contains(&request.model)`로 변경
3. `verify_model_identity`(~line 634)의 단일 모델 강제를 허용 집합 검사로 변경
4. `classify_deepseek_failure`와 `deepseek_failure_code`는 DeepSeek 응답 전용이므로 유지 (GLM은 별도 분류기 사용 - Task 5)
5. crate 이름(`deepseek-wire`)은 유지 — 바꾸면 전체 Cargo.toml 수정 필요

**테스트:** 기존 테스트 통과 + GLM model_id로 complete_stream 호출 시 거부되지 않는 테스트

### Task 4: Run-engine 모델 검증 일반화

**파일:** `crates/run-engine/src/lib.rs`

`validate_input`(~line 8336)과 `validate_episode`(~line 8500)의 `== DEEPSEEK_MODEL_ID` 검증 일반화:

1. `validate_input`: `request/snapshot의 requested/resolved_model == DEEPSEEK_MODEL_ID` → `ALLOWED_MODEL_IDS.contains(&model)`
2. `validate_episode`: 동일하게 일반화
3. `ProviderEpisodeV1::verify_model_identity` 호출은 그대로 (Task 3에서 내부 일반화)
4. `classify_deepseek_failure`는 DeepSeek 전용이므로 GLM용 분류 필요 (Task 5에서 처리)
5. 기존 DeepSeek recovery/gap 수정(2fc706b 커밋)은 전부 유지

**테스트:** 기존 68 테스트 전부 통과

### Task 5: GLM failure 분류기 추가

**파일:** `crates/run-engine/src/lib.rs`

GLM 응답 실패를 위한 분류 함수 추가:

1. `classify_glm_failure(error: &WireError) -> (bool, DeliveryCertainty)` 추가
   - DeepSeek과 동일한 WireError를 쓰므로(OpenAI 호환), 분류 로직도 유사
   - 차이점: GLM 고유 에러 코드 매핑 (`glm_http_*`, `glm_response_json_invalid` 등)
2. `glm_failure_code(error: &WireError) -> String` 추가
3. Provider trait 구현체에서 model_id 기반으로 DeepSeek/GLM 분류기 선택하는 로직 추가
   - 또는 `DeepSeekClient::complete`에서 api_base/model 기반 분기

**주의:** 새 Provider trait 구현체를 만들지 않음. `DeepSeekClient`가 복수 모델을 처리하되, failure 분류만 model_id 기반으로 분기.

**테스트:** classify_glm_failure 단위 테스트 추가

### Task 6: Provider catalog 복수 모델 허용

**파일:** `crates/runtime-persistence/src/executor.rs`

`DeepSeekProviderCatalog`이 복수 모델을 허용하도록 변경:

1. `compile_from`(~line 141)의 `seen != {DEEPSEEK_MODEL_ID}` 거부 제거
   - 허용 모델 집합의 부분집합이면 통과
2. `by_model` 맵이 여러 모델을 담을 수 있도록 보장 (이미 BTreeMap<String, Arc<DeepSeekClient>>이므로 구조 변경 불필요)
3. `compile_release_set_with_idle`가 복수 모델을 처리하도록 보장
4. 구조체 이름은 `DeepSeekProviderCatalog` 유지 (이름 변경은 ripple effect 큼)

**테스트:** 복수 모델 catalog 컴파일 테스트 추가

### Task 7: model-registry.yaml에 GLM 추가

**파일:** `deployments/local/model-registry.yaml`

GLM 모델 entry 추가:

```yaml
models:
  - model_id: glm-5.2
    api_base: https://open.bigmodel.cn/api/paas/v4
    api_version: chat-completions-v1
    max_context_tokens: 128000
    max_output_tokens: 16384
    max_in_flight: 32
    provider_wire_capabilities:
      # GLM-5.2 capability matrix
      ...
    profiles:
      - id: glm_high
        model_id: glm-5.2
        thinking: { type: enabled }
        reasoning_effort: high
      - id: glm_direct
        model_id: glm-5.2
        thinking: { type: disabled }
        reasoning_effort: null
```

**주의:** 실제 GLM API key는 `.env.local`의 별도 변수(`GLM_API_KEY`)로 관리. DeepSeek key와 별도.

### Task 8: agent.yaml GLM profile 옵션 추가

**파일:** `agents/krw-ontology/agent.yaml`

기본은 DeepSeek 유지, GLM profile 옵션 추가:
1. `required_model_profile`에 GLM profile 선택 가능하도록 주석/문서 추가
2. 실제 기본값은 DeepSeek(`flash_high`) 유지 — 런타임에 교체 가능
3. deployment-binding.yaml에서 per-run model 선택 지원 확인

## Build Sequence
1. Task 1 (protocol) → 기반, 다른 task가 의존
2. Task 2 (runtime-config) → Task 1 의존
3. Task 3 (deepseek-wire) → Task 1 의존
4. Task 4 (run-engine) → Task 1, 3 의존
5. Task 5 (GLM failure) → Task 3, 4 의존
6. Task 6 (executor) → Task 1, 2 의존
7. Task 7 (model-registry) → Task 1, 2 의존
8. Task 8 (agent.yaml) → Task 7 의존
