# 목표 아키텍처

## 계층

```text
protocol + canonical contracts
            |
            v
execution-contracts
  - provider port
  - capability port
  - persistence port
  - durable DTO
  - dependency outcome
            |
            v
run-engine
  - orchestrator
  - active run
  - recovery
  - finalization policy
            |
      +-----+-----+
      |           |
      v           v
local-builtins   remote-capability-runtime
  skill.load       MCP adapter
                   evidence normalization
            |
            v
runtime-persistence + provider runtime
            |
            v
krw-agentd composition root
```

상위 crate가 하위 구현 crate를 의존하지 않는다. `capability-runtime`이 `run-engine`의 trait를 가져가는 현재 방향을 `execution-contracts`로 역전한다.

## AgentImage capability 실행 대상

최종 schema는 실행 위치를 명시한다.

```yaml
execution:
  kind: remote
  binding_key: krw_ontology_query
```

```yaml
execution:
  kind: local
  builtin: skill_load
```

규칙은 다음과 같다.

- remote만 `DeploymentBinding`과 `ResolvedCapabilityFingerprint`를 가진다.
- local은 image hash와 prompt blob hash로 이미 봉인된다.
- local은 MCP pool, endpoint, credential, readiness와 무관하다.
- local builtin 종류는 closed enum이다. 모델이 arbitrary program/path를 고르지 않는다.
- local 결과는 evidence ledger에 들어가지 않는다.
- local 실패는 provider/transport retry 예산을 소비하지 않는다.

## run-engine 모듈 경계

최종 production 모듈은 다음과 같다.

```text
run-engine/src/
  lib.rs                 public exports only
  ports.rs               temporary facade; execution-contracts 전환 후 축소
  transcript.rs          internal provider transcript and scrubbing
  provider.rs            provider adapter and wire failure mapping
  recovery.rs            checkpoint/replay/recovery directives
  orchestrator.rs        main loop only
  active_run.rs          budgets, ledger, program state
  capability_dispatch.rs argument derivation, action receipts, dispatch
  provider_request.rs    prompt/tools/output encoding
  finalization.rs        sanitize, fallback, commit
  validation.rs          integrity validation only
  bounded_child.rs       isolated child execution
  timings.rs             timing DTO
```

`lib.rs`는 다른 crate가 필요한 타입을 re-export하고 orchestration 구현을 담지 않는다.

## provider architecture

provider는 model 이름 if/else로 분기하지 않는다.

```text
ModelDescriptor
  provider_kind
  wire_codec
  endpoint_ref
  credential_ref
  context_limit
  output_limit
  thinking policy
  wire capabilities
```

`ProviderCatalog`는 descriptor를 읽어 concrete adapter를 만든다. error classification도 `provider_kind + wire phase`로 결정한다. unknown provider는 startup registry compilation에서만 실패하며 모델 turn 중에는 새 provider 선택 실패가 생기지 않는다.

## capability 결과

원격 결과는 다음 outcome으로 정규화한다.

```rust
enum CapabilityOutcome {
    Complete(NormalizedCapabilityResult),
    Empty(QueryStatus),
    InputInvalid(QueryStatus),
    Unavailable(QueryStatus),
    Ambiguous(DispatchReceipt),
}
```

- `Empty`와 `Unavailable`을 합치지 않는다.
- `InputInvalid`는 evidence 없음으로 캐시하지 않는다.
- `Ambiguous`는 재실행하지 않는다.
- pagination, has_more, effective filter, warning, source anchor를 bounded query status로 보존한다.
- optional presentation sidecar는 capability success 여부와 분리한다.

## finalization

```text
model output
  -> parse
  -> integrity check
  -> sanitize/downgrade/drop unsupported claim
  -> if useful answer remains: commit
  -> otherwise ledger fallback
  -> atomic core commit
  -> best-effort memory/presentation/telemetry
```

core final은 다음만 필수다.

- run/session/tenant identity
- rendered Markdown 또는 deterministic unavailable answer
- answer hash
- cited evidence refs
- completion class

chart, memory delta, usage projection, diagnostics는 optional envelope field다.

## frontend 연결

frontend는 다음 두 파일만 본다.

1. `public-release.json`: runtime descriptor, run kinds, provider, hashes
2. `agent-v1-deployment-contract.json`: DB RPC/column/projection contract와 host limits

agent는 frontend route나 migration directory를 읽지 않는다. frontend는 agent source checkout 내부 installer 경로를 조립하지 않는다. controller가 release manifest에서 installer/action을 해석한다.

