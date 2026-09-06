# 자유 문(open_research) E2E 품질 테스트 결과 — 2026-09-04

비전 `docs/superpowers/specs/2026-09-06-tickerless-open-research-vision.md` §8
성공 기준에 대한 라이브 실증. 격리 dev 스택(포트 14518 게이트웨이, 파일럿
유니버스 17종목, GLM-5.3-flash glm_high)에서 2라운드 실행: 1라운드(실패)
→ 사후분석 → 하드닝(커밋 0268816, 03039bb, 57824e8, e0e9737) → 2라운드.

## 요약 판정

| # | 질문 | 문 | 런 | 판정 | 비고 |
|---|---|---|---|---|---|
| 1 | 반도체 병목 관련 회사 있나? (회사별 근거·지표) | 자유 | run_e9fb…aba0 (final) | **통과** | INTC·AMD·ADI·NXPI·AMAT 공시 문구 근거 + MU·NVDA 등 미확보 고지 |
| 2 | 인플레이션 오르면 유리한 섹터/종목? (거시 지표) | 자유(0티커) | run_f63c…af23 (final) | **부분 통과** | 어드바이저리 라벨·필링급 역직렬화 우수, 거시 시계열 관측 미실행(정직 고지) |
| 4 | SO 최근 실적은? | 회사 | run_a147…1d8 (final) | **통과** | §8 기준 4 회귀 — 필링급 수치 답변, 품질 손상 없음 |
| 5 | PLTR 최근 실적은? (미커버) | 자유 | run_419d…1fe (final) | **부분 통과** | 커버리지 밖 고지 + 무결성(추정 거부) + openbb 동반종목 수치 수신; PLTR 자체 수치 openbb 조회는 미실행 |

vendor 토큰(fmp/fred/polygon) 노출: **전 런 부재**.

## 1라운드 실패와 사후분석 (하드닝의 근거)

- Q1 run_f55f: 모델이 발견한 회사 드릴다운에 `tickers`(11개)/`ticker`(MU)
  인자 → question_only 스코프 불변식 위반 → 수리 예산 1회 이미 소진 +
  에피소드에 호출 2개 → 탈출 불가 → terminal(`immutable_scope_failure`).
- Q2 run_8805: 초기 발견 8조항 성공 + 거시 시계열 4개·market_series 1개
  실제 수신 후, 재발견 제안 7조항이 런 전체 12조항 상한 초과 → append
  탈출 경로가 회사 차선 도구명(`ontology.query_context`)을 하드코딩해
  유니버스 변형을 못 알아봄 → terminal(`model_proposal_rejected`).
- 에피소드 아티팩트 복호화(dump_episode) + agentd 로그로 위 경로 전부
  입증. 상세는 대화 로그 및 커밋 메시지 참조.

## 하드닝 변경 (2라운드 전 커밋)

1. **엔진**: append 탈출 경로가 `ontology.query_context_universe`도 수용
   (+회귀테스트). run-engine 178 테스트 green.
2. **워크플로(open_research_v1)**: assess_frontier의 재발견
   (append_context_plan) 에지 제거 — 발견은 1라운드, 후속은 topic 질의
   (조항 예산 무관)·시장 플레인·거시·뉴스로. `proposal_unrecoverable →
   compose_ir` 탈출 에지 추가.
3. **예산(소유자 지시, local+prod)**: 연구 프로필 전반 max_repairs 10,
   턴/호출 2배, 자유 문 60턴/60호출/2h 데드라인/32MB 증거. 단
   입력+출력 ≤ GLM-5.3 컨텍스트 204800 물리 제약에 맞춰 재분배
   (자유 문 167000+36864).
4. **프롬프트**: 하드 룰 3개(유니버스 역량 티커 인자 금지 / 결정당 호출
   1개 / 발견 1라운드) + 증거 등급 교리(필링급=사용자·서버 보증 스코프만,
   모델 선택 회사는 시장·뉴스급 라벨).
5. **에이전트서비스 라이브 버그 2건 수정**:
   - 디스패처가 OpenAI 호환 채널(잔액 없음 429)을 써 전 질문 `None`
     → 엔진과 동일한 Anthropic Messages 채널로 전환(57824e8). 실측:
     SO·PLTR·NVDA 추출, 회사명(삼성전자)은 변환 거부.
   - 커버리지 카탈로그가 `/mcp` 307 리다이렉트를 못 따라가 전건
     fail-open → follow_redirects + URL 보존(e0e9737). 수정 후
     covers(SO)=True / covers(PLTR)=False 실증.

## 2라운드 실증 상세

- **Q2가 수리 2회를 소진하고도 생존·완결** — 예산 인상의 직접 효과
  (구 예산 1이면 즉사하는 경로).
- **문 선택(전체 경로)**: "SO 최근 실적은?" → 디스패처 `SO` 추출 →
  커버리지 True → 제출 body에 ticker 키 → `run_kind: company_research`,
  `context: {kind: company_ticker_set, tickers: ["SO"]}` 스냅샷으로 확인.
  "PLTR" → 디스패처 `PLTR` → 커버리지 False → ticker 키 미포함 제출 →
  자유 문 진입 확인.
- **Q5 PLTR 답변**: "이번 조사에서 확보된 자료가 전혀 없어 어떤 숫자도
  근거 없이 쓸 수 없다" + 파일럿 유니버스(반도체 17종목) 커버리지 설명 +
  동반 종목(NVDA·AMAT·INTC) 실적 수치는 openbb에서 실제 수신 — 무결성과
  커버리지 고지는 완벽, 시장 플레인으로 PLTR 자체 조회까지 가지는 않음.

## 남은 개선 (후속)

1. **거시 시계열 관측 의무 강화**: Q2처럼 거시 질문에서 macro_series 호출
   없이 "미확보"로 넘어가는 경우 — assess_frontier 프롬프트에 관측 의무
   명시 또는 거시 목표 계획 시 강제화 검토.
2. **시장 플레인 적극 사용 유도**: 미커버 티커 질문에서 openbb 자체
   조회를 모델이 선택하도록 프롬프트/워크플로 유도(예: "커버리지 밖
   회사 실적은 openbb.income_statement로 조회" 안내).
3. **open_research_en(en-US 로케일)**: 계속 연기 상태 — 한국어 질문에
   한국어 답변은 정상 동작.
4. 파일럿 유니버스 한계: 커버리지 답변은 17종목(반도체 중심) 기준임을
   답변이 스스로 고지하고 있어 정직성은 유지.

## 환경 비고

- 스택: `scripts/with_local_env.sh scripts/start_local_agent_gateway_stack.sh`
  (상태 `.local/e2e-open`, 포트 14518/55433/19433/20433, openbb TLS 프록시
  20434, 커버리지 사이드카 8188, 에이전트서비스 8391).
- 오프라인 검증: run-engine 178, agent-service 90, check-all 전 패키지
  통과. mimosa 전체 스캔 실시(138건 정적 소견, 종전 기준선과 동일
  수준, 실드 scan-2026-09-06T12-47-48).
