# 로컬 피드백 루프

이 프로젝트의 로컬 개발 경로는 **한 번 띄운 스택을 재사용하고, 바뀐 계층만 확인하는 것**을
기본으로 한다. 질문마다 PostgreSQL·MCP·Gateway·daemon을 다시 시작하거나 AgentImage를 다시
만들지 않는다.

## 1. 세 개의 실행 레인

```text
코드 편집
  ├─ Rust kernel/provider 변경      → core 또는 adapter 단위 테스트
  ├─ Python ontology/market 변경    → 기존 스택 reload --skip-prepare + smoke
  ├─ TypeScript Gateway 변경        → 기존 스택 reload --skip-prepare + smoke
  └─ AgentSpec/prompt 변경          → image fingerprint 갱신 + smoke/quality

품질 확인이 필요할 때만
  └─ 이미 떠 있는 Gateway에 GLM 질문을 보내고 원문·trace·usage를 저장
```

각 레인은 서로 다른 실패를 확인한다. Rust 단위 테스트가 통과했다고 GLM 답변 품질이 보장되는
것은 아니며, 반대로 live quality 한 건이 통과했다고 전체 kernel 계약이 검증되는 것도 아니다.

## 2. 권장 명령

```bash
# 처음 한 번만: Rust daemon과 Gateway를 만들고 스택을 계속 유지
./scripts/dev-stack.sh prepare
./scripts/dev-stack.sh up

# Rust 변경을 빠르게 확인
./scripts/dev-stack.sh test core
./scripts/dev-stack.sh test adapter

# Python/TypeScript만 바꾼 경우: Rust 빌드는 건너뛰고 프로세스만 재기동
./scripts/dev-stack.sh reload --skip-prepare

# 실제 GLM 품질은 재사용 중인 Gateway에 소수 smoke부터 보냄
KRW_AGENT_GATEWAY_TOKEN=... \
  ./scripts/dev-stack.sh test smoke --case short_aapl_price_drop
```

`--skip-prepare`는 Rust binary나 AgentImage를 바꾸지 않았을 때만 사용한다. Rust 코드, AgentSpec,
prompt, image contract를 바꿨다면 먼저 `prepare`를 실행한다. `up`은 준비된 supervisor를 재사용하므로
매 질문마다 cold stack을 시작하지 않는다.

## 3. 캐시와 재빌드 경계

- Cargo incremental cache는 `target/` 하나를 지속해서 사용한다. 같은 상태의 재빌드는 Cargo가
  0초대에 종료한다.
- 개발 profile은 line-level debug symbol을 만들지 않아 linker 산출물과 macOS 파일 메타데이터
  작업을 줄인다. release/production profile은 변경하지 않는다.
- AgentImage·release authorization·deployment descriptor는 입력 fingerprint별로 로컬 state에
  보관한다. 동일 입력이면 image build/sign을 반복하지 않는다.
- PostgreSQL은 supervisor 종료 때도 유지한다. 재기동 시 migration checksum만 확인하고 pending
  migration만 적용한다.
- 품질 runner는 질문을 직접 서버에 보내며, 로컬 stack을 시작하지 않는다. 따라서 품질 시간은
  GLM·MCP 작업 시간만 측정한다.

## 4. 무엇을 매번 하지 않는가

- 질문마다 Rust compile, AgentImage compile, plugin subprocess spawn을 하지 않는다.
- `cargo test --workspace`를 모든 편집의 기본 게이트로 사용하지 않는다. 그것은 release/CI 전용
  넓은 검증이다.
- fixture가 planner objective/clause 개수를 한 값으로 고정하지 않는다. 품질 검사는 실제 근거
  커버리지, 불확실성 표현, 영향 분석, Markdown/trace/usage 존재를 본다.
- chain capability를 모든 질문에 강제하지 않는다. 필요한 근거가 없을 때만 ontology 조회를
  추가하고, 불필요한 왕복은 줄인다.

## 5. 실패와 과금 관측

최종 및 terminal failure 응답에는 가능한 provider usage를 함께 투영한다. GLM 입력 token이
제공되지 않는 output-only 응답은 `token_usage_status=output_only`로 표시하고 실제 output token만
billable로 계산한다. 형식이 모호한 usage는 성공으로 인정하지 않아 과금 중복을 막는다.

live 품질 report에는 질문 원문, Markdown 원문, terminal receipt, 제한된 action trace, provider
turn/retry/usage가 남는다. transport가 성공해도 사실성·근거 품질은 자동으로 `PASS`라고 하지
않고 `manual_review_required`로 남긴다.

## 6. 설계의 의도

이 구조는 OpenBB식으로 종목별 snapshot/store를 capability 계층에 모으되, 새 데이터베이스나
범용 plugin 실행기를 추가하지 않는다. 현재 market snapshot router의 bounded cache와 기존
ontology release가 데이터 라우팅 경계다. Claw-code에서 유용한 persistent session/usage/skill
표면은 유지하지만, 임의 코드를 runtime에 주입하는 plugin loader는 두지 않는다. 그래야 변경
범위와 실패 지점이 늘지 않는다.
