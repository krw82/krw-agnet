# Financial-Services 패턴 이식 — openbb 데이터 평면 확장 × 코드 계산 × 위젯 산출

- 날짜: 2026-08-31
- 상태: 사용자 지시 확정 ("financial-services-plugins 내부에 있는걸 구현해야할것같아… 장기적인 최선으로 엄격하고 정확하게… 과설계 금지… 모든 오류가 안나오게끔")
- 소스 분석: `~/Desktop/financial-services-plugins` (Anthropic 공식, Apache 2.0, 41스킬·38커맨드·11 MCP) — 4도메인 전수 분석 완료 (equity-research 10, financial-analysis 12, IB/PE/WM 25, partner-built 11=참고만)
- 관련: 2026-08-31-engine-development-roadmap.md (축3 폭·축2 접점), 2026-08-31-engine-v2-autonomy-design.md (E5 교차평면)
- 제약: 기존 스킬/프롬프트 문서는 수정하지 않는다(사용자 지시). 모든 변경은 커널·계약·바인딩 기계화로 간다.

## 1. 소스 레포가 가르치는 패턴 (구현에 직결되는 것만)

전체 분석 보고는 세션 기록; 여기엔 엔진 태스크로 직결되는 패턴만 남긴다.

| # | 패턴 | 원문 근거 (대표) | krw 이식 |
|---|---|---|---|
| P1 | **소스 래더**: 구조화 MCP 1순위 → 프라이머리 공시 → 웹 최후, 폴백은 "라벨링"이지 대체 소스 아님 | comps-analysis: "FIRST: Check for OpenBB MCP… NEVER use web search as a primary numerical data source"; spglobal: "NEVER fall back to web search… label it N/A" | 이미 우리 교리와 동형(관측≠근거). openbb 데이터 평면을 1순위 관측소스로 확장 |
| P2 | **"코드는 계산, LLM은 해석"** | LSEG 전 스킬 "let the tools compute, you interpret"; dcf-model "Formulas Over Hardcodes (NON-NEGOTIABLE)"; tear-sheet "calculation-only step… calculations.csv" | 결정론 계산 빌트인(Task C). 모델은 입력·해석만, 산술은 Rust |
| P3 | **빌드 후 스크립트 검증** | validate_dcf.py: g≥WACC=CRITICAL, TV/EV 50-70%, exit code | 계산 빌트인이 불변식을 스스로 검증하고 위반 시 결과 반려 |
| P4 | **민감도 5×5, 중심=베이스** | dcf-model: 축 [base±2Δ], center cell = 모델 실제 출력(배선 자체증명), 3표(할인율×g, 성장×마진, β×Rf) | Task C 계산능력의 출력 스펙 |
| P5 | **데이터 영역 명세는 카테고리로, 도구명 하드코딩 없음** | equity-research 전 스킬: "OpenBB MCP earnings calendar / transcripts / peer data… when available" (함수명 미지정 — 구현 독립성) | capability model_input_contract가 카테고리 문 (티커+기간+제한) |
| P6 | **거시 대시보드 골격** | LSEG macro-rates-monitor: 지표표(Current/Prior/Direction/Signal) + 수익률커브 + 실질금리 분해 + 스왑스프레드 + 총평 | Task A에 yield_curve·macro_calendar 추가로 재료 확보; 조립은 기존 컴포저 |
| P7 | **어닝 이벤트 생명주기** | preview→analysis→model-update→thesis→catalyst (5스킬 시간축) | earnings_calendar(티커 스코프)·consensus(PT)로 preview/beat-miss 재료 확보 |
| P8 | **컴스→DCF 입력 매핑** | commands/dcf.md: "Peer median EV/EBITDA → Terminal exit multiple range; 25th-75th → Sensitivity range" | peers·metrics·statements가 컴스/DCF 입력; 계산은 Task C |
| P9 | **위젯/stat cards + 딥링크** | funding-digest: stat cards 4개 + Top deals 표(CapitalIQ 딥링크) + "AI-generated" 배너; openbb-ai 계약 create_widget | Task D: 엔진 산출물 → openbb 워크스페이스 위젯 번역 계층 |
| P10 | **과잉산출 금지** | initiating-coverage: "DELIVER ONLY THE SPECIFIED OUTPUTS… extras waste context" | 새 능력마다 상태 1회 방문 상한 유지 (기존 패턴) |

partner-built(LSEG·S&P)는 참고만: LSEG의 "툴 이름 정확 지정+역할분담"과 spglobal의 "빈 결과=버그, 재시도→N/A 라벨" 사다리만 설계 언어로 차용.

## 2. 사용자 지시 → 엔진 태스크 매핑

| 사용자 지시 | 태스크 |
|---|---|
| "회사 관련 자세한거는 온톨로지도 같이 조회" | Task A 회사평면 8도구(quote·metrics·income·balance·cash·consensus·peers·earnings_calendar) — 전부 티커 스코프, 온톨로지 질의와 병행 광고 |
| "거시,미시 데이터도 openbb 처음조회하고 온톨로지 관련 업종 조회" | Task A 거시평면 2도구(yield_curve·macro_calendar) + Task B 관측 액션 개방 + 교차평면 힌트(관측→온톨로지) |
| "dcf 나 이런거 계산관련 된거는 코드도 같이 들어가서 계산" | Task C 결정론 계산 빌트인(quant): DCF·컴스 통계·민감도 — LLM 산술 금지, Rust가 계산+불변식 검증 |
| "그거 관련된 파일이나 그런거는 openbb 위젯에 나오게" | Task D 위젯 산출 계층: 엔진 차트/표 아티팩트 → openbb 워크스페이스 위젯 페이로드 번역 (services/, 게이트웨이 어댑터 제품화 포함) |

## 3. 실측 근거 (2026-08-31 프로브)

- 로컬 openbb-mcp (127.0.0.1:8001) **219 도구 생존**, 자격증명은 **fmp 1개** (+fred 무키). benzinga 뉴스·transcript(fmp 422)·unemployment/interest_rates(fred 부적합)·sp500_multiples 사용 불가.
- 실제 호환 확인: `equity_price_quote` `equity_fundamental_metrics|income|balance|cash` `equity_estimates_consensus`(PT 포함) `equity_compare_peers`(9 peers) `equity_calendar_earnings`(symbol 지정 시 소량) `fixedincome_government_yield_curve`(12 테너) `economy_calendar`(fmp, importance 필터 필요).
- 결과 엔벨로프는 전부 `{results: [records]}` — 기존 `map_openbb_series` 레코드 투영(스크럽·바운드·advisory)이 그대로 재사용 가능. 단 기록에 `cik`가 살아남으므로 식별자 스크럽 목록에 추가한다(P: 식별자는 어드바이저리 콘텐츠가 아님).
- 실업·금리 개별 도구는 불필요: 기존 `openbb.macro_series`(FRED)가 UNRATE·DFF·T10Y2M 등을 이미 커버 — **중복 큐레이션 금지**.
- news_company은 제외: 이미 3단 뉴스 사다리(파일링카탈로그→피드→웹)가 있어 4번째 럭은 과설계.

## 4. 태스크 정의

### Task A — openbb 큐레이션 라운드 2 (3 → 13)

기존 3도구와 **동일한 패턴** 그대로 (schema 2개×도구, krw-contracts 상수+검증기, agent.yaml 계약+capability, 바인딩 예시 4파일, InputDerivation 폐쇄집합 확장, 프리플라이트 테스트 확장). 전부 `advisory_only`, `research_action` 없음 유지(플래너 개방은 Task B에서 별도 정책으로).

| capability id | MCP 도구 | provider | 스코프 | 모델 입력 (카테고리 문) |
|---|---|---|---|---|
| openbb.quote | equity_price_quote | fmp | ticker | {ticker} |
| openbb.fundamentals_metrics | equity_fundamental_metrics | fmp | ticker | {ticker, limit≤4, period?} |
| openbb.fundamentals_income | equity_fundamental_income | fmp | ticker | {ticker, limit≤4, period?} |
| openbb.fundamentals_balance | equity_fundamental_balance | fmp | ticker | {ticker, limit≤4, period?} |
| openbb.fundamentals_cash | equity_fundamental_cash | fmp | ticker | {ticker, limit≤4, period?} |
| openbb.estimates_consensus | equity_estimates_consensus | fmp | ticker | {ticker} |
| openbb.peers | equity_compare_peers | fmp | ticker | {ticker} |
| openbb.earnings_calendar | equity_calendar_earnings | fmp | ticker(symbol 필수화) | {ticker, start_date?, end_date?} |
| openbb.yield_curve | fixedincome_government_yield_curve | fmp | unscoped | {date?} |
| openbb.macro_calendar | economy_calendar | fmp | unscoped | {start_date, end_date, importance∈{high,medium}} (≤31일 창) |

- 워크플로우: 회사평면 8개는 `assess_obligations` 이후 광고(관측 보강 시점), 거시 2개는 기존 openbb 매크로 룩업 옆. 각 상태 max_visits 1. `ingest_evidence` 상한 17→27 (계산: 기존 17 + 10).
- 커널 핀: 모델은 provider를 결코 보지 않는다(input_derivation이 fmp 고정).
- 결과 한도: 260 레코드 투영 상한 존중 — calendar 계열은 모델 계약이 창/중요도로 사전 바운드.

### Task B — 관측 액션 개방 (플래너 게이트, E5·후보3 통합 설계)

**2026-08-31 완결 (dda04e7)**: assess 경로는 라운드2 배선(1c21d35)으로 즉시 열렸고, 플랜 시점도 Observation 액션 종류로 확장 완료. 티커 스코프 9개 도구(시세·회사평면 8)만 정책 보유 — map_goals가 "같은 티커의 미해결 조항 존재"로만 매핑하고 혜택 0개(서버추천·직접성·계산 커버리지 없음)라 동일 조항에서 파일링 정밀조회가 항상 우선. 비스코프 거시 4도구는 매핑 불가능하므로 정책 없이 assess 직접 경로 유지(플래너 경로 이동 시 항상 unmapped 거부 = 회귀 방지). 관측 완료는 자기 지문만 소거, 조항 해소는 근거장부 판단.

**품질 게이트 실행 (2026-08-31, 스택 해제 후)**: 블로커는 B7이 아니었다 — dev 릴리스 20260830_193811은 이미 spine-projection/v7+company-shard/v3을 담고 있었다. 실제 블로커 4건을 해제: (a) 라이브 스파이크 openbb 어댑터(9444)가 이전 턴의 pkill 정규식(`.`이 `_` 매칭)으로 사망 — 제품화 어댑터(services/openbb-gateway)로 교체·구동(이로써 Task D가 실전 스택 경로에 투입됨), (b) round-2 바인딩 flow-style이 스택 시작 스크립트의 블록 단위 신원 치환을 우회 → 블록 스타일로 재작성(커밋 6fa01a5), (c) 운영자 env의 sha256 이중 접두사, (d) 게이트웨이 3종+웹뉴스 운영자 env 재구성(URL·핀은 기술값 복원, 토큰 3종은 로컬 게이트웨이가 베어러를 검증하지 않으므로 랜덬 재발급 — 0600 env 파일). 스택 기동: 4엔드포인트 프리플라이트(13 openbb 도구 포함) 통과.

**수정 루프 (2026-09-01, 커밋 7229e3a·e05ecbc·f464fe0)**: 두 고장 등급을 근본 수리했다. ① 사다리 위반은 하드실패 대신 `ModelProposalRejection::Order`→모델 복구 지시로 전환(모델이 위반된 순서를 배우고 이른 런그로 재시도, 기존 repair 예산에 묶임). ② 오염된 답변은 3층 백스톱: 프롬프트 언어 고정+프로세스 서술 금지(e05ecbc), 검증기 무결성 클래스 2종(`answer_control_payload_leak`: 커널 제어 토큰·도구 제안 스키마 echo, `answer_language_mismatch`: 한글 0 또는 외래 문자 우세 — f464fe0) — 전부 bounded compose repair로 라우팅.

**검증 스모크 (같은 3케이스 + fix_dcf 재검증)**: short_aapl_price_drop **2회 연속 클린 통과**(한국어, `filing.search_events` 이른 런그 사용 — 웹뉴스 직행 소멸). normal_aapl_mix_cost_cash 정직 강등 유지(외부 의존성 타임아웃 — 인프라 과제). short_fix_dcf는 여전 RED: 오염이 매번 형태를 바꿈(아랍어→프로세스 서술→제안 echo→`InvalidProviderEpisode("final episode must finish with stop")`). 백스톱이 작동해 **오염 답변은 더 이상 사용자에게 전달되지 않음**(정직 실패로 차단) — 사전에는 "final"로 824자 쓰레기가 배달되던 케이스. 근본 원인 가설: 이 케이스의 compose 턴 입력 자체가 혼란 상태(마지막 도구 결과 오염 등) — 출력 필터를 더 쌓기보다 compose 턴 입력 덤프에 대한 트랜스크립트级 디버깅이 다음 단계.

- `ResearchActionKind` 폐쇄집합 {Context, Targeted, Trace}에 **`Observation`** 추가: 계약 표면 = openbb 모델 요청 계약(폐쇄 목록) + `OpenbbSeriesV1` ingest + normalized-capability-result/v1 출력, permission Read, idempotency canonical_args, conflict_domain `openbb.<도구별 리소스>`.
- research-planner `ResearchActionKind` {QueryContext, TargetedQuery, Trace}에 `Observation` 대응 추가, capability_dispatch 매핑, 점수·충돌 도메인은 선언적 estimate로 (커널 조건문 없음 — 기존 설계 원칙 유지).
- 교차평면 힌트: 관측 결과 ingest 후 온톨로지 쪽 미해결 조항이 있으면 ladder-hint 패턴(29ad96b와 동일 경로, 지문 중복 제거·자기소거)으로 "오픈 의무" 힌트 — 매크로→관련 업종/기업 온톨로지 조회 유도. 역방향(온톨로지→관측)은 ExternalFactorExposure 힌트(기존 E5 설계 §3).
- 게이트: GLM 매트릭스 서브셋 스모크 후 전체 (로드맵 운영 규칙).

### Task C — 결정론 계산 빌트인 (quant)

- `LocalCapability::QuantModel` 신설 (SkillLoad·WebNewsSearch와 동일 계층, capability-runtime에서 실행).
- 모델 입력 계약 `quant-dcf-request/v1`: {티커 라벨, 기간 FCF 배열(출처 라벨 필수), wacc, terminal_growth, method: perpetuity|exit_multiple, exit_multiple?, net_debt, shares} + `quant-comps-request/v1`: {피어 지표 배열} + 민감도 축 파라미터.
- 실행은 순수 Rust: PV(중간연도 컨벤션 옵션), 터미널값, EV→Equity 브리지, 5×5 민감도(축 [base±2Δ], 중심=베이스 출력), 컴스 5통계(max/75th/median/25th/min).
- **불변식 게이트 내장** (validate_dcf.py의 계승): `terminal_growth < wacc` 위반=오류 반려, TV/EV ∉[40,80]% 경고 라벨, wacc ∉[5,20]% 경고. 계산 결과는 `advisory`(계산 입증 가능하나 입력 근거 등급은 입력의 것), 결과에 formula+components 감사 필드(tear-sheet calculations.csv 패턴).
- 결과 ingest: 신규 `QuantModelV1` — "계산 결과"는 근거도 관측도 아닌 제4의 종류(입력의 계보 보존). 강한 주장은 여전히 파일링 직접근거에만.
- 워크플로우: `assess_obligations` 이후 1회 상태.

### Task D — 위젯 산출 계층 (services/)

- `live-spike/openbb_gateway_adapter.py` → `services/openbb_gateway/` 이동+테스트(제품화 백로그 이행): 준비성 지문 프록시. URL은 http/https만, 호스트 검증(루프백/사설 거부 — 업스트림은 명시적 로컬 프로파일에서만 허용), 자격증명은 환경변수만.
- 위젯 퍼블리셔: 엔진 AnswerBundle 시각화(chat_message_visualizations 호환) → openbb-ai 워크스페이스 `create_widget` 페이로드 번역(결정론적 UUID: origin+widget_id 해시, 표·시계열·카드 종류 매핑). 엔진 코어는 건드리지 않는다(축2 원칙: 번역은 서비스층).
- 테스트: 페이로드 번역 순수 단위테스트(네트워크 없음) + 어댑터 프록시 라운드트립(로컬 fixture 서버).

## 5. 순서와 게이트

A(기계적, 리스크 최소) → B(게이트 개방, 매트릭스 필요) → C(새 계산 의미) → D(서비스층). 각 태스크: cargo build+test 전 녹색 + 해당 크레이트 테스트 확장. A+B 후 GLM 스모크(quality matrix --case 서브셋), 전체 매트릭스는 야간.

## 6. 하지 않는 것 (과설계 금지)

- partner-built 재구현 금지(참고만 — 사용자 지시).
- news_company·transcript·unemployment·interest_rates·sp500_multiples 미큐레이션 (3단 뉴스 사다리 존재·자격증명 부재·FRED 중복).
- LLM 산술 허용 금지(Task C가 대체), 위젯 SQL·브라우저 브리지 재구현 금지(워크스페이스 소관), 멀티 LLM 오케스트레이션 금지.
- 기존 스킬/프롬프트 문서 수정 금지(사용자 지시) — 새 어휘는 커널 광고로만.

## 9. 2026-09-01 근본원인 디버깅 — fix_dcf/mix 실패 해부 (커밋 1bffbba)

### 방법
실패 런(run_bccaf2a5, provider_protocol_failure)의 암호화된 provider 에피소드 7건을
`crates/artifact-store/examples/dump_episode.rs`(신규 운영자 도구, 마스터키는 env만)로 복호화해
턴 단위로 재구성. 조사 가설이었던 "compose 입력 오염"은 기각 — 오염 없음. 세 결함 발견:

1. **compose 재시도 토큰 기아 (fix_dcf 치사원인)**: 연구 턴은 `remaining − reserve(16,384)`로
   예약을 지키지만 검사는 턴 사이에만 실행. GLM compose 턴이 16,384를 전부 사고(thinking이
   ~15.8k 소모, 본문 539자에서 절단), 재시도엔 693토큰만 남아 재절단 → provider_protocol_failure.
   `compose_retry_reserve_tokens`(16,384)는 문서로만 존재하고 실제 지급 로직은 없었음.
   → 수리: 재시도 턴은 선언된 재시도 예약을 불가침 지급(소프트 초과, 1회 한도; 재시도 레인은
   thinking 비활성이라 토큰이 본문에 착지). remaining==0 조기 에러도 재시도 대기면 우회.
2. **관측(Observation) 매핑 갱김 (모델 자율성 결함)**: ep05에서 모델이 openbb.balance_statement(FIX)를
   자발 제안했으나 (a) 후보 매핑이 티커의 첫 후보만 보고 이미 해소된 조항에 묶여 목표 0개로
   실종, (b) intent 영수증이 조항을 안 덮으면 매핑 없이 폐기 — 네트워크에 도달조차 못함.
   compose가 "시세 데이터가 들어오지 않아" 헤징. → 수리: 티커의 후보 전체를 순회하며 frontier
   목표가 남은 조항에 바인딩, intent 공간 폴백은 '미해소 계산 목표'로 한정(정성 전선은 여전히
   unmapped — 파일링 읽기가 그 조항을 해소함). 동일 조항 경합 시 파일링 우선 원칙 유지(테스트).
3. **폴백 미스라벨 (mix_cost_cash)**: 출력예산 36,000 전량 소진 → NoRemainingOutputBudget →
   ledger 폴백이 "외부 연결(provider/MCP) 문제"로 진단 — 사실은 자체 예산 소진.
   → 수리: 예산류 트리거는 `output_budget_exhausted` 사유코드와 "응답 예산 소진" 안내문으로
   정직화. (동일 원인군: 재시도 예약 수리가 도달 자체를 예방.)

### 검증
- 워크스페이스 919+ 테스트 녹색(`--no-fail-fast`). 재시도 예약 테스트는 수리 전 빨강(판별력 확인).
- 라이브 재검(수리 엔진, 스택 리로드 PID 37205): **short_fix_dcf 완주** — 에피소드 증거로
  수리 경로 실증: ep05 = finish `length`·본문 0자(9,798토큰 전부 thinking) → 직접재시도
  발동(thinking 비활성) → ep06 = `stop`·본문 1,832자 완결. 이전 실패 런(run_bccaf2a5)은
  정확히 이 지점에서 693토큰 재절단으로 사망. short_aapl_price_drop도 파이널라이즈
  (오염 0·한글 클린·정직 강등). 두 케이스 모두 transport passed, 실패 클래스 없음.
  24케이스 풀매트릭스(parallelism 8)는 별도 실행 — 판정은 커밋 시점 기준 진행 중.

### Mimosa 전체 감사 (2026-09-01, scan-job-mtiea6c8)
완주·봉인(seal sha256:388fb31d…, 307 패키지, finding 51). 전 수 미완(enobufs) 문제 해소.
트리아지: sql-injection 37건은 정적 식별자 f-string(COUNT/PRAGMA 등 외부입력 불가)·
hardcoded-credential 5건은 env 패스스루/키 '경로' 문자열 · ssrf 2건은 의도된 로컬 운영
러너(URL 검증 내장) · path-traversal 1건은 내부 신뢰 경로 — 전부 기존 코드 패턴이며
금일 변경 파일(crates/run-engine·research-planner·artifact-store)에는 0건.
"프로젝트 안전" 선언은 하지 않는다(정적 증거 경계, coverage=partial: 동적 파생 콜그래프 미완).

## 10. 2026-09-01 풀매트릭스 판정 + 2차 결함 2건 (커밋 d55d525·사후)

**풀매트릭스(18케이스, parallelism 4, 수리 1-3 탑재 빌드)**: **17 PASS / 1 FAIL**.
- 17개 전부 `final`+`transport passed`, 한글 클린, control-토큰 누출 0. fix_dcf·mix_cost_cash 둘 다 완주.
- `short_google_price_outlook`에서 **관측 시리즈(series) 라이브 발동 실증** (수리 2의 경로).
- `complex_klac` 폴백이 **신규 정직 라벨**("응답 예산(출력 토큰) 소진")로 안내 — 수리 3 실전 확인.
- 유일 FAIL = `short_google_pullback_long_term`: filing.search_events 응답이 계약 위반
  (`ladder_exchange_invalid`, NotDispatched) → **시작된 액션 행이 begun으로 잔류** → 폴백
  commit_final이 `pending_action` 거부 → answer-always 탈출로가 자기 트리거를 못 구함.
  → **수리 4 (d55d525)**: 디스패치 실패의 양 확실성 레인이 모두 begun 행을 ambiguous로 해소.
  이 매트릭스는 수리 전 빌드라 이 케이스만 죽음(판별 데이터 포인트).

**2차 오염 변형 2건 (전달된 답변에서 포획, 어휘 확장 커밋)**:
- AMZN: 답변 전체가 assess 판단 객체(`{"assessment":"user_judgment","goal_id":…}`) — 판단 레인
  JSON이 그대로 배달. → CONTROL_PAYLOAD_TOKENS에 `"assessment"`·`user_judgment`·`goal_id` 추가.
- INTC: 답변 전체가 "커널이 거부했습니다" 류의 프로세스 진술(한글이라 기존 영문 토큰을 우회).
  → `커널` 토큰 추가(투자자 답변은 커널을 언급하지 않음).
- 워크스페이스 921 녹색. 3케이스(구글 폴백·INTC·AMZN) 재검으로 폐쇄 루프.

## 11. 폐쇄 루프 완료 (2026-09-01 심야)

- 3케이스 재검(수리 4+어휘 탑재 빌드): **구글 폴백 = final+passed, 액션 `ambi` 해소 실증**
  (시작된 행이 해소되고 폴백이 확정 — 수리 4 라이브 증명). **AMZN = final+passed, 오염 0**
  (kernel 에코 재발 시 클래스 토큰이 integrity 실패로 차단→수리 레인). INTC = final+passed
  (예산 폴백, 정직 라벨).
- 제3변형(`kernel {"capability":"krw_ontology_query__…}`)에 대해 개별 키 추격을 멈추고
  **클래스 차단**: `krw_`(소문자+언더스코어 — 통화 KRW와 무충돌)와 `kernel` 토큰으로
  모든 제어 봉투 에코를 차단. 워크스페이스 921 녹색.
- 잔여 관찰(결함 아님, 후속 레버): GLM 턴당 thinking 소모로 일부 궤적이 36K 출력예산을
  태워 정직 폴백으로 끝남(배달은 보장, 깊이가 제한) — 재시도 예약이 배달을 담보하므로
  심도 튜닝(연구 턴 캡 등)은 별도 최적화 과제. filing MCP의 `ladder_exchange_invalid`
  근본 원인은 diagnostic이 해시 익명화라 capabilityd 디버그 로깅이 필요 — 폴백이 사용자를
  보호하므로 긴급 아님.

## 12. 잔여 관찰이 결함이었던 사례 — 전수 조사 판정 (2026-09-01 심야2)

§11의 두 "잔여 관찰"을 근본 원인까지 파헤쳤고, 둘 다 실제 결함(+추가 3건)이었다.

### 증거 (전수)

- 7일 고장 전수(공유 postgres agent_store): 204 final / 21 failed. 수리 빌드 이후 실패는
  5건 — 3건 dependency_contract_failure(전부 d55d525 이전 빌드, 이미 수리), 1건
  provider_protocol_failure(bccaf2a5, 이미 수리), 1건 answer_verification_failed(최종
  어휘 빌드 이전, 매트릭스로 폐쇄).
- 09-01 매트릭스 31 final 중 **3건(9.7%)이 예산소진 폴백**(AMZN·KLAC·INTC). 3건 모두
  **입력 토큰 누적 174,580~193,381 > 상한 168,000**(성공 런은 154~160K). 같은 질문이
  궤적에 따라 통과(6턴)/폴백(7~8턴)로 갈림.
- 에피소드 복호화(AMZN a03ad083): ep07 첫 compose가 cap 16,384 전량을 소진하며
  reasoning 38,684자 + 본문 1,343자에서 절단. 재시도 비트는 설정됐으나 8번째 턴은
  디스패치되지 않음(provider_turns=7).
- z.ai 직접 프로브 2회: `{"type":"disabled"}`는 확실히 존중(JSON 제약 하에서도).
  `budget_tokens`는 **무시**(1,024 지정에 thinking 9,271자) — 와이어에서 유일한
  thinking 제어 수단은 완전 비활성.

### 결함 5종 + 수리

1. **A — 재시도 턴이 입력 예산 하드 체크에 사냥당함**: `reserve_provider_turn` →
   `check_budget` → `ensure_within`이 input_tokens>상한을 하드 에러로 돌린다. 출력
   예약 우회(1bffbba)와 동일한 설계 결함이 입력 차원에 존재. 재시도는 배포되기 전에
   사망 → 저품질 폴백. 수리: 재시도 대기 턴은 provider_turns 상한만 적용(입력/출력
   초과분은 이 한 턴으로 유계).
2. **B — 답변 턴이 thinking에 예산을 태움**: 첫 compose가 cap의 ~90%를 reasoning에
   소모(z.ai는 budget_tokens 무시). 재시도 레인은 이미 thinking 비활성이고 정상
   배달을 실증. 수리: 답변 방출 턴(compose/섹션 포함)은 thinking 비활성 — 검증된
   재시도 의미론을 첫 시도로 확장.
3. **C — ladder_exchange_invalid = 주식종류 티커 표기**: 카탈로그가 GOOGL 요청에
   GOOG 태그 이벤트를 반환(라이브 재현; FOXA·BRK.B 등은 빈 결과라 무해). 계약이
   문자열 동등을 요구해 교환 전체 거부 → 사다리 데이터 전량 상실. 수리: 결과 전체가
   단일 CIK·단일 별칭 티커일 때(주식종류 재표기) 수용 + 관측 상태 바인딩 동일 규칙.
4. **D — 진단 해시 블라인드**: capability 계열 실패의 reason_code가 임의 어댑터
   가정으로 해시 익명화됨. 우리 reject() 코드는 이미 유계 snake_case — 보존한다.
   경고 로그에 바운드된 메시지도 추가.
5. **E — 사다리 배치 마찰**: 비연구 캐피빌리티(사다리 3종)는 배치 불가인데 모델이
   3개를 묶어 제안 → `decision_batch_size_invalid`로 턴 낭비(GOOGL ep04에서 모델이
   스스로 증언). 수리: 캐피빌리티 설명에 "턴당 1호출" 명시(커널 규칙은 불변).

### 수리 후 라이브 재검 (2026-09-01 심야3)

- 결함 F(라이브 재검 중 발견): AMZN 재검런이 여전 폴백 — 6턴 연구 후 169,699 입력 >
  168,000 상한. 게이트는 compose로 잘 보냈으나 **compose 턴 자체가 입력 하드 체크에서
  입장 거부**(재시도가 아닌 답변 턴은 면제 없었음). 수리: 답변 방출 턴(compose/섹션)
  전반에 재시도와 동일한 불가침 1턴 그랜트 적용 — `reserve_provider_turn(image)` 서명
  확장. TDD: `compose_turn_is_admitted_even_when_research_crossed_the_input_cap`.
- **AMZN 최종 재검 = 실제 컴포지, 폴백 마커 0**(세그먼트 매출·이익률 실숫자 표 포함,
  5턴/입력 148K — 이전 폴백 궤적은 7~8턴/169~193K).
- **GOOGL = 실제 컴포지 + 사다리 데이터 반입 실증**(답변에 "8-K 1건 2026-08-10 접수",
  "Form 4 약 11건(7/29~8/31)" — GOOG 태그 카탈로그가 반환하던 바로 그 이벤트).
  수리 전 동일 클래스는 ladder_exchange_invalid로 사망.
- 워크스페이스 **927 통과 / 0 실패**(기준선 921 + 신규 6).

## 13. 품질 무한루프 루프1 — "모른다" 금지 (2026-09-02 심야)

목표(사용자): "모르면 모른다고 말하기"가 아니라 거시(openbb)+미시(openbb)+온톨로지 삼각조사로 인사이트.

- **조사**: GOOGL 답변의 "시세 데이터가 확보되지 않아" = ① FMP 키 부재로 스냅샷 프리페치 21ms 즉시 실패 ② 프롬프트 계약 자체가 "say that plainly"로 종결 유도 ③ openbb.* 디스패치 역사 0건 — 모델이 openbb 배치를 제안하면 `non-research decision batch`로 거부돼 턴 소진(GOOGL 재검 3+6호출 배치 2회 거부 → 의존성 폴백 실측).
- **수리 5종**: G 스냅샷 부재/unavailable 프롬프트 → openbb 검색 유도(자체 조건부) · H 커널 보충 배치 대기열(동종 비연구 배치 ≤4 수용, 첫 호출 디스패치 후 나머지를 상태차트 순회로 자동 드레인 — 프로바이더 턴 0 추가) · I 관측 no_data 결과에 openbb 폴백 노트(전사 채널) · J query_context 결과에 시장 관측 미조회 힌트(조회 시 자동 소멸) · **K 결정타: 시장 프리페치의 openbb 종가 폴드** — 주 스냅샷이 well-formed `unavailable`로 와도(검증 통과가 함정) sealed openbb price-history 호출로 최근 종가 2개를 동일 폐쇄 스냅샷 형태로 접음(레코드 정규화 형태 반영).
- **라이브 8라운드 검증**: 라운드 4 뉴스 러그 첫 실전 발동(보도→심리조정 인사이트), 라운드 8 **실제 시세 반입 — TSLA "356.76달러(직전 367.95달러, 약 -3%)", GOOGL "335.33달러(직전 339.35달러)" + 공시 이벤트 결합**. 시장 다리가 결정론적으로 채워짐.
- 워크스페이스 **930 통과 / 0 실패**(신규 4: 배치 드레인·no_data 힌트·시장 힌트·종가 폴드).
- **루프2 관찰(신규 결함 후보)**: TSLA 재시도 궤적 하나가 답변 전체를 `{"action_type":"capability_alternatives","alternatives":[]}` 에코로 배달 — 어세스 레인 JSON 누출 변형. CONTROL_PAYLOAD 토큰에 `action_type`/`alternatives` 추가 필요.

## 14. 루프2 — 어세스 배치 에코 차단 + 신규 질문 검증 (2026-09-02)

- 루프1에서 포획한 신규 변형(`{"action_type":"capability_alternatives","alternatives":[]}`가
  답변 전체로 배달)을 CONTROL_PAYLOAD 토큰 3종(`"action_type"`,
  `capability_alternatives`, `"alternatives":`)으로 클래스 차단 + 테스트 주장 추가.
- 신규 질문 2종 라이브: **NVDA = 실제 시세 반입**("219.56달러 vs 전일 220.78달러,
  약 -0.6%") + "하루치뿐" 정직 고지 + 실적 가속 표. **MSFT = 클라우드 매출 궤적**
  (FY21 691억→FY26 2,144억 달러, +27%). 에코 재발 0.
- 루프3 후보 실측: ① 폴드가 종가 2개만 주니 "급등락 흐름"류 질문에 계열이 얕음
  → 최근 종가 계열(≤8) 스냅샷 반입 ② 거시(금리·CPI) 수치 여전 정성 서술
  → 매크로 폴드(별도 과제).

## 15. 루프3 — 종가 계열 반입 (2026-09-02)

- 루프2 실측("하루치뿐") 개선: 스냅샷 계약에 `recent_closes`(≤8, date+close) 정규화
  추가 + 폴드가 최근 8종가 전달 + 프롬프트 안내("계열 범위와 날짜를 인용하라").
- **라이브 실증(NVDA 동일 질문)**: 답변이 8일 종가 테이블(8/21 214.72 → 8/27
  227.98 급등 +5.7% → 9/1 218.21)을 날짜와 읽는 포인트와 함께 배달 — "급등락
  흐름" 질문에 실제 다일 흐름 분석 완성.
- 워크스페이스 930/0. 루프4 후보: 거시(금리·CPI) 수치 폴드, GLM 풀매트릭스 재측정.

## 16. 루프4 — 거시(금리) 수치 폴드 (2026-09-02)

- 조사: FRED 계열(openbb.macro_series, openbb.macro_cpi)은 **openbb 서버에
  `fred_api_key`가 없어 전면 차단**(라이브 프로브). 대신 **키프리 경로 발견**:
  `openbb.yield_curve`(pinned fmp, 연준 커브 데이터)가 키프리 작동.
- 구현: `TrustedMacroContext`(폐쇄 정규화: 시리즈 ≤3 × 포인트 ≤6) + sealed
  `openbb_yield_curve_preflight_invocation`(provider=fmp) + executor 폴드가
  3M/2Y/10Y 벤치마크 최근 2일치를 반입 + `<trusted-macro-context>` 프롬프트
  (소수 표기 0.0441=4.41% 유닛 안내, 2Y-10Y 스프레드 활용 안내).
- **라이브 실증(MSFT 동일 질문)**: 답변이 "8월 31일 기준 미 국채 10년물 4.75%,
  2년물 4.34%, 2Y-10Y 스프레드 +0.41%p 양의 기울기" + 주가(501.30 vs 507.29) +
  공시 수치표를 한 답변에 결합 — 거시·미시·온톨로지 삼각조사 완성.
- 워크스페이스 931/0. 잔여: CPI(물가) 다리는 FRED 키 필요(또는 pinned provider
  fred→oecd 변경 — 모델 경로 영향 검토 후 루프5+ 과제).

## 17. 루프5 — 풀매트릭스 통계 확정 + CPI(oecd) 전환 (2026-09-02)

- **GLM 풀매트릭스(수리 17종 탑재 빌드): 18/18 완료 · transport_failed 0 ·
  폴백 0/18** — 폴백율 9.7%→**0% 수렴 통계 확정**. 어제 실패 케이스 전부 클린:
  short_fix_dcf(provider_protocol_failure였음)가 4년 현금흐름 궤적 DCF 답변,
  complex_klac(예산 폴백였음)가 세그먼트 실숫자 표 답변. compose thinking
  비활성화의 품질 영향 관찰되지 않음(표·수치·정직 라벨 모두 유지).
- **CPI fred→oecd 전환(5층)**: 계약 provider enum += oecd(JCS 단행 정규형 재작성
  + sha 1fd213ec 재핀 — pretty-print가 NonCanonical으로 거부되는 것 실증) ·
  OpenbbPinnedProvider::Oecd 배리언트 · 의미 검증기 fred|oecd · agent.yaml 핀+
  content_hash · 어셈블리/이미지 검증기 2곳. 폴드에 CPI 시리즈(CPIYOY, oecd
  transform=yoy = 연 인플레이션율 소수) 추가 — 매크로 프리페치가 이제
  금리(UST 3종)+물가(CPI) 모두 반입.
- 라이브(KO 물가 질문): CPIYOY+UST10Y가 프롬프트에 도달(에피소드 실증),
  답변은 공시 기반 가격/믹스 인플레 분석으로 고품질 — MSFT 궤적은 금리를
  직접 인용했으므로 인용 여부는 모델 프레젠테이션 분산, 데이터 다리는 상시 제공.
- 워크스페이스 931/0. 루프6 후보: 폴백 0 달성 후 잔여 품질 축 = 심층 케이스
  답변 길이/완결성 프로파일링, EN 이미지 동일 폴드 적용, 원본 반영 승인 건.

## 18. 루프6 — 완결성 프로파일링 + 마크다운 게이트 갭 + EN 라우팅 (2026-09-02)

- **18케이스 프로파일링**: len med 1,647 · 표 16/18 · USD 16/18 · 수치밀도 med 21 ·
  후속질문 3/3 고정 — 단 **complex_intc 퇴화(153자·무구조)** 포획.
- **INTC 근명**: ep05 어세스 턴 thinking 소진 절단 → ep06 재시도가 어세스 숙고문
  ("커널이 중복 거부… A안/B안")을 답변으로 배달. typed 레인의 CONTROL_PAYLOAD
  클래스 토큰이 **직접 마크다운 레인(final-markdown/v1)의 이미지
  forbidden_user_terms에는 없었음** → 양 이미지(ko/en)에 커널/kernel/krw_ 추가로
  클래스 차단.
- **EN 라우팅 버그(선결함)**: 게이트웨이가 locale ko-KR 하드코딩 — 영문 질문이 EN
  이미지에 도달 불가. 한글 여부 감지 라우팅 추가 + **릴리즈 핀 awareness 폴백**
  (현재 핀은 한 달 된 ko-only 엔트리 4개 — EN 엔트리가 없으면 ko-KR 폴백, 400 회귀
  없음 실증). EN 이미지에 openbb 3캡+계약 6종 이식(폴드는 커널 시드라 상태 불필요,
  카탈로그 테스트로 3 프리페치 빌더 검증) — EN 라이브 e2e는 릴리즈 파이프라인이
  EN 엔트리를 싣는 순간 자동 활성(루프7 과제: 서명 릴리즈 재빌드).
- 워크스페이스 932/0 · host-ts 30/0.

## 19. 루프7 — 어세스 재시도 레인 + EN 엔트리 라이브 (2026-09-02)

- **INTC 근본 수리(7-A)**: 어세스/decision 턴이 thinking에 전체 캡 소진해
  절단(15,102자 reasoning, finish=length)되면 복구 턴이 오염되던 것 — One-shot
  `decision_retry_requested`(AtomicBool) 플래그로 **다음 비답변 턴 1회 한정
  thinking 비활성**(소멸성, build_provider_request에서 swap). 계측으로 라이브
  컴포즈 턴이 이미 thinking=Disabled로 작동함도 실증(AAPL 라이브 로그).
- **EN 엔트리 라이브(7-B)**: 스택 패키지 목록에 krw-ontology-en 추가 → 캐시
  디스크립터가 (company_research_en, en-US) 엔트리 확보 → 게이트웨이 라우팅을
  EN 질문→run_kind company_research_en로 정정(기존 감지는 존재하지 않는
  (company_research, en-US) 엔트리를 목표했음). **영문 질문 라이브 202 +
  en-US + company_research_en 라우팅 실증** — 폴드·연구 통과 후 컴포즈에서
  신규 결함 포획: EN 모델이 answer IR을 `answer_ir` 래핑으로 내보내 파서가
  거부(`unknown field answer_ir`) — EN 이미지가 한 번도 라이브였던 적 없어
  처음 드러난 compose 형태 버그(루프8 과제).
- 워크스페이스 **933/0** · host-ts 30/0.

## 20. 루프8 — EN compose 형태 드리프트 사다리 수리 (2026-09-02)

- 첫 EN 라이브(루프7)에서 `answer_ir` 래핑으로 시작해 **10차례 라이브 라운드**로
  드리프트 5종을 연속 포획·수리(GLM은 json_schema 강출력 불가 — 원칙적 해법은
  결정론적 정규화):
  ① `answer_ir` 단일 래핑 → 1회 언랩 · ② 미지 키(`interpretation`) →
  클레임/섹션 허용키 필터 · ③ 문자열 필드에 map → 잘못 타입 선택필드 드롭 ·
  ④ `id`→`claim_id`/`section_id` 별칭 · ⑤ `kind`/`strength` 누락·무효값 →
  유효 변형 기본값(fact/qualified) · ⑥ `text` 누락 → claim/statement/content
  별칭 복구, 본문 없는 클레임 드롭(전체 실패 대신).
- **EN 전용 예산 프로필**(company_research_en_glm): 영어 연구 reasoning이
  턴당 18k-49k자로 한국어(12k-31k)의 2-3배 — 동일 204000 누적 한도 안에서
  출력 비중 재조정(150K 입력/54K 출력). claim/executor fixture가 프로필 채택.
- EN 컴포저 프롬프트 재작성: 최상위 키 명시 + 코드펜스 금지 + 관측(시세·금리)
  인용 지침(KO 패리티).
- 라이브 결과: 6-9차는 연구→컴포즈 도달(형태 수리마다 진행), 10차는 37초 만에
  **입장 단계 의존성 탈출(간헐적, 재기동 직후 2회 상관)** — 루프9 1차 과제.
- 워크스페이스 935/0.

## 21. 루프9 — 재시도 전파 + 정규화 사다리 확장 + 로케일 매개변수화 (2026-09-02)

- **재시도 의존성 전파(9-A)**: 오케스트레이터가 재시도 가능 의존성(콜드 MCP 풀)을
  answer-always 폴백으로 삼키던 것을 Err로 전파해 실행자 2초 지연 재시도 경로
  활성(TDD: `retryable_dependency_failure_propagates_instead_of_falling_back`).
- **정규화 사다리 확장(드리프트 6~10)**: 루트 스칼라(schema_version 문자열
  에코→1, locale→요청 바인딩) · 섹션 필드(intent/heading/claim_ids 기본값과
  별칭) · id 발명(별칭 후 결정론적 발행) · 배열 필드 강제(문자열→빈 배열) ·
  계산 정규화(미지 키 제거+ids 강제).
- **로케일 매개변수화**: 검증기의 `locale != "ko-KR"` 하드코딩을 AnswerPolicy.
  expected_locale로 — EN 이미지(en-US) 답변이 구조적으로 통과.
- 라이브 9차: locale_mismatch 해소 — 잔여 EN 검증 갭 실측 =
  answer_language_mismatch(클레임 언어 휴리스틱) + invalid_section_id(정규화 후
  유니크 위반) + unrendered_claim×9(섹션-클레임 바인딩 누락) = 루프10 과제.
- 폴백 warn에 trigger 상세 추가(진단 관측성). 워크스페이스 936/0.

## 22. 루프10 — EN 검증 갭 3종+α 수리 (2026-09-02)

- **클레임 언어 게이트 매개변수화**: ko-KR(한글 0=내부 아티팩트) 대칭으로
  en-US(라틴 0=아티팩트, 한글/외래 우세=언어 오답) 허용 — 루프9의
  answer_language_mismatch 해소.
- **섹션-id 유니크 발행**: 정규화 시 인덱스 기반 발행 + BTreeSet 충돌 회피
  (invalid_section_id 해소). **unrendered_claim 방지**: 미바인딩 클레임을
  첫 섹션에 결정론적 부착(×9 해소).
- **후속질문 정규화**(신규 드리프트, 라이브 포획): 번호 프리픽스 제거·개행
  제거·'?' 종결 보장·빈 항목 드롭(invalid_follow_up_question×3 해소).
- 라이브: 루프10 1차에서 3종 전부 소멸 확인(다음 드리프트=후속질문 → 수리).
  2차는 deadline_exceeded(3턴만에 20분 소진 — 턴당 EN reasoning 4-5분 대기
  추정) = 루프11 과제(EN 데드라인 헤드룸 프로파일 조정).
- 워크스페이스 936/0.

## 23. 루프11 — EN 데드라인 헤드룸 + 폴드 EN 배선 + 빈-클레임 수비 (2026-09-02)

- **deadline 1,200,000→2,400,000ms**(KO deep 프로필의 40분 선례와 동일 — 턴당
  4-5분 EN reasoning × 연구 턴). 데드라인 소진 소멸.
- **폴드 EN 배선(근명)**: 프리페치 두 게이트가 `run_kind ==
  "company_research"`만 허용 — EN 진입점(company_research_en)은 폴드가 **전량
  스킵**되고 있었음. 게이트 확장 후 **에피소드로 실증: EN 모델 reasoning에
  "UST3M/2Y/10Y 3.92%/4.39%/4.79%, 2s10s +40bp" 반입**(루프11 3차).
- 후속질문 헤더 로케일화(EN="Suggested Follow-up Questions"). EN 이미지의
  claim_has_evidence 규칙을 KO 패리티로(관측 기반 오리엔테이션 클레임 거부
  해소).
- **빈-클레임 수비**: 정규화가 전 클레임을 드롭하면 468자 제목뿐인 답변이
  커밋되던 것 — AnswerValidation 레인으로 라우팅해 bounded repair 후 정직한
  `answer_no_surviving_claims` 코드로 종료(라이브 실증).
- 라이브: 3차 = 폴드 반입+클린 final(468자 얇음 — 수비로 차단 대상),
  5차 = 수비 작동 실증. 잔여 = EN 컴포저의 클레임 방출 형태 정렬(루프12 과제).
- 워크스페이스 936/0.
