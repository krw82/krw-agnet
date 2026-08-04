# KRW Agent deferred and operator actions

이 파일에는 운영자 조치와 실측 trigger가 필요한 항목만 둔다. 현행 구현 기준은
[`docs/IMPLEMENTATION_STATUS.md`](docs/IMPLEMENTATION_STATUS.md)다.

## Live 실행 전 운영자 조치

- [ ] **T001: 노출 가능성이 생긴 DeepSeek API credential revoke/rotate**
  - 이유: read-only 조사 명령의 tool output에 설정값이 출력된 정황이 있었다.
  - 완료 증거: old credential invalidation, new secret-manager version, redacted smoke, repository/log scan.
  - secret 값이나 일부 문자열도 이 저장소에 기록하지 않는다.

## Empirical promotion decisions

- [ ] **T010: Rust promotion thresholds pre-register**
  - 기본값은 architecture의 process-tree `<=75%`, active slope `<=60%`, orchestration p95 `<=10ms`,
    latency/throughput 비열등이다. baseline과 측정식을 candidate 결과 전에 고정하고 결과를 본 뒤
    기준을 완화하지 않는다.
- [ ] **T011: optional DRF activation**
  - trace에서 dominant-resource noisy-neighbor가 확인되고 DRR baseline 대비 fairness 개선 및
    throughput/fragmentation 손실 5% 이하일 때만 켠다.
- [ ] **T012: native capability promotion**
  - 기존 MCP/sidecar를 제거하고 end-to-end p95 또는 PSS가 10% 이상 개선될 때만 native로 옮긴다.

## Deferred extensions

- [ ] **T020: WASI SDK/runtime**
  - Capability ABI 밖의 실제 third-party local transform 수요가 확인된 뒤에만 시작한다.
  - 기본 daemon binary/RSS에는 WASM engine을 넣지 않는다.
- [ ] **T021: Windows service/installer**
  - protocol은 named pipe를 미리 보존하지만 첫 release target은 macOS/Linux arm64/x64다.
  - Windows packaging은 Phase 7 conformance 이후 진행한다.
- [ ] **T022: beam/bandit/MCTS research**
  - width-2 beam과 macro-policy bandit은 equal-budget Pareto 우위가 있을 때만 canary한다.
  - MCTS/LATS는 faithful cached simulator, calibrated value, actual outcome replay, zero external rollout
    call을 모두 만족할 때 offline 연구로만 재검토한다.

현재 unresolved user choice는 없다. 위 항목은 측정 또는 명시된 trigger가 생길 때만 열린다.
