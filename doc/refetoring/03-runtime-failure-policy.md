# 런타임 실패 정책

## 목적

사용자 질문이 admission을 통과했다면 가능한 모든 경우에 답변을 남긴다. 답변은 근거 수준에 따라 완전 답변, 제한 답변, dependency unavailable 안내가 될 수 있다. integrity 위반만 `run.failed`다.

## 통합 outcome

```rust
enum ResearchCompletion {
    Accepted,
    AcceptedWithWarnings,
    UnavailableButAnswerable,
    IntegrityFailure,
}
```

### Accepted

질문 범위에 맞는 evidence와 claim이 있고 core final이 commit됐다.

### AcceptedWithWarnings

일부 objective, period, document 또는 citation이 부족하지만 검증된 범위 내에서 유용한 답을 만들 수 있다. unsupported claim은 제거하고 limitation을 표시한다.

### UnavailableButAnswerable

provider/MCP/dependency 문제가 bounded deadline 안에 해결되지 않았다. 내부 경로나 secret은 노출하지 않고 이번 연구가 완료되지 못했다는 deterministic 안내를 commit한다. 이것은 정상 research success와 별도 completion class다.

### IntegrityFailure

다음에만 사용한다.

- ownership mismatch
- image/contract/deployment pin mismatch
- fence/cancel mismatch
- request hash/action receipt mismatch
- forged/unknown evidence reference
- uncommitted calculation lineage
- ambiguous dispatch를 재실행하려는 시도
- final atomic commit이 실패하거나 결과가 불명확함

## answer sanitizer

모델 출력은 다음 순서로 정리한다.

1. parse 가능한 section/claim만 보존한다.
2. unknown evidence를 참조하는 claim은 제거한다.
3. calculation이 없는 numeric claim은 숫자 단정을 제거하거나 qualified fact로 완화한다.
4. counter evidence가 없는 interpretation은 fact/limitation으로 낮춘다.
5. unsupported goal binding은 실제 evidence support에 맞춰 다시 계산한다.
6. 빈 section을 제거한다.
7. heading을 locale에 맞춰 정규화한다.
8. follow-up은 유효한 것만 0..N개 보존한다.
9. 답할 material이 없으면 ledger fallback으로 전환한다.

sanitizer는 evidence를 만들거나 claim을 강하게 승격하지 않는다.

## ledger fallback

fallback은 추가 LLM turn을 만들지 않는다. 이미 admitted된 ledger에서 다음을 결정적으로 렌더한다.

- 질문의 ticker와 scope
- 직접/metric lineage evidence의 핵심 사실
- qualified/related evidence의 제한된 시사점
- uncovered objectives
- dependency/query status
- 최신/기간 경고

근거가 전혀 없으면 이유를 구분한다.

- `not_disclosed`
- `not_indexed`
- `retrieval_empty`
- `dependency_unavailable`
- `out_of_scope`

MCP 오류를 회사 미공시로 표현하지 않는다.

## budget

- pre-dispatch에는 deadline, turn, token, capability call budget을 적용한다.
- response 수신 후 overage는 현재 response를 버리지 않고 다음 dispatch를 금지한다.
- thinking 최소 token보다 남은 output이 적으면 direct finalization으로 전환한다.
- input+output이 provider context를 넘으면 output cap 조정, compaction, fallback 순으로 처리한다.
- model repair, provider transport retry, capability recovery budget을 분리한다.

## MCP delivery certainty

- DNS, connect, pool acquire, write 전 실패: `NotDispatched`
- request body 전송 이후 timeout, stream 절단: `MayHaveDispatched`
- remote dedupe 계약이 없는 `tools/call`: at-most-once
- control read만 bounded retry

## session memory

- ownership/hash가 맞는 memory만 주입한다.
- memory read/rebuild 실패는 `None`으로 실행을 계속한다.
- final answer commit과 memory commit을 같은 성공 조건으로 묶지 않는다.
- memory 실패는 다음 turn 품질 저하 신호이지 현재 답변 삭제 사유가 아니다.

## compaction

compaction은 total function이어야 한다.

1. identity, trusted scope, current question, unresolved required goal을 우선 보존한다.
2. ticker/period/goal별 최소 evidence를 reserve한다.
3. 나머지를 rank하여 잘라낸다.
4. omitted count, reason, hash receipt를 남긴다.
5. metadata가 커도 bounded essential view를 항상 만든다.

## presentation

- chart는 optional이다.
- malformed/oversized presentation pack은 text capability result를 실패시키지 않는다.
- evidence ref가 모두 current ledger에 있을 때만 artifact를 노출한다.
- frontend projection에서 chart 오류는 chart만 누락한다.

## required regression matrix

- malformed AnswerIR + repair 0에서도 admitted fact만 commit
- numeric lineage 누락 시 downgrade 후 commit
- provider response 이후 budget overage에서도 current answer commit
- 512개 초과 evidence/8192개 초과 fact에서도 compaction receipt와 final 생성
- connection-before-send는 retry, after-send timeout은 ambiguous
- structured content + 추가 metadata/content는 성공
- memory hash 오류는 stateless 실행과 answer commit
- DB commit 뒤 workflow edge 오류가 API final을 뒤집지 않음
- 64 KiB~1 MiB Markdown과 malformed optional projection에서 core answer read 가능

