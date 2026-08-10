# KRW Canonical Runtime Convergence Plan

상태: canonical 설계 고정 · standalone 핵심 구현 및 로컬 결정론 gate 완료 · 실제 provider/MCP
환경의 production admission 증거 대기

목표: `krw-agnet`가 DeepSeek Flash, agent kernel, ontology/Guru online capability
runtime, 계약, 품질 검증을 함께 소유하는 독립 실행 시스템이 된다. 웹앱에는 아직 연결하지 않는다.

이 계획은 기존 입력 외피, Claude 계열 런타임, 외부 `krw-ontology` 소스 경로, legacy
session 및 fallback을 지원하지 않는다. 문제가 생겼을 때 이전 구조로 우회하는 대신 동일한
canonical 계약과 릴리스 artifact 안에서 수정하거나 직전 새 런타임 릴리스로 되돌린다.

> **출력 계약 정정 (2026-08-04):** `company_research` final path는 Flash가 research와 투자 해석을
> 끝낸 뒤 쓰는 direct Korean Markdown이다. kernel은 그 Markdown을 AnswerIR로 역추출하지 않고,
> 실제 MCP에서 ingest한 immutable EvidenceLedger hash/ID receipt와 함께 atomic commit한다. 아래
> AnswerIR 언급은 typed product contract 또는 이 정정 이전의 설계 기록에만 적용된다.

## 현재 기준선

- 원본의 tracked commit만 provenance로 가져온 27개 read capability runtime을
  `services/krw-ontology-runtime/`에 고정한다. 원본 checkout은 온라인 실행 의존성이 아니다.
- `ontology.query_context`의 외부 MCP 입력은 이미 `SearchPlan` 루트 객체다. 기존
  `{ "search_plan": ... }` 형태는 명시적 structured correction으로 거부한다.
- DeepSeek request/message/tool envelope은 Rust typed wire로 전환한다. JSON 값은 근거와
  tool payload 안에서만 쓰고, provider protocol 외피에는 쓰지 않는다.
- 다음 구현의 우선순위는 “더 많은 도구”가 아니라 schema/hash admission, 선언형 capability
  ABI, 최소 충분 조사 planner, 실제 리서치 품질 증명이다. 웹앱 연결은 그 뒤다.

## 1. 고정 결정

1. 온라인 실행의 단일 source of truth는 `~/krw-agnet`다.
2. `~/krw-ontology`는 최종적으로 offline ontology build와 immutable data release 생성만
   담당한다.
3. ontology MCP 13개와 Guru MCP 14개, 총 27개 도구의 online runtime 동작을
   `krw-agnet`로 가져온다.
4. 가져온 runtime은 하나의 공유 Python sidecar `krw-capabilityd`로 실행한다. 세션마다 Python
   또는 MCP 프로세스를 만들지 않는다.
5. production MCP 등록은 `FastMCP`를 사용하지 않는다. Python MCP SDK의 low-level server와
   명시적 `ToolDescriptor` registry를 사용한다.
6. `krw_ontology_query_context`의 MCP `arguments`는 `SearchPlan` 루트 객체다.
   `{ "search_plan": SearchPlan }` 외피는 지원하지 않는다.
7. provider는 좁은 `ResearchIntent v1`만 제안한다. trusted planner가 이를 검증·최소화해
   canonical `SearchPlan v2` action으로 컴파일하고, MCP는 그 root `SearchPlan`만 받는다.
   provider proposal과 physical action을 같은 JSON으로 취급하지 않는다.
8. DeepSeek 메시지와 도구 정의는 Rust 타입으로 표현한다. provider request transcript에
   `Vec<serde_json::Value>`를 사용하지 않는다.
9. 초기 리서치 계획과 후속 재탐색은 하나의 `EvidenceGoalGraph`와 동일한 가치/비용 정책을
   사용한다.
10. 사례별 `정확히 한 clause` 규칙은 제거한다. 최소 충분 계획은 일반적인 최적화 결과로
    결정한다.
11. DeepSeek 모델은 `deepseek-v4-flash` 하나만 허용한다. alias, silent fallback, 다른 모델은
    시작 단계에서 거부한다.
12. 웹앱 연결, 다중 세션 대량 soak, 과금/UI migration은 리서치 품질과 standalone 원자성
    검증 뒤에 진행한다.

## 2. 근본 원인과 설계 원칙

현재 반복 오류의 원인은 세 계약을 `serde_json::Value` 하나로 연결한 것이다.

```text
Provider wire schema
        ↓
Canonical agent schema
        ↓
Physical MCP schema
```

FastMCP는 Python 함수의 매개변수를 JSON 객체의 필드로 바꾼다. 따라서
`query_context(search_plan: SearchPlan)`은 자동으로 `{search_plan: ...}` schema가 된다.
반면 agent kernel은 내부 SearchPlan 객체를 canonical action으로 사용했다. DeepSeek 도구 결과도
내부 JSON 객체를 그대로 전달했지만 provider wire는 문자열을 요구했다.

최종 구조는 다음 원칙을 지킨다.

- 의미 차이가 없으면 세 경계가 정확히 같은 canonical schema를 사용한다.
- provider 고유 문법은 하나의 provider codec에서만 처리한다.
- MCP transport 고유 문법은 하나의 MCP codec에서만 처리한다.
- domain별 입력 생성은 transport 변환과 분리한다.
- capability ID를 비교하는 `if`/`match`로 wire 예외를 추가하지 않는다.
- 모든 변환은 typed identity 또는 제한된 선언형 derivation program이다.
- schema/hash가 맞지 않으면 요청을 보내기 전에 실패한다.

## 3. 최종 실행 구조

```text
RunRequest + pinned AgentImage + DeploymentSnapshot
                         │
                         ▼
               Rust Agent Kernel
                         │
         ┌───────────────┴────────────────┐
         │                                │
         ▼                                ▼
Typed DeepSeek Codec             Canonical Capability ABI
deepseek-v4-flash                 typed input/output/hash
         │                                │
         ▼                                ▼
DeepSeek native API          pooled MCP client in Rust
                                          │
                                          ▼
                            shared Python krw-capabilityd
                            low-level MCP, 27 tool registry
                                          │
                      ┌───────────────────┴──────────────────┐
                      ▼                                      ▼
              ontology read runtime                   Guru read runtime
                      │                                      │
                      └──────── immutable release ────────────┘
                                          │
                                          ▼
                    EvidenceLedger → AnswerIR → verifier
                                          │
                                          ▼
                              PostgreSQL atomic final
```

한 머신에서 세션이 늘어나도 증가하는 것은 Rust의 bounded run state와 queue receipt뿐이다.
DeepSeek HTTP pool, MCP pool, AgentImage, schema registry, ontology indexes, SQLite page cache는
공유한다.

## 4. 저장소 소유권과 MCP 코드 이전

### 4.1 가져올 범위

현재 `~/krw-ontology`의 commit과 파일 hash를 기록한 뒤 다음 online runtime closure를 가져온다.

- `src/krw_ontology/mcp_server/`
- `src/krw_ontology/agent_index/` 중 read runtime 전체
- `src/krw_ontology/guru/` 중 MCP read runtime과 직접 의존 모듈
- runtime에 필요한 `config`, `release`, `schema`, registry, taxonomy, sector pack, resources
- ontology/Guru MCP 계약 및 모든 도구 unit/integration fixture
- stdio와 streamable HTTP transport가 공통으로 사용하는 readiness/fingerprint 의미

현재 기준 도구 inventory는 ontology 13개와 Guru 14개다. import gate는 정확히 27개의 도구가
명시적 disposition을 갖도록 강제한다. 도구를 누락하거나 이름만 남긴 stub은 허용하지 않는다.

### 4.2 가져오지 않을 범위

- Claude Agent SDK와 `agent_index/claude_sdk.py`
- plugin, SKILL.md, Claude/Codex compatibility loader
- ingestion/download/extraction pipeline
- CLI authoring workflow
- 웹앱, queue, billing, SSE 코드
- source repository의 `.env`, workspace data, SQLite data, cache, log
- build-time에만 필요한 BeautifulSoup, PDF, browser, LLM dependencies

offline build 결과는 versioned release bundle로만 새 runtime에 들어온다. Python import path,
사용자 홈 절대경로, source checkout은 production dependency가 아니다.

### 4.3 target layout

```text
services/krw-ontology-runtime/
  pyproject.toml
  uv.lock
  src/krw_capability_runtime/
    contracts/
    registry/
    ontology/
    guru/
    store/
    release/
    transport/mcp/
      descriptors.py
      dispatcher.py
      server.py
      http.py
    resources/
  tests/
  IMPORT_PROVENANCE.json
```

`IMPORT_PROVENANCE.json`은 source commit, 원본/대상 파일 hash, 포함/제외 이유를 저장한다.
이는 runtime sync 기능이 아니다. 첫 이전을 재현하기 위한 감사 artifact이며 이후 대상 코드가
canonical source가 된다.

원본 저장소의 working tree가 dirty한 상태에서는 복사하지 않는다. tracked source commit을
기준으로 가져오고, runtime에 영향을 주는 uncommitted diff는 별도 검토 후 의도적으로 적용한다.

## 5. Root SearchPlan MCP

### 5.1 ToolDescriptor

FastMCP decorator 대신 모든 도구를 다음 registry 의미로 등록한다.

```text
ToolDescriptor
  logical_capability_id
  mcp_tool_name
  title
  description
  input_model
  success_output_model
  error_output_models
handler
  lane
  auth_scope
  read_only
  parallel_safe
```

`list_tools`는 descriptor에서 input/output JSON Schema를 생성한다. `call_tool`은 이름을 한 번
조회한 뒤 동일한 공통 dispatcher로 실행한다. 도구별 transport wrapper나 거대한 분기문은 없다.

### 5.2 query-context 직접 입력

```text
MCP tools/call
  name: krw_ontology_query_context
  arguments: SearchPlan
```

처리는 다음과 같다.

1. `arguments` 루트 객체를 `SearchPlan.model_validate(arguments)`로 검증한다.
2. 유효하면 동일 객체를 canonical plan으로 사용한다.
3. 유효하지 않으면 field-local violation을 `QueryContextInputCorrection`으로 만든다.
4. retrieval 결과는 `ResearchState` structuredContent로 반환한다.
5. provider에 전달할 때만 Rust DeepSeek codec이 structured JSON을 JCS 문자열로 직렬화한다.

`call_tool(validate_input=False)`를 사용하되 공통 dispatcher가 모든 입력을 반드시 Pydantic으로
검증한다. low-level SDK의 generic JSON Schema error가 먼저 응답하여 구조화된 correction을
잃는 일을 방지하기 위해서다.

### 5.3 명시적인 비호환성

- `{ "search_plan": {...} }`는 unknown-field/shape error다.
- root SearchPlan 이전 episode replay를 허용하지 않는다.
- old schema hash를 가진 AgentImage는 admission에서 거부한다.
- 자동 unwrap, fallback, alias migration은 없다.

## 6. Canonical contract pipeline

Pydantic domain model을 authoring source로 유지하되 생성 결과 하나만 배포한다.

```text
Pydantic model
  → deterministic JSON Schema 2020-12
  → RFC 8785 canonical bytes
  → SHA-256 manifest
  → generated Rust types/validators
  → generated TypeScript inspection types
  → AgentImage and DeploymentSnapshot pins
```

새 bundle은 `contracts/krw-capabilities/v3/`에 둔다. SearchPlan, ResearchState,
targeted-query, trace, Guru input/output, correction contract를 포함한다.

필수 invariant:

- MCP `tools/list.inputSchema`의 JCS hash가 manifest input hash와 같다.
- Rust provider tool parameter hash는 capability의 model-input contract hash와 같고, MCP
  `tools/list.inputSchema` hash는 physical input contract hash와 같다.
- `ResearchIntent → SearchPlan` derivation은 pinned compiler version/receipt와 양쪽 canonical hash로
  연결되며, physical MCP request hash는 컴파일된 root `SearchPlan` hash와 같다.
- MCP dispatcher가 검증한 value의 canonical hash가 durable action request hash와 같다.
- output structuredContent는 pinned success/error contract 중 정확히 하나를 만족한다.
- Python, Rust, TypeScript에 독립적으로 손으로 쓴 같은 계약이 존재하지 않는다.

### 6.1 Sidecar readiness / deployment identity ABI

`tools/list`만 맞아도 잘못된 release나 다른 Python build에 연결할 수 있다. 따라서 sidecar는
listener를 열기 전에 다음 세 값을 스스로 계산하고, deployment는 이 결과를 pin한다.

| Pin | 계산 원본 | 바뀌는 경우 |
| --- | --- | --- |
| `build_id` | 실행 중인 `krw_capability_runtime` source bundle + Python/MCP/Pydantic/Starlette/Uvicorn dependency version | sidecar code 또는 runtime dependency 변경 |
| `tool_schema_sha256` | 실제 immutable `ToolDescriptor` 28개에서 생성한 complete `tools/list` wire bundle | tool name/input/output schema/description/mapping 변경 |
| `release_manifest_sha256` | startup admission을 통과한 release `manifest.json`의 정확한 bytes | ontology/Guru data release 변경 |

모든 HTTP MCP sidecar는 `krw-capabilityd/readiness/v1` shape로 다음을 반환한다.

```json
{
  "schema_version": "krw-capabilityd/readiness/v1",
  "ok": true,
  "fingerprint_match": true,
  "service": "...",
  "transport": "streamable-http",
  "protocol_version": "2025-06-18",
  "build_id": "...",
  "tool_schema_sha256": "sha256:...",
  "release_manifest_sha256": "sha256:...",
  "tool_count": 28
}
```

Rust는 loose JSON field lookup이나 old health endpoint fallback을 두지 않는다. version, shape,
transport, protocol, count bound, 세 fingerprint가 모두 맞아야 MCP `initialize`를 보낸다. 현재
sidecar는 `run-scoped` session만 제공하므로 readiness 또는 initialize에 stateless attestation을
거짓으로 넣지 않는다.

운영 배포에서는 `krw-capabilityd`가 loopback HTTP로만 listen하고, same-origin TLS termination/proxy가
`/mcp`와 `/healthz`를 외부 HTTPS endpoint로 제공한다. endpoint URL·certificate·credential은
`DeploymentBinding`/endpoint registry의 deployment data이며 AgentImage에는 들어가지 않는다.
Rust transport는 `system-roots-v1` 또는 명시적으로 주입한 PEM trust anchor를 추가하는
`system-plus-pinned-ca-v1`만 허용한다. 후자의 PEM 원문은 snapshot/pool key/log에 남기지 않고
hash만 execution fingerprint와 pool partition에 포함한다. 인증서 검증을 끄는 profile이나
`accept_invalid_certs` 우회는 없다. MCP HTTP client는 process-wide proxy environment도 읽지 않으며,
향후 proxy가 필요해도 별도 deployment ABI로 명시해야 한다.
`KRW_CAPABILITYD_EXPECTED_BUILD_ID`,
`KRW_CAPABILITYD_EXPECTED_TOOL_SCHEMA_SHA256`,
`KRW_CAPABILITYD_EXPECTED_RELEASE_MANIFEST_SHA256`를 prod에서 모두 설정해 sidecar 자체도 pin
drift에서 startup fail-closed한다.

## 7. Capability ABI 정리

현재 `CapabilityArgumentAssembly`는 trusted domain input 생성과 transport 외피를 섞고 있다.
이를 다음 둘로 분리한다.

```text
InputDerivation
  identity
  sealed_object_v1
  evidence_projection_v1

TransportCodec
  canonical_mcp_v1
```

query-context는 `InputDerivation::identity`와 `TransportCodec::canonical_mcp_v1`이다.

`DeploymentBinding`은 schema v3부터 두 물리 식별자를 별도로 가진다. `binding_key`는 endpoint,
credential, pool, release pin을 고르는 symbolic deployment key이고, `mcp_tool_name`은 그 endpoint에
보낼 실제 MCP method다. AgentSpec은 전자만 참조한다. 따라서 physical method rename이나 여러
logical capability의 한 deployment 공유가 agent 의미나 Rust transport 분기로 새지 않는다.

Guru처럼 이전 committed artifact를 이용해 입력을 봉인해야 하는 capability는 선언형 derivation
program을 사용한다. 허용 opcode는 `proposal`, `artifact`, `project`, `object`, `array`, `const`,
`require`, `hash_bind`처럼 bounded data construction만 포함한다. filesystem, network, loop,
arbitrary code는 허용하지 않는다.

compiler는 derivation program의 입력/output contract, artifact dependency, state precedence를
검증하고 AgentImage에 bytecode와 hash를 넣는다. runtime은 capability ID가 아니라 compiled
program을 실행한다.

삭제 대상:

- `QueryContextSearchPlanEnvelopeV1`
- `query_context_search_plan_envelope_v1`
- query-context 전용 `provider_tool_parameters` 분기
- `normalize_provider_tool_arguments`의 unwrap/fallback
- `EvidenceMapping::mcp_arguments`의 `{search_plan: arguments}` 생성
- prompt의 `outer argument`, `exactly one search_plan key` 문구
- root와 wrapped plan을 모두 받는 compatibility test

## 8. Typed DeepSeek wire

### 8.1 메시지 타입

```text
ProviderMessage
  SystemMessage { content: SecretString }
  UserMessage { content: SecretString }
  AssistantMessage {
    content: Option<SecretString>,
    reasoning_content: Option<SecretString>,
    tool_calls: Vec<ToolCall>
  }
  ToolMessage {
    tool_call_id: ToolCallId,
    content: CanonicalJsonText
  }
```

`CanonicalJsonText`는 validated JSON value에서만 생성되며 생성 시 JCS 문자열이 된다. 따라서
tool content에 JSON 객체를 넣는 코드는 타입 검사에서 막힌다.

도구 정의도 `ProviderToolDefinition`, `ProviderFunctionDefinition`, `JsonSchemaDocument` 타입을
사용한다. `serde_json::Value`는 contract payload 내부에서만 허용하고 메시지/도구 envelope에는
허용하지 않는다.

### 8.2 provider symbol codec

logical capability ID는 사람이 읽는 dotted name을 유지한다. DeepSeek 함수명은 하나의
결정론적 codec이 생성한다.

```text
ontology.query_context
  → readable sanitized stem + fixed hash suffix
```

규칙은 모든 capability에 동일하게 적용한다. 역매핑은 이번 AgentImage의 tool registry에만 있다.
도구별 별칭 표와 수동 이름 예외는 없다.

### 8.3 요청 전 상태기계 검증

DeepSeek HTTP 전송 전에 다음을 모두 검증한다.

- role sequence와 message별 허용 필드
- assistant tool call ID와 뒤따르는 tool result ID의 정확한 일대일 대응
- tool result content가 canonical string인지
- thinking tool turn의 `reasoning_content` replay가 보존됐는지
- 함수명 grammar와 길이
- tool schema hash와 현재 AgentImage hash
- 요청/관측 model이 정확히 `deepseek-v4-flash`인지
- output, conversation, tool argument byte limits

검증 실패는 네트워크 요청 0회인 typed local error다. provider 400 원문은 보존하지 않고
allowlisted error category와 body hash만 남긴다.

## 9. ResearchIntent, ClaimGraph, 최소 충분 계획

### 9.1 모델의 역할

DeepSeek는 질문을 보고 다음 후보를 제안한다.

- 질문의 명시적 목표
- 답변에 필요한 load-bearing claim 후보
- claim dependency
- 필요한 근거 directness와 계산 여부
- SearchPlan clause draft와 해당 clause가 cover하는 goal ID
- 사용자 질문의 exact source span

모델 confidence와 모델이 만든 비용 점수는 production 결정에 사용하지 않는다.

### 9.2 trusted planner의 역할

kernel은 기존 `crates/planning`의 `EvidenceGoalGraph`와
`crates/research-planner`의 fixed-point score를 확장해 초기 계획부터 사용한다.

1. 사용자 span과 authenticated scope를 검증한다.
2. 명시적 질문 또는 output contract에 필요한 claim만 required goal로 인정한다.
3. 답변을 위한 하위 근거는 dependency goal로 연결한다. 별도 답변 주제로 확장하지 않는다.
4. 같은 goal set을 cover하는 중복 clause를 제거한다.
5. scope가 넓거나 사용자 목표와 연결되지 않은 clause를 거부한다.
6. 남은 후보에서 최소 충분 clause set을 선택한다.

### 9.3 선택 알고리즘

선택은 다음 lexicographic objective를 사용한다.

1. 모든 required goal을 cover한다.
2. directness와 calculation requirement를 만족한다.
3. 예상 실패/중복 위험을 최소화한다.
4. latency, token, tool cost, result bytes를 최소화한다.
5. 동률이면 clause 수와 canonical fingerprint 순으로 결정한다.

후보 dominance 제거 후 작은 frontier는 bounded branch-and-bound set cover로 최적해를 구한다.
fuel 한도를 넘거나 후보가 큰 경우 동일한 점수의 deterministic lazy-greedy maximum coverage를
사용한다. 두 경로 모두 fixed-point 정수 연산과 안정된 tie-break를 사용한다.

비용/성공률은 signed deployment telemetry registry에서 가져온 보수적 confidence bound다.
모델이 임의로 입력할 수 없다.

### 9.4 adaptive replan과 stop

첫 ResearchState가 들어온 뒤에는 같은 graph를 업데이트한다.

- `missing_parts`, clause coverage, directness, calculation coverage를 goal progress로 변환한다.
- 서버가 추천한 precise action만 follow-up 후보가 될 수 있다.
- 새 action의 marginal value가 양수일 때만 query/trace/context append를 실행한다.
- required goal이 충족됐거나 positive candidate가 없으면 멈춘다.
- deadline/budget 소진 또는 자료 부재 시 partial/insufficient answerability로 종료한다.

`한 회사 질문이면 clause 1개` 같은 수량 규칙은 없다. 단순 질문은 목적함수 때문에 자연스럽게
작은 plan을 만들고, 비교/계산 질문은 필요한 만큼 확장된다.

## 10. MCP runtime 성능 및 메모리 계획

품질 parity를 먼저 통과한 뒤 profile evidence로 최적화한다.

### 10.1 process와 pool

- 머신당 `krw-capabilityd` 한 프로세스
- ontology/Guru registry와 immutable indexes 공유
- Rust MCP connection pool은 endpoint/release/auth scope로 partition
- public read는 공유 가능, tenant/principal/run scope는 각각 분리
- queued session마다 Python task, timer, SQLite connection을 만들지 않음

### 10.2 read path

- immutable release를 startup에서 한 번 검증
- SQLite connection은 bounded read pool, `query_only`, prepared statement cache 사용
- mmap/page cache는 실제 PSS와 page-fault 측정으로 크기를 고정
- Pydantic schema와 validator는 startup에서 한 번 생성
- structured result를 먼저 만들고 text/JCS projection은 한 번만 생성
- cache key에 data release, tool name, canonical argument hash, auth scope 포함
- cache는 byte-bounded LRU이며 entry count만으로 제한하지 않음
- raw question, reasoning, 개인 data는 shared cache key/value에 넣지 않음

### 10.3 최적화 승인 규칙

- 의미 변경 없는 profile-driven change만 성능 최적화로 분류
- native Rust 승격은 sidecar 제거 후 end-to-end p95 또는 process-tree PSS가 10% 이상 개선될 때만
  검토
- ontology retrieval 의미를 Rust에 중복 구현하지 않음
- microbenchmark만 좋아지고 end-to-end가 나빠지는 변경은 거부

## 11. 품질 평가 체계

현재 단일 fixture는 contract smoke test로 유지하되 품질 승인 근거로 사용하지 않는다.

현재 구현은 그 smoke test를 `evals/krw-research-quality/v4`의 content-addressed recorded replay로
교체했다. 단일 주장, 두 개의 독립 주장, 그리고 부분 근거에서 시작해 trace 결과로 같은 사용자 목표의
새 clause만 append하는 selective-replan case를 포함한다. 마지막 case는 관련 없는 모델 제안을 planner가
dispatch 전에 거절하고, 근거 그래프 안의 추가 조사만 허용하는지를 검증한다. 각 case는 run request,
capability별 응답 script, 모든 recorded assistant turn의 raw-byte hash를 manifest에 pin한다.
`krw-agent quality replay`는 credential 없이 production `RunEngine`을 끝까지 실행해 provider wire,
`ResearchIntent → SearchPlan` compilation, direct-root capability invocation, EvidenceLedger, AnswerIR와
final commit을 함께 검증한다. 이는 deterministic regression gate일 뿐 live model quality의 대체물은 아니다.

### 11.1 eval 축

최소 다음 case family를 만든다.

1. 한 회사 단일 원인 질문
2. 한 회사 복수 독립 주장
3. 두 회사 비교
4. 수치 및 계산 lineage
5. 기간 모호성/기간 우선순위
6. 자료 부족과 정직한 중단
7. partial coverage 후 targeted query
8. trace lineage가 필요한 strong claim
9. 상충 근거
10. source prompt injection
11. 한국어 질문과 영어 내부 분석
12. duplicate/retry/recovery
13. Guru sealed workflow
14. universe/idea-generation scope

### 11.2 deterministic assertions

- authenticated ticker/universe scope 위반 0
- required goal 누락 0
- user intent와 연결되지 않은 required goal 0
- unsupported strong/numeric claim 0
- evidence/calculation lineage 누락 0
- 내부 용어, policy text, reasoning 노출 0
- 같은 action fingerprint 중복 실행 0
- wrapped SearchPlan 수락 0
- exact root schema/hash mismatch dispatch 0

clause 개수는 정답 하나로 고정하지 않는다. required goal coverage, 중복, marginal value, 예산을
기준으로 허용 범위를 판정한다.

### 11.3 live quality

- pinned real ontology release와 실제 imported MCP를 사용
- 동일 모델/예산에서 반복 실행하여 stochastic pass rate 측정
- deterministic gate는 100%, model behavior gate는 사전에 고정한 confidence bound 사용
- 기존 답변과 blind A/B judge를 쓰되 deterministic verifier보다 높은 권한을 주지 않음
- report에는 prompt, answer, reasoning, tool arguments를 저장하지 않고 hash/shape/assertion만 기록

## 12. 구현 순서와 exit gate

### Phase 0. Import freeze와 baseline

작업:

- 27개 tool inventory와 transitive runtime import closure 생성
- source commit/file hash provenance 저장
- 원본 MCP의 contract/tool fixture를 oracle로 동결
- 현재 direct DeepSeek quality 실패를 redacted artifact로 저장
- offline builder와 online runtime 경계 확정

Exit:

- 모든 도구가 import/retire/exclude 중 하나로 분류
- source dirty file을 실수로 복사하지 않았음
- secret/data/cache 파일 포함 0

### Phase 1. `krw-capabilityd` import

작업:

- runtime-only Python package 생성
- ontology/Guru read dependency closure 이동
- Claude SDK와 build-only dependency 제거
- 기존 domain behavior test 이전
- 하나의 shared process/lifespan/store 생성

Exit:

- 27개 handler parity test 통과
- original checkout 없이 clean machine에서 시작
- idle process와 one-read memory/latency baseline 확보

### Phase 2. Contract v3와 low-level MCP

작업:

- canonical v3 schema bundle 생성
- ToolDescriptor registry와 generic dispatcher 구현
- root SearchPlan query-context 구현
- structured success/correction output 구현
- HTTP/stdio, readiness, metrics, release fingerprint 구현

Exit:

- root SearchPlan call 성공
- wrapped SearchPlan call 실패
- 27개 `tools/list` schema hash가 manifest와 일치
- FastMCP production import 0

### Phase 3. Rust Capability ABI 수렴

작업:

- query-context envelope enum/branch/fallback 삭제
- InputDerivation과 TransportCodec 분리
- provider-facing model input을 `ResearchIntent`로, physical MCP input을 root `SearchPlan`으로 분리
- `ResearchIntent → SearchPlan` compiler receipt와 canonical physical action identity를 연결
- v3 schema pin과 service readiness 연결

Exit:

- provider `ResearchIntent`와 compiler receipt가 immutable scope/question에 연결되고,
  canonical/MCP `SearchPlan` argument hash가 동일
- capability ID 기반 transport 분기 0
- old image/schema admission 0

### Phase 4. Typed DeepSeek codec

작업:

- typed message/tool/request 구현
- provider conversation state-machine validator 구현
- exact thinking/tool replay와 JCS tool content 구현
- provider symbol codec와 registry 역매핑 구현
- safe error category/report 구현

Exit:

- provider transcript envelope의 `Vec<Value>` 0
- malformed request가 HTTP 전송 전에 모두 거부됨
- two-turn thinking + tool live canary 연속 통과
- `deepseek-v4-flash` 외 model 실행 0

### Phase 5. ResearchIntent와 초기 최소 충분 planner

작업:

- ResearchIntent/ClaimGraph/ClauseCandidate contract 추가
- user span/scope validator 추가
- initial EvidenceGoalGraph 생성
- bounded set-cover와 기존 VOI scorer 결합
- exact-one prompt와 case-specific tool description 제거
- initial plan과 follow-up planner state 통합

Exit:

- 단순/복합/비교/계산 case에서 goal coverage와 최소성 gate 통과
- 동일 입력/후보/registry에서 bit-for-bit deterministic plan selection
- 모델 confidence가 production score에 들어가지 않음

### Phase 6. Real research quality gate

작업:

- 14개 case family fixture와 pinned real-release eval 구축
- EvidenceLedger/AnswerIR/strong claim/calculation 검증
- 반복 live Flash 평가와 blind comparison
- 실패를 provider, plan, retrieval, evidence, compose 단계로 분류

Exit:

- deterministic safety assertion 100%
- unsupported claim/leakage/scope widening 0
- 사전 고정 quality confidence bound 통과
- 품질 실패를 prompt 한 줄 추가로 고친 사례 0, 원인 계층 수정만 허용

### Phase 7. Profile-driven 성능 최적화

작업:

- shared store, bounded connection/cache, serialization copy profile
- latency/token/result-size 기반 planner cost registry 보정
- cold/warm MCP, one/ten/hundred active run profile
- queue는 실행하지 않고 메모리 구조만 검증

Exit:

- quality gate 유지
- legacy source MCP보다 end-to-end p95 비열등
- process-tree memory 개선
- queued receipt 수에 비례한 Python task/connection 0

### Phase 8. PostgreSQL atomic final과 crash matrix

작업:

- real MCP result와 AnswerIR을 PostgreSQL atomic final에 연결
- provider/action/final receipt recovery
- cancel/final, ack loss, SIGKILL matrix
- outbox와 redacted progress event 검증

Exit:

- duplicate visible final 0
- action redispatch divergence 0
- answer/message/run/usage/outbox 원자성 위반 0

### Phase 9. Standalone release와 장시간 검증

작업:

- Rust binaries, Python service, schema, AgentImage, release manifest 패키징
- doctor/contract probe/live quality command
- 장시간 품질 반복 후 다중 세션 soak
- 직전 canonical release artifact rollback 검증

Exit:

- 외부 source checkout 없이 설치/실행
- heap/FD/task 지속 증가 0
- 웹앱 연결 없이 standalone 완료 판정

## 13. 파일별 변경 지도

| 현재 위치 | 최종 처리 |
|---|---|
| `~/krw-ontology/src/krw_ontology/mcp_server/*` | runtime 의미를 새 service로 이전, FastMCP 등록은 재작성 |
| `~/krw-ontology/src/krw_ontology/agent_index/*` | read dependency closure만 이전 |
| `~/krw-ontology/src/krw_ontology/guru/*` | 14개 Guru MCP의 read closure 이전 |
| `services/krw-ontology-runtime/` | 새 canonical Python online runtime |
| `contracts/krw-ontology/v2` | v3 생성 후 runtime pin에서 제거 |
| `crates/agent-image/src/lib.rs` | envelope variant 삭제, derivation/transport 분리 |
| `crates/capability-runtime/src/lib.rs` | query-context MCP wrapper 삭제 |
| `crates/run-engine/src/lib.rs` | unwrap/fallback 삭제, typed provider conversation 사용 |
| `crates/deepseek-wire/src/lib.rs` | typed request/message/tool codec의 단일 소유자 |
| `crates/planning/src/lib.rs` | initial goal graph와 set-cover 선택 확장 |
| `crates/research-planner/src/lib.rs` | initial/follow-up 공통 planner로 확장 |
| `agents/krw-ontology/agent.yaml` | ResearchIntent model input, root SearchPlan physical input, v3 pins |
| `agents/krw-ontology/prompts/retrieval-planner.md` | exact-one/outer-envelope 지침 제거 |
| `crates/research-quality/` | multi-family real MCP quality gate |
| `crates/tool-mcp/` | v3 tools/list/readiness/hash 검증과 shared pool |

## 14. 실패 모드와 방지책

| 실패 | 근본 방지책 |
|---|---|
| Python/Rust schema drift | generated v3 bundle과 동일 hash admission |
| 다시 생기는 MCP 외피 | root inputSchema hash test와 wrapped-input negative test |
| provider 400 반복 | typed wire state machine과 live two-turn canary |
| 한 fixture 과적합 | 다중 intent family와 goal 기반 assertion |
| broad research 폭주 | required-goal linkage, set-cover, positive marginal value |
| 자료 부족인데 계속 호출 | no-frontier/non-positive stop와 partial answerability |
| copied runtime가 다시 fork | 새 service를 canonical source로 선언, sync path 없음 |
| offline pipeline까지 runtime에 유입 | import manifest와 forbidden-import CI gate |
| 세션 수만큼 Python 자원 증가 | one shared process, bounded pool/cache, queue receipt only |
| Guru 입력 위조 | declarative sealed derivation과 artifact hash binding |
| data release 혼합 | composite release fingerprint와 evidence별 component hash |

## 15. CI와 완료 증거

필수 CI gate:

```text
python contract generation determinism
python runtime unit/integration
MCP root-schema conformance for all 27 tools
Rust generated contract tests
DeepSeek wire property/fuzz/golden tests
Capability ABI roundtrip tests
Research planner deterministic/property tests
fixture quality suite
pinned real-release MCP suite
live redacted Flash canary
PostgreSQL crash/atomic-final matrix
performance and memory release gates
standalone package verification
```

정적 금지 gate:

- production `FastMCP` import
- query-context wrapper 생성 또는 unwrap 코드
- query-context transport를 위한 capability ID 비교
- provider message/transcript의 raw `Vec<Value>`
- Claude Agent SDK dependency
- model alias/fallback
- runtime의 외부 `~/krw-ontology` import/path
- prompt 안의 사례별 clause 개수 강제

## 16. 최종 완료 정의

다음을 모두 증명하기 전에는 완료가 아니다.

- 27개 MCP 도구가 새 shared runtime에서 실행된다.
- query-context는 root SearchPlan만 받는다.
- Agent model proposal, trusted compiler, physical MCP input의 서로 다른 schema/hash가 명시적으로
  pin되고 round-trip contract 검사를 통과한다.
- typed DeepSeek thinking/tool 연속 호출이 실제 Flash에서 안정적으로 동작한다.
- 초기/후속 리서치가 하나의 goal graph와 가치/비용 정책으로 결정된다.
- 단순 질문과 복합 질문 모두 사례별 하드코딩 없이 품질 gate를 통과한다.
- unsupported strong/numeric claim, scope widening, reasoning/secret leakage가 0이다.
- real MCP, EvidenceLedger, AnswerIR, PostgreSQL atomic final이 crash matrix를 통과한다.
- 한 머신의 queued/active session 증가가 Python process/connection/task 선형 증가를 만들지 않는다.
- standalone 패키지는 웹앱과 원본 ontology checkout 없이 실행된다.

## 17. 현재 제외 범위

- `krw-ontology-front` 연결 및 배포
- 기존 웹앱 session migration
- legacy runtime shadow/canary/fallback
- arbitrary plugin code, WASI marketplace
- ontology build/extraction pipeline의 Rust 이전
- profile evidence 없는 native ontology rewrite

웹앱 연결은 이 문서의 standalone 완료 조건을 모두 통과한 뒤 별도 계획으로 다룬다.
