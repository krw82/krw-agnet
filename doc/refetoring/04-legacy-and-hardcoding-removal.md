# 레거시 및 하드코딩 제거

## 삭제 원칙

코드가 오래됐다는 이유만으로 삭제하지 않는다. 다음 조건을 모두 만족할 때 삭제한다.

1. 현재 checked-in agent/deployment에서 참조가 없다.
2. production config에서 도달할 수 없다.
3. 동일 책임을 가진 현행 경로가 있다.
4. 삭제 후 workspace/agent/release 검증이 통과한다.

사용자가 rollback/legacy compatibility를 요구하지 않았으므로 persisted old episode replay를 위한 compatibility branch도 최종적으로 제거한다. Git history가 복구 수단이다.

## 즉시 삭제 대상

### phantom local MCP

- `krw_skill_local` deployment binding
- local skill에 대한 MCP session reuse matrix
- capability runtime의 remote `SkillContent` 경로
- local skill release/data hash

### 미지원 transport

- `TransportKind::McpStdio`
- `TransportKind::Native`
- 단일값 `TransportKind::McpHttp`
- `CapabilityBinding.transport` selector

remote capability의 실행 방식은 스키마 선택지가 아니라 MCP HTTP 계약 그 자체다.
local builtin은 `CapabilityExecution::Local`로 이미 분리되어 있으므로 physical binding에는
transport 필드가 없다. fixture도 production과 같은 remote binding shape를 사용하고 실제
network boundary만 test double로 대체한다. 이 breaking change는 deployment binding schema
v4와 resolved capability fingerprint v5에서만 허용하며, v3 및 `transport` 필드는
`deny_unknown_fields`와 version 검증으로 거부한다.

### model profile alias

- `flash_*` compatibility profile
- `model-registry.yaml` GLM duplicate alias
- `DeepSeekProviderCatalog::compile`
- compatibility-only `compile_release_set`

provider는 명시적 registry file과 provider kind를 사용한다.

### rollback

- launchd installer의 previous symlink/plist/overlay 저장
- `--rollback` mode
- frontend rollback image
- local agent/capability/gateway restore functions
- non-terminal cutover recovery graph
- manual GCP recovery owner 기록

retain할 것은 rollback이 아니라 idempotent staging, fencing, admission close, terminal receipt다.

### build fallback

- `KRW_ONTOLOGY_ROOT` ambient sibling fallback
- source 부재 시 empty metric mapping 생성

checked-in runtime snapshot을 유일한 compile input으로 사용한다.

## provider 하드코딩

현재 하드코딩:

- GLM model id와 DeepSeek model id closed list
- `is_glm ? glm : deepseek`
- unknown model을 `InvalidDeepSeekModel`로 분류
- 두 provider API key 필드
- release package가 정확히 `glm|deepseek` 두 디렉터리라고 가정
- frontend가 provider model 문자열을 중복 보유

최종 필드:

```text
provider_id
provider_kind
wire_codec
model_id
credential_ref
endpoint_ref
context_limit
output_limit
thinking_policy
wire_capabilities
```

provider secret은 `credential_ref -> SecretSource`로 한 번만 해석한다.

## limit 하드코딩

현재 서로 다른 층의 상한이 어긋난다.

| 항목 | 현재 예 |
| --- | ---: |
| engine capability result | 8 MiB |
| engine final | 1 MiB |
| engine conversation | 16 MiB |
| host Markdown | 64 KiB |
| MCP raw | 8 MiB |
| provider assembler tool calls | 32 |
| engine tool calls | 8 |
| presentation pack | 64 KiB |

하나의 versioned `RuntimeLimits` descriptor를 source of truth로 만들고 Rust/TS artifact를 생성한다. 기존 상한 중 더 안전한 값을 기본으로 사용하되 optional field는 drop 가능하게 한다. descriptor mismatch는 startup artifact 검증에서만 발견하며 model turn 중 새 gate를 만들지 않는다.

## frontend coupling 삭제

삭제할 agent-side coupling:

- frontend migration directory 재귀 scan
- 특정 날짜 migration 파일명 검사
- `route.ts` source scan
- `/run/krw-agent/...` container path 생성
- frontend runtime env 생성

삭제할 frontend-side coupling:

- agent source checkout의 build script 이름 지식
- bundle 내부 installer path 조립
- dual provider tree 내부 구조 추론
- newest candidate directory 선택

대체물은 versioned contract JSON과 manifest-declared actions다.

## 보존 대상

- DeepSeek provider 자체: 실서비스 경로
- GLM provider 자체: 테스트와 선택 가능한 runtime 경로
- `RunScoped`와 `AttestedStatelessV1`: endpoint별 현재 실제 사용을 확인한 뒤 MCP 2026 전환에서 제거
- session snapshot과 audit rebuild: 둘 다 live recovery 경로
- `.local/`: DB, key, artifact가 있을 수 있으므로 자동 삭제 금지
