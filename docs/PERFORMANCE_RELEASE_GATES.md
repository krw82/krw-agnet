# Performance, load and soak release gates

작성일: 2026-08-02  
상태: **ACTIVE TEST AUTHORITY**

이 문서는 저사양 호스트와 대량 session에 대한 실행 가능한 성능 계약이다. 임계값의 canonical
source는 [`perf/release-gates.json`](../perf/release-gates.json)이며 측정 결과를 본 뒤 같은 release에서
임계값을 완화하면 안 된다.

## 무엇을 실제로 측정하는가

`krw-agent-perf-harness`는 production key나 외부 endpoint 없이 다음을 측정한다.

1. **10k/100k queued pressure**: 전체 queue는 DB에 남고 daemon에는 256개 resident admission만
   존재하는지, live heap과 RSS가 queue 길이에 비례하지 않는지 확인한다.
2. **1/2/4/8/16/32 post-tool active runs**: 유효한 bounded `RunRequest`와 typed `RunContextV1`으로
   실제 `RunEngine`을 시작한다. 첫 provider tool-call episode, capability dispatch, 기본 256 KiB
   capability result의 durable observe/commit, evidence/conversation ingest와 active-state checkpoint를
   모두 통과한 뒤 **두 번째 provider 호출 경계**에서 동시에 멈춰 run당 live heap/RSS slope와 cleanup
   잔류량을 측정한다. AgentImage, deployment, fixture template과 latch는 machine-wide `Arc`로 공유하지만
   committed result와 run-local conversation/evidence state는 run별로 유지한다. 256 KiB payload를
   `question`에 넣지 않으며 질문은 RunEngine의 64 KiB 상한보다 작다.
3. **MCP pool**: 같은 key의 동시 초기화가 한 slot으로 합쳐지고, principal/release별 key가 많아도
   pool entry가 hard cap을 넘지 않는지 확인한다. HTTP scheme을 의도적으로 잘못 준 local-only
   fixture이므로 socket이나 운영 endpoint를 사용하지 않는다.
4. **vertical-slice + active-task soak**: keyless fixture를 반복 실행하면서 32개 `RunEngine` active
   task wave를 주기적으로 생성·join한다. live heap, RSS, FD, OS thread와 spawned/joined task 수가
   warm baseline 이후 계속 증가하지 않는지 확인한다.
5. **scope separation**: 이 프로세스는 자신이 직접 측정한 성능만 증명한다. 별도 프로세스에서
   실행된 MCP·fault test를 Boolean 인자로 신뢰하거나 JSON report에 통과했다고 대신 기록하지 않는다.
   전체 release 검증기는 test log와 산출물 hash를 별도 evidence manifest로 묶어야 한다.

live heap은 process 전역 allocation counter의 `allocated - deallocated` 값이다. RSS는 OS `ps`, FD는
Linux `/proc` 또는 macOS `lsof`, thread는 Linux `/proc` 또는 macOS `ps -M`으로 별도 측정한다.
OS metric을 얻을 수 없는 host에서는 해당 gate를 조용히 통과시키지 않고 `unavailable`로 기록한다.

active-run JSON에는 `measurement_phase=second_provider_wait_after_committed_capability`, configured/observed
retained bytes per run, first-provider/capability/commit/checkpoint/second-provider entered·completed count와
rejected phase-entry count가 level별 및 aggregate로 기록된다. 하나라도 두 번째 provider phase에
도달하지 못하거나 malformed post-tool request가 발생하면 메모리 sample을 active run으로 세지 않고
하네스 자체를 실패시킨다.

## 실행 명령

빠른 PR/개발 gate:

```bash
bash scripts/run-performance-gates.sh ci
```

2 vCPU/4 GB release candidate의 6시간 churn gate:

```bash
bash scripts/run-performance-gates.sh release-4gb
```

production topology에서 최종 7일 soak:

```bash
bash scripts/run-performance-gates.sh production-7d
```

두 번째 인자로 report 경로를 지정할 수 있다. 기본값은
`target/perf/<profile>-report.json`이다. 모든 하네스는 JSON report를 먼저 저장하고 gate가 하나라도
실패하면 non-zero로 종료한다. report에는 harness version과 threshold file SHA-256이 들어가므로
서로 다른 기준으로 생성한 결과를 같은 baseline처럼 비교하면 안 된다.

개별 진단은 다음처럼 실행한다.

```bash
cargo run --release -p krw-agent-perf-harness -- \
  --profile ci --scenario scheduler --root .

cargo run --release -p krw-agent-perf-harness -- \
  --profile ci --scenario active-runs --root .

cargo run --release -p krw-agent-perf-harness -- \
  --profile ci --scenario mcp-pool --root .

cargo run --release -p krw-agent-perf-harness -- \
  --profile ci --scenario soak --root .
```

성능 숫자는 반드시 `--release` build에서 비교한다. debug 결과는 correctness 진단일 뿐 release
근거가 아니다.

## 고정 gate

| 항목 | 임계값 |
|---|---:|
| scheduler/orchestration p95 | `<= 10 ms` |
| 10k queued live heap/RSS delta | `<= 5 MiB` |
| 100k queued live heap/RSS delta | `<= 10 MiB` |
| active normal live heap slope | `<= 12 MiB/run` |
| active normal working set | `<= 16 MiB/run` |
| MCP live entries | `<= 32` |
| MCP same-key initializing/failed slot | exactly `1` |
| warm soak live heap growth | `<= 1 MiB` |
| warm soak FD/thread growth | `0` |
| active churn spawned/joined task 차이 | `0` |
| active churn post-join heap/FD | `<= 1 MiB` / `0` |
| 6h/7d RSS growth | warm baseline 대비 `<= 5%`, 절대 `<= 16 MiB` |

queue 측정은 `AdmissionItem` 100k개를 보관하는 시험이 아니다. 그런 구현은 DB queue를 daemon heap에
복제하는 잘못된 topology다. 하네스는 100k개의 claim candidate를 흘려보내되 resident hard cap 이후
즉시 거절되는지 측정한다. 따라서 100k queued가 100k Tokio task, timer 또는 listener를 만들지 않는다.

active 측정의 baseline과 cleanup은 동일한 shared provider/capability/persistence control plane을 유지한
상태에서 수집한다. payload-sized template은 warm-up 전 한 번만 만들고 공유하며, latch/counter는 payload
크기에 비례해 확장되지 않는다. persistence fixture는 result bytes를 검사하고 commit receipt를 반환하되
payload 복사본을 control plane에 보관하지 않으므로 측정값은 두 번째 provider wait에서 살아 있는
run-local conversation/evidence/action-cache state를 중심으로 한다.

## 판정 범위와 남은 외부 비교

`ci`는 leak과 큰 회귀를 빠르게 찾는 smoke profile이며 장기 안정성 승인 권한이 없다.
`release-4gb`와 `production-7d`만 각각 6시간과 7일 wall-clock을 강제한다. 실행 시간을 줄인 report는
release evidence로 사용할 수 없다.

이 하네스만으로 다음을 승인하지 않는다.

- live DeepSeek TTFT/end-to-end 품질과 throughput
- 실제 TLS/socket buffer를 포함한 production MCP process tree
- production canonical contract guard 자체의 비용(offline post-tool fixture는 public production ABI와
  실제 compiled AgentImage를 사용하지만 외부 remote schema 검증은 structural guard로 대체한다)
- 기존 TypeScript/Claude worker 대비 RSS `<= 75%`, active slope `<= 60%`
- 2 vCPU/4 GB cgroup pressure와 OOM recovery

위 비교는 동일 질문, 동일 model/release, 동일 token/tool budget을 고정한 별도 A/B topology에서
수집해야 한다. 이 report의 `limitations`, `performance_authority`,
`complete_performance_evidence` 필드가 그 경계를 machine-readable하게 남긴다. 이 필드들은 전체
production release 승인을 뜻하지 않는다.
