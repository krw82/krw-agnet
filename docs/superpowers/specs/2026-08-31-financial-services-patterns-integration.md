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

**품질 게이트 상태 (2026-08-31)**: 구현 5커밋 전부 녹색(Rust 68 스위트 + Python 19 테스트). GLM 스모크 시도 → 샌드박스 dev 스택이 **P1/B7 대기 상태**(릴리스 검증 실패: manifest_builder_binding·metric_dictionary 핀 불일치, dev 릴리스 선택 env 부재 시 prod/current 폴백)로 기동 불가 — 본 스레드 변경과 무관(16:52 크래시와 동일 계열, 온톨로지 데이터 릴리스 검증 단계에서 사전 차단). 해제 조건: B7 해시 범프 + v2-dev 신규 릴리스(P1 스레드) 후 `dev-stack.sh up && dev-stack.sh test quality --provider glm`.

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
