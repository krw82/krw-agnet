# KRW Embedded Agent 검증 계획

> **SUPERSEDED AS RELEASE AUTHORITY — 역사적 검증 초안**
>
> fault, quality, memory test 아이디어는 계속 참고할 수 있지만 phase/approval, TypeScript comparison,
> rollback과 legacy migration gate는 현행 release 계약이 아니다. 현재 범위와 완료 조건은
> [`IMPLEMENTATION_STATUS.md`](./IMPLEMENTATION_STATUS.md)를 따른다.
>
> **출력 계약 정정 (2026-08-04):** `company_research` normal final은 direct Markdown + immutable
> EvidenceLedger receipt다. 아래 AnswerIR/render mutation 항목은 typed product output 또는 역사적
> 검증 아이디어로만 읽고, current Korean research final의 필수 gate로 해석하지 않는다.

작성일: 2026-08-02  
상태: 역사적 사전 등록 초안  
대상: 과거 [`EMBEDDED_AGENT_ARCHITECTURE.md`](./EMBEDDED_AGENT_ARCHITECTURE.md)

## 1. 목적

이 계획은 Rust 구현을 정당화하기 위한 체크리스트가 아니다. 다음 주장을 가능한 한 빨리 반증한다.

1. 동일 DeepSeek와 동일 evidence budget에서 기존 품질을 떨어뜨리지 않는다.
2. machine-wide Rust daemon이 동일 기능 TypeScript reference보다 process-tree memory와 tail behavior가
   충분히 낫다.
3. crash, cancel, retry, rolling upgrade에도 visible final, billing, action이 중복되지 않는다.
4. queued/idle session 수가 daemon heap/task/timer 수에 비례하지 않는다.
5. Capability ABI 범위의 plugin은 kernel 수정 없이 안전하게 추가된다.

candidate 결과를 본 뒤 metric, threshold, fixture를 바꾸면 해당 비교는 무효다.

## 2. 고정 불변조건

| ID | 불변조건 | 실패 허용 |
|---|---|---:|
| INV-01 | stale fencing token write | 0 |
| INV-02 | run당 visible final/message | 1 이하 |
| INV-03 | reservation당 settlement/release terminal outcome | 정확히 1 |
| INV-04 | action mutation의 divergent replay | 0 |
| INV-05 | dispatch 전 durable provider/action receipt 누락 | 0 |
| INV-06 | cancel이 먼저 commit된 뒤 final content/citation write | 0 |
| INV-07 | raw model draft/reasoning의 public event/log 노출 | 0 |
| INV-08 | cross-principal pool/cache/singleflight/CAS hit | 0 |
| INV-09 | unsupported strong claim/Guru seal violation | 0 |
| INV-10 | queued/idle session별 task/timer/listener | 0 |
| INV-11 | blind side-effect retry | 0 |
| INV-12 | active provider episode의 partial reasoning/tool replay | 0 |

## 3. Test diagram

```text
AgentSpec source
  -> compiler/importer
  -> signed AgentImage
  -> registry/revocation
  -> daemon load/pin
  -> claim_vN receipt
  -> admission/permit
  -> ProviderEpisodeV1
  -> DeepSeek SSE parser
  -> tool proposal
  -> capability/auth/budget validation
  -> begin_action receipt
  -> native | MCP | WASI execution
  -> EvidenceLedger/ResearchGraph
  -> checkpoint_vN
  -> AnswerIR
  -> deterministic + semantic defect verification
  -> deterministic Markdown renderer
  -> commit_final_vN
  -> outbox
  -> existing SSE/web presentation
```

각 화살표는 success뿐 아니라 invalid input, timeout, cancellation, stale version, crash-before,
crash-after 분기를 가진다.

## 4. 경로별 coverage map

| 경로/분기 | 주 테스트 | 보조 테스트 | 필수 gate |
|---|---|---|---|
| AgentSpec include/precedence/hash | unit, property | fuzz, golden | same source = same hash |
| path traversal/symlink/remote include | adversarial unit | fuzz | production image build 거절 |
| image signature/expiry/revoke/downgrade | integration | rolling upgrade | revoked new run 0 |
| Capability ABI schema/release match | contract | MCP drift fault | mismatch dispatch 0 |
| claim/renew/expire | DB integration | model-based concurrency | stale fence 0 |
| queue cap/deadline/ETA/degrade | scheduler simulation | overload live | infinite wait 0 |
| ProviderEpisode replay | wire golden | property/fuzz | DeepSeek 400/lineage loss 0 |
| fragmented SSE/UTF-8/tool JSON | parser fuzz | live probe | partial tool args dispatch 0 |
| pre-token retry | deterministic fault | live 429/503 | retry budget 준수 |
| post-token/dispatch failure | state model | crash injection | blind retry 0 |
| begin/observe/commit action | DB+tool integration | mutation replay | divergent replay 0 |
| native tool panic | integration | soak | daemon survival |
| MCP disconnect/schema/data drift | integration | rolling release | scope/release mixing 0 |
| WASI trap/fuel/memory | sandbox tests | soak | daemon survival, cap 준수 |
| EvidenceLedger ingest/dedup | property | mutation | lineage/scope 보존 |
| ResearchGraph update/termination | model-based | fuzz | cycle/unbounded loop 0 |
| AnswerIR validation/render | mutation | golden snapshot | invented rendered claim 0 |
| semantic verifier repair | eval ablation | false-edit suite | correct->wrong critical 0 |
| memory compaction | 1/12/50 turn | mutation | pinned field loss 0 |
| final vs cancel race | DB concurrency | kill injection | single linearized outcome |
| final/message/budget/outbox | DB transaction | ack-loss replay | exactly-once visible outcome |
| daemon duplicate/leader loss | process integration | soak | simultaneous claim authority 0 |
| N→N+1→N+2/rollback | upgrade integration | long run | incompatible state corruption 0 |
| 10k/100k queued | load | memory profiling | heap/task slope gate |
| 1/4/8/16/32 active | load | PSS/USS profiling | bounded linear slope |

## 5. Unit, property, fuzz

### AgentImage/compiler

- RFC 8785 canonicalization, Unicode normalization, ordering, duplicate keys
- include graph cycle, diamond, ambiguous precedence, root escape, symlink swap
- unknown/unsupported JSON Schema keyword fail-closed
- signature key rotation, expiry, revocation, min-kernel/current-previous ABI
- content-address collision fixture와 corrupted/truncated blob
- same semantic source의 deterministic hash; material change의 hash divergence

### Kernel/state machines

- execution/cognitive/action transition reachability와 forbidden transition
- every non-terminal state에서 cancel, deadline, fence loss
- ResearchGraph cycle, dependency, supersession, no-progress termination
- action score NaN/overflow/negative budget/unknown cost
- bounded replan/repair/role recursion hard ceiling

### DeepSeek wire

- SSE CRLF/LF, comment, empty data, split UTF-8, split JSON, duplicate/unknown event
- content/reasoning/tool-call interleave와 multi-tool ordering
- missing/duplicate call ID, invalid incremental args, stop 없는 stream
- exact `ProviderEpisodeV1` encode/decode/replay semantic equality
- old episode를 splice하지 않고 fresh conversation을 시작하는 compaction
- 400/401/402/422 no retry; 429/500/503/transport pre-token full-jitter bounds

### Evidence/output

- numeric sign/unit/currency/scale/rounding/ticker/period mutations
- directness/source grade/release/principal mismatch
- causal/comparison claim에 comparison basis 누락
- AnswerIR claim omission, evidence ID forgery, renderer invention/reordering
- Markdown escaping/link/citation anchor가 IR lineage를 바꾸지 않음

### Scheduler/memory

- token bucket refill/clock jump/overflow
- DRR deficit/aging/fairness invariants
- model/tool permit 동시 장기 보유 금지
- bounded channel/CAS threshold/arena release accounting
- red/critical pressure에서 admission 정지와 safe-boundary cancellation

## 6. DB procedure ABI tests

실제 Postgres에서 `agent_v1` role/privilege와 transaction을 검증한다.

- daemon role은 table CRUD/sequence/schema DDL이 거절되고 approved procedure EXECUTE만 성공
- claim receipt가 immutable input/message/budget/release snapshot과 fence/version을 반환
- same mutation ID + same payload retry는 동일 receipt
- same mutation ID + different payload는 named conflict
- stale fence/seq/run version/image/runtime mismatch는 0-row가 아니라 typed rejection
- checkpoint와 event batch가 같은 transaction에서 commit/rollback
- begin_action receipt commit 전 외부 tool dispatch가 없음
- final transaction에 message, AnswerBundle, citations/claims, run terminal, usage settlement/release,
  renderer/terminal/billing outbox가 모두 있거나 모두 없음
- cancel-first와 final-first 두 outcome만 존재
- ack loss 후 readback은 새 final/settlement/outbox를 만들지 않음
- current/previous procedure ABI가 rolling migration 동안 동일 semantic fixture를 통과

DB model checker는 cancel, final, renew, lease expiry, daemon A/B, ack loss 순서를 무작위로 섞어 최소
100,000 schedule을 탐색하고 실패 seed를 fixture로 고정한다.

## 7. End-to-end vertical slice

Phase 1 최소 경로:

```text
queued run
-> claim receipt
-> DeepSeek thinking + one tool call
-> durable ProviderEpisode/action receipt
-> real representative read-only MCP
-> evidence ingest/checkpoint
-> next DeepSeek turn
-> AnswerIR validation/render
-> atomic final/outbox
-> existing web SSE/readback
```

필수 fault injection 지점:

1. claim commit 전/후
2. provider request 전/first reasoning/first tool delta/tool block complete 후
3. provider episode checkpoint 전/후
4. begin_action 전/후, MCP request 전/response 후/observe 전/후
5. AnswerIR 생성/검증/render 각 전/후
6. cancel request와 final commit의 모든 상대 순서
7. final transaction commit 전/후, ack 전/후
8. DB, DeepSeek, MCP disconnect와 daemon SIGKILL

각 seed는 Rust와 TypeScript reference에 동일 적용해 normalized semantic trace를 비교한다.

## 8. Capability/plugin conformance

- native read, pooled MCP, declarative transform을 서로 다른 작성자가 구현
- 30분 안에 source 작성 -> doctor -> compile -> local replay -> conformance 결과까지 도달
- input/output schema hash, protocol, build, data release mismatch fail-closed
- principal/credential/image/release별 pool/cache/singleflight key isolation
- representative read-only plugin corpus 표현률 95% 이상
- arbitrary shell/JS/native dynamic escape가 필요하면 실패로 기록
- 새 auth/evidence primitive가 필요한 capability는 kernel ABI change로 명확히 분류

## 9. Quality/eval

### 사전 등록

Phase 0에서 다음을 candidate 결과 전에 동결한다.

- run-kind strata와 질문/세션 sampling frame
- primary paired quality rubric과 human adjudication protocol
- hard endpoint 정의와 mutation corpus
- primary/critical release margin `0`, alpha, `delta_pass`에서 LCB 통과 power 90%, negative
  `delta_detect` 회귀 탐지 power 90%, sample-size calculation. margin 변경은 재승인 대상
- model alias/probe fingerprint, image/release, token/tool/time budget
- tuning set/final blind holdout 분리

### Gates

- zero-margin primary paired delta one-sided 95% LCB `>= 0`
- critical/security/evidence regression 0
- unsupported strong claim, cross-principal evidence, Guru seal violation 0
- citation entailment, numeric/period, personal boundary 각각 비열등
- same-model judge는 triage만; human/contract disagreement는 human/contract 우선
- 50Q와 Guru 30Q는 fast smoke이며 final statistical proof가 아님

### Algorithm ablations

- deterministic recommended order vs best-first gap
- typed planner skip vs planner
- verifier off/deterministic/semantic risk-gated
- replan/repair bounds
- safe parallel width 1/2/N
- Flash high/max/direct profile policy per run kind
- width-2 beam, bandit, DRF는 baseline을 equal-budget Pareto 지배할 때만 승격

## 10. Performance/load

### Matrix

| 축 | 값 |
|---|---|
| host | 2vCPU/4GB, 4vCPU/8GB, 8vCPU/16GB |
| active | 0, 1, 2, 4, 8, 16, 32 |
| queued | 0, 10k, 100k |
| clients | 1, 4 |
| history | 1, 12, 50 turn |
| tool result | 0, 64KiB, 256KiB, 1MiB |
| cache | cold, warm, auth-high-cardinality |
| workload | normal, deep, Guru, mixed burst |
| failure | 429, 503, DB/MCP loss, disk full, memory pressure |

측정:

- aggregate process-tree RSS/PSS/USS/HWM와 page faults
- allocator heap, mmap, socket/TLS/CAS buffer
- active-run retained memory slope와 forced-idle retained bytes
- CPU, context switch, FD/socket/task/timer/channel slope
- dispatch overhead, DeepSeek TTFT, end-to-end p50/p95/p99, tokens/s
- jobs/min, provider bucket utilization, tool pool wait
- queue sojourn, interactive p95/p99, background max wait, Jain fairness
- cache/singleflight reuse를 auth-scope cardinality별 분리

promotion:

- Rust process-tree footprint `<= 75%` of same-function TS reference
- Rust active-run slope `<= 60%` of TS reference
- orchestration/IPC p95 `<= 10ms`
- p50/p95/TTFT/throughput 비열등
- 10k queued delta `<= 5MiB`, 100k `<= 10MiB`
- 6-hour churn과 7-day soak에서 unbounded slope 0

### Developer experience timing

지원 대상은 installed signed CLI, clean checkout, 2 vCPU/4 GB macOS/Linux다. package cache의 cold/warm
결과를 분리하며 실패한 attempt도 시간에 포함한다.

- `krw-agent quickstart --fixture vertical-slice`: command 시작부터 validated AnswerBundle까지
  p50 `<= 60초`, p95 `<= 120초`
- representative read-only MCP plugin: template 생성부터 fixture replay까지 human walkthrough median
  `<= 30분`; generated template automated path는 CI마다 실행
- `plugin check` local feedback p95 `<= 10초`
- common doctor fixture의 actionable diagnosis/recovery 성공률 `>= 90%`
- migration rollback과 crash resume drill 성공률 `>= 99%`, critical data loss 0

quickstart는 fake provider라고 명시하며 production 품질 측정에 포함하지 않는다. production-shaped
vertical slice는 disposable Postgres, real stored-procedure ABI와 별도 live provider test를 사용한다.

## 11. Upgrade/security/operations

- duplicate daemon, stale socket, peer credential failure, nonce replay
- launchd/systemd crash-loop budget와 bounded recovery
- image signing root/online key rotate, revoke, expire, registry offline
- in-flight run pin, idle session versioned memory migration
- N→N+1→N+2에서 old checkpoint가 남은 경우 third rollout block/drain
- current/previous DB/image/protocol compatibility와 rollback
- CAS quota/TTL/encryption/delete tombstone/vacuum, disk full
- personal deletion이 cache/singleflight/pinned artifacts lineage까지 제거
- log/debug/SSE secret, PII, reasoning/raw draft leak scan
- prompt injection이 capability/auth/budget/image를 바꾸지 못함
- `TracePolicyV1` field allowlist 밖의 raw prompt/response, personal identifier, credential, free-form tool
  payload가 baseline artifact에 0
- sanitized trace secret/PII high-severity finding 0, injected canary detection 100%, HMAC scope isolation,
  encryption/ACL/30-day TTL/deletion lineage drill
- `BaselineManifestV1` canonicalization, blob hash, signature, endpoint formula, threshold authority와 approval receipt
  tamper test

## 12. 예상 실행 명령

실제 crate/package가 생길 때 command 이름을 CI contract로 고정한다.

```bash
cargo test --workspace
cargo nextest run --workspace
cargo fuzz run deepseek_sse
cargo fuzz run agent_image
cargo fuzz run answer_ir
cargo test -p persistence --test model_check
cargo test -p agent-kernel --test crash_matrix
pnpm --dir packages/reference-ts test
pnpm --dir packages/host-ts test
krw-agent quickstart --fixture vertical-slice
krw-agent baseline verify baseline/phase0.json --offline
krw-agent plugin check agents/krw-ontology
krw-agent image build agents/krw-ontology --dev
krw-agent replay vertical-slice --engine rust
krw-agent replay vertical-slice --engine ts-reference
krw-agent replay vertical-slice --compare rust,ts-reference
krw-agent bench --profile 2vcpu-4gb --active 0,1,4,8 --queued 0,10000,100000
krw-agent eval --manifest evals/release.yaml --blind
```

각 command는 machine-readable result, git/image/model/DB release fingerprint와 실패 seed를 남긴다.

## 13. Phase exit evidence

| Phase | 필수 artifact |
|---|---|
| 0 | verified signed `BaselineManifestV1`, `TracePolicyV1` scan/deletion receipt, power calculation, DX stopwatch spec |
| 1 | keyless quickstart timing, vertical trace, DB ABI fixture, crash matrix, TS/Rust paired report, live wire fixtures |
| 2 | plugin coverage/conformance, timed 30-minute walkthrough, image diff/sign/revoke/rollback report |
| 3 | evidence/output/memory mutation report, differential trace parity |
| 4 | 4GB load/soak, queue/CAS/admission/fairness report |
| 5 | shadow quality/perf report and session migration drill |
| 6 | run-kind별 canary decision and rollback receipt |
| 7 | optional WASI-specific authoring/security/perf report |
| 8 | Agent SDK/Claude/ccSwitch absence proof and 7-day production soak |

gate가 실패하면 평균 개선으로 덮지 않는다. 실패 slice만 rollback하거나 architecture decision을 다시 연다.
