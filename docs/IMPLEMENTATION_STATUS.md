# KRW Agent 현행 구현 기준

작성일: 2026-08-12
상태: **ACTIVE IMPLEMENTATION AUTHORITY**

이 문서는 현재 무엇을 만들고 있으며 어떤 결정이 확정됐는지를 기록한다. 오래된 architecture,
simulation, test, DX 문서는 판단 근거로 보존하지만 구현 권한은 갖지 않는다. 충돌 시 우선순위는
다음과 같다.

1. 사용자의 최신 명시 지시
2. 이 문서
3. 실제 code, schema와 통과한 test가 증명하는 현재 기능
4. `SUPERSEDED` 배너가 붙은 과거 문서

## 1. 한 문장 설계

**직접 작성한 8개 KRW AgentSpec을 변경 시점에만 immutable AgentImage로 만들고, 한 개의
machine-wide Rust daemon이 정적 release set으로 모두 적재해 Anthropic-compatible provider API와 새 shared
Python `krw-capabilityd`를 공유하면서 많은 세션을 bounded memory로 실행한다.**

## 2. 확정 결정

### Agent source

- 기존 `SKILL.md`나 Claude/Codex plugin을 자동 import하지 않는다.
- maintainer가 KRW 의미를 canonical AgentSpec, prompt, typed contract와 validator로 직접 수정한다.
- 현재 canonical source는 `agents/krw-ontology/agent.yaml`과 같은 directory의 prompt다.
- 임의 validator script나 plugin runtime code를 daemon에 동적 load하지 않는다.
- `post_action` validator는 capability ID가 아니라 declared `result_ingest_scope`로 한정한다. 따라서
  ResearchState 전용 contract rule이 trace/targeted-result ABI에 적용될 수 없다.

### Compile timing

- AgentSpec은 source가 변경됐을 때 CI/build/release 단계에서만 검증·컴파일한다.
- daemon은 source나 Markdown을 해석하지 않고 검증된 AgentImage만 load한다.
- daemon 시작 시 1..=64개 이미지 디렉터리를 하나의 immutable release set으로 검증하며, exact hash
  중복은 intern하고 agent identity 또는 `(run_kind, locale)` 소유권 중복은 거부한다.
- session/run마다 compile하지 않는다.
- run 시작 시 resolved image hash와 deployment contract를 고정하며, 실행 도중 바뀌지 않는다.
- hot reload, 기본 단일 이미지 fallback, rollback selector를 두지 않는다.

따라서 변경 시 compile 비용은 배포 전 한 번뿐이고, 대량 session의 latency와 resident memory에는
포함되지 않는다. runtime에서 규칙을 매번 parse하는 것보다 빠르고 결정적이다.

### Runtime and provider

- production target은 기존 Claude plugin worker와 같은 이 Mac의 user-level `launchd` Rust daemon이다.
- provider wire는 Anthropic Messages 계약을 사용한다. GLM-5.3와
  `deepseek-v4-flash`를 각각 exact registry/model lane으로 생성·검증할 수 있으며,
  한 queue에서는 한 provider만 admission한다. GLM은 staging/quality lane,
  DeepSeek는 service lane으로 선택할 수 있지만 hot-swap은 하지 않는다. 현재 이
  개발 세션에서는 실제 provider network 호출과 production activation은 아직 하지
  않았다.
- 물리 model/profile은 immutable registry에서 exact-match로 검증하며 alias와 silent fallback은 startup에서 거절한다.
- model ID는 deployment registry에서 정확히 allowlist하며 alias, silent fallback과 provider 자동 매핑을
  허용하지 않는다.
- thinking + tool turn의 `reasoning_content`와 tool-call lineage는 provider 계약 그대로 checkpoint하고
  replay한다.
- Claude Agent SDK, Claude Code subprocess, ccSwitch와 Anthropic compatibility API는 runtime dependency가
  아니다.

### Capability boundary

- online read runtime의 canonical source는 `services/krw-ontology-runtime/`다. 원본
  `~/krw-ontology`의 tracked commit에서 가져온 13개 ontology + 14개 Guru handler는 이 service 안에서만
  실행하며, 원본 checkout은 offline build/release 생성에만 쓴다.
- `ontology.query_context`의 MCP input은 `{ "search_plan": ... }`가 아니라 `SearchPlan v2` 루트
  객체다. wrapped input은 자동 변환 없이 typed correction으로 거부한다.
- DeepSeek가 보는 query-context input은 물리 `SearchPlan`이 아니라 `ResearchIntent v1`이다.
  kernel의 trusted planner가 사용자 질문·인증 scope·goal graph를 검증한 뒤 최소 충분한 root
  `SearchPlan`으로 컴파일한다. 따라서 모델 제안, canonical action, MCP transport의 계약을 같은
  JSON으로 혼동하지 않는다.
- Rust kernel은 pooled Streamable HTTP MCP로 이 의미 엔진을 호출한다. `binding_key`는
  endpoint/pool/credential 선택이고 `mcp_tool_name`은 실제 MCP method이므로 둘을 혼동하지 않는다.
- HTTP MCP listener는 시작 시 실제 27-tool descriptor registry, 실행 중인 Python source/dependency
  bundle, 그리고 이미 검증한 immutable release manifest bytes에서 build/schema/data identity를 계산한다.
  `/healthz`는 versioned readiness v1 typed document로만 그 세 pin을 내보내며, Rust는 그 문서와
  `initialize` protocol을 모두 검증한 뒤에만 session을 만든다. 각 ontology/Guru binding은 같은
  `krw-capabilityd` endpoint와 동일한 세 pin을 사용한다.
- HTTPS client trust는 deployment의 closed TLS profile로 결정한다. `system-roots-v1`은 platform
  root만 사용하고, private ingress는 CA PEM secret의 hash까지 pinned execution/pool identity에
  묶는 `system-plus-pinned-ca-v1`을 사용한다. certificate verification bypass는 지원하지 않는다.
- Rust adapter는 ResearchState를 EvidenceLedger로 안전하게 ingest하고 실행 정책을 적용한다.
- MCP endpoint, credential, protocol/server build/server schema bundle/data-release pin은 DeploymentBinding이 소유한다. 도구 input/output contract hash는 AgentImage가 별도로 소유한다.
- AgentImage에 URL, key, 사용자 ID, DB table 의미를 넣지 않는다.
- 온톨로지 검색 엔진 자체를 Rust로 재작성하지 않는다.

### Dual-provider release status (2026-08-12)

- `scripts/build_dual_provider_release.sh`가 Rust binary와 8개 AgentImage를 한 번만
  만들고, 동일한 공통 bytes를 `glm`/`deepseek` bundle로 분리한다.
- provider별 `prepare → seal → finalize`와 exact physical model 검사가 구현되어
  있다. DeepSeek bundle을 GLM bundle로 조용히 대체하거나 alias로 fallback할 수 없다.
- front는 provider별로 다시 빌드하지 않고, 선택한 sealed descriptor와 이 Mac의
  `~/.local/share/krw-agent/current` atomic symlink로 provider를 고른다. GCP에는
  public descriptor와 hash만 전달된다.
- offline manifest/evidence/budget/front contract 검증은 통과했다. 실제 clean release,
  운영 endpoint·서명·DB/TLS 입력, GLM/DeepSeek live acceptance와 canary는 운영 gate로
  남아 있다.

### Compatibility and recovery

- legacy plugin importer, legacy executor, dual runtime과 legacy session migration을 만들지 않는다.
- rollback 제품 경로를 별도로 만들지 않는다.
- 변경 실패는 source를 고쳐 새 검증 이미지로 전진 수정한다.
- 이는 crash recovery와 같은 의미가 아니다. action receipt, fencing, idempotent replay, cancel/final
  linearization은 데이터 정합성을 위해 필수로 구현한다.
- 모델이 만든 도구 호출의 형식·현재 frontier·조사 제안 오류는 각 오류마다 별도 statechart를 만들지
  않는다. kernel은 raw payload 없이 공통 `recovery_required` 결과를 transcript에 넣고, Flash가 현재
  광고된 도구·전이 중에서 재조사, 범위 축소, 답변 완료를 다시 선택하게 한다. 일시적 provider/MCP
  장애는 kernel retry/defer가 맡고, release·scope·receipt 무결성 위반은 이 loop에 넣지 않는다.

## 3. 최종 실행 경로

```text
AgentSpec source + prompt
  └─ change-time compiler
       └─ immutable AgentImage + content hash

Static AgentImage release set + global DeploymentBinding/Budget/Model registries
  └─ image별 exact effective binding/profile subset + precompiled catalogs

ClaimReceipt.agent_image_hash + RunRequest
  └─ ResolvedExecutionSnapshot
       └─ Rust Agent Kernel
            ├─ selected GLM-5.3 or deepseek-v4-flash HTTPS/SSE lane (live)
            ├─ bounded scheduler and budgets
            ├─ durable provider/action receipts
            └─ pooled MCP
                 └─ shared Python krw-capabilityd
                      └─ ResearchState v2
                           └─ EvidenceLedger (kernel-owned)
                                └─ direct Korean Markdown (Flash)
                                     └─ output boundary + ledger receipt
                                          └─ atomic final + outbox
```

## 4. 계층별 책임

| 계층 | 소유하는 것 | 소유하지 않는 것 |
|---|---|---|
| AgentSpec | workflow, role, evidence/period/output policy, prompt, bounded validator | endpoint, secret, 사용자·세션 |
| AgentImage | 위 의미의 검증된 immutable 실행 표현과 hash | source parsing, 환경별 주소 |
| DeploymentBinding | MCP transport, endpoint/credential ref, server schema bundle/build/data pin | 도구 input/output contract, 투자 판단 규칙 |
| Model Registry | 정확한 GLM/DeepSeek model/API allowlist | prompt와 도메인 workflow |
| Rust Kernel | provider loop, state, budget, authz, retry/cancel/fence, EvidenceLedger/final-output receipt | Python ontology 검색 의미 |
| Python `krw-capabilityd` | root SearchPlan/ResearchState의 canonical 의미와 실제 ontology·Guru 조회 | 전체 agent 판단과 final 저장 |
| Host/DB | user/session ownership, queue, billing, SSE, atomic outcome/outbox | model 내부 tool loop |

## 5. 구현되어 있는 범위

### Authoring, image, state program

- `krw-ontology`, `krw-ontology-en`, `krw-feed`, `krw-source-filing`,
  `krw-guru-advisor`, `krw-router`, `krw-display`, `krw-notebook`의 직접 작성 AgentSpec과 prompt가
  canonical source다. 기존 SKILL/plugin은 runtime 또는 importer 입력이 아니다.
- deterministic JCS + SHA-256 AgentImage compiler/writer/loader, source path confinement,
  prompt content interning, 1..=64 이미지 static release set, image/entrypoint ownership 충돌 차단이
  구현됐다. compiler는 source의 모든 contract ID와 SHA-256 pin을 closed canonical registry와 즉시
  대조하므로 unknown contract나 형식만 맞는 잘못된 hash로는 image를 만들 수 없다.
- image별 effective binding/model/budget subset을 startup에 한 번 resolve하고, precompiled
  capability/engine catalog를 machine-wide로 공유한다. receipt가 고정한 image hash 외의 fallback은 없다.
- bounded Rule ISA와 workflow graph/fuel/visit 제한, typed `StateArtifactEnvelope`, 그리고 하나의
  `StateInterpreter`가 실행 상태의 유일한 workflow authority다. 예전 cursor/checkpoint 이중 권한은 없다.
- router, notebook, display의 typed product input/output과 input/source linkage 검증이 구현돼 있다.
  특히 display는 committed answer source unit 밖의 사실을 만들거나 순서를 바꾸지 못한다.

### Provider, capability, Guru

- provider-native HTTP/SSE client, fragmented delta decoder, exact request/observed model 검증,
  thinking/tool `reasoning_content` replay, bounded response handling이 있다. GLM TypedJson 상태는
  Z.AI가 실제 지원하는 `response_format.type=json_object`를 전면 사용하고, canonical schema는
  trusted prompt + kernel local validation으로 계속 검증한다. Anthropic `output_config.json_schema`
  및 strict tool-input은 문서화·admission 근거가 부족해 비활성으로 유지한다.
  GLM이 `ResearchProposal v4`의 tagged goal `kind`를 생략해도 완전한 필드 집합으로
  의미가 하나로 결정되는 경우에만 kernel이 해당 discriminator를 보완한다(qualitative,
  metric observation/change). 필드가 혼합되거나 불명확하면 기존 recovery loop가
  그대로 수정 요청을 보내며, objective 분할이나 사용자 범위를 대신 결정하지 않는다.
  Claude Agent SDK, subprocess, compatibility API는 runtime dependency가 아니다.
- pooled Streamable HTTP MCP는 `run-scoped` 또는 readiness+initialize가 증명한
  `attested-stateless-v1` 세션만 사용한다. pool key는 principal/release/fingerprint를 분리하고,
  single-flight initialize, hard entry cap, negative cache와 idle eviction을 가진다.
- readiness는 loose health JSON이 아니라 `krw-capabilityd/readiness/v1` typed ABI다. extra/old field
  shape, protocol drift, build/schema/release mismatch는 tool call 전에 거부한다. 현재 sidecar는
  `run-scoped`만 제공하므로 stateless attestation을 꾸며 내지 않는다.
- imported low-level Python MCP registry는 ontology 13개와 Guru 14개를 하나의 process-wide registry와
  bounded lane으로 제공한다. `FastMCP`와 stdio compatibility server는 production runtime에 없다.
- ontology `ResearchState v2`, front feed/공개 filing, 그리고 sealed Guru query/brief/review mapping은
  모두 pinned contract와 bounded normalization을 거쳐 EvidenceLedger에 들어간다. capability
  argument/result canonicalization, schema/identity checks, evidence projection과 recovery replay는 하나의
  machine-wide CPU limiter에서 수행한다.
- capability의 authenticated input scope와 typed result projection은 `AgentImage`의 closed
  `scope_binding`/`result_ingest` declaration으로 고정된다. capability 이름을 비교하는 transport 또는
  evidence-mapping fallback은 허용하지 않는다.
- Guru의 `company_evidence_researcher`는 별도 SDK subprocess나 제한된 하위 에이전트가 아니라 일반
  회사 리서치 역할이다. 하나의 workflow 안에서 sealed Guru brief를 기준으로 회사 ontology의
  `query_context → query/trace/chain → evidence assessment` loop를 사용하고, 결과는
  `guru.review_company_evidence`의 typed review를 거쳐 composer로 전달된다. 따라서 Guru도 일반
  회사 리서치처럼 필요한 스킬을 여러 개 로드하고, 질문의 영향·반례·연결고리를 자율적으로 확장한다.

### Kernel, persistence, session memory

- provider episode → action intent → MCP dispatch → observe → policy/finalize 순서, logical action key,
  ambiguous read 경계, durable replay, cancel/final linearization, fencing과 lease heartbeat가 실제
  `RunSupervisor`/`ProductionClaimedRunExecutor` 경로에 연결돼 있다.
- PostgreSQL은 TLS-only bounded pool과 arbitrary SQL 없는 20개 고정 `agent_v1.*(jsonb)` procedure ABI를
  사용한다. queue는 database-resident principal-fair queue이고, queued/idle session마다 task/timer를
  만들지 않는다.
- encrypted versioned-key recovery artifact CAS, per-run/principal scope, exact hash/size/schema preflight,
  outbox의 durable receipt와 host-owned ACK boundary가 구현돼 있다.
- SessionMemory v3는 append-only delta, frontier/source-lineage chain, latest snapshot + bounded tail,
  audit rebuild, bounded question-conditioned view를 사용한다. snapshot checkpoint도 owner/fence/run-version
  mutation이고 실제 PostgreSQL integration test로 검증된다.

### CLI·host Gateway·front integration

- `krw-agent run`은 provider/MCP/agent DB를 직접 우회하는 별도 실행기가 아니다. authenticated
  Agent Gateway에 question+ticker와 optional session ID만 보내는 typed HTTP client이며, gateway token은
  process environment에서만 읽는다.
- `@krw-agent/host`의 `prepareGatewayCompanyResearch`는 Gateway v1의 작은 request를 existing
  `prepareEnqueueRun` contract로만 변환한다. browser/CLI가 run kind, model, budget, ownership,
  immutable snapshot, session-memory carrier를 넣을 표면은 없다.
- 같은 session의 후속 질문은 새 run으로 queue에 들어간다. DB active-run constraint가 session당 하나의
  turn만 실행하게 하고, 다음 turn의 fenced worker가 마지막 atomic final 뒤의 bounded SessionMemory v3를
  읽는다. direct Markdown은 conversation context일 뿐 새 사실의 authority가 아니다.
- `krw-ontology-front`의 `/api/chat/run` route가 authenticated user/tenant/session ownership을
  확인하고, 회사 질문은 `company_research`, 티커 없는 질문은 `wide_research`로 typed context를
  만들어 `@krw-agent/host`의 Rust Gateway queue에 넣는다. 같은 `session_id`의 후속 질문은 같은
  채팅방의 새 run으로 이어지고, `agent-v1-outbox`가 durable final을 제품 메시지로 투영한다.
  enqueue 실패도 질문을 저장한 뒤 같은 방에서 재시도하라는 terminal 안내로 끝나므로 영구 pending이
  되지 않는다. SSE/reconnect와 release descriptor/provider readiness는 front runtime이 소유한다.
  상세 계약은 [`AGENT_GATEWAY.md`](AGENT_GATEWAY.md)와 front의
  `src/app/api/chat/run/route.ts`에 있다.

### Evidence, final output, release

- directness/grade 독립성, load-bearing direct premise, global answerability, number/period/unit/calculation
  lineage, counter-signal 없는 inference 차단을 EvidenceLedger와 capability policy가 강제한다. Korean
  `company_research` final은 Flash의 direct Markdown이며, kernel은 raw prose에서 새 claim graph를
  재구성하지 않는다. 대신 exact EvidenceLedger hash/ID receipt를 final bundle과 같은 transaction에 묶는다.
- final answer bundle, message completion, memory delta/frontier, terminal run, billing/presentation outbox는
  `commit_final` 한 transaction으로 저장된다. raw model token은 사용자에게 직접 stream하지 않는다.
- standalone bundle template, secret-free public descriptor, Ed25519 offline keygen/sign/verify,
  expiry/key revocation/minimum sequence rollback guard, live daemon startup authorization이 구현돼 있다.
  서명 authorization은 descriptor, release set, runtime/kernel version, model을 함께 묶는다.

### 과거 로컬 검증 기록 (2026-08-09)

2026-08-09 현재 구현 검증은 `cargo check --workspace --all-targets --locked`와 rustfmt 검사까지
통과했다. GLM-5.3 live admission probe에서 baseline, JSON mode, strict transition-tool input이
모두 수락됐다. TypedJson의 GLM JSON mode는 활성화했고, JSON Schema output은 provider contract
미지원으로 계속 비활성이다. 당시에는 운영 안전 정책상 DeepSeek live 호출을 하지 않았다. 현재
provider-neutral Gateway runner와 dual-provider release gate는 GLM/DeepSeek 각각의 sealed
descriptor를 요구하며, 실제 network acceptance는 운영 자격증명과 승인된 환경에서만 수행한다.

- quality suite manifest의 stale vertical-slice run-request hash를 현재 fixture 바이트와 정합화했다.
- 세 deterministic `quality replay` 케이스는 모두 score 100으로 통과했다.
- quality fixture는 이제 objective/clause 개수를 단일 값으로 고정하지 않고 최대 12개까지 허용하며,
  각 dispatched clause를 의미가 맞는 fixture evidence에 연결한다. 연결되지 않는 objective는
  근거 부족으로 남긴다.
- (2026-08-19 실측 정정) 이전에 기록된 "GLM proposal이 다중 objective를 만든 뒤 canonical
  research-plan contract에서 거절되어 live gate가 아직 green이 아니다"는 실측으로 부정되었다.
  2회 라이브 측정에서 제안/컴파일 거절은 한 번도 발생하지 않았다(3~5개 objective가 무수리·
  무재계획으로 정상 컴파일, engine_error None). 실제 실패 사슬은: v4 제안 계약이
  `document_types` 공란을 허용(`krw-contracts`)하고 라이브 GLM 제안이 실제로 공란으로 제출 →
  planner가 이를 축자로 강등된 SearchPlan에 통과(`research-planner` lowering) → fixture 게이트
  `required_document_type_present`(10-K 요구) 실패로 `quality_fixture_plan_rejected` → 증거
  원장이 비어 `retrieval_empty` 폴백 답변. 수정: lowering 시점에 제안의 `document_types`가
  공란일 때만 커널가 표준 공시 세트 `["10-K","10-Q"]`를 기본값으로 적용한다(모델 명시값은
  그대로 통과). 이는 기간 정책 driver 순서(`agent.yaml`의 `latest_confirmed_10q/10k`)와
  정합하지만 해당 정책은 모델용 프롬프트로만 전달되고 planner에 데이터로 plumbed되지 않아
  문서화된 상수로 반영했다. 계약/스키마/해시는 불변(강등 시점 커널 기본값이며 제안 계약
  변경이 아님). 수정 후 동일 dual-claim 케이스 라이브 1회가 score 100으로 통과했다
  (`fixture_plan_accepted`·`minimum_evidence`·`required_evidence_is_committed` 모두 통과,
  engine_error null, 재계획 0/수리 0, 증거 2건 커밋, `retrieval_empty` 소멸). 남은 한계:
  라이브 확인은 이 케이스 1회뿐이고(나머지는 deterministic replay로만 검증), 기본값이
  planner 내 상수라 향후 기간 정책을 planner에 데이터로 전달하면 단일 진실 공급원으로
  통일해야 한다.

- `cargo test --workspace --no-fail-fast`, strict workspace clippy, rustfmt
- `krw-agent quality replay`는 credential/network 없이 recorded provider turn을 GLM-5.3 snapshot으로
  full kernel에 재생한다. DeepSeek endpoint/credential은 읽지 않는다.
  현재 단일 주장, 두 독립 주장, 그리고 partial ResearchState → trace → 동일 목표의 selective replan
  case가 있다. 마지막 case는 관련 없는 모델 제안을 dispatch 전에 거절하고, 새 근거로 실제 coverage를
  높일 수 있는 append만 실행하는지를 검증한다. 이 gate는 typed provider replay,
  `ResearchIntent → SearchPlan`, root MCP action, ResearchState ingest, direct Markdown output boundary,
  EvidenceLedger receipt, atomic final을 함께
  검증한다. fixture 파일은 raw-byte hash로 suite manifest에 pin된다.
- 8개 image compile/verify 및 semantic evaluation suite
- host-ts typecheck/test
- real temporary PostgreSQL에서 모든 migration과 snapshot/tail/audit-rebuild/fencing 검증
- release CLI의 keygen → sign → verify → version mismatch rejection 및 daemon의 signed startup preflight
- standalone bundle writer/manifest verifier의 exact inventory, tamper, extra file, symlink rejection
- 10k/100k queued pressure, post-tool active-run memory slope, MCP pool, task/FD/heap soak을 포함한 CI
  performance gate
- **실제 ontology TLS transport acceptance (2026-08-03)**: 새 `krw-capabilityd`를 기존 운영 MCP와
  별도 loopback port에서 임시 기동하고, prod immutable release `20260712_021509`을 직접 읽게 했다.
  Rust `McpHttpClient`는 별도 test CA를 `system-plus-pinned-ca-v1`으로 pin한 HTTPS 경로에서
  readiness의 build/schema/release hash, MCP `initialize`, 27개 `tools/list`, AAPL 매출 root
  `SearchPlan`을 모두 통과했다. 응답은 실제 `ResearchState`의 `answerable` 및
  `strong_claim_allowed=true`를 반환했다. 임시 sidecar/proxy는 검증 직후 종료했다. 이 test는
  [`crates/tool-mcp/tests/live_tls_mcp_smoke.rs`](../crates/tool-mcp/tests/live_tls_mcp_smoke.rs)이며,
  `scripts/local_tls_reverse_proxy.py`는 production ingress가 아닌 loopback test 보조 도구다.

`krw-agent quickstart --fixture vertical-slice --root .`는 API key 없이 다음 orchestration 계약도 빠르게
재현한다.

```text
AgentSpec compile
→ durable provider episode
→ action receipt
→ ResearchState fixture ingest
→ direct Markdown output boundary + EvidenceLedger receipt
→ atomic final commit
```

fixture는 implementation regression을 검증할 뿐, live provider/MCP의 품질이나 production 승인 자체를
대체하지 않는다.

## 6. 코드 밖에서만 남은 production admission

아래는 이 repository만으로 만들거나 정직하게 통과시킬 수 없는 운영 환경 증거다. 의도적으로
fixture나 가짜 receipt로 대체하지 않는다.

1. 노출 가능성이 있는 기존 DeepSeek credential revoke/rotate, secret-manager 교체와 redacted scan
2. 실제 production HTTPS MCP endpoint/credential, server build/schema/data-release fingerprint를 deployment registry에 pin
   - loopback Python sidecar는 직접 HTTP로 노출하지 않고, same-origin TLS termination/proxy 뒤의
     `/mcp`와 `/healthz`만 Rust daemon에 등록한다. Rust client의 HTTPS 요구를 낮추지 않는다.
   - real immutable release를 사용한 loopback TLS transport acceptance는 통과했지만, production ingress와
     credential의 admission evidence는 아직 별도다.
3. 새 credential로 exact Flash → live MCP → PostgreSQL atomic final acceptance와 provider/MCP/DB/SIGKILL/
   cancel 경합 fault matrix 실행
4. 같은 모델·질문·tool budget에서 기존 topology 대비 품질/numeric/citation 회귀와 end-to-end latency,
   throughput, process-tree memory 비교
5. 2 vCPU/4 GiB 6시간 churn 및 production topology 7일 soak evidence
6. production `krw-ontology-front` image에 검증된 host package와 outbox worker를 설치하고,
   target DB role·migration·service supervisor를 실제 환경에 적용

`krw-ontology-front`의 로컬/dev wiring은 이미 `/api/chat/run` → pinned host enqueue →
`agent-v1-outbox` → 같은 `session_id`의 후속질문 경로로 연결되어 있다. 다만 위 외부 evidence가
없는 상태에서 production-ready라고 주장하지 않는다.

Front의 `agent-v1-outbox` 컨테이너 healthcheck는 이제 단순 worker 파일 존재가 아니라
최근 성공한 DB/outbox pass heartbeat를 확인한다. projection 실패나 ACK 실패가 발생하면
`degraded` 상태로 닫혀, worker가 살아 있기만 한 상태에서 production admission이 열리지 않는다.
worker가 재시작할 때도 먼저 `degraded/starting` heartbeat를 기록하므로 이전 프로세스의
최근 `ready` 파일을 새 프로세스의 성공으로 오인하지 않는다.

## 7. 수정 방법

KRW 규칙을 바꾸는 정상 경로는 다음과 같다.

```text
agents/krw-ontology/agent.yaml 또는 prompt 수정
→ krw-agent spec check
→ unit/contract/fixture test
→ krw-agent image build
→ krw-agent image verify
→ 새 image hash 배포
```

Rust kernel을 바꿔야 하는 경우는 새로운 auth/evidence semantics, provider/MCP protocol, persistence
invariant처럼 generic ABI 자체가 달라질 때뿐이다. 투자 분석 문구나 기존 evidence/workflow rule 변경은
AgentSpec과 prompt source 수정으로 끝나야 한다.

## 8. 완료 판정 조건

“최종형이 완성됐다”는 다음이 모두 통과했을 때만 말한다.

- live end-to-end 실행과 atomic final
- crash/cancel/retry에서 duplicate visible final 및 divergent action 0
- raw reasoning, secret, cross-principal data leakage 0
- unsupported strong/numeric claim 0
- queued/idle session 수에 비례하는 task/timer/listener 0
- workload의 품질·속도·throughput 비열등과 process-tree memory 개선
- 장기 soak에서 heap/FD/task의 지속 증가 0
- 모든 KRW/Guru workflow의 명시적 AgentSpec과 회귀 fixture

## 9. 현재 안전 차단

DeepSeek live 호출과 production admission은 별도 승인된 환경에서만 수행한다. provider 전환은
`KRW_AGENT_PROVIDER`와 immutable registry를 함께 바꾸는 bounded restart이며, hot switch가 아니다.
새 key는 secret manager 또는 process environment를 통해서만 주입하고 source, image, fixture, log와
debug bundle에는 저장하지 않는다.

## 10. 과거 문서의 사용법

- `EMBEDDED_AGENT_ARCHITECTURE.md`: 경계·불변조건의 역사적 설계 근거
- `SIMULATION_REPORT.md`: Claw Code와 기존 process-tree를 검토한 과거 분석
- `TEST_PLAN.md`: 활용 가능한 fault/performance test 아이디어
- `DEVELOPER_EXPERIENCE.md`: 향후 CLI/DX 아이디어
- `IMPLEMENTATION_PLAN.md`: 폐기된 shared Node/legacy migration 계획

이 문서들 안의 compatibility importer, TypeScript fallback executor, legacy migration, rollback, 단계별
추가 승인과 per-run/startup source compile 표현은 현행 결정이 아니다.
