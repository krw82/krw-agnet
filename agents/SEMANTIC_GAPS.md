# KRW Agent semantic-admission inventory

이 문서는 AgentSpec이 compile된다는 사실과 production admission을 구분한다. 현재 source는
plugin importer나 prompt-only escape hatch를 쓰지 않는다. AgentImage가 선택할 수 있는 것은 closed
workflow, closed capability mapping, pinned contract, bounded validator와 closed Rust handler뿐이다.

## 현재 code로 닫힌 의미

- 8개의 직접 작성 package와 모든 public KRW entrypoint가 static release set에 들어간다.
- registry에는 ontology, kernel, front feed/공개 filing, default sealed Guru, router/notebook/display의
  51개 canonical contract와 exact hash가 있다.
- `SearchPlan v2`/`ResearchState v2`의 server-owned 의미는 Python authority export/conformance vector에
  고정되고, Rust는 source runtime을 import하지 않는다.
- feed와 source-filing의 request/result identity, accession/CIK/SEC URL, prior list membership은
  capability call과 recovery replay 양쪽에서 검증된다. personal filing capability는 source와 binding
  catalog에 없다.
- Guru는 fixed author, sealed question/brief, company context, review evidence linkage를 typed contract로
  고정하며, 부모 예산 예약을 소비하는 depth-1 durable child receipt로 실행된다.
- routing decision은 immutable routing request에, notebook output은 input hash/ticker에, display plan은
  committed answer receipt와 source-unit order에 각각 bound된다. 이 세 product path는 더 이상
  “prompt에만 적힌 향후 계약”이 아니다.
- provider/model state, capability state, builtin verifier, terminal state 모두 `StateArtifactEnvelope`와
  한 개의 typed interpreter로 transition한다. 임의 hook, validator script, dynamic native code는 없다.

## production 전에 외부 authority가 제공해야 하는 것

1. **실제 deployment fingerprints** — example binding의 zero data-release hash와 fixture build를 실제
   server schema bundle/build/data release 값으로 바꿔야 한다. runtime은 그 값 없이 production mode를
   시작하지 않는다.
2. **credentialed MCP acceptance** — pinned server가 expected request/result/attestation을 실제로
   반환하고, run/principal scope가 leak 없이 유지되는지 live fault/recovery/soak으로 확인해야 한다.
3. **upstream producer envelope caps** — Rust consumer는 field/array/whole-response를 fail-closed로
   제한하지만, 현재 front TypeScript producer 일부는 serialization 전에 동일한 cap을 강제하지 않는다.
   producer-side cap은 webapp integration이 허용될 때 추가해야 한다.
4. **feed context integrity** — `krw-feed-context/v2.context_hash`가 일부 nested object의 JavaScript
   insertion order에 의존한다. Rust는 shape를 확인하지만 generic JSON 재직렬화 뒤 독립적으로 hash를
   다시 계산할 수 없다. producer가 RFC 8785 bytes를 hash하거나 raw envelope를 sign하기 전에는 이
   field를 독립 integrity proof로 승격하지 않는다.
5. **trusted host materialization** — existing answer, authenticated ownership, product outbox/SSE는
   [`packages/host-ts`](../packages/host-ts/) kit과 typed ABI까지 준비돼 있지만, 요청대로
   `krw-ontology-front`에 wiring하지 않았다. browser JSON은 이 authority를 만들 수 없다.
6. **live quality/performance evidence** — credential rotation 뒤 exact Flash model, actual data release,
   numeric/citation quality, end-to-end latency, and long soak evidence는 fixture로 대체할 수 없다.

## 허용하지 않는 우회

다음은 위 admission 항목을 해결하는 방법이 아니다.

- SKILL.md 전체를 `prompt_text`로 넣고 typed policy가 구현됐다고 주장하기
- custom JavaScript/Python/Rust validator를 daemon에서 실행하기
- source runtime의 DB, user/session, billing 값을 AgentImage에 넣기
- 미확정 MCP result를 generic JSON이나 model 설명으로 evidence화하기
- 실제 endpoint/pin이 없을 때 fixture hash나 model alias를 production에 쓰기

새 capability나 product workflow가 필요하면 canonical schema, bounded Rust validator, AgentSpec pin,
binding key, capability normalization, recovery replay test를 함께 추가한다. 외부 evidence가 필요한
항목은 [`TODOS.md`](../TODOS.md)와
[`docs/IMPLEMENTATION_STATUS.md`](../docs/IMPLEMENTATION_STATUS.md)에만 남긴다.
