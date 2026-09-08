# 최적화 루프 로그 — 2026-09-07 (이터레이션 1~7)

## 이터레이션 7 (같은 날): 소문형 질문·라벨 폴백 검증 + 조사 3건

- **I1(AVGO 인수 소문) 통과** — 공시 확인 사항(M&A 전략 리스크 공시,
  VMware 영업권 542억·무형자산 456억, 2025년 35억 인수, 자사주
  110억 확대)과 보도성 정보("보도 기준, 공시 미확인" 명시)를 분리하고
  8-K 갱신 트리거 제시. 회사 문이 filing 이벤트·feed 뉴스 레인 사용.
- **I2(장비 업황 AMAT/LRCX) 통과** — 제품 믹스(파운드리·로직 67%,
  NAND 4→7%), AGS 서비스 성장, 3분기 누적 시스템 181.5억, 고객집중
  리스크(삼성·TSMC 15-20%) + 시세·금리 배경. LRCX 미확보 솔직 고지.
- 인용 라벨 폴백("공시 근거") 라이브 확인 — ev:hash 노출 없음.
- 조사: 트랜스크립트 재검(여전히 상류 422 — 외부 블로커 유지),
  차트(visualizations)=서버 _meta 프레젠테이션 팩 필요 — 엔진 컴파일
  파이프라인(3개 한계·핑거프린트·원장 그라운딩 필터)은 완비, 팩을
  실어주는 쪽(온톨로지 릴리스/어댑터)이 비어 있음 → 크로스-리포
  기능으로 문서화. 광고 스키마 정합(model_input_contract 쌍)은
  남은 비용이 '간헐적 수리 1턴'(correction 에지로 회복 가능)이라
  과설계 판정으로 보류. 누적 15문 통과.


무한 루프: 질문 → 답변 확인 → 0부터 문제점 검토 → 수정 → 반복.
슬라이스 1(2026-09-04, 4문 통과) 이후 첫 루프 이터레이션.

## 이터레이션 6 (같은 날): 워크스페이스 인용 체인 개통

모든 라이브 답변에서 인용이 0건이었던 근원: 게이트웨이 v1 final_output에
애초에 evidence 필드가 없었다(서비스의 유연 파서는 빈손). 답변 번들은
처음부터 evidence_ids를 갖고 있었고, 노출되지 않았을 뿐.

- **0024 마이그레이션**(642f939): agent_v1.read_final_projection이
  유계(문자열·64개·순서 보존) evidence_ids 배열 반환. host-ts 클라이언트
  exact 검증 + 로컬 게이트웨이 통과. 서비스(be0300f, e46b1b1)는 id의
  구조 필드(kind:티커:기간:문서)에서 자연 라벨("NVDA · CY2024 · 10-K
  발췌") 유도, 불투명 ev:hash는 "공시 근거".
- **배터리 H**: H1(MU 전망형) 통과 — HBM4 36GB 12단 샘플, HBM→일반
  DRAM 전환 리스크(공시 직접 인용), Clay fab·CHIPS 조건, 수행의무
  1/3, 3항 체크리스트 + 판단 전환 조건. **인용 45건 라이브**.
  H2(SHOP) 1차는 프로바이더 일시 장애로 조회 0건 폴백(자연화 폴백
  정상 출력) → **H2B 통과**: 4분기 표(제출일 포함), 흑자전환의 질
  의심(순이익 +15억 vs 영업손익 근사 -7억 — 영업 외 가능성 명시),
  openbb.news 사용, **인용 19건**. 누적 13문 통과.

## 이터레이션 5 (같은 날): 근본 해결 강조 — correction 라우팅 완결 + 비미국 상장

소유자 재강조("근본문제 해결 1순위")에 따라 두 근본 수정:

1. **correction 라우팅 완결(3edb2ac)**: 엔진의 input_correction_required
   라우팅은 원래 일반 메커니즘이지만 5개 워크플로 모두 query_context에만
   에지가 있어, 조회 상태(타겟 질의·추적·시세/거시/오픈비비 전 조회·웹뉴스)
   에서 서버 4xx가 나면 전환이 실패해 런이 capability_failure로 사망.
   전 조회 상태에 correction_required/unresolved → frontier 에지를
   선언해 모델이 위반 내용을 보고 재발행하는 경로를 완성(커널 거절이
   이미 쓰는 회복 형태와 동일). capability-runtime 픽스처도
   response_format 제거로 갱신(81fd707 이후 미실행이었음).
2. **비미국 상장 라우팅(9e5b3e2)**: 삼성전자(005930.KS) 질문이 외국
   발행인 조항이 든 유니버스 계획 4회 재시도 + 외국 티커 시장 배치로
   수리 예산을 전부 태우고 데이터 0건 사망(G2). 시장 데이터면은 미국
   상장 전용임을 프롬프트에 명시 — 비미국 주체는 커버 동종업종 공시로
   간접 판독(은행 문 D3 패턴).

배터리(신규 3문 + 재검 1문): **G1(AMD vs 인텔 비교형) 통과** — AMD
공시급 심층(영업현금흐름 30.4→77.1억, MI308 충당금 소멸 분해, 수출통제
경고 직접 인용) + 인텔 수치 부재를 솔직히 고정하고 비교 축 제시.
**G3(금리인하→반도체) 통과** — KLAC/AMD 공시의 금리-수요 논리 역방향
적용, AMAT 100bp→$462M 금리민감도(공시 수치), LRCX 주주환원, 실측
금리곡선(단기 역전 관측 + 양날 해석). **G2→G2B(삼성전자 동일문)
실패→통과** — G2B는 경쟁사 5개사 분기 영업이익으로 축별 판 세도 +
메모리 사이클 추정(제 추정) + 판 전환 관측 포인트. 누적 11문 통과.

## 이터레이션 4 (같은 날): 탐색 정밀도 + 폴백 노트화 — F 배터리 통과

이터레이션 3의 E3(COIN) 결함(무관 종목 홍수 → 원장 오염 → 출력 예산
소진 → 8KB 덤프형 폴백)을 양쪽 끝에서 수정(744acc8):

1. **폴백 노트화**: 렌더 상한 16기록×8팩트 → 6×3(계산 4, 목표 4),
   `metric_context`(내부 인증 JSON) 팩트는 렌더에서 완전 제외.
2. **주체형 질문 탐색 축소**: "COIN 요즘 분위기"형 질문은 섹터 키워드에
   묶은 1개 조항으로 동종업종 존재만 확인하고 본체 시장면 조회에 예산을
   쓰도록 프롬프트 개정. 넓은 다중 조항 탐색은 산업형 질문 전용.

검증(동일문+신규문):
- **F1(COIN 동일문) 통과** — 폴백 아님, 완결 답변. 4분기 순이익
  (+4.3→−6.7→−3.9→−3.6억달러, 제출일 포함), 매출총이익 회복,
  EBIT 개선, 8-K 관찰(인과는 미확인 명시), 관전 레벨(50일선 163).
  역량: 발견+query_universe+시장면 5종 — 원장에 무관 종목 0.
- **F2(AI 데이터센터 전력, 산업형 신규문) 통과** — 넓은 탐색 회귀
  확인. 전력 공급(SO 발전 635억/송전 178억/배전 317억 설비 분해) →
  인프라 부품(ADI 데이터센터 하위시장 성장 근거+전력화 수요) →
  장비·반도체(AMD Instinct/EPYC+관세 리스크, MU HBM/서버 DRAM,
  AMAT/MRVL 간접 경로). 근거 강도별 서열 + 반대 신호(관세) +
  역설 경로(전력 병목→송배전 투자) + 솔직한 갭.

## 이터레이션 3 (같은 날): 새 질문 배터리 + 뉴스 레인 개통

새 질문 5건(AMD 경쟁구도 / TSLA 실적 후 / 은행 업황 / NFLX ×2 / COIN):

- **D1(AMD, 커버) 통과** — FY23-25 매출 급등 시리즈 + MI350X 채택(공시) +
  NVIDIA 생태계 장악·Intel 공세적 가격·고객 자체칩 리스크(공시 위험요인)로
  경쟁 구도 구성. 순수 온톨로지 회사 문.
- **D2(TSLA, 비커버) 통과** — 본체 분기 손읩 테이블 + 8-K 발표일 이후
  주가·컨센서스 목표가 + "반등 확인 단계" 관점과 되돌림 기준치.
- **D3(은행, 무티커) 통과** — 커버 은행이 없는 업종에서 실제 금리곡선
  (1M 3.79%→2Y 4.37%) + 커버 기업 공시의 이자 수입/비용 신호(AVGO·AMAT·
  TER·ADI)로 금리 국면을 읽고 누락(NIM·대출 성장)을 솔직히 구분.
  "못 찾았음 끝이 아님" 교리의 실전 예시.
- **E1(NFLX) 실패 → 수정 → E2 통과**: 런이 배치 거절(decision_batch_
  size_invalid — 발견 직후 상태는 1호출)과 뉴스 사다리 거절(web_search
  선행)로 수리 턴 5개를 태우고 관측 0회로 "근거 부족" 답을 냄.
  프롬프트에 실제 커널 행동을 정확히 기술(77c2555) → 같은 질문에서
  본체 손익+캘린더+시세 5역량, 두 실적발표 갭(-9.7%/-7.3%) 분석,
  관전 레벨까지 갖춘 답으로 반전.
- **E3(COIN) — 뉴스 개통 + 신규 결함**: openbb.news 첫 실사용 확인
  (역량: 발견+filings+income+**news**+price+quote). 그러나 커버 밖
  섹터 탐색이 무관 종목(QCOM·MPWR·ADI·SO·AVGO·MRVL) 홍수를 반환해
  원장이 오염되고 **출력 예산 소진**으로 폴백. 8c6bc12의 자연화 폴백이
  실전 출력됨(사유 코드 없음, 자연 문장) — 폴백 자체는 교리 준수.
  **이터레이션 4 최우선: 커버 밖 섹터 질문의 탐색 정밀도 + 폴백 기록 상한.**

뉴스 미사용(7런 연속)의 근원은 모델 선택이 아니라 **상태 머신 누락**:
open_research_v1에 openbb.news 상태·has_value 에지·복귀 에지가 없어
도달 불가였음(프롬프트가 없는 문을 가리킴) → 추가(90210d6,
ingest 29→30). 추가 수정: degraded 폴백 자연화(8c6bc12 — 사유 코드
제거, 구조체 필드로 분리), news 의무 강화(25908b0).

## 이터레이션 2 (같은 날): 계약 드리프트 — C문 부분통과의 진짜 원인

배터리 재실행 결과: **A(SO) 통과, B2(PLTR) 통과, C2(반도체) 재사망**.
C2 사망 원인을 capabilityd 직접 재현으로 특정:

- 모델이 `ontology.query_universe` 호출에 `response_format: "json"`을
  붙임 → 라이브 릴리스의 서버 계약은 이 필드를 **extra_forbidden**로
  거절(input_correction_required, 모델 수정 가능 4xx).
- 그런데 커널의 정규 내보내기 스키마(체크아웃 = 미래 코드 기준)는 이
  필드를 **허용** — 즉 "체크아웃 ↔ 라이브 릴리스" 계약 드리프트가
  커널 검증을 통과시키고 서버에서 죽였다. 실패는 ambiguous(재시도
  가능)로 분류되지만 회수 예산을 태우고 dependency_unavailable 폴백
  (영어 인용 덤프)로 직행.
- **수정 81fd707**: 런타임 검증자(수제 Rust)에서 `response_format`을
  모델 저작 표면에서 제거 — 정규 스키마/해시 핀은 불변, company_context
  의 기존 교리("전송 어휘는 모델 표면이 아니다")와 동일. trace 입력도
  동일 처리. 필드 제거 시 서버가 정상 응답(38KB)함을 실측 확인.
- **C3 통과**: AMAT/ADI/AMD 회사별 재고 수치(온톨로지 공시) + 사이클
  국면 판독(조정→소진→재충전, 과잉 vs 정상 재충전 기준 제시) + 양방향
  시나리오 + 범위 갭 고지 + 후속 질문. 세 문(커버/비커버/무티커)
  전부 통과.

알려진 잔여 마찰: 모델에게 광고되는 도구 스키마(정규 내보내기)에는
필드가 남아 있어 첫 호출 1턴이 수리로 소비될 수 있음 — 차기
후보로 model_input_contract 쌍 도입(광고 정합) 기록.

## 배터리와 판정 (이터레이션 1)

| 문 | 질문 | 런 | 1차 판정 | 근거 |
| --- | --- | --- | --- | --- |
| A (커버 회사) | SO 최근 실적은? | run_f7acb45f | **통과** | 공시급 심층: 상반기 순이익 $25.31억(+14%), OCF $42.8억, 가스 비용 전가 87%, O&M 분해(보수 +$30M, Nicor +$11M, 법무 −$20M), Southern Power 스윙. 조건부 전망 + 관찰 지점 + 데이터 갭 고지. |
| B (비커버) | PLTR 실적발표 이후 분위기? | run_0b02e610 | 1차 부분 → **B2 통과** | B2(배치 드레인 후): 본체 실적(순이익 $10.6억, EPS 0.41, 매출총이익 16.4억) + 내부자 거래 색 + 역합의 프레이밍("강한 실적 + 높은 기대치 줄다리기"). |
| C (무티커 업황) | 미국 반도체 업황? 재고 사이클 | run_0248ec1b | 1차 부분 → **C3 통과** | 계약 드리프트 수정 후: 회사별 재고 수치 + 사이클 국면 판독 + 양방향 시나리오. |

## 이터레이션 1 수정 (7 커밋)

1. **a2dd806** openbb.news(news_company, fmp) wiring — 라운드4.
   스키마/JCS 해시/계약 매처/디스패치 암(기사 수 1..=10, 기본 5, 최근 2주
   윈도는 업스트림 소유)/agent.yaml 레지스트리+도구/프롬프트(최근 사건
   컬러 의무). 배포 바인딩 예약(deployments/local) + capability-runtime
   어드미션 목록 등록 포함(별도 2 커밋).
2. **8b58674** 플래너 원장 단조성: `apply_intent_progress`가 재유도
   커버리지 하락을 보면 런 전체를 죽이던 것(IntentGoalProgressDrift,
   라이브 run_cd071289 사망)을 상위 수역 병합으로 교체. 재현 테스트
   `rederived_lower_clause_coverage_keeps_committed_progress`. 라이브
   A 런(2개 인텐트 query_context)이 이후 생존·통과.
3. **eaa88e6** 자유 문 관측 배치 드레인: question_only ProposalUnmapped
   오버라이드가 리더만 실행하던 것을 ≤4 전부 pending_supplemental_calls
   큐로 드레인(각 호출은 동일 상태머신 전환·검증 통과).
4. **3720bb6** (krw-agent-service) 게이트웨이 429 흡수(제출+폴, 상한
   지수 백오프) + 러너 진행 콜백 → SSE 중간 reasoning step(60초 간격,
   "N분 경과"). 3동시 SSE 폴링이 120req/min 가드를 치면 서비스가 런을
   버리고 거절하던 라이브 결함.
5. fmt 정규화(1fbb178 등).

오프라인: contracts 37 / agent-image 33 / run-engine 179 /
research-planner 58 / agent-service 94 — 전부 통과.

## 확인된 외부 블로커 (내 샌드박스 밖)

- **실적발표 트랜스크립트**(equity_fundamental_transcript): 무지정·연도만
  → fmp 유료 `earnings-transcript-list` 402; quarter 지정 → OpenBB→FMP
  직렬화 버그 422(문자열 '2' literal 거부, PLTR/AAPL 공통). 해소 조건:
  fmp 플랜 또는 OpenBB Platform 수판 수정. 그 전까지 openbb.news가
  실적발표 커버리지 색을 대신 담당(프롬프트에 명시).

## 다음 이터레이션 백로그 (0부터 재검토 예정)

1. **온톨로지 일시 실패 회복**: query_universe 1건 실패(retryable_read=t)
   가 복구 예산을 태우지 않고 dependency_unavailable 폴백으로 직행한
   경로 추적 — 재시도 정책 또는 폴백 컴포저 자연화(한국어 서술형).
2. **상태별 배치 규칙 정합**: PLTR ep05 — post-discovery 상태에서
   4배치가 decision_batch_size_invalid로 거절(프롬프트의 "≤4 배치
   허용"과 불일치). 상태 머신 규칙을 프롬프트에 정확히 반영하거나
   해당 상태의 배치 허용 여부 재검토.
3. transcript 블로커 해소 후 라운드5 wiring(모델 계약
   {ticker, year, quarter 필수} — list 엔드포인트 402 회피).
4. 워크스페이스 경험: 진행 상황 스트리밍 배터리 검증, 인용 라벨
   한국어화, agents.json 기능 플래그(widget-dashboard-*) 재검토.
5. 잔여 openbb 도구 어휘 검증(~215건)과 4xx 재시도 분류는 슬라이스 1
   백로그에서 이월.

## 운영 메모

- 스택 재기동 절차: `cargo build -p krw-agent -p krw-agentd` 후
  boot-stack.sh(포그라운드 대기형 — run_in_background로 실행) →
  start-coverage-sidecar.sh / start-agent-service.sh. teardown.sh는
  사이드카까지 정리.
- 게이트웨이 레이트 가드 기본 120req/60s(노드 local-gateway.ts,
  KRW_AGENT_GATEWAY_RATE_LIMIT_MAX). 서비스 러너는 이제 429를 흡수.

## 이터레이션 8 — fmp 스타터 전수 조사 → 배치1 8라인 실전 배선 (2026-09-07 오후)

에이전트팀 3종(fmp 46콜 실측 / transcript·추정치 심층 / 워크스페이스 Apps·SDK)을
돌려 사용자 백엔드(OpenBB MCP 3.4.7)의 실제 커버리지 지도를 만들었다.

**실측 결론(Starter 플랜)**
- forward EPS/EBITDA/과거 추정치는 `limit≤10` 명시 시 연간 단위로 열림
  (기본 호출이 limit 초과로 402 — "유료"가 아니라 플랜 상한). 분기는 유료.
- transcript: 이 설치에서 provider가 fmp 단일(목록 엔드포인트 자체가 Starter
  위) + quarter 직렬화 버그. 워크스페이스 UI에 보이는 건 OpenBB 호스티드
  백엔드 경로라 내 키와 무관 — 로컬로는 불가 확정.
- Apps = 대시보드 템플릿(위젯 조합), 새 데이터 레인 없음. 번들 스킬 4종은
  개발 문서. 단, 워크스페이스 차트의 진짜 프로토콜 발견:
  `copilotMessageArtifact`(type table/chart/html + chart_params) —
  `presentationSeries` _meta는 공개 코드 전체 0건(우리 관습이었다).
  → 차트 백로그 재판정: 서비스가 엔진 visualizations를 이 이벤트로
  변환하면 됨(온톨로지 릴리스 불필요). 이후 슬라이스로 예정.
- fmp 비지원 20종 중 무키 대체 경로 발견: SEC(MD&A·13F·company_facts),
  finra(공매도), finviz(섹터 성과), yfinance(저평가 발굴), cboe(티커 검색)
  — 배치3에서 제공자 핀 확장(sec/finra/finviz/yfinance/cboe) 후 부착.
- FRED 라인은 백엔드에 fred_api_key가 없어 현재 사망(CPI는 OECD 핀이라
  정상). 사용자 액션 필요(무료 키).

**배치1 라인 8종 배선** (a9eb7c0): revenue_segment, revenue_geography,
price_performance, profile, insider_trading, forward_eps, forward_ebitda,
estimates_historical — 요청 계약 {ticker} 단일, 추정 3종은 limit=10 커널
핀. 스키마 16 + 디스크립터 113 + 런타임 목록 + 어셈블리 2암 +
open_research_v1 상태/엣지/교정 라우팅 + 바인딩 + 프롬프트 레인.
ingest 정원 가드(32)가 정확히 발동해 상한 동기화.

**라이브 배터리 B1/B2/B3 (커버 밖 발행체, 서비스 경유) — 3/3 통과**
- B1 VRT: 부문(제품 76%)·지역(미국 40%)·forward EPS 5개년(6.72→17.35,
  CAGR 27%) 테이블 + 프로필 색. 인용 34.
- B2 APP: 기간수익(-52% YTD)·내부자 Form 4 5인 'F-InKind 현물 이전' 정독
  ·뉴스 색(보도/공시 분리)·forward EPS. 인용 22.
- B3 RKLB: forward EBITDA 5개년(-5,592만→+9.58억)·EPS 경로·흑자전환 테제
  + 반증 포인트. 인용 33. 전 런 금지 토큰 0.

**루프가 잡아낸 결함 4종(전부 근본 수정)**
1. 이중 부트로 낡은 이미지 캐시(도구 없음)가 14518을 잡고 있었음 —
   게이트웨이가 서빙하는 agentd의 --image-dir와 캐시 이미지에 새 도구
   grep으로 검증하는 절차를 부팅 검증에 추가(운영 교훈).
2. openbb 업스트림 4xx가 평문 텍스트로 오면 mcp_text_json 단말 의존성
   실패로 런 전체 사망(APP 1차 런, period=quarterly 422) → 에러 플래그
   참이면 재시도 가능 provider_tool_error_text로 분류(성공 비JSON은
   기존대로 단말). 5d3e351 + 테스트.
3. 폴백 미달성 목표의 goal-<sha> 원본 id가 공개 마크다운으로 새어 나감
   (기계 검사 적중) → 건수 한 줄로 자연화. 5d3e351.
4. 계획 검증 플레이크 2종(objectives priority 누락 / period=quarterly)
   → 프롬프트에 스키마 필드·리터럴 규칙 명시(라이브 사례 인용).

**다음(이어서)**: 차트 copilotMessageArtifact 변환(서비스+엔진 팩 합성)
→ 배치2(fmp 부가) → 배치3(제공자 핀 확장 + MD&A·13F·공매도·섹터·발굴·
티커검색). mimosa 전체 감사 재실행 예약(enobufs 2회).

## 이터레이션 9 — 차트 파이프라인 실전 개통 (2026-09-07 심야)

워크스페이스 최고 경험의 마지막 큰 조각. 이전 재판정(copilotMessageArtifact가 진짜
프로토콜, presentationSeries _meta는 우리 관습)대로 두 반쪽을 만들었다.

**엔진(ecfc365)**: openbb 관측 시리즈 인제스트 직후 커널이 같은 데이터로
presentation pack을 합성(`run-engine/src/openbb_presentation.rs`). 모든 포인트가
인제스트된 증거 레코드 id를 참조 — 근거 필터·용량 상한 전부 기존 메커니즘 재사용,
합성 결함은 차트 누락으로만 발생(텍스트 답변 불변). 라이브가 가르친 가드 3종:
① 어댑터는 price 레인만 content ticker를 심어준다 — 호출부의 physical symbol을
별도 전달(ingest 시그니처 +3 호출부). ② 같은 메트릭에 두 fmp 필드(기본/희석 EPS,
지속/최종 순이익)가 별칭 매핑되면 조항 조인에서 2×2=4벌 중복 선으로 렌더 —
메트릭당 첫 필드만 채택. ③ TTM 행은 축에서 제외, 연간·분기 혼합은 연간 승리.
forward EPS 레인은 `mean` 필드를 capability 게이트로만 eps로 읽는다(타 레인
오독 방지).

**서비스(fbfed73)**: 엔진 visualizations를 SDK 2.2.0 공식 chart() 헬퍼로
copilotMessageArtifact 이벤트로 변환(청크 후·인용 전). 아티팩트당 주(主) 뷰만
발행 — 파생 성장률 뷰는 음수 기저에서 -3248%식 오해 유도 라인(라이브 VRT).
라벨을 범례 키로, 시리즈 키는 충돌 시 폴백.

**라이브 실증(VRT 추이 질문, 서비스 경유)**: SSE에 copilotMessageArtifact
2건 + 인용 22건 + 금지 토큰 0. CRWV 런은 answer_bundles.visualizations=1
(EPS 추이 라인, 근거=openbb-advisory id). 커버된 종목(SMCI)은 회사 문(온톨로지
경로)이라 차트 대상이 아닌 것도 확인 — 차트는 자유 문 openbb 관측에 성립.

오프라인: run-engine 184 그린(합성 유닛 5종 포함), 서비스 101 그린.

**차기**: 배치2(fmp 부가) → 배치3(제공자 핀 확장). mimosa 전체 감사 재실행
요청 3회 누적 — 차기 이터레이션 첫 작업으로 강제.

## 이터레이션 10 — 답변 깊이 3종 세트 + 배치 2 (2026-09-08 새벽)

소유주 피드백 "길이가 너무 짧다, 보고서 안 같다"에 대한 1·2·3단계 전부 즉시 적용.

**깊이(26776ac)**: ① 출력 계약에 애널리스트 노트 뼈대+길이 기대치(주제형
4천 자+/업황형 6천 자+, 단 "근거 확보 축만 깊이, padding 금지"). ② 프론티어에
3축 커버 규칙 — 실적/전망/환경 중 빈 축이 있으면 evidence_sufficient 금지,
남은 방문 예산은 가장 빈 축부터. ③ GLM 레인 전체를 glm-5.3-flash → glm-5.3으로
교체(엔진 설계상 role은 모델을 못 고르고 registry 슬롯이 단일 모델 — 컴포저만
바꾸는 건 구조적으로 불가능하여 워크플로 전체 승격. 프로토콜 상수+2 registry+
스택 스크립트+릴리스 도구+systemd/launchd+host-ts 미러+픽스처 핀까지 30파일).
전후 비교(고정 3문): VRT 1,835→2,286자·뼈대 7섹션·표 5개, 금리↔반도체 3,016자·
전달경로 3갈래(동종업종 공시 인용: AVGO 672억 달러 부채 만기구조 등)·표 7개,
금지토큰 0 유지. 액션 5→9회로 3축 규칙 실제 작동 확인(실적 배치→전망 배치→환경).

**같은 밤에 잡은 결함 2종**: ① 게이트웨이가 evidence_ids(642f939)와 타이밍
필드를 응답에 실으면서 Rust CLI의 deny_unknown_fields 파서가 매 폴링 실패 —
CLI 계약 확장(144588e). ② APP 재실행에서 mid-stream 전송 오류가 "timeout도
connect도 아님→재시도 불가"로 분류돼 증거 다 모은 뒤 폴백 강제(run_3262739a) —
2026-09-02 포스트모템이 run-engine 미러만 고치고 프로덕션 분류기
(execution-contracts)를 놓친 것. Http는 항상 재시도로(6f70032).

**배치 2(6f70032)**: 실측 프로브 14종 중 12통과 → 티커형 8(income_growth,
market_cap, share_statistics, management, eps_history, government_trades,
price_target, ratios — 후자 4종은 limit=10 핀, ratios는 period=annual 추가 핀)
+ 비티커형 4(news_world limit 핀, treasury_rates 날짜창 패스스루,
risk_premium, discovery_active). sp500_multiples은 multpl 전용 제공자,
screener은 fmp 응답 파싱 500 — 둘 다 배치 3(제공자 enum 확장)로 이월.
용량 3곳 동반 상향: ingest 32→44, MAX_CONTRACTS_PER_OPERATION 32→64
(프론티어 메뉴 36 상태), 바인딩 예제 35→47. 첫 부트에서 낡은 바이너리가
구 상한으로 거부 — 재빌드 후 정상(이중 확인 절차 다시 입증).

## 이터레이션 11 예비 — 배치 3 검증과 회복 경로 3결함 (2026-09-08 01:00)

배치 3(제공자 enum 6종 확장+7라인) 배선·커밋. 실전 검증: 테슬라 문
(mda+short_interest+income_growth+eps_history 9콜) 통과 — MD&A는 바우처
분할 규칙(시장 평면=관측)대로 "공시 원문 미확인"으로 정직 강등, 공매도는
FINRA 실데이터(2021 정산까지)를 날짜와 함께 렌더. MU 문(배치2) 통과 —
ratios·price_target·eps_history 실사용, 목표가 테이블 8개.

**미해결: 섹터 업황 문 2/2 실패(재시도도 동일 단계)**. 도구 평문 오류
(provider_tool_error_text, 재시도 분류 정상 작동) 뒤에 회복 경로가
3가지 양상으로 죽음 — ① InvalidRecoverySnapshot("checkpoint contains
an unreplayed action") ② InvalidProviderEpisode("provider episode
contract mismatch") ③ WorkflowResolution{outcome: "non-research
capability decision batch"}. 세 양상 모두 재시도 직후 발생 → 재시도-
재개(replay) 경로의 엔진 결함군으로 보임(핀비즈 레인이 오류를 자주
던져 첫 노출). 차세션 루트픽스 1순위. APP 재실행(전송 수정 검증)은
폴백 없이 통과했으나 뉴스 읽기 누락으로 1.4천 자 — 3축 규칙의 환경 축에
"왜 움직였지 문의 뉴스 의무" 명시로 대응(배치3 커밋에 포함, 효과는
차회 측정).

## 이터레이션 11 종결 — 회복 경로 4결함 루트픽스 + 멀티티커 라우팅 (2026-09-08 오전)

재시도-재개(deferral resume)가 재생(replay)에서 죽던 3양상의 뿌리는 하나:
**재생이 라이브 경로의 관용 경로 없이 결정을 재도출**. 4결함 수정(0bfecbb):
① 드레인된 관측 액션 재생 부재(accepted 영수증은 완전 재생, ambiguous는
실패 시도로 소진, 미도달은 펜딩 큐 복원) ② "action frontier advanced
without a pending episode" 불변식 — 드레인 영수증이 커밋된 에피소드에
묶이는 게 합법이므로 고아 액션 검사로 교체 ③ 재생 중 decide 오류에 라이브의
수리 지시 패리티 부재(5콜 배치 회복 에피소드가 재생에서 단말 사망)
④ retryable_read의 ambiguous 행 재시작 사망(canonical-args 멱등 읽기는
Begun으로 정규화해 재발행). 재생 에피소드 검증은 자기 기록 tool_schema로
(request_hash 신뢰와 같은 결정). 회귀 테스트: 드레인 중 재시도 가능 도구
오류→지연→재개→재드레인→커밋 전 주기(모킹에 fail_invoke_on 추가 — 첫
구현이 중첩 락으로 데드락, take() 시맨틱 함정).

**멀티티커(소유주 지적)**: 분류기가 여러 티커 중 하나만 뽑아 단일 종목
문으로 새어 들어 비교가 성립하지 않음 — "서로 다른 티커 2개 이상=null
(자유 문)" 규칙 추가, 라이브 실측(NVDA/AMD·PLTR/SNOW→null, SO·MU→추출).
서비스는 다음 부팅 때 반영.

**운영 함정 일반화**: e2e 게이트웨이-에이전트가 DB 클레임 큐로 연결되므로
같은 postgres를 보는 agentd가 몇 개든 런을 나눠 잡음 — 수정 빌드 후에도
**낡은 agentd 프로세스가 런을 잡아 "수정이 안 먹은 것처럼" 보임**(01:50
배터리의 정체; 3개 잔존). 부트 절차에 "debug agentd 정확히 1개" 단언 추가.
launchd 관리 production agentd(~/.local/share, deepseek)는 별도 DB라 무관.
f32c0076 런의 온톨로지 서버 순단(ambiguous→창 밖)은 진짜 인프라 플레이크
— 폴백이 설계된 답.
