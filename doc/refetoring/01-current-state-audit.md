# 현재 상태 감사

## 규모

2026-08-16 정적 감사 기준 주요 파일은 다음과 같다.

| 영역 | 파일 | 규모 |
| --- | --- | ---: |
| 실행 | `crates/run-engine/src/lib.rs` | 19,000줄 이상, production 약 12,600줄 |
| image | `crates/agent-image/src/lib.rs` | 약 5,000줄 |
| ontology adapter | `crates/krw-ontology-adapter/src/lib.rs` | 약 3,600줄 |
| capability runtime | `crates/capability-runtime/src/lib.rs` | 약 3,100줄 |
| session memory | `crates/session-memory/src/lib.rs` | 약 3,000줄 |
| runtime config | `crates/runtime-config/src/lib.rs` | 약 2,900줄 |
| research planner | `crates/research-planner/src/lib.rs` | 약 2,800줄 |
| quality | `crates/research-quality/src/lib.rs` | 약 2,700줄 |
| MCP | `crates/tool-mcp/src/lib.rs` | 약 2,400줄 |
| frontend deploy | `deploy-production-fast.sh` | 약 2,500줄 |

큰 파일 자체보다 중요한 것은 변경 fan-out이다. 예를 들어 capability 실행 종류 하나를 바꾸면 AgentImage, runtime config, run engine, capability runtime, quality fixture, release scripts와 deployment binding이 함께 바뀐다. 이는 타입 경계가 잘못 놓였다는 증거다.

## 실제 배포 장애의 근본 원인

최근 운영 스키마는 `agent_v1.heartbeat_daemon`에 `mcp_ready`를 필수로 요구한다. 현재 소스의 새 데몬은 이를 전송하지만 배포 실패 시 스크립트가 `dabeb82` 기반 구버전 release로 되돌렸다. 구버전은 새 DB ABI를 모르기 때문에 heartbeat가 `K1000 / missing_request_field`로 거절되고 launchd가 crash loop를 만든다.

이 장애는 다음 순서로 발생한다.

```text
forward DB migration
  -> 새 heartbeat ABI 활성화
  -> 후속 gateway/schema smoke 실패
  -> rollback trap 실행
  -> 구 krw-agentd symlink 복구
  -> 구 daemon이 새 heartbeat ABI 호출
  -> missing_request_field
  -> daemon restart loop
```

즉 rollback은 안전장치가 아니라 migration 이후의 확정적인 incompatibility injector다. 최종 구조에서는 DB migration 이후 rollback이 없다. 실패하면 admission을 닫고 새 release를 fix-forward한다.

## `skill.load`의 잘못된 원격 모델링

기존 AgentImage는 `skill.load`에 `binding_key: krw_skill_local`을 선언했다. 실제 run engine은 immutable prompt blob을 로컬에서 읽으므로 MCP 요청이 전혀 없지만 다음 항목을 가짜로 요구했다.

- endpoint registry
- deployment binding
- tool session reuse
- server schema hash
- server build
- data release hash
- startup readiness/fingerprint resolution

이 구조는 기능과 무관한 startup failure를 추가한다. 최종형에서는 `execution: { kind: local, builtin: skill_load }`를 사용하고 remote deployment catalog에서 제외한다.

## 모델 답변을 잃게 하는 런타임 경로

### 답변 검증

최종 AnswerIR 검증은 다음 품질 항목을 hard error로 만들 수 있다.

- locale
- number claim의 unit, period, calculation
- interpretation claim의 counter evidence
- 내부 용어
- section/claim 구조
- follow-up 개수와 물음표

이것들은 integrity가 아니라 presentation/quality다. repair가 고갈되면 이미 확보한 evidence와 유효 claim까지 버린다.

### 응답 이후 budget

provider usage를 응답 수신 후 charge하고 즉시 hard budget check를 수행한다. 이미 비용을 지불해 받은 final answer가 누적 overage 때문에 폐기될 수 있다. post-call overage는 다음 호출만 금지해야 한다.

### session memory와 post-commit

session memory delta/hash/frontier 생성 실패가 final commit 전에 전체 답을 막는다. DB commit 성공 후에도 statechart terminal edge 검증이 실패할 수 있다. memory는 optional이고 commit 이후 로직은 infallible해야 한다.

### compaction

현재 compaction은 상한을 맞추지 못하면 축약된 context 대신 error를 반환한다. context가 클수록 답변 가능성이 높아져야 하는데 반대로 run failure 확률이 커진다.

### frontend projection

Rust final은 1 MiB까지 허용하지만 host projection은 Markdown 64 KiB 제한을 갖는다. answer가 이미 commit된 뒤 optional usage/visualization/projection 필드 문제로 gateway가 500을 반환할 수 있다.

## 배포 과설계

### 서로 다른 canonical path

- `deploy-production-full.sh -> deploy-production-fast.sh`
- `deploy-production-simple.sh`
- `deploy-all-production.sh`
- `deploy-with-queue-pause.sh`
- `pause-production-cutover.sh`
- `recover-production-cutover.sh`

각 경로가 phase 상태와 허용 조건을 다르게 판단한다. `.deploy`에는 77개 cutover 디렉터리, 약 14 GB의 기록이 남아 있고 non-terminal 상태도 존재한다.

### 중복 검증

bundle은 build, prepare, seal, finalize, frontend verifier, local staging, activation에서 반복 검증된다. 반대로 GCP target, remote env와 같은 빠른 결정적 오류는 expensive Rust/Python/web build 이후에 발견된다.

### mutating preflight

full deploy의 preflight가 gateway JSON과 persistent runtime env를 수정한다. preflight 실패가 live 상태를 바꾸므로 원인 분리가 어려워진다.

### unrelated schema smoke

Rust release 배포가 billing, judgment 등 전체 frontend schema smoke를 반복 수행한다. 최근 실패는 research ABI가 아니라 `save_user_filing_judgment_revision` timeout이었다. 배포 unit과 검증 unit이 맞지 않는다.

## 빌드 과결합

- `krw-contracts/build.rs`와 `research-planner/build.rs`가 sibling `KRW_ONTOLOGY_ROOT`를 ambient fallback으로 사용한다.
- source가 없으면 empty mapping을 생성하는 경로가 있어 빌드는 성공하지만 품질이 조용히 무너질 수 있다.
- admin/release CLI가 quality/test/http dependency graph를 함께 compile한다.
- frontend migration과 route 구현을 agent build script가 직접 crawl한다.

최종 빌드는 checked-in canonical snapshot만 사용하고, source 누락을 empty output으로 바꾸지 않는다.

