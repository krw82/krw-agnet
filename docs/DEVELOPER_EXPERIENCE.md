# KRW Agent 개발자 경험 계약

> **SUPERSEDED — 역사적 DX 초안**
>
> 이 문서는 구현 전 CLI/DX 아이디어를 보존한다. 현재 구현 권한은
> [`IMPLEMENTATION_STATUS.md`](./IMPLEMENTATION_STATUS.md)다. 아래의 importer, publish/rollback,
> migration, TypeScript fallback 및 미구현 command는 현행 약속이 아니다.

작성일: 2026-08-02  
상태: 역사적 참고 자료  
대상: private-project maintainer와 platform engineer

## 1. 목적과 사용자

주 사용자는 현재 `krw-ontology-front`와 `krw-ontology`를 유지하면서 DeepSeek 전용 agent runtime을
도입하는 소수의 maintainer다. 이 사용자는 agent algorithm은 이해하지만 Rust daemon, image compiler,
Postgres ABI, MCP fixture를 각각 수동 조립하고 싶지는 않다.

첫 경험은 다음 두 질문에 즉시 답해야 한다.

1. 이 runtime이 plugin 의미를 agent 판단에 실제로 내장했는가?
2. 실패했을 때 무엇이 잘못됐고 어떻게 안전하게 복구하는가?

## 2. 두 개의 시간 계약

### 2.1 Time to first helpful result

지원 대상 2 vCPU/4 GB macOS 또는 Linux 머신에서 signed CLI 설치가 끝난 시점부터 다음 명령의
validated `AnswerBundle` 출력까지 p50 60초, p95 120초 이하다.

```bash
krw-agent quickstart --fixture vertical-slice
```

이 경로는 API key가 필요 없는 pinned fake DeepSeek provider, representative read-only MCP fixture,
disposable local Postgres와 unsigned development AgentImage를 사용한다. 실행 결과는 answer, citation,
`img_` image ID, `run_` run ID, trace ID를 함께 보여 준다. fixture임을 결과 상단에 명확히 표시하며
production 품질을 주장하지 않는다.

live 환경 확인은 별도다.

```bash
krw-agent doctor --live
krw-agent run --agent krw-ontology --question '최근 두 보고기간을 비교해줘'
```

`doctor --live`는 credential 값을 출력하지 않고 존재, scope, provider contract, model allowlist만
redacted probe한다. secret은 CLI 인자나 shell history에 직접 넣지 않고 OS keychain 또는 지정 secret
provider에서 읽는다.

### 2.2 Time to embedded plugin

clean checkout에서 template 생성 시작부터 representative read-only MCP capability가 conformance와
fixture replay를 통과하고 cited AnswerBundle을 만들 때까지 median 30분 이하다.

```bash
krw-agent plugin new issuer-filings --template readonly-mcp
$EDITOR agents/issuer-filings/agent.yaml
krw-agent plugin check agents/issuer-filings
krw-agent image build agents/issuer-filings --dev
krw-agent plugin test agents/issuer-filings --fixture vertical-slice
krw-agent replay vertical-slice --agent issuer-filings
```

`plugin new`는 capability descriptor, schemas, evidence mapping, budget, fixture, contract test를 모두
생성한다. 작성자가 kernel Rust code, process supervisor, raw SQL을 수정하게 만들면 실패다.

시간 측정은 새 maintainer가 문서를 연 순간부터 마지막 replay까지 wall clock으로 기록한다. CI는
generated template의 automated path를 측정하고, Phase 2 exit 전 최소 3회의 human walkthrough를 한다.

## 3. 단일 golden-path CLI

사용자 표면은 `krw-agent` 하나다. `krw-agentd`는 supervised daemon이고 `krw-agentc`는 compiler
implementation binary일 수 있지만, 일반 문서와 오류 recovery는 다음 noun 체계만 사용한다.

```text
krw-agent quickstart
krw-agent doctor [--fix] [--live]
krw-agent run | replay | status | inspect
krw-agent baseline freeze | verify
krw-agent plugin new | check | test
krw-agent image build | diff | sign | publish | rollback
krw-agent migrate inspect | apply | rollback
krw-agent debug-bundle
```

- 모든 명령은 stable `--json` envelope와 documented exit code를 제공한다.
- resource ID는 `run_`, `img_`, `cap_`, `ev_`, `act_` prefix를 가진다.
- mutating command는 실행 전 대상과 diff를 보여 주고 `--dry-run`을 지원한다.
- `doctor --fix`는 secret rotation, destructive migration, permission 확대를 자동 수행하지 않는다.
- terminal 출력은 사람이 읽고 다음 행동을 복사할 수 있어야 한다.

## 4. 오류와 recovery 계약

모든 오류는 같은 순서로 표시한다.

```text
KAI-1002  AgentImage schema is newer than this daemon.
Problem:  img_... requires ABI 3; running daemon supports ABI 2.
Cause:    image and daemon were upgraded out of order.
Fix:      krw-agent image rollback img_...  # safe, no active-run rewrite
Docs:     docs/errors/KAI-1002.md
Trace:    tr_...
Expected: ABI <= 2
Actual:   ABI 3
```

- error registry의 모든 code에는 copy/paste recovery, rollback, data-loss 여부, escalation condition이 있다.
- `krw-agent debug-bundle --run run_...`은 prompt, raw response, credential, personal payload를 제외한
  manifest hash, state transitions, error codes, resource counters만 묶는다.
- `doctor`는 DB ABI, daemon generation, image signature/revocation, provider probe, MCP schema/build/data
  release, auth-scope pool, disk/CAS quota를 한 번에 진단한다.
- 실패 후 같은 idempotency key로 resume할지 새 run을 만들지 명령이 명확히 알려 준다.

## 5. AgentImage authoring journey

template이 만드는 최소 source는 다음 의미를 명시한다.

```text
identity -> goal -> capability -> input/output schema
         -> evidence mapping -> auth scope -> idempotency
         -> resource budget -> stop/verifier rules -> fixtures
```

`plugin check`는 schema validation만 하지 않는다. 다음을 local에서 빠르게 검사한다.

- capability ID/version과 canonical schema hash
- MCP protocol/server build/data release pin
- evidence directness와 strong-claim boundary
- auth-scope cache/pool isolation
- bounded output, timeout, cancellation, idempotency
- ResearchGraph termination과 unreachable workflow state
- current and previous runtime/image ABI compatibility

build 결과는 prompt byte diff가 아니라 semantic diff를 출력한다. 예를 들어 capability 추가, auth scope
확대, evidence grade 변경, budget 증가, verifier 완화는 각각 별도 위험으로 보인다.

## 6. Upgrade journey

```bash
krw-agent migrate inspect --to vNext
krw-agent image diff img_old img_new
krw-agent migrate apply --canary general-research --dry-run
krw-agent migrate apply --canary general-research
krw-agent status --watch
krw-agent migrate rollback
```

- N/N+1 daemon, DB ABI, AgentImage 조합을 지원하고 N→N+2는 drain/migrate가 필요하다.
- active run은 old image에 pin되고 idle session은 image bytes를 pin하지 않는다.
- migration은 affected run kind, session count, rollback window, irreversible step을 사전에 보여 준다.
- codemod가 가능한 AgentSpec 변경은 compiler가 제안하되 source를 자동 overwrite하지 않는다.

## 7. Journey map

| 단계 | 사용자의 질문 | 시스템이 주는 확신 | 합격 기준 |
|---|---|---|---|
| quickstart | 정말 동작하나? | fixture AnswerBundle + citation + IDs | p95 <= 120초 |
| live doctor | 실제 DeepSeek/MCP가 준비됐나? | redacted readiness와 정확한 fix | common failure 해결률 >= 90% |
| plugin scaffold | 어디부터 써야 하나? | 완전한 read-only template | 첫 edit까지 <= 5분 |
| check/build | core를 깨뜨렸나? | semantic diff와 contract failure | local feedback p95 <= 10초 |
| replay | 답변 품질이 유지되나? | deterministic fixture diff | median total <= 30분 |
| canary | production에 안전한가? | run-kind gate와 rollback timer | critical regression 0 |
| incident | 왜 실패했나? | redacted bundle와 one-step recovery | resume/rollback 성공 >= 99% |

## 8. Magical moment

maintainer가 typed capability descriptor와 evidence rule만 추가한 뒤 `krw-agent replay`를 실행한다.
CLI는 kernel code나 새 process 없이 semantic image diff를 보여 주고, 새 capability로 얻은 evidence가
어느 claim을 지지했는지 연결된 cited answer를 출력한다. 이것이 “plugin을 실행했다”가 아니라
“agent 판단에 내장했다”는 첫 체감 지점이다.

## 9. 현재와 목표 scorecard

| 항목 | 현재 계획 | Phase 2 목표 |
|---|---:|---:|
| first helpful result | 0/5 | 5/5 |
| local reproducibility | 1/5 | 4/5 |
| plugin authoring/conformance | 1/5 | 4/5 |
| diagnostics/recovery | 2/5 | 4/5 |
| safe defaults/upgrade | 2/5 | 4/5 |
| overall | 1.2/5 | 4.2/5 이상 |

현재 점수는 source가 없어서 낮다. 목표 점수는 문서의 존재가 아니라 timed walkthrough, fixture CI,
error-recovery drill 결과로만 인정한다.

## 10. Phase 배치

- Phase 0: 두 시간 지표의 start/end/profile을 pre-register하고 quickstart UX fixture를 freeze한다.
- Phase 1: keyless quickstart와 production-shaped `local-contract` profile을 vertical slice와 함께 만든다.
- Phase 2: `plugin new/check/test`, semantic image diff, 30-minute human walkthrough를 exit gate로 둔다.
- Phase 4: doctor/debug bundle/migration/rollback을 failure matrix와 함께 harden한다.
- Phase 7: WASI가 실제 필요할 때 SDK를 추가한다. 기본 read-only plugin DX를 Phase 7까지 미루지 않는다.

이 문서의 command는 아직 구현되지 않았다. 구현 전 contract이며 README는 runnable로 오해시키면 안 된다.
