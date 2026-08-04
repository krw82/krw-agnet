<!-- /autoplan restore point: ~/.gstack/projects/krw-agnet/main-autoplan-restore-20260802-031153.md -->
# KRW Agent Runtime 구현 계획

> **역사적 migration 초안 — 구현 기준으로 사용하지 않음**
>
> 이 문서는 shared Node reference 경로를 검토했던 초기 계획을 보존한 것이다. 사용자의
> 장기 종결형 요구를 반영한 현행 구현 권한은
> [`IMPLEMENTATION_STATUS.md`](./IMPLEMENTATION_STATUS.md)다. 아래의 Node runtime, compatibility
> importer, legacy migration/fallback과 rollback 내용은 구현하지 않는다.

상태: **SUPERSEDED (reference/migration analysis only)**

작성일: 2026-08-02

Canonical source: `~/krw-agnet`

통합 대상: `~/krw-ontology-front`

## 1. 결론

실제로 만들 수 있다. 권장안은 Claude Agent SDK를 다른 거대한 에이전트 프레임워크로
교체하는 것이 아니라, 현재 제품에 필요한 좁은 기능만 독립적인 TypeScript 런타임으로
구현하는 것이다.

선택할 구조는 다음과 같다.

1. `@krw/agent-runtime`은 기존 장수 Node worker 프로세스 안에서 실행한다.
2. DeepSeek Anthropic 호환 API는 공유 `@anthropic-ai/sdk` 클라이언트로 직접 호출한다.
3. MCP 서버 연결, 도구 schema, skill bundle, HTTP 연결은 모든 활성 세션이 공유한다.
4. 각 세션은 실행 중에만 작은 `RunContext`를 갖고, 대기/유휴 상태는 Postgres에만 둔다.
5. 현재 앱의 queue, lease, SSE, cancel, billing, tool/citation persistence는 유지한다.
6. `runAgent(options, writer)` 호환 경계를 먼저 만들고, 실행 종류별 shadow/canary로 바꾼다.
7. 고정 50문항, Guru 30문항, 장기 세션, 장애/부하 평가를 통과하기 전에는 기본 실행기를 바꾸지 않는다.

가장 큰 성능 개선은 프롬프트 미세 최적화가 아니다. 현재 활성 실행마다 생길 수 있는
Claude Code subprocess를 없애고, 한 프로세스 안에서 DeepSeek와 MCP 연결을 공유하는 것이다.

이 프로젝트의 성공은 runtime 구현 자체가 아니다. 동일한 DeepSeek와 동일 질문 집합에서
답변 품질과 근거 정확도를 유지하면서, 동일 머신의 전체 process-tree RSS와 사용자 체감
지연을 낮추고, 새 read-only plugin을 runtime core 수정 없이 추가할 수 있을 때 성공이다.
세 조건은 모두 충족해야 하며 한 조건의 개선으로 다른 조건의 퇴행을 상쇄하지 않는다.

## 2. 확정 전제

- 모델 공급자는 DeepSeek만 사용한다.
- migration 기간의 legacy Agent SDK 경로도 DeepSeek만 사용한다. 문서에서 이 실행기를
  `agent_sdk_legacy`로 부르고, 모델 공급자처럼 보이는 `claude_agent` 명칭은 DB 호환 경계에만 둔다.
- 현재 기본 동작과 같은 `deepseek-v4-flash`를 첫 비교 기준으로 삼는다.
- 플러그인 및 읽기 전용 MCP가 주 사용 형태다.
- 한 컴퓨터에서 수천 개의 저장/대기 세션과 여러 활성 세션이 생길 수 있다.
- 단일 질문뿐 아니라 긴 후속 대화와 Guru sealed workflow도 품질 기준에 포함한다.
- 품질, 인용 정확도, 개인 데이터 경계가 낮아지면 전환하지 않는다.
- 현재 더러운 `krw-ontology-front` worktree는 건드리거나 초기화하지 않는다.

## 3. 현재 기준선과 재사용할 것

### 3.1 실측 방향성 기준

현재 호스트에서 얻은 단일 프로세스 smoke 수치다. 정식 gate 전 Phase 0에서 같은 방법으로
다시 캡처한다.

| 항목 | 방향성 수치 |
|---|---:|
| bare Node 최대 RSS | 약 39.9 MB |
| Agent SDK import 최대 RSS | 약 80.3 MB |
| MCP + Agent SDK import 최대 RSS | 약 104.4 MB |
| Claude native executable 디스크 크기 | 약 226 MB |
| `claude --version` 최대 RSS | 약 197.5 MB |
| 현재 Mac supervisor 동시성 | 8 |

따라서 Node를 Rust로 바꿔 base RSS 수십 MB를 줄이는 것보다 세션별 native process를
제거하는 편이 먼저다.

### 3.2 그대로 유지할 제품 계층

| 기존 기능 | 현재 위치 | 계획 |
|---|---|---|
| 런타임 외부 계약 | `src/lib/agent/runner.ts:234`, `:352` | 호환 adapter 유지 |
| durable job/lease | `src/lib/agent/job-queue.ts:197` | 재사용 |
| 한 프로세스 supervisor | `src/worker/agent-supervisor.ts:52` | 재사용 및 resource-weight 확장 |
| job lifecycle/cancel/billing | `src/lib/agent/job-runner.ts:180` | 재사용 |
| durable SSE/reconnect | `src/lib/agent/run-events.ts:82`, `:135` | 재사용 |
| accumulator/tool/citation callback | `src/lib/agent/stream-handler.ts` | event adapter로 재사용 |
| 표준 MCP client | `src/lib/agent/mcp-client.ts:6` | pool 계층으로 추출 |
| 최근 대화 window | `src/lib/agent/system-prompt.ts:46` | 초기 한도 12개/48K chars 유지 |
| DeepSeek Anthropic gateway | `services/llm-gateway/src/server.ts:16` | migration compatibility 옵션으로 유지 |
| run-kind skill routing | `src/lib/agent/workflow.ts` | 명시적 SkillResolver로 이전 |
| tool permission/Guru guards | `src/lib/agent/runner.ts` | host policy adapter로 이전 |

### 3.3 교체할 숨은 상태

현재 `getSessionInfo`와 `resume`은 Claude 로컬 transcript를 세션 원장처럼 사용한다.
새 런타임에서는 Postgres를 유일한 원장으로 만든다. 최종 user/assistant 메시지만으로
부족한 장기 세션에는 구조화된 session summary와 evidence reference를 추가한다.

## 4. 검토한 대안

| 안 | 설명 | 장점 | 단점 | 결정 |
|---|---|---|---|---|
| A. Agent SDK 얇은 wrapper | 현재 SDK를 유지하고 옵션만 줄임 | 가장 작은 변경 | subprocess와 hidden prompt 의존이 남아 메모리 목표를 못 달성 | 제외 |
| B. Node 라이브러리형 도메인 런타임 | 기존 worker에 `@krw/agent-runtime`을 import | 현재 TS 정책 재사용, 프로세스 공유, 단계 전환 가능 | tool loop와 세션 checkpoint를 직접 정확히 구현해야 함 | **권장** |
| C. Rust daemon 전면 교체 | 별도 native service가 모든 세션 처리 | 낮은 base RSS 가능 | TS 정책/Guru/SSE/DB 중복, IPC와 배포 복잡도, 품질 위험 | v1 제외, profiling 후 재검토 |

권장안 B는 단순한 최소 patch가 아니라 장기 구조다. 다만 배포는 strangler 방식으로 한다.
즉 현재 실행기를 한 번에 제거하지 않고 같은 외부 계약 뒤에서 새 실행기를 실행 종류별로
대체한다.

## 5. 전체 구조

```text
Browser / Desktop
       |
       v
existing API -> Postgres durable queue/events/messages
                        |
                        v
             existing AgentSupervisor
             (one long-lived process)
                        |
                 RuntimeRegistry
                /               \
   agent_sdk_legacy fallback   krw_agent
                                  |
                         @krw/agent-runtime
          +-----------------------+------------------------+
          |                       |                        |
   DeepSeekProvider        FairScheduler             PluginRegistry
   shared HTTP pool       run/model/tool permits     skill snapshots
          |                       |                        |
          +------------ AgentLoop / GuruChildLoop --------+
                                  |
                           ToolRegistry/Policy
                    +-------------+-------------+
                    |                           |
              shared MCP pool             local read tools
                    |
       ontology / feed / filings / guru MCP
```

중요한 경계는 세 개다.

- Runtime core는 Supabase나 Next.js를 모른다.
- Host adapter는 현재 DB, SSE, billing, domain tool policy를 연결한다.
- Plugin은 선언적 skill/MCP/capability만 제공한다. v1에서 임의 JavaScript를 process 안에서 실행하지 않는다.

## 6. 새 저장소 모듈 구조

```text
krw-agnet/
  src/
    index.ts
    contract/          # public input/result/event/error contracts
    runtime/           # AgentLoop, RunContext, cancellation tree
    provider/          # DeepSeek Anthropic adapter, retry classification
    scheduler/         # DRR, weighted permits, AIMD governor
    session/           # transcript/checkpoint/compaction interfaces
    plugins/           # manifest, skill compiler, content-hash snapshot
    tools/             # registry, policy, budget, execution, result envelope
    mcp/               # pooled MCP connections and tool-schema adapter
    context/           # stable-prefix builder and byte budgets
    observability/     # metrics, traces, structured events
    testing/           # fake provider, fake MCP, deterministic clock
  examples/
    read-only-agent/
  benchmarks/
    cases/
    fixtures/
    results/           # gitignored
  docs/
```

초기 public surface는 작게 유지한다.

```text
AgentRuntime.run(request, observer, abortSignal) -> AgentRunResult
Provider.streamMessages(request, abortSignal) -> AsyncIterable<ProviderEvent>
SessionStore.load/checkpoint/compact
PluginSource.snapshot(pluginRoot, runKind)
ToolRegistry.list/execute
RunObserver.onEvent
```

## 7. DeepSeek adapter 결정

공식 DeepSeek Anthropic 호환 endpoint `https://api.deepseek.com/anthropic`와
`@anthropic-ai/sdk`만 사용한다. Agent SDK는 사용하지 않는다. standalone 및 저사양의
기본 배포는 worker가 공식 endpoint를 직접 호출해 별도 gateway 프로세스를 요구하지 않는다.
기존 gateway는 migration compatibility 또는 조직 정책상 필요한 경우에만 선택한다.

### 7.1 필수 규칙

- 현행 제약으로 허용되는 물리 모델은 정확히 `deepseek-v4-flash` 하나다.
- 알 수 없는 모델은 요청 전에 거절한다. DeepSeek의 조용한 flash 자동 매핑을 허용하지 않는다.
- credential/baseURL별 client singleton을 공유한다. session별 client를 만들지 않는다.
- assistant의 `thinking` 및 `tool_use` block은 활성 tool turn 동안 opaque JSON으로 보존한다.
- reasoning이 포함된 tool turn을 일부 필드만 잘라내지 않는다.
- SSE keep-alive comment와 모르는 event는 안전하게 무시하되 원본 event type을 metric으로 센다.
- `metadata.user_id`에는 실제 ID 대신 HMAC-base64url stable ID를 사용한다.
- `mcp_servers`, MCP content block, `tool_result.is_error`, provider citation 지원에 의존하지 않는다.
- tool error는 `{ok:false, code, retryable, message}` content envelope로 보낸다.
- 구조화 결과는 forced `emit_result` tool + local schema validation + 최대 1회 repair를 사용한다.
- `/messages/count_tokens`를 correctness 경로에 넣지 않는다. local 추정치는 admission 용도일 뿐이다.

### 7.2 모델 정책은 평가로 선택한다

현재 비교 기준은 사실상 DeepSeek flash다. 아래 조합을 run kind별로 paired 평가한 뒤
Pareto frontier에 있는 조합만 허용한다.

| 후보 | 예상 용도 |
|---|---|
| flash + thinking high | 일반 리서치 기본 후보 |
| flash + thinking max | 복잡한 일반/Guru 후보 |
| flash + thinking disabled | deterministic router, summary, repair만 후보 |

처음부터 cheap/fast model routing을 켜지 않는다. 동일 run kind에서 품질 gate를 통과한 뒤에만
라우터, summary, repair 순서로 non-thinking을 허용한다. 실제 response의 model을 저장한다.
provider가 fingerprint/header를 제공하면 함께 저장하되 존재를 가정하지 않는다. 미제공 시
release probe의 응답 hash와 고정 canary로 alias 변경을 감지한다.

### 7.3 검증한 공식 사양

- [Anthropic API compatibility](https://api-docs.deepseek.com/guides/anthropic_api/)
- [Thinking mode](https://api-docs.deepseek.com/guides/thinking_mode)
- [Tool calls](https://api-docs.deepseek.com/guides/tool_calls)
- [Context caching](https://api-docs.deepseek.com/guides/kv_cache)
- [Rate limits](https://api-docs.deepseek.com/quick_start/rate_limit) 및
  [error codes](https://api-docs.deepseek.com/quick_start/error_codes)

provider behavior는 학습 데이터나 Claude/Claw 내부 동작을 추정하지 않고 이 사양과 golden
wire fixture를 기준으로 구현한다.

## 8. Agent loop

```text
LOAD DB WINDOW + SESSION SUMMARY
             |
             v
BUILD STABLE PREFIX + DYNAMIC CONTEXT
             |
             v
WAIT MODEL PERMIT -> STREAM DEEPSEEK
             |
       +-----+------+
       | tool calls | no tool calls
       v            v
VALIDATE/POLICY   FINAL CONTRACT CHECK
       |            |
EXECUTE SAFE SET    +--> COMPLETED
       |
CHECKPOINT OPAQUE ASSISTANT + TOOL RESULTS
       |
       +--------------------> next model turn
```

### 8.1 종료 규칙

- no tool call + non-empty final text이면 정상 종료 후보
- 빈 final, open tool call, contract 위반이면 명명된 오류
- schema 결과는 forced tool과 validator를 통과해야 종료
- max model turns, max tool calls, deadline, byte budget 중 하나를 넘으면 즉시 중지
- run kind별 `max_tokens`와 model-visible output byte cap을 baseline p99에서 고정하고 hard cap을 둔다.
  1M context/384K output 한도는 사용 가능한 예산이 아니라 provider 상한일 뿐이다.
- 사용자 취소는 parent와 모든 child/MCP/HTTP AbortSignal에 전파
- reasoning은 사용자 SSE로 내보내지 않음

### 8.2 Guru

generic recursive subagent를 구현하지 않는다. Guru에는 `company_evidence_researcher`라는
하나의 bounded child loop만 둔다.

- parent cancellation/deadline/budget 상속
- `query_context`, `query`, `trace`만 허용
- child of child 금지
- 정확히 한 child가 기본 상한
- sealed brief/hash/question IDs를 host guard가 검증
- child 결과는 현재 ResearchState v2에서 deterministic context로 변환
- main Guru가 직접 ontology filing tool을 호출하지 못하게 차단

### 8.3 durable state machine과 fencing

`queued -> claimed -> model_stream -> tools -> checkpoint -> terminal` 전이는 명시적 상태기계다.

- claim마다 예측 불가능한 `claim_token`과 단조 증가 `claim_version`을 발급한다.
- heartbeat, step checkpoint, tool/event persistence, final message와 terminal transition은 모두
  `run_id + claim_token/version` 조건부 쓰기다. lease를 잃은 process의 늦은 쓰기는 0 row update로 실패한다.
- 다음 model/tool 외부 호출 전에 직전 완결 step을 먼저 checkpoint한다.
- crash 시 `started`만 있고 결과가 없는 tool은 `readOnly && idempotent`일 때만 동일
  invocation key로 재실행한다. 그 외에는 `ambiguous_tool_outcome`으로 종료한다.
- final은 먼저 fenced transaction으로 message/evidence/terminal event를 commit한 뒤 `done`을
  관찰자에게 알린다. terminal state는 compare-and-set으로 정확히 하나만 허용한다.
- 재시작 supervisor는 local state를 신뢰하지 않고 DB lease/ledger를 reconcile한다.

### 8.4 stream parser와 backpressure

- content block index별 `start -> delta* -> stop` 상태를 추적하고 text, thinking,
  tool-use JSON delta를 서로 다른 bounded accumulator로 조립한다.
- duplicate start, stop 없는 block, 잘못된 index, invalid incremental JSON, terminal event 뒤 delta는
  이름 있는 protocol error다. 일부 조립된 tool input을 실행하지 않는다.
- observer는 async backpressure contract다. text delta는 짧은 bounded batch로 durable event에
  합치고, 느리거나 끊긴 browser SSE가 provider socket을 직접 붙잡지 않게 한다.
- 각 block, 전체 response, queued observer event에 byte hard cap을 적용하고 초과 시 abort한다.

## 9. Plugin 및 skill 전략

기존 plugin layout을 깨지 않는다.

- `.claude-plugin/plugin.json` 또는 `.codex-plugin/plugin.json`
- `.mcp.json`
- `skills/<skill>/SKILL.md`
- skill-local `references/`

선택된 run kind의 skill만 읽는다. 모든 plugin 파일을 매 실행에 주입하지 않는다.

추가로 선택적 `krw-agent.lock.json`을 만든다.

```text
plugin content hash
skill entry path
required reference paths
optional reference paths
MCP tool schema hash
capabilities: readOnly / personalData / parallelSafe
size limits
```

lock이 없으면 안전한 호환 모드로 `SKILL.md`만 읽고, path-contained
`read_skill_reference` 도구를 제공한다. lock이 있으면 필수 reference를 stable prefix에
결정적 순서로 합친다. path traversal, plugin root 밖 symlink, hash mismatch는 실행 전에 거절한다.

`krw-agent plugin doctor <path>`와 conformance suite를 제공한다. 지원 manifest, MCP transport,
auth scope, skill/reference resolution, capability, tool name conflict precedence를 compatibility
matrix로 고정한다. 대표 기존 plugin 하나가 runtime core 수정 없이 30분 이내 연결되어
conformance와 live read-only smoke를 통과해야 확장성 gate를 만족한다.

현재 한국어 research `SKILL.md`는 약 51 KB이고 reference 합계는 약 92 KB다.
정확히 선택된 bundle만 content hash로 캐시해야 한다.

## 10. 다중 세션 스케줄링

대기열 전체를 process heap으로 읽지 않는다. Postgres가 queued/idle 원장을 유지하고
process는 active run만 가진다.

### 10.1 durable admission과 active-step scheduling 분리

공정성은 runtime이 이미 FIFO로 claim한 작업을 재정렬하는 방식으로 구현하지 않는다.

- DB claim은 permit이 실제로 있을 때만 수행하고, effective priority, wait aging,
  user/session active count를 반영한 fair candidate query로 admission을 결정한다.
- enqueue 때 immutable `resource_class`와 conservative `resource_units`를 계산해 row에 저장한다.
  claim RPC는 worker의 available units 이하인 candidate만 고르고, global limit도 run count가 아니라
  running units 합으로 검사한다. claim 결과를 받은 뒤 추가 permit을 기다리지 않는다.
- claim 뒤 process에서 오래 대기시키는 prefetch queue는 기본값 0이며, 필요해도 model permit의
  작은 배수와 절대 상한을 둔다.
- model/tool 단계 사이의 공정성은 아래 DRR이 담당한다.
- DB lease와 process permit의 순서를 고정해 deadlock과 lease를 잡은 채 대기하는 상태를 막는다.
  순서는 `local unit reservation -> fenced DB claim -> run`이며 claim 실패 시 reservation을 즉시 반환한다.
- 여러 worker가 동시에 claim해도 global unit limit을 넘지 않도록 env별 capacity row를
  `FOR UPDATE`로 잠그고 claim+unit reservation을 한 transaction에서 처리한다. terminal/expiry는
  같은 claim token으로 unit을 한 번만 반환하고, reconciliation은 ledger 합과 running row를 대조한다.

### 10.2 계층형 Deficit Round Robin

공정성 단위는 `priority class -> user -> session`이다.

- interactive가 background보다 높은 quantum을 받는다.
- 같은 class 안에서는 user별 DRR로 한 사용자의 대량 세션 독점을 막는다.
- 오래 기다린 작업은 aging으로 우선순위가 천천히 올라간다.
- session당 active turn은 1개다.
- model/tool step마다 permit을 다시 얻어 긴 run이 모든 연결을 독점하지 못한다.

### 10.3 세 가지 별도 permit

| Permit | 보호 대상 | 초기 정책 |
|---|---|---|
| Run admission | local working bytes | normal 1 unit, Guru 2~3 units |
| Model stream | DeepSeek/HTTP 및 활성 transcript | adaptive global limit |
| Tool server | MCP별 capacity | server별 semaphore |

단순 job count만 보지 않고 예상 prompt/result byte를 weighted semaphore에 반영한다.

### 10.4 AIMD governor

초기값은 저사양에 안전하게 둔다. 안정 window에서 한 slot씩 올리고, 다음 신호가 생기면
곱셈 감소한다.

- 429/503 burst
- provider p95 상승
- event-loop delay 상승
- RSS 또는 heap budget 접근
- MCP timeout 증가

환경 변수 hard max가 항상 상한이다. 자동 조정은 상한을 넘지 않는다.
fixed conservative semaphore가 항상 safe fallback이며 DRR/AIMD에는 독립 off switch가 있다.
Phase 0 replay에서 고정 동시성의 starvation, quota pressure 또는 resource oscillation을 먼저
측정하고, feature flag로 켠 뒤 단순 fallback보다 낫다는 것을 증명한다.

### 10.5 timer/queue 메모리와 용량 약속

- session별 timer를 만들지 않는다.
- deadline은 중앙 min-heap 하나로 관리한다.
- DB LISTEN/NOTIFY와 기존 reconciliation timer를 유지한다.
- 10,000 queued session을 넣어도 process 객체 10,000개를 만들지 않는다.
- “대량 세션”은 queued/idle density와 active execution capacity를 분리해 보고한다.
  4/8/16 GB profile마다 저장/대기 session 수, 동시 model stream, tool concurrency,
  sustained jobs/min, p99 queue wait, user별 fairness와 starvation 0을 capacity matrix로 공개한다.

## 11. MCP, cache, tool 결과 알고리즘

### 11.1 연결

- public ontology/feed는 장수 connection 1개부터 시작한다.
- transport가 안전한 동시 호출을 보장하지 못하면 작은 bounded pool로 확장한다.
- personal tool은 auth scope별 bounded LRU+TTL pool이며 절대 사용자 간 공유하지 않는다.
- MCP tool-session reuse는 deployment binding schema v2의 required
  `run-scoped | attested-stateless-v1` enum으로 선언한다. 미선언 binding은 startup에서 거부한다.
  `run-scoped`는 auth scope와 무관하게 tenant+principal+run partition을 강제하고,
  `attested-stateless-v1`은 readiness와 initialize의 versioned JCS SHA-256 attestation이 정확히
  일치할 때만 auth-scope pooling을 허용한다. pool key에는 server/config hash, privacy partition,
  credential version, release를 포함하고 raw token/cookie는 포함하지 않는다.
- credential rotation/logout 시 이전 version entry는 새 checkout을 막고 active call이 끝나면
  drain/close한다. stateful session/cookie/notification은 isolation scope 밖으로 전달하지 않는다.
- readiness 및 tool schema는 TTL + release fingerprint로 캐시한다.

### 11.2 실행

- 같은 assistant turn에 나온 tool call만 병렬 후보다.
- `readOnly && parallelSafe`가 둘 다 true일 때만 병렬 실행한다.
- 자동 재시도는 `readOnly && idempotent`가 모두 true인 tool만 허용한다.
- 결과를 provider에 돌려주는 순서는 원래 tool call 순서를 보존한다.
- per-server semaphore와 전체 tool budget을 동시에 적용한다.
- 동일 canonical request는 singleflight로 합친다.

### 11.3 cache

entry count가 아니라 byte 기준 LRU를 사용한다.

```text
cache key = plugin/tool + canonical args + release fingerprint + auth scope
```

- immutable ontology 결과는 release 변경 전까지 재사용 가능
- current news는 짧은 TTL
- personal 결과는 cross-user cache 금지
- cache 총량 기본 32 MiB, 4 GB host 기준 configurable hard cap
- prefix는 system -> common policy -> tool schemas -> selected skill -> dynamic context 순서
- object key/tool ordering을 결정적으로 유지해 DeepSeek prefix cache 적중을 높인다.

### 11.4 큰 tool 결과

- model-visible 일반 결과 목표 64 KiB
- filing 특례 목표 128 KiB
- raw hard cap 256 KiB
- 초과 원문은 host blob/DB에 저장하고 digest, evidence IDs, result ref만 prompt에 유지
- 필요한 경우 bounded `read_tool_result_chunk`로 특정 부분만 재조회
- 문자열 chunk는 array에 모은 뒤 한 번 join해 quadratic concat을 피한다.

## 12. 세션과 context

### 12.1 메모리 원칙

- idle/queued session object 수: 0
- active run만 message window와 ledger를 load
- `finally`에서 모든 listener, timer, buffer, MCP checkout을 해제
- tool raw result는 여러 복사본을 만들지 않는다.
- admission memory는 prompt 원문뿐 아니라 UTF-8/JSON 직렬화 복사본, opaque provider blocks,
  queued observer bytes, inline tool 결과와 SDK buffering의 예측치를 포함한다.
- soft watermark에서 새 claim과 cache fill을 중단하고 permit을 줄인다. hard watermark에서는
  새 외부 호출을 막고 가장 안전한 완결 경계에서 실행을 종료한 뒤 supervisor restart를 허용한다.

기본은 저메모리 장수 process 하나다. 이 process가 죽으면 모든 active run이 영향을 받는 대신
fenced checkpoint와 lease recovery로 silent corruption 없이 복구한다. CPU/event-loop profile에서
단일 process가 병목임이 확인될 때만 세션별 process가 아닌 고정 2~N worker shard를 허용한다.
shard 수와 총 cache/permit budget은 host 단위 hard cap을 나누어 사용한다.

### 12.2 active turn checkpoint

DeepSeek tool reasoning을 활성 turn 도중 정확히 보존하기 위해 임시 step ledger가 필요하다.

```text
run_id + step_no
opaque assistant content blocks
tool result envelope/ref
provider model/fingerprint
prompt/plugin/tool hashes
created_at + retention deadline
```

이 테이블은 service role만 접근하고 짧은 TTL을 가진다. 완료된 turn 경계에서는 최종 답변,
구조화 요약, evidence refs로 compact한다. 과거 tool reasoning 일부만 잘라 보내지 않는다.

### 12.3 장기 세션

- 초기에는 현재와 같은 최근 12개/48K chars를 유지한다.
- 한도 초과 시 완결된 user turn 경계에서만 compact한다.
- summary는 flash non-thinking 후보지만 evidence ID와 entity/기간/미해결 질문 schema를 검증한다.
- summary 실패 시 원본 최근 window로 fallback하고 조용히 정보 손실을 만들지 않는다.
- 1턴, 12턴, 50턴 품질 gate를 각각 통과해야 한다.

### 12.4 legacy session 이관

- DB에 충분한 final message, evidence state, 구조화 summary가 있는 완결 turn에서만
  `agent_sdk_legacy` 세션을 `krw_agent`로 전환한다.
- Claude local transcript에만 존재하는 상태가 필요한 세션은 legacy executor에 고정하거나
  검증 가능한 rehydrate job으로 DB canonical state를 만든 뒤 전환한다.
- 실행 중 turn, 미완결 tool call, opaque reasoning ledger는 executor 사이에서 이동하지 않는다.
- sticky assignment와 runtime version을 session에 저장하고, 사용자에게 보이는 답변 도중에는
  executor를 바꾸지 않는다.

## 13. 보안 경계

- v1 plugin은 read-only가 기본이며 capability allowlist 없이는 tool이 노출되지 않는다.
- arbitrary shell, filesystem write, arbitrary plugin JavaScript는 제공하지 않는다.
- 모든 tool input은 JSON schema와 host domain policy를 모두 통과해야 한다.
- user text, URL, retrieved text, attachment text는 untrusted data다.
- plugin path는 canonical realpath가 root 안인지 검사한다.
- API key는 env/secret store에만 두고 log/event에 넣지 않는다.
- DeepSeek `user_id`는 HMAC 값이며 PII를 포함하지 않는다.
- cache, singleflight, connection pool key에 auth scope를 포함한다.
- personal filing tool의 cross-user 결과 재사용은 critical violation으로 취급한다.
- thinking/reasoning block은 user-visible SSE, analytics, 일반 debug log에 넣지 않는다.

## 14. 오류 및 복구 registry

| Codepath | Failure | Retry | Runtime action | 사용자 영향 |
|---|---|---:|---|---|
| provider request | invalid model/400/422 | N | 요청 전 whitelist, named failure | 설정 오류 메시지 |
| provider auth/balance | 401/402 | N | circuit open, operator alert | 일시 이용 불가 |
| provider pre-token | 429/500/503 | 제한적 | full-jitter backoff, AIMD decrease | 대기 후 투명 재시도 |
| provider partial stream | disconnect after visible content/tool | N blind retry | checkpoint 상태로 fail/resume 판단 | 명시적 재시도 안내 |
| provider stream | 10분 inference 미시작/timeout | N after deadline | abort tree | 시간 초과 |
| provider payload | unknown SSE event | N | ignore + metric; required block 누락은 fail | silent corruption 금지 |
| tool args | malformed/unknown field | correction 1회 | execute 금지, structured correction | 보통 노출 없음 |
| tool policy | unauthorized tool | N | deny + security event | 안전한 범위 답변 |
| MCP public | disconnect before read result | 1회 | reconnect + idempotent retry | 잠시 지연 |
| MCP personal | auth failure | N | pool entry 폐기, no cross-user fallback | 개인 자료 제외 안내 |
| MCP tool | timeout/429/5xx | bounded | per-server breaker/backoff | 근거 범위 축소 |
| tool result | oversized | N | offload + digest/ref | 필요한 부분만 추가 조회 |
| checkpoint | DB write failure | bounded before next step | 다음 model turn 금지 | 실행 실패, 재시도 가능 |
| context | hard byte/token estimate 초과 | compact 1회 | whole-turn compaction | 정보 손실 검증 |
| contract | empty final/invalid structured result | repair 1회 | then named failure | 불완전 답변 미노출 |
| process | RSS/event-loop pressure | N | stop claims, reduce permits | queue 대기 증가 |
| user | cancel | N | cascade AbortSignal, close open calls | 즉시 취소 상태 |
| lease | heartbeat lost | N | abort and stop persistence | 중복 실행 방지 |

모든 row는 unit 또는 integration test를 갖는다. `catch (Error) -> log and continue` 형태로
오류를 삼키지 않는다.

### 14.1 crash-point recovery matrix

| 마지막 durable 경계 | 자동 복구 |
|---|---|
| provider 요청 전 | 새 attempt로 안전하게 재호출 |
| 요청 후 첫 event 전 | 중복 provider 비용 가능성을 기록하고 bounded retry |
| user-visible text delta 이후, final 전 | 같은 run에서 blind replay 금지; partial terminal error로 종료 |
| 완결 tool-use block 전 | tool 실행 금지, protocol failure |
| tool started, result 미저장 | read-only+idempotent만 동일 invocation key로 재조회; 나머지는 ambiguous failure |
| tool result fenced checkpoint 후 | opaque assistant block+result를 replay해 다음 model step부터 재개 가능 |
| final fenced transaction 후, done 관찰 전 | DB terminal을 재전달하고 model 재호출 금지 |

heartbeat는 DB 시간과 충분한 safety margin을 사용한다. event-loop stall로 갱신 window를 놓치면
즉시 abort하고, fencing token이 이후의 모든 저장을 차단한다. 위 각 행에 process kill,
network reset, delayed write를 주입하는 deterministic integration test를 둔다.

## 15. 관측성

각 run에 다음 hash와 지표를 저장한다.

- runtime version, executor, run kind
- provider model, system fingerprint
- system prompt, plugin, selected skill, tool schema hash
- queue wait, provider TTFT, provider total, MCP total, orchestration overhead
- model/tool turn count, retry count, cache hit/miss tokens
- input/output/cache token과 성공 답변당 provider 비용, shadow 전용 비용
- tool result bytes inline/offloaded
- process RSS, heap/external, event-loop delay, FD/socket/child count
- terminal error code와 rollback trigger

reasoning 본문과 secret/tool raw 개인 결과는 metric에 넣지 않는다.

## 16. 테스트와 비열등성 gate

### 16.1 평가 세트

- 익명화한 실제 workload replay: prompt/result bytes, model/tool turns, MCP latency,
  burst concurrency, active/idle 비율과 run-kind 분포
- 일반 고정 50Q, 각 최소 3회
- Guru gold 30Q, 각 최소 2회
- Agent SDK gold 4건
- 1턴/12턴/50턴 후속 대화
- 취소, timeout, malformed args, MCP 429/5xx, 100KB/256KB result
- unsafe tool, internal leak, cross-user personal filing
- model alias/fingerprint 변경 canary

현재 50Q script는 절대 경로, 약한 regex grade, 순차 실행만 있으므로 질문 세트만 가져오고
새 harness에서 runner와 judge를 다시 만든다.

### 16.2 채점

1. deterministic contract: citation/evidence ID, 숫자 근거, ResearchState, open tool 0,
   unsafe tool 0, 내부 용어 0
2. DeepSeek blind A/B judge: runtime 이름 제거, 좌우 순서 무작위화
3. 사전 정의한 domain rubric으로 무작위 human blind holdout을 항상 수행하고,
   judge 불일치와 critical case는 전수 사람 blind review
4. paired bootstrap으로 `candidate - baseline` 단측 95% CI 계산

baseline threshold, rubric, release/plugin/prompt/tool hash와 workload trace는 candidate 결과를
보기 전에 immutable artifact로 동결한다. 같은 모델, 같은 release fingerprint, 같은 trace에서
legacy와 candidate를 비교한다.

### 16.3 품질 release gate

- critical regression 0
- 일반/Guru 필수 contract 100% 통과
- 종합 품질 차이의 paired bootstrap 단측 95% 하한 >= 0/100
- 인용 정확도, 숫자 정확도, 개인 데이터 경계 margin = 0
- 장기 세션 gate 별도 통과
- Guru sealed workflow 위반 0

데이터 부족과 통계적 불확실성은 통과가 아니라 보류다. 인용, 숫자, personal-data boundary,
Guru sealed workflow는 독립 conjunctive gate이며 종합 평균으로 상쇄하지 않는다.

### 16.4 성능/메모리 gate

| 지표 | 목표 |
|---|---:|
| standalone idle RSS | <= 128 MiB |
| normal active working set hard budget | <= 16 MiB/run |
| active normal heap slope p95 | <= 12 MiB/run |
| 10,000 idle/queued session heap 증가 | < 10 MiB after GC |
| cache 기본 hard cap | <= 32 MiB on 4 GB host |
| warm claim -> provider dispatch p95 | <= 250 ms |
| event-loop delay p99 | < 100 ms |
| orchestration p95 | baseline 대비 >= 20% 개선 |
| concurrency 8 process-tree RSS | baseline 대비 >= 50% 감소 목표 |
| 6시간 soak RSS | warm baseline 대비 <= 5% 증가 |
| orphan child/FD slope | 0 |
| end-to-end p95 | baseline 이하 |
| TTFT p50/p95 | baseline 이하 |
| sustained jobs/min | 동일 host/workload에서 baseline 이상 |
| p99 queue wait/fairness | starvation 0, 사전 고정 SLO 통과 |
| 성공 답변당 token/API cost | baseline 이하 또는 사전 승인된 hard budget 이내 |

2 vCPU/4 GB 기본값은 normal 2 active unit 또는 Guru 1개다. 더 높은 동시성은 측정 후
AIMD가 올리며 hard max를 넘지 않는다.

baseline은 legacy worker와 모든 Claude child/gateway를 포함하고, candidate는 worker와 실제
배포에 필수인 모든 gateway/MCP child를 포함한 process tree 전체를 측정한다.
`claude --version` smoke 수치는 가설 근거일 뿐 release 판정 근거로 사용하지 않는다.

## 17. 구현 단계

### Phase 0. 기준선 동결 및 contract 추출

Deliverables:

- 현재 Agent SDK + DeepSeek flash 결과와 process-tree profile 저장
- 익명화된 실제 workload profile과 replay fixture 동결
- fixed-50Q/Guru/장기 세션/장애 benchmark harness
- current SSE, tool, citation, error golden contract
- tool 선택/미선택, malformed args correction, 불충분 근거 재탐색, instruction precedence,
  partial/empty stream, citation linkage, compaction, Guru sealed flow의 behavioral contract map
- prompt/plugin/tool hash capture

Exit: 재현 가능한 baseline artifact가 있고 같은 release에서 paired 실행 가능.

### Phase 1. 독립 core와 provider

Deliverables:

- package/tooling, public contracts, fake provider/clock
- DeepSeek Anthropic singleton adapter
- streaming text/thinking/tool block golden tests
- no-tool request, cancel, timeout, retry classification

Exit: mock 10,000 session queue와 live no-tool smoke 통과.

### Phase 2. Plugin, MCP, tool loop

Deliverables:

- plugin snapshot/skill resolver/optional lock compiler
- pooled MCP client, tool schema adapter
- tool policy, budget reservation, parallel-safe execution
- byte LRU, singleflight, offload envelope

Exit: 일반 read-only research fixture와 MCP fault tests 통과.

### Phase 3. Session, scheduler, memory governor

Deliverables:

- active step checkpoint/turn compaction interface
- hierarchical DRR, weighted permits, AIMD
- DB fair-admission query와 DRR/AIMD feature flags 및 fixed-semaphore fallback
- central deadline heap, leak/soak instrumentation
- 1/12/50-turn session tests

Exit: 2-core/4-GB profile와 memory gates 통과.

### Phase 4. `krw-ontology-front` 호환 adapter

Deliverables:

- executor type `krw_agent`
- `claim_token/version`, `resource_class/units`, fenced checkpoint/terminal RPC와 fair claim migration
- `RuntimeRegistry.run(executor, options, writer)`
- existing accumulator/SSE/tool/citation callback adapter
- `KRW_AGENT_RUNTIME_MODE=legacy|shadow|krw` kill switch
- session-sticky runtime assignment
- legacy session eligibility/rehydrate 검사

Exit: 기존 runner contract tests가 두 executor에서 통과.

### Phase 5. 일반 run kind 순차 이전

순서:

1. ambiguous router와 no-tool helper
2. company research
3. news/market move/filing follow-up
4. scenario/idea generation
5. visualization/local tools

Exit: 각 run kind가 따로 offline gate와 internal canary를 통과.

### Phase 6. Guru bounded child loop

Deliverables:

- sealed brief -> bounded company evidence child -> review -> final flow
- hash/question/source linkage guards
- Guru 30Q와 current runner regression suite port

Exit: Guru critical violation 0, quality non-inferiority, memory gate 통과.

### Phase 7. shadow/canary/제거

Rollout:

```text
offline paired
  -> read-only shadow 1%
  -> internal canary 1%
  -> 5% -> 25% -> 50% -> 100%
  -> 7일 이상 soak
  -> Agent SDK dependency 제거
```

shadow 결과는 user SSE/messages/billing에 반영하지 않는다. 쓰기성/개인 tool은 shadow를
금지한다. shadow는 production model/tool permit과 분리된 hard token/cost budget을 쓰고,
quota pressure, 429 또는 latency 상승 시 가장 먼저 중지한다. Guru는 일반 research가 안정된
뒤 별도로 rollout한다.

자동 rollback 조건:

- critical evidence/security 위반 1건
- error rate +1%p
- p95 10% 이상 악화가 15분 지속
- RSS/FD hard limit 초과
- model fingerprint 변경 후 canary 실패

Agent SDK 제거 전 rollback target은 `agent_sdk_legacy`다. 제거 후에는 legacy를 되살리지 않고
직전 검증된 `krw_agent` package/version과 plugin lock으로 롤백한다. 최소 두 개의 검증된
artifact를 보관하고 DB schema는 해당 버전들과 호환되게 유지한다.

## 18. 현재 앱에서 예정된 변경점

승인 전에는 아래 파일을 수정하지 않는다.

| 변경 | 위치 |
|---|---|
| executor union에 `krw_agent` 추가 | `src/types/api-contracts.ts`, DB migration |
| hardcoded Claude executor 제거 | `src/app/api/chat/run/route.ts`, `src/lib/agent/job-runner.ts`, `src/lib/agent/job-queue.ts` |
| runtime registry 연결 | `src/lib/agent/job-runner.ts:327` |
| 기존 runner를 fallback adapter로 유지 | `src/lib/agent/runner.ts` |
| MCP pool host adapter | `src/lib/agent/mcp-client.ts` |
| SDK helper local tool 변환 | question router, notebook refresh, Yahoo, visualization |
| deploy/preflight에서 runtime package pin | worker build/deploy scripts |
| ccSwitch/base URL/key 충돌 탐지와 DeepSeek 전용 migration | worker env/preflight scripts |

production은 `file:../krw-agnet`에 의존하지 않는다. 새 저장소에서 versioned tarball 또는
private package를 만들고 checksum/commit을 pin한다. local development만 workspace/file link를
허용한다.

runtime은 ccSwitch에 의존하지 않는다. 기존 Anthropic-compatible base URL/key는 명시적
migration command로 한 번만 DeepSeek 전용 설정으로 변환한다. 동시에 여러 key/base URL,
지원하지 않는 model alias, gateway/direct 모드 충돌이 있으면 startup preflight에서 문제,
감지된 값의 출처, 정확한 수정 명령을 포함한 actionable error로 거절한다.

## 19. 병렬 구현 lane

Phase 0과 public contract가 먼저다. 그 뒤 다음 세 lane을 병렬화할 수 있다.

| Lane | 작업 | 선행 조건 |
|---|---|---|
| A | provider, stream parser, AgentLoop | Phase 0 contract |
| B | benchmark harness, fixtures, blind judge | Phase 0 baseline |
| C | plugin compiler, MCP pool, tool registry | public tool contract |
| D | host RuntimeRegistry/DB migration | A의 public interface |
| E | Guru workflow | A+C+일반 research gate |

A+B+C를 병렬 실행하고 합친 뒤 D, 마지막에 E를 진행한다. 현재 앱의 `runner.ts`를 여러 lane이
동시에 수정하지 않게 host adapter 변경은 D 한 lane이 소유한다.

## 20. 개발자 경험 계획

대상 개발자는 이 private runtime을 현재 KRW worker에 연결하고 plugin을 운영하는 maintainer다.

### 20.1 하나의 golden path와 TTHW

```text
npm ci
npm run smoke:mock
# .env.local에 DEEPSEEK_API_KEY 설정
npm run smoke:deepseek -- --mcp examples/read-only-agent/mcp.json
```

- checkout 이후 `smoke:mock`의 첫 `ANSWER_OK`까지 API key 없이 <= 2분
- key 설정 이후 실제 DeepSeek+read-only MCP의 `TOOL_ANSWER_OK`까지 <= 5분
- 선택 질문 없이 위 한 경로가 README 첫 화면에 있고 모든 command는 copy/paste 가능
- smoke output은 runtime/model/tool hash, TTFT, total time, peak RSS와 다음 command를 출력하되
  prompt, reasoning, secret, 개인 tool 원문은 출력하지 않음
- `examples/read-only-agent`는 cancel, timeout, tool error를 같은 작은 API로 보여주는 executable test

### 20.2 API와 CLI

primary API는 `createKrwAgentRuntime(config).run(request, observer, signal)` 하나다. in-memory
session/fake provider 기본값으로 시작하고, production host만 `SessionStore`, scheduler,
observability adapter를 주입한다. `RunId`, `SessionId`, `ToolCallId`는 branded type이며 error는
throw/string 혼합이 아니라 discriminated `KrwAgentError`로 통일한다.

```text
krw-agent doctor                 # provider/env/runtime preflight
krw-agent plugin doctor <path>   # manifest/tool/auth/isolation conformance
krw-agent migrate --check        # host/checkpoint/plugin schema compatibility
krw-agent bench replay <trace>   # baseline/candidate local replay
```

각 CLI는 terminal에서는 읽기 쉬운 표, pipe에서는 stable JSON을 출력하고 exit code 계약을 둔다.
mutation성 migration은 `--check`와 diff가 기본이고 명시적 `--apply` 없이는 쓰지 않는다.

### 20.3 오류와 debugging

모든 public error는 `code, category, retryable, message, cause, fix, docsUrl, safeDetails,
runId`를 가진다. 첫 줄에 가장 실행 가능한 수정법을 보여주고, 실제 충돌 model/base URL,
plugin path/line 또는 schema field를 redaction 후 포함한다. `krw-agent explain <code>`는
versioned error catalog와 최소 재현 fixture를 연다.

`debug` bundle은 config/plugin/tool/prompt의 hash, state transition, timing, byte budget,
fencing version만 포함한다. prompt 본문, secret, reasoning, raw personal result는 어떤 level에서도
수집하지 않는다. fake clock/provider/MCP로 같은 run ID를 재생할 수 있어야 한다.

### 20.4 docs, versioning, migration

- Quickstart, core concepts, plugin authoring, host integration, concurrency/memory tuning,
  error catalog, ccSwitch migration, rollback/runbook, architecture decision records를 repo에 둔다.
- README와 docs의 모든 command/snippet을 CI에서 실제 실행한다. docs가 없는 public feature는 release하지 않는다.
- runtime API, checkpoint schema, plugin lock, host adapter compatibility를 각각 versioning한다.
- semver, CHANGELOG, migration guide, deprecation policy와 `migrate --check`를 첫 release부터 둔다.
- breaking upgrade는 작은 idempotent migration pipeline으로 만들고 dry-run/rollback fixture를 제공한다.
- 검증된 runtime package와 plugin artifact 두 버전을 유지해 downgrade compatibility를 CI에서 검사한다.

### 20.5 환경, 생태계, 측정

- 재현 가능한 lockfile과 Node 22를 기준으로 macOS/Linux arm64/x64를 CI한다. Windows와
  외부 public ecosystem 지원은 parity 이후 별도 결정이며 v1 약속이 아니다.
- `npm run check:fast`는 unit/type/lint 핵심을 <= 60초, `npm run test:watch`는 변경 관련 test만 실행한다.
- private 생태계의 성공은 대표 KRW plugin compatibility matrix, template, conformance fixture,
  release note와 30분 plugin integration acceptance로 측정한다.
- CI artifact에 install/TTHW, test feedback time, plugin integration time, error-to-fix time을 기록한다.
  분기별 maintainer survey로 cognitive load와 만족도를 확인하며 opt-in 없는 개발자 telemetry는 보내지 않는다.

## 21. 일정과 완료 정의

한 명의 숙련 엔지니어와 Codex 기준의 계획 수립용 예상이다. 코딩 속도보다 실제 workload
replay, live 평가, provider 변동성과 soak가 calendar를 결정하므로 날짜 약속이 아니라 각
milestone의 go/no-go gate를 우선한다.

| 결과 | 예상 |
|---|---|
| baseline + no-tool/provider core | 3~5 working days |
| 일반 read-only research candidate | 5~10 working days |
| 모든 일반 run kind | 2~4 weeks |
| full Guru + long-session parity | 3~6 weeks |
| production 100% + SDK 제거 | gate 통과 후 최소 7-day soak |

완료는 코드가 실행되는 시점이 아니다. 다음이 모두 참일 때다.

- fixed-50Q/Guru/장기 세션 quality gate 통과
- 저사양 load/soak/memory gate 통과
- 100% canary에서 자동 rollback 조건 미발생
- Agent SDK fallback 없이 7일 안정 운영
- rollback/runbook/package pin/문서 완료

## 22. NOT in scope

- Claude Code/Claw Code의 코딩-agent 전체 기능 복제
- Bash, 임의 filesystem write, arbitrary code execution
- v1 Rust rewrite
- v1 다중 provider 추상화의 일반 공개 플랫폼화
- UI/대시보드 재설계
- DeepSeek beta strict tool mode 의존
- 품질 gate 없이 flash/pro 자동 model roulette
- 현재 durable queue/SSE/billing의 전면 재작성

## 23. Rollback flow

```text
metric/contract alarm
        |
        v
freeze new krw_agent assignments
        |
        +--> active safe runs finish
        |        or abort on critical security event
        v
session-sticky fallback to agent_sdk_legacy at next turn boundary
        |
        v
preserve candidate trace/hash -> incident review -> fix -> offline gate
```

DB schema는 additive로 배포하고 Agent SDK 제거 전까지 rollback-compatible하게 유지한다.
제거 후에는 직전 검증된 `krw_agent` package/plugin artifact로 같은 흐름을 수행한다.

## 24. 승인 gate

권장 승인안:

- TypeScript/Node library-first
- existing worker/DB/SSE 재사용
- DeepSeek Anthropic API + standard MCP 직접 loop
- 일반 research 먼저, Guru 마지막
- 품질 비열등성과 memory gate를 통과한 run kind만 전환

승인되면 Phase 0부터 구현한다. 승인 전에는 현재 앱의 source/deploy/database를 바꾸지 않는다.

## GSTACK REVIEW REPORT

| Review | 결과 | 반영 사항 |
|---|---|---|
| CEO / scope | PASS, 8/10 -> 10/10 | 성공 정의, 실제 workload replay, 엄격한 0-margin 품질 gate, 세션 이관, 비용/처리량, 제거 후 rollback |
| Engineering | PASS, critical gap 3 -> 0 | claim fencing, stream state machine, crash matrix, atomic capacity ledger, MCP auth isolation, memory pressure recovery |
| Product design | SKIPPED | backend/private library/CLI 계획이며 제품 UI 변경 없음 |
| Developer experience | PASS, plan 5/10 -> 9/10 | 2분 mock/5분 live golden path, 작은 API, actionable errors, docs/migration/measurement |

- Review mode: `SCOPE_EXPANSION`; 범용 coding agent 확장은 제외하고 KRW read-only runtime의 완성도를 확장했다.
- Outside voice: 독립 CEO 리뷰 완료, 독립 engineering 리뷰의 P0 findings 반영. DX outside subreview는 시간 제한으로 중단하고 main review가 8개 reference pass를 직접 완료했다.
- Restore point: `~/.gstack/projects/krw-agnet/restore-points/2026-08-02-pre-review-implementation-plan.md`
- Unresolved technical decisions: 0. 모델 정책과 capacity 숫자는 추측으로 고정하지 않고 Phase 0 측정 결과로 선택한다.
- Implementation approval: pending. 현재 앱 source, deploy, DB에는 변경이 없다.
- Plan completeness: **10/10**. 구현 완료도가 아니라 구현·검증·롤백 경로의 계획 완성도 점수다.
