# 구현 계획

## 수행 원칙

- 모든 behavior change는 실패하는 회귀 테스트에서 시작한다.
- mechanical module move는 같은 behavior test가 green인 상태의 refactor 단계로 수행한다.
- 한 wave가 workspace green이 되기 전 다음 semantic wave를 시작하지 않는다.
- LLM 호출 횟수, reasoning 수준, capability 예산을 줄여 성능을 맞추지 않는다.
- 품질 규칙은 sanitizer/fallback으로 바꾸고 hard gate를 추가하지 않는다.
- ontology repository/schema는 변경하지 않는다.

## Wave 0 — baseline과 장애 봉쇄

상태: 완료. baseline commit(`refactor/final-form-waves`), workspace green,
heartbeat ABI mismatch 장애 chain 문서화(01-current-state-audit.md)를
확정했다.

- clean tree와 commit 기록
- current release/launchd/heartbeat 확인
- source size 및 dependency 감사
- 실제 heartbeat ABI mismatch 문서화
- full workspace check baseline

완료 조건:

- 장애가 source line, release commit, DB required field로 재현 가능
- broad 추측이 아니라 exact failure chain이 기록됨

## Wave 1 — local builtin 경계

상태: 완료.

작업:

- `CapabilityExecution::{Remote, Local}` 추가
- `LocalCapability::SkillLoad` 추가
- 모든 agent YAML을 explicit execution target으로 전환
- runtime config는 remote binding만 resolve
- capability catalog는 remote descriptor만 compile
- run engine은 local을 deployment-independent frontier로 노출
- fake `krw_skill_local` binding/session/fingerprint 삭제
- release/session docs와 tests 갱신

테스트:

- local binding을 제거한 runtime fixture가 compile
- physical binding count가 local capability에 의해 증가하지 않음
- skill body load 결과 동일
- MCP invocation count 0
- full workspace check
- `agents/check-all.sh`

## Wave 2 — run-engine 모듈화

상태: 완료. `recovery.rs`, `capability_dispatch.rs`, `provider_request.rs`,
`finalization.rs`, `active_run.rs`, `orchestrator.rs`, `validation.rs`가
`provider.rs`/`transcript.rs`에 이어 분리됐다. 각 이동 후 focused tests
(103/103)와 workspace check가 green이며 checkpoint hash, action key,
provider request JCS에 변화가 없다. `agents/check-all.sh`와
`scripts/test_dual_provider_release.py`도 통과했다.

순서:

1. `provider.rs`
2. `transcript.rs`
3. `recovery.rs`
4. `capability_dispatch.rs`
5. `provider_request.rs`
6. `finalization.rs`
7. `active_run.rs`
8. `orchestrator.rs`
9. `validation.rs`

각 이동 후 다음을 확인한다.

- public API 변화 없음
- serialized checkpoint hash 변화 없음
- action key 변화 없음
- provider request JCS 변화 없음
- focused tests + workspace check

## Wave 3 — execution-contracts와 dependency inversion

새 crate `crates/execution-contracts`에 다음을 옮긴다.

- `DependencyFailure`
- `DeliveryCertainty`
- Provider/Capability/Persistence port
- `CapabilityInvocation/Result`
- durable action/final DTO의 engine-facing 부분

변경 방향:

- `run-engine -> execution-contracts`
- `capability-runtime -> execution-contracts`
- `runtime-persistence -> execution-contracts`
- composition은 `krw-agentd`

`Persistence`의 bounded-child default error body를 제거해 구현 누락을
compile error로 바꾼다.

현재 상태: 완료. `crates/execution-contracts`가 ports(DependencyFailure,
DeliveryCertainty, Provider/CapabilityRuntime/Persistence),
CapabilityInvocation/Result, durable DTO, recovery snapshot DTO,
RuntimeStageTimings, deterministic_action_key를 소유한다.
capability-runtime은 run-engine 의존을 완전히 제거했고(Phase B),
runtime-persistence bridge/episode_provider도 contract crate를 본다.
bounded-child persistence 필수 구현 전환은 이전 단계에서 완료.

## Wave 4 — answer-always finalization

테스트를 먼저 추가한다.

- malformed answer sanitizer
- post-response overage
- session memory optional
- compaction totality
- post-commit infallibility
- optional projection isolation

구현:

- `ResearchCompletion` outcome
- `sanitize_answer`
- `fallback_answer_from_ledger`
- core/aux commit 분리
- post-call budget semantics
- total compaction

현재 상태: 완료. post-call budget semantics와 session-memory auxiliary
isolation에 이어 total compaction(oversize는 bounded essential view +
omission receipt, 실패 없음), sanitize_answer + ResearchCompletion
(quality 결함은 AcceptedWithWarnings로 downgrade, integrity만 run.failed),
fallback_answer_from_ledger(dependency/budget 고갈 시 deterministic
ledger 렌더링 final 커밋, UnavailableButAnswerable), post-commit
infallibility(commit 이후 workflow 부속 오류가 final을 뒤집지 않음)까지
구현됐다. run-engine 112 테스트 green.

이 wave는 사용자 답변 성공률을 직접 개선하며 새로운 model turn을 만들지 않는다.

## Wave 5 — MCP result와 delivery certainty

상태: 완료. structuredContent canonical dual-payload 검증, 추가
content/metadata 무시, tools/call at-most-once과 control-read bounded
retry는 선행 작업에 존재했다. 이번에 connect/pool/closed 등 write 전
단계 실패를 NotDispatched+retryable로 세분하고(McpError::is_pre_write),
oversize 성공 결과를 deterministic bounded selection + truncation
receipt로 수렴시켰다(캐시·replay 동일 receipt). Empty/Unavailable/
InputInvalid 구분을 regression test로 고정했다.

- connect-before-send와 after-send 오류 분리
- structuredContent canonical, JSON text fallback
- 추가 content/metadata 무시
- authoritative dual payload mismatch만 hard
- oversize evidence deterministic selection + omitted receipt
- tool application error와 empty result 분리

## Wave 6 — provider registry 일반화

상태: 완료. `ProviderCatalog`(구 DeepSeekProviderCatalog)가
`protocol::ProviderKind` descriptor 기반으로 compile하고 failure
classification도 kind dispatch로 동작한다. model-name if/else,
이중 api-key 필드, unknown model의 DeepSeek 낙하 분류를 제거했다.
동일 interface로 GLM/DeepSeek을 compile하며 credential은 선택된
release에만 요구한다. 잔여: provider_request.rs의 wire-encoding
model 분기는 PinnedExecutionContract 매트릭스 확장이 필요해 별도
작업으로 남긴다(문서화됨).

- `DeepSeekProviderCatalog` -> `ProviderCatalog`
- provider kind/wire codec/credential ref descriptor
- model-name if/else 제거
- duplicate registry 삭제
- GLM과 DeepSeek를 동일 interface로 compile
- provider별 live credential은 선택된 release에만 요구

## Wave 7 — build graph

상태: 완료. build.rs는 checked-in sealed snapshot만 입력으로 사용하며
(`test_sealed_metric_build_inputs.py`로 봉인), ambient sibling fallback과
empty mapping 경로는 없다. admin/release CLI(krw-agent)의 quality
replay/quickstart 하위명령은 `dev-tools` feature(비기본) 뒤로 분리되어
기본 release 그래프에서 kernel/test-support subgraph가 빠진다.

- checked-in ontology runtime snapshot만 사용
- ambient sibling fallback 삭제
- empty generated mapping 삭제
- admin/release CLI에서 quality/http/test-support 분리
- generated canonical contract를 checked-in artifact + verifier로 전환

## Wave 8 — deployment controller

상태: controller core 완료. `bins/krw-agent-deploy`(신규 workspace member)가
명시적 config parser(schema v1, provider 필수, 절대 경로), 15개 read-only
preflight 검사, immutable preflight/terminal receipt, 12단계 forward-only
stage table을 갖는다. `preflight`/`dry-run`/`deploy` CLI에서 build/seal/
activation 단계는 이번 revision에서 fail-closed로 terminal failure
receipt(admission closed)를 남긴다 — 절반 활성화는 불가능하다. 73 테스트.
잔여(별도 세션): stage 3-11 실구현, frontend `prod:deploy:full`의 얇은
adapter 전환, 그 후 Wave 9 삭제. 완료 경로는
`bins/krw-agent-deploy/README.md`에 문서화됐다.

agent repo에 단일 controller를 둔다.

- config parser
- read-only preflight
- receipt writer
- build/seal
- forward migration
- admission close/open
- local/remote activation
- terminal outcome

frontend의 `prod:deploy:full`은 controller adapter만 남긴다.

## Wave 9 — legacy deletion

상태: agent-side 삭제 완료, frontend-side는 계획된 전제 조건 대기 중.

agent repo에서 이미 제거된 항목(Wave 1~7에서 수행):
- `krw_skill_local` phantom binding과 local skill MCP 경로
- `TransportKind::McpStdio/Native` 등 미지원 transport selector
- `flash_*` profile alias와 암묵 `model-registry.yaml`(local/prod 모두)
- launchd installer의 rollback 저장/`--rollback` 경로(forward-only +
  `test_forward_only_launchd_installers.py`)
- `KRW_ONTOLOGY_ROOT` ambient sibling build fallback과 empty mapping 경로
- frontend source crawler `check_product_projection_compat.py`
- dead `DeepSeekProviderCatalog::compile`

frontend repo에 남아 있고 계획된 전제("새 controller가 dry-run과
production에서 검증된 후 즉시 삭제")를 만족하면 삭제할 항목:
- old deploy/pause/recover scripts(deploy-all-production.sh 등)
- `.deploy` phase state machine과 rollback image, newest candidate selection
- frontend `prod:deploy:full`을 controller adapter로 전환

이 전제는 Wave 8 controller의 stage 3-11 실구현과 production 검증을
선행한다(05문서의 forward-only 원칙).

## 검증 명령

최종 검증 결과(2026-08-16, branch `refactor/final-form-waves`):

- `cargo check --workspace --all-targets`: 0 errors
- `cargo test --workspace`: 703 passed, 0 failed
  (run-engine 112, capability-runtime 32, context-compaction 15,
  krw-agent-deploy 73, runtime-persistence 35, tool-mcp 30+1 등)
- `agents/check-all.sh`: all direct-authored agent sources and images verified
- `python3 scripts/test_dual_provider_release.py`: 8/8 OK
- `python3 scripts/test_sealed_metric_build_inputs.py`: OK
- `python3 scripts/test_frontend_deployment_contract.py`: OK
- `python3 scripts/test_forward_only_launchd_installers.py`: 4/4 OK
- `bash -n agents/check-all.sh scripts/dev-stack.sh
  scripts/build_sealed_dual_provider_release.sh`: OK
  (packaging/launchd installer는 forward-only python 검증이 내용을 커버)

```bash
env PATH=~/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin:$PATH \
  cargo check --workspace --all-targets

env PATH=~/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin:$PATH \
  cargo test -p krw-agent-runtime-config

agents/check-all.sh
python3 scripts/test_dual_provider_release.py
bash -n <changed shell scripts>
```

배포 검증은 실제 activation 전에 read-only dry-run receipt까지 수행한다.

## 최종 acceptance

- 단순 회사 질문, 복합 질문, wide research, Guru, 후속 질문이 모두 core answer를 commit
- provider/MCP 장애에도 deterministic unavailable answer
- memory/chart 실패가 text answer를 방해하지 않음
- frontend가 exact same session/run outcome을 읽음
- deploy 실패 후 old daemon crash loop가 발생하지 않음
- subsequent fix-forward deploy가 별도 recovery command 없이 실행 가능
- source tree와 generated artifacts가 분리됨
