# KRW Embedded Agent 방향성 시뮬레이션 보고서

> **SUPERSEDED AS AUTHORITY — 역사적 분석 자료**
>
> Claw Code와 기존 process-tree에 대한 관찰 및 성능 가설을 보존한다. 현재 구현 결정과 완료 상태는
> [`IMPLEMENTATION_STATUS.md`](./IMPLEMENTATION_STATUS.md)를 따른다. 아래의 TypeScript comparison/fallback,
> migration/rollback 또는 phase gate 제안은 현행 구현 권한이 아니다.

작성일: 2026-08-02  
상태: 역사적 설계 검증 자료, release benchmark 아님

## 1. 결론

시뮬레이션은 세 가지를 지지한다.

1. 가장 큰 1차 이득은 실행별 Claude Code/Agent SDK subprocess 제거에서 나온다.
2. 대량 대기 세션과 활성 실행은 완전히 다른 용량 문제다. 대기 세션은 DB row로만 존재해야 한다.
3. 최종 목표가 한 머신의 다수 client와 수십 개 active run이라면, machine-wide 공유 daemon이
   in-process Node library보다 캐시, 연결, 메모리 상한을 한 곳에서 통제하기 쉽다.

다만 아래 숫자는 가정에 민감하다. Rust daemon 전환은 믿음으로 통과시키지 않는다. 동일한
workload trace에서 품질 비열등성, process-tree memory, latency, throughput을 모두 통과해야 한다.

## 2. 공개 Claw Code 소스 점검

점검 기준 commit은
[`4ea31c1`](https://github.com/ultraworkers/claw-code/tree/4ea31c1bc91c4e9bcbd67d51c550c01e127e6d0d)이다.

공개 소스는 볼 수 있다. 하지만 이 저장소만으로 Claude Code 유출물인지, 어떤 clean-room 절차를
거쳤는지까지 증명할 수는 없다. 현재 README도 Anthropic과 무관한 공개 Rust 구현이라고 설명하고,
동시에 production 프로젝트가 아니라는 경고를 둔다. 따라서 provenance를 추정해 복제하지 않고,
공개 인터페이스와 검증 가능한 패턴만 참고한다.

확인한 구현 특성:

- `ConversationRuntime`은 매 model iteration에 system prompt와 전체 session message vector를
  clone한다. 또한 한 응답에 여러 tool call이 있어도 loop에서 순차 실행한다.
  [source](https://github.com/ultraworkers/claw-code/blob/4ea31c1bc91c4e9bcbd67d51c550c01e127e6d0d/rust/crates/runtime/src/conversation.rs#L325-L517)
- 외부 plugin tool은 `Command::new(...).spawn()`으로 호출마다 child process를 만든다.
  [source](https://github.com/ultraworkers/claw-code/blob/4ea31c1bc91c4e9bcbd67d51c550c01e127e6d0d/rust/crates/plugins/src/lib.rs#L307-L348)
- 기본 compaction은 최근 4개 message를 남기고 요약하는 일반 transcript 방식이다.
  tool-use/result 경계는 보존하지만 KRW의 claim/evidence lineage를 별도 불변 데이터로 유지하지 않는다.
  [source](https://github.com/ultraworkers/claw-code/blob/4ea31c1bc91c4e9bcbd67d51c550c01e127e6d0d/rust/crates/runtime/src/compact.rs#L10-L150)

판정:

- session, permission, MCP, parity harness의 구조는 참고할 가치가 있다.
- plugin subprocess 방식과 full transcript clone 방식은 KRW 목표에 맞지 않는다.
- Claw Code를 fork하는 것보다 KRW의 `ResearchState v2`, evidence policy, Guru state machine을
  first-class core로 컴파일하는 편이 더 작고 빠르며 품질도 보존하기 쉽다.

## 3. 현재 KRW 기준선

현재 코드에서 확인한 구조:

- `runner.ts` 4,663 lines, `mcp-client.ts` 1,001 lines, `job-runner.ts` 1,583 lines다.
- supervisor는 한 Node process에서 active Promise만 보유하며 기본 production concurrency는 8이다.
- 대기 run은 DB에서 claim되므로 queued session마다 process object가 생기지는 않는다.
- 현재 Agent SDK `query()` 경로는 run마다 Claude Code executable을 사용한다.
- durable queue, SSE, cancel, billing, citation 저장은 재사용 가치가 높다.
- 현재 plugin package에는 Markdown 93개, 519,504 bytes가 있다. content hash 기준 73개가
  unique이며, 정확히 같은 파일 20개를 제거하면 81,652 bytes, 15.7%가 줄어든다. 이는 prompt
  절감률이 아니라 AgentImage source/parse dedup의 하한 예시다.

이전 방향성 측정:

| 항목 | 관측값 | 해석 제한 |
|---|---:|---|
| bare Node max RSS | 약 39.9 MiB | 현재 머신의 짧은 smoke |
| Agent SDK + MCP import max RSS | 약 104.4 MiB | live run 아님 |
| `claude --version` max RSS | 약 197.5 MiB | 실제 agent workload 아님 |
| 현재 supervisor active cap | 8 | resource-weighted cap 아님 |

## 4. Plugin 실행 microbenchmark

같은 머신에서 실제 업무가 없는 최소 실행만 비교했다.

| 경로 | 반복 | 총 시간 | 호출당 |
|---|---:|---:|---:|
| in-process 단순 함수 | 1,000,000 | 1.598 ms | 측정 해상도 이하 |
| `/usr/bin/true` fork/exec | 200 | 187.585 ms | 0.938 ms |

이 결과는 실제 plugin latency를 예측하지 않는다. `/usr/bin/true`는 interpreter 시작, JSON parse,
plugin code, network가 모두 빠진 lower bound다. 결론은 좁다. 호출마다 process를 띄우는 방식은
고빈도 read tool에 불필요한 고정비와 RSS spike를 추가한다. 신뢰된 고빈도 도구는 native adapter,
원격 기능은 pooled MCP, 비신뢰 로컬 확장만 sandboxed WASM으로 보내는 것이 맞다.

## 5. 메모리 민감도 모델

### 5.1 가정

다음은 release claim이 아니라 topology 비교용 가정이다.

```text
Legacy process tree MiB
  = 104.4 worker
  + active * (197.5 Claude child + 12 run state)

TypeScript in-process reference MiB
  = 150 worker/runtime base + 64 shared cache + active * 10

Rust machine daemon MiB
  = 32 daemon base + 64 shared cache + active * 6
```

Rust 수치는 실측 전 가설이다. production host가 별도로 반드시 필요하면 그 process도 release
process-tree에 포함한다. 최종 daemon이 DB queue를 직접 claim하면 전용 Node agent-worker는 제거된다.

### 5.2 계산 결과

| active run | Legacy | TS shared reference | Rust daemon target |
|---:|---:|---:|---:|
| 1 | 313.9 MiB | 224 MiB | 102 MiB |
| 4 | 942.4 MiB | 254 MiB | 120 MiB |
| 8 | 1,780.4 MiB | 294 MiB | 144 MiB |
| 16 | 3,456.4 MiB | 374 MiB | 192 MiB |
| 32 | 6,808.4 MiB | 534 MiB | 288 MiB |

해석:

- subprocess 제거의 효과가 압도적이다.
- Rust와 shared Node의 차이는 subprocess 제거 효과보다 작다.
- 그래서 TypeScript reference executor는 빠른 parity 확보에 유리하다.
- 그래도 장기 목표가 machine-wide 다중 client라면 Rust daemon은 runtime 중복 제거, bounded
  allocation, mmap AgentImage, plugin isolation 면에서 더 높은 상한을 준다.
- 최종 선택은 1/4/8/16/32 active live trace에서 검증한다. Rust가 Node reference 대비
  process-tree RSS를 의미 있게 줄이지 못하거나 품질/운영성이 악화되면 Rust rollout을 중단한다.

queued/idle session 수는 위 식에 들어가면 안 된다. queued session마다 task, timer, listener,
transcript object가 하나라도 생기면 설계 위반이다.

## 6. Burst queue simulation

### 6.1 실험 A

결정적 discrete-event proxy를 30 seed로 실행했다.

- 한 noisy tenant가 20초 안에 600 jobs를 burst한다.
- 나머지 23 tenant는 각각 25 jobs를 120초에 걸쳐 보낸다.
- workload mix는 Normal 68%, Deep 24%, Guru 8%다.
- 추상 capacity는 model 8, tool 16, memory 12 units다.
- FIFO, per-user cost-aware fair queue, resource-aware DRF proxy를 비교했다.

| 정책 | 전체 p95 wait | noisy tenant p95 | 나머지 tenant p95 | makespan |
|---|---:|---:|---:|---:|
| FIFO | 2,171 s | 1,563 s | 2,473 s | 2,985 s |
| user fair queue | 2,078 s | 2,517 s | 1,772 s | 2,995 s |
| DRF-aware proxy | 2,123 s | 2,411 s | 1,612 s | 3,070 s |

DRF proxy는 다른 tenant의 p95를 FIFO 대비 약 35% 줄였지만 makespan을 약 2.8% 늘렸다.
공정 스케줄링은 처리 능력을 만들지 않는다. noisy neighbor가 기다리는 몫을 늘리고 다른 tenant의
tail latency를 줄인다.

### 6.2 실험 B

별도 보수 모델은 noisy background 100건 뒤 interactive 65건을 넣고 4/8/16GB 추상 profile을
비교했다. interactive p95 queue wait는 다음과 같았다.

| profile | FIFO | user DRR | priority + DRF/DRR proxy |
|---|---:|---:|---:|
| 4GB | 223 min | 88 min | 91 min |
| 8GB | 74 min | 23 min | 12 min |
| 16GB | 28 min | 9.6 min | 3.8 min |

절대 시간은 실제 latency 예측이 아니다. 4GB에서 유입량이 처리량을 압도하면 scheduler만으로
문제가 해결되지 않는다는 stress example이다. admission, backpressure, background degradation이
반드시 필요하다.

### 6.3 스케줄러 판정

이 simulation이 지지하는 production baseline은 다음 계층이다. DRF-aware 결과는 optional promotion의
상한 후보이지 기본값 채택 근거가 아니다.

```text
priority class
  -> global/tenant queue, deadline, cost and concurrency admission
  -> user weighted deficit round robin
  -> session FIFO, active turn 1
  -> step-level model/tool/memory permits
```

- interactive reserve는 background가 빌릴 수 있는 work-conserving reserve로 둔다.
- background는 aging으로 최대 대기를 보장한다.
- model inference 중 tool permit을 보유하지 않고, tool execution 중 model permit을 반납한다.
- DRF는 trace에서 dominant-resource unfairness가 관측되고 user DRR baseline보다 fairness가 개선되며
  throughput/fragmentation 손실이 5% 이하일 때만 올린다. 실패하면 baseline을 유지한다.
- capacity controller는 latency-gradient로 천천히 증가하고, 429/503/RSS pressure에는 즉시
  multiplicative decrease한다. provider와 MCP별 loop를 분리한다.

## 7. 자율 조사 시나리오 simulation

### 7.1 일반 질문

```text
classify -> required claims 2개 -> recommended_actions 1개
-> native/public read -> evidence ledger -> deterministic verifier
-> answer compose -> commit
```

예상 특성: single DeepSeek main trajectory, reflection 0, child actor 0. 빠른 질문에 tree search를
강제하지 않는다.

### 7.2 다중 기간 심층 리서치

```text
SearchPlan -> ResearchState gap DAG
-> 독립 period/metric action 병렬 실행
-> content-hash dedup + singleflight
-> contradiction node 발견
-> targeted follow-up 1개
-> claim verifier -> answer composer
```

동일 broad query 반복은 금지한다. planner가 제시한 action보다 `ResearchState v2.recommended_actions`
와 unresolved clause가 우선한다.

### 7.3 Guru

```text
sealed investigation brief
-> bounded company evidence actor
-> immutable company research context
-> Guru advisor workflow
-> numeric/citation/domain rule verifier
-> Korean answer
```

child actor는 별도 process가 아니라 같은 run budget과 EvidenceLedger를 상속한 task actor다.
main Guru와 company evidence가 같은 tool을 중복 호출하면 singleflight/CAS가 하나만 실행한다.

### 7.4 MCP 일부 장애

capability graph에서 실패한 server의 action만 degraded 처리한다. plugin-independent run까지 모든
claim을 중단하지 않는다. 필수 claim이 해당 capability에 의존하면 bounded partial answer 또는
실패로 끝낸다. 다른 사용자의 personal pool로 fallback하지 않는다.

### 7.5 Plugin hot reload

새 source는 새 immutable AgentImage hash로 compile, validate, shadow evaluation한다. active run은
기존 hash에 pin된다. 통과 후 registry pointer를 atomic swap하며, old image는 마지막 run 종료 후
unmap한다. turn 중 policy/schema가 바뀌지 않는다.

### 7.6 50-turn session

raw transcript 전체를 heap에 올리지 않는다.

- L0: active turn scratch
- L1: 최근 완결 turn
- L2: typed episodic summary + unresolved goals + evidence IDs
- L3: content-addressed raw artifacts

compaction은 tool-use/result pair와 DeepSeek reasoning block을 깨지 않는 whole-turn boundary에서만
수행한다. claim/evidence IDs, user constraints, unresolved decisions는 요약 대상이 아니다.

## 8. Crash-point model

run state는 append-only event와 fencing token으로 전이한다.

```text
QUEUED -> CLAIMED -> PLANNING -> EXECUTING -> VERIFYING
       -> COMPOSING -> COMMITTING -> SUCCEEDED
```

action state는 별도로 관리한다.

```text
PROPOSED -> AUTHORIZED -> LEASED -> DISPATCHED -> OBSERVED -> COMMITTED
```

| crash point | recovery | 금지 사항 |
|---|---|---|
| provider 요청 전 | 같은 step 재개 | stale fence write |
| partial text/reasoning 후 | partial 폐기 후 step 재요청 | partial answer commit |
| tool call parse 후, 실행 전 | action ledger 확인 후 실행 | duplicate dispatch |
| read tool 실행 후, 응답 유실 | idempotency/CAS hit면 reuse | blind retry |
| checkpoint 후 | 다음 state부터 재개 | 앞 state 재적용 |
| final compose 후, commit 전 | final hash로 compare-and-set | final 2개 생성 |
| commit 후 ack 유실 | committed row 반환 | 이중 billing/event |

release invariant:

- visible final result는 run당 최대 1개다.
- billing reservation/settlement은 idempotency key당 최대 1회다.
- old fencing token의 event/checkpoint/final write는 0개 허용한다.
- ambiguous side effect는 자동 retry하지 않는다.

## 9. DeepSeek contract simulation

최종 runtime은 DeepSeek native Chat Completions format을 직접 사용한다.

- 현행 물리 model은 `deepseek-v4-flash` 하나이며 context length는 1M이다.
- 긴 context가 가능해도 전체 session을 매번 보내지 않는다. stable prefix와 retrieved segments만 보낸다.
- tool chain에서는 `reasoning_content`를 이후 request에 완전히 되돌려 보내야 한다. 누락 시 400이다.
- native MCP content block은 Anthropic compatibility path에서 지원되지 않으므로 runtime이 MCP를
  직접 실행하고 일반 tool result로 변환한다.
- unsupported Anthropic model name이 flash로 자동 mapping될 수 있으므로 model whitelist 밖 이름은
  startup에서 거절한다.
- provider cache는 exact prefix 기반 best-effort다. AgentImage의 stable prompt segment를 앞에 고정하고
  usage의 cache hit/miss token을 release metric으로 기록한다.
- 429/500/503만 bounded retry한다. 400/401/402/422는 자동 retry하지 않는다.

공식 근거:

- [Models & Pricing](https://api-docs.deepseek.com/quick_start/pricing/)
- [Thinking Mode](https://api-docs.deepseek.com/guides/thinking_mode/)
- [Anthropic API compatibility](https://api-docs.deepseek.com/guides/anthropic_api/)
- [Context Caching](https://api-docs.deepseek.com/guides/kv_cache/)
- [Rate Limit & Isolation](https://api-docs.deepseek.com/quick_start/rate_limit/)
- [Error Codes](https://api-docs.deepseek.com/quick_start/error_codes/)

## 10. Release에서 다시 측정할 것

### 10.1 workload capture

- run kind 분포와 active/queued ratio
- prompt, reasoning, output, tool-result byte 분포
- tool calls/run, independent tool fan-out, MCP latency/error rate
- tenant burst 크기와 queue sojourn
- 1/12/50 turn session의 retrieved context 크기

### 10.2 memory and performance

- 동일 topology의 RSS/PSS/USS, heap/external/mmap, FD/socket/task 수
- idle, 1/2/4/8/16/32 active slope
- 10k/100k queued에서 daemon heap delta
- DeepSeek TTFT, end-to-end p50/p95/p99, tokens/s, cache hit tokens
- throughput, utilization, max background wait, Jain fairness
- 6시간과 7일 soak, repeated crash/restart

### 10.3 품질

- 기존 fixed 50Q, Guru 30Q, contract fixtures
- 1/12/50 turn memory/constraint retention
- citation precision/recall, numeric accuracy, period policy, personal boundary
- equal-token/equal-tool-budget ablation
- TypeScript reference vs Rust daemon paired replay
- human blind holdout. 같은 DeepSeek self-judge만으로 release하지 않는다.

## 11. Go/no-go

Rust daemon은 아래가 모두 참일 때만 production target이 된다.

- critical quality regression 0
- paired quality delta 단측 95% lower bound `>= 0`
- citation, numeric, personal-data, Guru gate 각각 비열등
- end-to-end p95와 sustained throughput 비열등
- process-tree RSS가 Node reference보다 사전 고정한 최소 개선폭 이상
- 10k/100k queued가 active memory와 분리됨
- crash matrix에서 stale write, duplicate final, double billing 0
- 7-day soak에서 unbounded heap/FD/task growth 0

하나라도 실패하면 Node reference를 유지하고 원인을 계측한다. “Rust니까 빠를 것”은 release gate가 아니다.
