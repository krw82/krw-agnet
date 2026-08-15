# KRW Agent 최종형 리팩토링

이 디렉터리는 `krw-agnet`의 최종 목표 구조와 실제 전환 순서를 정의한다. 경로 이름은 운영자가 요청한 `doc/refetoring`을 그대로 사용한다.

## 결론

현재 문제는 단순한 코드 줄 수가 아니다. 다음 세 종류의 결합이 동시에 누적되어 있다.

1. `run-engine`이 provider wire, workflow, recovery, capability dispatch, prompt 작성, 검증, final commit까지 소유한다.
2. 로컬 내장 기능, MCP 기능, provider 기능, persistence 부속 기능이 모두 동일한 런타임 실패 경로에 놓인다.
3. `krw-agnet`와 `krw-ontology-front`가 작은 공개 계약으로 만나는 대신 서로의 소스 경로, 마이그레이션 파일, 설치 스크립트와 rollback 상태를 직접 안다.

최종 구조는 다음 원칙을 따른다.

- 모델이 만든 문체·형식·섹션·follow-up 오류는 사용자 답변을 없애지 않는다.
- provenance, tenant ownership, fence, idempotency, ambiguous dispatch, atomic commit만 hard invariant로 유지한다.
- 로컬 builtin은 endpoint, credential, MCP readiness, deployment binding을 갖지 않는다.
- 원격 capability만 배포 fingerprint와 MCP preflight에 참여한다.
- 배포는 하나의 명시적 config, 하나의 controller, 하나의 terminal receipt를 사용한다.
- 데이터베이스 마이그레이션은 forward-only다. 마이그레이션 후 구버전 데몬으로 되돌리지 않는다.
- frontend와 agent는 versioned deployment contract와 public release descriptor로만 연결한다.
- `~/krw-ontology`와 ontology schema는 변경하지 않는다.

## 문서 지도

- [01-current-state-audit.md](01-current-state-audit.md): 코드 크기, 실제 장애와 결합 증거
- [02-target-architecture.md](02-target-architecture.md): crate 및 런타임 목표 구조
- [03-runtime-failure-policy.md](03-runtime-failure-policy.md): 실패와 제한 답변의 최종 정책
- [04-legacy-and-hardcoding-removal.md](04-legacy-and-hardcoding-removal.md): 삭제 목록과 하드코딩 제거 기준
- [05-deployment-and-front-contract.md](05-deployment-and-front-contract.md): forward-only 배포와 frontend 경계
- [06-implementation-plan.md](06-implementation-plan.md): 작업 순서, 검증, 완료 조건

## 이번 리팩토링에서 이미 시작한 최종형 변경

- `CapabilitySpec`의 실행 대상을 `remote`와 `local builtin`으로 분리했다.
- `skill.load`를 `local / skill_load`로 선언하고 가짜 `krw_skill_local` MCP binding을 제거했다.
- runtime config, capability catalog, quality fixture, release binding에서 로컬 skill을 physical route로 세지 않도록 변경했다.
- provider HTTP adapter와 provider failure classification을 `run-engine/src/provider.rs`로 분리했다.
- 내부 대화 표현을 `run-engine/src/transcript.rs`로 분리했다.
- provider가 이미 반환한 최종 답은 사후 usage overage 때문에 폐기하지 않도록 바꿨다.
- session-memory delta/hash/frontier 생성 실패는 core answer가 아니라 해당 부속물만 생략한다.
- bounded-child persistence는 선택 시점의 런타임 오류가 아니라 구현 누락의 compile error가 되도록 바꿨다.
- metric dictionary build는 sibling checkout이나 빈 fallback 없이 저장소 내부 봉인 snapshot만 사용한다.
- `flash_*` profile alias와 provider를 암묵 선택하던 기본 model registry를 삭제했다.
- frontend 소스 crawl 대신 작은 versioned deployment contract를 검증한다.

이 변경은 호환 shim이 아니다. AgentImage의 source schema를 최종 형태로 바꾸고 checked-in agent YAML을 전부 같은 형태로 전환한다.

## 절대 유지할 hard invariant

- tenant, principal, session, run ownership
- immutable AgentImage, deployment, model, contract, schema, data release pin
- fencing token과 cancel generation
- canonical action key, request hash, durable action receipt
- send 이후 결과가 불명확한 capability의 무조건 재실행 금지
- TLS, credential partition, endpoint origin, readiness identity
- evidence provenance와 committed calculation lineage
- final answer의 atomic commit과 ambiguous commit 복구
- untrusted input의 byte/depth/count 상한

## hard failure가 아니어야 하는 것

- Markdown 제목이나 섹션 수
- follow-up 질문의 개수와 물음표 여부
- locale, 문체, 표현 품질
- counter-signal 누락
- 일부 claim의 근거 부족
- optional chart/presentation 생성 실패
- session memory 읽기 실패
- compaction에서 중요도가 낮은 자료가 잘리는 상황
- commit 이후 telemetry, memory, projection 부속 정보 오류
- provider 응답을 이미 받은 뒤의 소폭 budget overage

이 항목은 sanitize, downgrade, omit, limitation 또는 ledger 기반 제한 답변으로 수렴한다.

## 완료 정의

리팩토링은 다음이 모두 만족될 때 완료다.

1. `run-engine/src/lib.rs`의 production 코드가 명확한 모듈 경계로 분리된다.
2. capability runtime은 remote capability만 알고, local builtin은 별도 executor가 담당한다.
3. model/provider 선택은 registry descriptor에서 오며 `GLM 아니면 DeepSeek` 분기가 없다.
4. model output 품질 오류가 `run.failed`를 만드는 경로가 사라진다.
5. session memory, chart, presentation, auxiliary projection은 core answer commit을 막지 않는다.
6. frontend는 agent 소스나 migration 파일을 crawl하지 않는다.
7. `npm run prod:deploy:full`은 단일 controller를 호출하고 provider를 명시적으로 선택한다.
8. migration 이후 구 release 복구 코드는 없다.
9. 실패한 배포도 terminal failure receipt를 남기며 admission은 닫힌 상태로 끝난다.
10. 집중 테스트, workspace check, agent source validation, deployment dry-run이 모두 통과한다.

### 2026-08-16 상태 감사 (branch `refactor/final-form-waves`)

- (1) 완료 — lib.rs 18,761줄에서 8,753줄(tests 포함)로; recovery/
  capability_dispatch/provider_request/finalization/active_run/
  orchestrator/validation 모듈 분리.
- (2) 완료 — `CapabilityExecution::{Remote,Local}` + closed builtin enum.
- (3) 선택/분류 완료 — `ProviderKind` descriptor 분기. 잔여:
  provider_request.rs의 wire-encoding 두 곳(output_config.effort,
  response_format)은 pinned wire-capability 매트릭스 확장(계약 버전
  변경)이 필요한 인코딩 성형 조건으로 문서화됨.
- (4) 완료 — 품질 결함은 sanitize 후 AcceptedWithWarnings 커밋,
  integrity만 run.failed.
- (5) 완료 — total compaction, optional projection isolation,
  post-commit infallibility.
- (6) agent-side 완료(crawler 스크립트 삭제). frontend-side 전환은
  Wave 9 전제와 함께 대기.
- (7) 대기 — controller core(`bins/krw-agent-deploy`)는 fail-closed로
  dry-run receipt까지 검증됨. stage 3-11 실구현 후 frontend adapter
  전환.
- (8) agent-side 완료(launchd forward-only, rollback 경로 제거).
  frontend repo 구 스크립트/.deploy rollback은 (7) 검증 후 삭제.
- (9) 완료 — controller가 terminal failure receipt + admission closed
  원칙을 구현.
- (10) 완료 — workspace 703 tests, check-all.sh, python 검증 4종,
  controller dry-run 73 tests 모두 green.
