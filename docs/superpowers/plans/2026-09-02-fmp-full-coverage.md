# FMP 전체 커버리지 조사 및 계획 — 2026-09-02 (조사·계획 단계, 개발 미착수)

## 목적

FMP 개발자 문서(https://site.financialmodelingprep.com/developer/docs)의 전
엔드포인트 중 openbb(ODP)가 FMP 프로바이더로 이미 노출하는 것과 놓치는 것을
전수 대조하고, 갭을 채우는 최적 경로를 결정한다. 사용자 요구: "내부 데이터가
필요하면 전부 FMP로 이끌어낸다" + "FMP 위젯에 없는 것도 싹다" + DCF 위젯.

## 조사 방법与 실측 근거

- FMP 문서 전문 리더로 수집(24개 섹션 전체).
- ODP 실측: `127.0.0.1:6900/widgets.json`(위젯 403개, 고유 라우트 203개) 및
  `/openapi.json`(플랫폼 전체 라우트 278개).
- FMP 백킹 라우트 69개 실측 추출(펀더멘털 17·소유권 6·추정 6·캘린더 5 등).
- 라이브 동작 실측(스타터 키): income/metrics(AAPL·VIPS·GRAB), discovery/filings.

## §1 커버리지 매핑 (FMP 문서 섹션 → openbb FMP)

| FMP 섹션 | openbb FMP | 비고 |
|---|---|---|
| Company(프로필·피어·시총·임직원·주식통계) | ✅ 9종 | profile/peers/historical_market_cap/market_snapshots/share_statistics/management(+comp)/employee_count |
| Financial Statements(손익·재무·현금·성장·메트릭·비율·EPS·배당·분할) | ✅ 14종 | as-reported/원시필링 포함 |
| 세그먼트(제품·지역 매출) | ✅ 2종 | revenue_per_segment/geography |
| ESG | ⚠️ 일부 | esg_score만; ratings/benchmark 풀은 갭 |
| Quote/Chart | ✅ 대부분 | equity quote·historical·performance; 애프터마켓 갭 |
| Calendar | ✅ 5종 | dividend/earnings/ipo/splits/events |
| Economics | ✅ 3종 | treasury_rates/calendar/risk_premium(+yield_curve) |
| Earnings Transcript | ✅ 1종 | fundamental/transcript |
| News | ✅ 2종 | company/world; 변형(언론보도 검색 등) 갭 |
| Analyst(추정·가격목표) | ✅ 5종 | ratings/grades 계열은 갭 |
| Market Performance(gainers/losers/active) | ✅ 3종 | 섹터·산업 스냅샷/역사 갭 |
| Screener | ✅ 1종 | equity/screener |
| SEC Filings(8-K/10-K 등) | ✅ 2종 | discovery/filings + fundamental/filings |
| Insider Trades | ⚠️ 기본 | insider_trading; by-name·유형·통계 변형 갭 |
| 13F | ⚠️ 기본 | ownership/institutional; 홀더 성과·산업 분해 갭 |
| Senate/House | ⚠️ 기본 | government_trades; by-name/profiles/net-worth 갭 |
| Indexes | ✅ 3종 | available/constituents/historical |
| Crypto/FX | ⚠️ 부분 | search+historical; 풀시세·일괄·1분봉 갭 |
| ETF | ✅ 10종 | holdings/info/sectors/countries/exposure/nport/… |
| Mutual Funds | ❌ 전체 | openbb에 fund/ 라우트 없음 |
| **Commodity** | ❌ 전체 | openbb 커모디티 라우트는 spot뿐(FRED 전용). FMP list/quotes/historical 전부 갭 |
| **DCF** | ❌ 전체 | 기본/levered/custom×2 — openbb 어디에도 없음 |
| Technical Indicators | ⚠️ 대체 | openbb /technical/* 라우트 존재(278 중 다수, POST) — FMP 라우트와 무관한 내장 계산, MCP로 노출 가능. 위젯은 안 됨(POST) |
| M&A | ❌ | mergers-acquisitions |
| Market Hours/Holidays | ❌ | |
| TipRanks 제휴 | ❌ | 상위 플랜 가능성 |
| Bulk | ❌ | openbb는 개별 라우트 설계 — 대체 불필요 |
| Fundraisers(크라우드펀딩·증자) | ❌ | |
| Search(symbol/CIK/CUSIP/ISIN) | ⚠️ 대체 | equity/search는 존재하나 FMP 아님(SEC·Nasdaq·Intrinio 등) — 대체 충분 |
| COT | ⚠️ 대체 | CFTC 프로바이더가 전담 |

요약: **핵심 조사 재무(재무제표·메트릭·필링·소유권·캘린더·뉴스)는 이미
FMP 경유로 존빈(69 라우트). 진짜 갭은 ①Commodity ②DCF ③Mutual Funds
④M&A ⑤Market hours ⑥세부 변형(insider/13F/상원·하원/애널리스트 등)
⑦TipRanks·Fundraisers·Bulk 저빈도.**

## §2 전략 옵션

### 옵션 A — FMP 공식 MCP 병렬 등록 (즉시, 공수 0.5일)

FMP가 **자체 MCP 서버**를 운영: `https://financialmodelingprep.com/mcp?apikey=<키>`
— "전 REST 엔드포인트를 도구로 래핑, 기존 API 한도 그대로" (공식 문서 실측).
엔진 MCP 엔드포인트 레지스트리(feed/filings/openbb와 동일 6단계 패턴)에 등록.

- 장점: 갭 **전체**가 당장 도구화. 유지보수 FMP 측. 플랜 한도 공유.
- 단점: 위젯·ODP 통합 없음(엔진 전용). 원격 의존 + 키가 URL 쿼리(보관 격리
  필수, 로그·아티팩트 비출 협정 준수). 도구 수 수백 개 → 큐레이션 필수(교리상
  어차피 필수). openbb 단일 경로 원칙과는 병렬이 됨.

### 옵션 B — openbb 확장 프로바이더 (사용자 원 방향, 위젯+MCP 동시)

독립 pip(예: `openbb-fmp-extra`, AGPL 격리 원칙: krw 번들 금지)에서:
1. 기존 표준 라우트에 FMP fetcher 추가: `commodity/price/spot`.
2. 신규 커스텀 라우트: `commodity/price/historical`, `equity/valuation/dcf`,
   `equity/ownership/ma`, `economy/market_hours`, … 큐레이션 서브셋.

- 장점: ODP 위젯 **자동 생성** + openbb-mcp **자동 도구화** + 코파일럿 —
  1구현 3서비스. 로컬 캐시·레이트리밋·직렬 통제 용이.
- 단점: 엔드포인트당 fetcher+모델 작성 공수, FMP 문서 변경 추적 부담.

### 옵션 C — 혼합 (권고)

- **0단계(즉시)**: 옵션 A로 FMP 공식 MCP 등록 → 갭 데이터(커모디티·DCF 재료·
  M&A 등)를 엔진 조사에 당장 사용. 큐레이션은 krw 실사용 엔드포인트만.
- **1단계(위젯 수요분만)**: 옵션 B로 DCF·커모디티 확장 — 위젯이 필요한 것만
  프로바이더로 승격(사용자 요청 = DCF 위젯).
- **롱테일**: TipRanks·Bulk·Fundraisers·세부 변형은 옵션 A 경로 유지, 위젯
  수요 발생 시 승격.

## §3 스타터 플랜 게이트 (실측 필요)

- 실측된 스타터 동작: 펀더멘털(income·metrics·니치티커)·필링검색·시세.
- 미실측(상위 플랜 가능성): TipRanks, Bulk, 일부 상세 변형, 1분봉 실시간.
- 검증법: 각 엔드포인트 첫 호출 시 402/403 관찰 — 옵션 A 등록 직후 큐레이션
  스파이크에서 자동 기록. (플랜 매트릭스 비공개 부분 존재.)

## §4 실행 계획 (승인 대기)

| 단계 | 내용 | 산출 | 공수 |
|---|---|---|---|
| P0 | FMP 공식 MCP 등록 스파이크: 엔진 MCP 레지스트리에 `fmp-official` 바인딩(키=env), 도구 목록 덤프, 스타터 게이트 실측 표 | 갭×플랜 실측표 | 0.5일 |
| P1 | openbb 바인딩 병합(대기 중 feat/openbb-endpoint) + 큐레이션에 FMP 필링·펀더멘털 흡수 | 엔진 FMP 직접 경로 | 0.5일 |
| P2 | `openbb-fmp-extra` 확장 프로바이더: commodity(표준 fetcher) + DCF 신규 라우트(결정론 계산: FMP 현금흐름 입력, 시나리오 3종) + market_hours | ODP 위젯 자동 생성(DCF·커모디티) | 1~2일 |
| P3 | "/dcf" 코파일럿 스킬(브리지 스킬 경로 이미 지원) — 위젯 숫자와 동일 라우트 참조 | 워크스페이스 /dcf | 0.5일 |
| P4 | 품질 루프 검증: 커버 밖 티커(VIPS 등) FMP 폴백 답변 + "온톨로지 밖" 배너 실측 | 판정서 | 루프 1회 |

리스크: ①AGPL — 확장 pip 독립·krw 번들 금지(기존 원칙) ②키 보관 — env
주입·토큰 미출력(기존 규칙) ③openbb-core <2.0 핀 호환 ④FMP 스타터 한도 —
큐레이션 상한 + 캐시 ⑤원격 MCP 키가 URL 쿼리 — 레지스트리 비밀 보관 격리.

## §5 결정 필요 사항 (사용자)

1. 옵션 C(혼합) 확정 여부 — 본 문서의 권고.
2. P0의 FMP 공식 MCP 등록 승인(원격 MCP + 키 env).
3. P2 확장 프로바이더 명명/위치(샌드박스 `~/krw-ontology-v2/openbb-fmp-extra` 권장).
