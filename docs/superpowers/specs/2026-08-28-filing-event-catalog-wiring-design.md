# 파일링 이벤트 카탈로그·뉴스 폴백 연결 설계

- 날짜: 2026-08-28
- 상태: 사용자 리뷰 대기
- 브랜치: refactor/final-form-waves (작업 트리)
- 관련: 미공시→추정 3파일 수정(미커밋, 동반 배포), docs/MCP_SESSION_ISOLATION.md

## 1. 배경

2026-08-28 프로덕션 실사용자 세션 3건(NVDA·LRCX·CTAS)이 모두 "정기보고서 밖의 세계"를 물었다:

| 세션 | 판별 문서 | 당시 상태 | 프로덕션 답변 |
|---|---|---|---|
| LRCX 이사 2인 사임 | 8-K | **카탈로그에 있었음**(8/27 21:20 접수, Item 5.02, 태그 leadership_or_board_change) — 질문 9.5시간 전 | "평가할 수 없습니다" (사실과 어긋남) |
| NVDA 메모리 동반 하락 | 시세·뉴스 | **뉴스 계층에 있었음**(market_issues 8/27 15:46, 마이크론 -2%·샌디스크 -1.7%·WDC -2.7% 확정 팩트) | "시세 데이터가 없어 확인할 수 없고" + "이번 실행에서" 프로세스 누출 |
| CTAS 재선거 불참 | DEF 14A | 미제출(9월 패턴) | "아직 판별 문서 없음" (구조적으로 정확) |

근본 원인은 데이터 부재가 아니라 **연결 부재**다. 프론트는 EDGAR 폴링 카탈로그(`sec_filing_events`: 8-K 2,130건/353티커 + Form 4 11,606건)와 한글 뉴스 이슈 계층(`market_issues`)을 이미 운영하며, 이를 읽는 MCP 도구 8종(`search_catalog_filings`, `get_filing_brief`, `list_feed_items`, `get_feed_context` 등)이 배포돼 있고 krw-feed 에이전트가 사용 중이다. 채팅 질문이 도달하는 krw-ontology(company_research) 배포 바인딩에는 이 엔드포인트들이 전부 없다.

## 2. 목표 / 비목표

**목표**: 사건형·시장반응형 전제 질문에서 분석가가 3단 폴백 사다리를 타고, "미확인"은 세 단계를 전부 확인한 뒤에만 선언한다.

**비목표(명시적)**:
- 섹션 열독(`read_filing_section`), Form 4 내부자거래, DEF 14A 수집 — 후속 확장 후보
- 야후파이낸스 등 비공식 외부 API — 약관·안정성 문제로 거부(아래 7. 결정 기록)
- 뉴스/시장 엔트리포인트 라우팅 전환(라우터↔엔트리포인트 갭) — 별도 스레드
- DCF·밸류에이션 (사용자 지시로 별도 스킬로 분리 예정)

## 3. 증거 폴백 사다리 (핵심 설계)

| 단계 | 소스 | 증거 등급 | 답변 표현 |
|---|---|---|---|
| 1. 파일링 카탈로그 | sec_filing_events (공시) | direct / strong | "8-K(2026-08-27 접수, Item 5.02)" |
| 2. 피드 뉴스 | market_issues (등록 매체) | related / medium | "등록 매체 보도 기준(공시 미확인)" 라벨 의무 |
| 3. 웹 뉴스 | 외부 뉴스 API(FMP 엔진, 중립 이름) | related / medium | "외부 보도 기준(공시 미확인)", 원 매체명 인용 |
| 없음 | — | — | 정직 스코핑 + 빠진 앵커 후속질문 1번 |

- 2·3단 히트 시에도 "공시 확인은 후속" 프레임 유지 — 뉴스는 사건의 존재 근거지, 결론의 최종 근거지가 아니다.
- 확정/미확정 팩트 구분(market_issues.confirmed_facts/unconfirmed_facts)을 답변 근거 문구에 반영("보도에 따르면" 계열 유지).

## 4. 변경 구성요소

### 4.1 배포 바인딩 (local + prod, krw-ontology 배포에 추가)
- `krw-filings` 엔드포인트: `search_catalog_filings`, `get_filing_brief` (값은 krw-feed와 동일: run-scoped, KRW_FILINGS_MCP_TOKEN)
- `krw-feed` 엔드포인트: `list_feed_items`, `get_feed_context` (동일 패턴)
- 신규 바인딩 `krw_web_news_search`(중립 이름): krw-agnet 소유 서빙 경로에서 FMP 뉴스 엔드포인트 호출. 구현 위치(기존 market.snapshot FMP 서빙 경로 확장 vs tool-mcp 기반 신규 서버)는 구현 계획에서 확정.

### 4.2 agent.yaml (agents/krw-ontology/agent.yaml)
capability 5종 추가:
- `filing.search_events` → binding `search_catalog_filings`. 입력 krw-filing-search-input/v1(ticker 필수, form_type/limit 선택). scope_binding trusted_ticker_set /ticker (krw-feed 패턴).
- `filing.event_brief` → binding `get_filing_brief`. filing_event_id는 직전 검색 결과에서 관찰된 ID로 제한(chain 도구의 관찰 object_id 가드와 동일).
- `news.feed_list` → binding `list_feed_items`. 티커 스코프.
- `news.feed_context` → binding `get_feed_context`. issue_ids는 관찰된 목록 결과로 제한(입력 계약: 1~8개).
- `news.web_search` → binding `krw_web_news_search`. 티커 스코프. 오류 시 fail-open.

워크플로 company_research_v2:
- `assess_obligations`에서 신규 전이 `event_lookup_has_value` → `event_filing_search`(max 1) → ingest → `event_filing_brief`(max 1, 조건부) → ingest → 복귀.
- `assess_obligations`에서 신규 전이 `news_lookup_has_value`(사건형 전제 & 카탈로그 빈 결과, 또는 시장반응형 질문) → `news_feed_list`(max 1) → ingest → `news_feed_context`(max 1) → ingest → 복귀.
- 카탈로그·피드 모두 빈 결과일 때만 `news_web_search`(max 1) 허용.
- validators action_limits: search ≤1, brief ≤1, feed_list ≤1, feed_context ≤1, web_search ≤1 + 선행 규칙(search→brief, list→context, "web_search는 1·2단 확인 후").
- ingest_evidence max_visits 7 → 12 조정(기존 7종 + 신규 5종 전부 허용 기준 — 기존 주석의 "advertised evidence-producing read 전부 수용" 원칙 유지).

### 4.3 증거 처리 (capability-runtime / krw-ontology-adapter)
- 파일링 이벤트 증거: directness=direct, grade=strong, 근거 필드 = 서류유형·접수일·item·event_tag·브리프 발췌. PublicCitation은 중립 제목(예: "SEC 파일링 이벤트(8-K)") — 벤더/내부 경로 명칭 없음.
- 뉴스 증거(2·3단 공통): directness=related, grade=medium, citation에 원 매체명+발행시각. 벤더명 금지.
- 동반 수정: 기존 market snapshot citation 제목의 "FMP" 문자열 중립화(krw-ontology-adapter lib.rs의 PublicCitation title).

### 4.4 프롬프트 3곳 (미공시→추정 수정과 동일 파일 — 한 배포로 통합)
- `evidence-analyst.md`: 신규 트리거 — 질문 전제가 특정 사건·발표·변동·시장 반응을 언급하면 '미확인' 선언 전에 사다리 조회가 의무. 8-K 메타데이터(서류유형·날짜·item·태그)의 직접 근거 지위 명시. 뉴스 근거의 등급·라벨 의무.
- `research-synthesis.md`: 인용 형식 — "8-K(2026-08-27 접수, Item 5.02)", "등록 매체 보도 기준(공시 미확인)". 뉴스 히트 시에도 공시 확인 후속질문 유지.
- `final-markdown-contract.md`: 체크리스트 신설 — 사건형/시장반응형 질문에서 사다리 조회 없이 '확인할 수 없음' 계열로 종결 금지. 뉴스 근거에 "보도 기준" 라벨 확인.

### 4.5 벤더 은닉 정책 (사용자 요구: "FMP라고 인식하면 안 됨")
- 도구·바인딩 이름 중립(news.web_search / krw_web_news_search).
- agent.yaml answer_policy.forbidden_user_terms에 FMP, Yahoo Finance, 야후파이낸스 추가.
- 프롬프트에는 "등록 외부 뉴스 조회"로만 소개. 에이전트와 최종 답변 어디에도 API 운반자명 미노출. 인용은 원 매체명.

## 5. 데이터 흐름 (재현 시나리오)

**LRCX**: 질문 → query_context(10-K/10-Q 없음) → 이벤트 갭 인지 → filing_event_search(LRCX, 8-K) → 8-K(8/27, Item 5.02) 적입 → 브리프 적입 → 기존 근거(주주환원)와 결합 → "8-K로 확인 + 전이 경로 + 제 판단" 답변.

**NVDA류(시장반응형)**: 질문 → query_context(공시 근거 병행 확보) → 뉴스 갭 인지 → news_feed_list(NVDA) → 이슈 "엔비디아 실적 호조에도 메모리 주식 동반 하락" 관찰 → news_feed_context → 마이크론 -2% 등 확정 팩트 적입 → 공시 근거(FY2026 실적)와 결합 → "보도 기준 전제 확인 + 공시 근거 분석 + 제 판단" 답변.

**1·2단 모두 빈 경우**: news_web_search(1회, fail-open) → 히트 시 원 매체 인용, 미힛/오류 시 현재와 동일한 정직 폴백.

## 6. 오류 처리

- 카탈로그·피드 장애/타임아웃: 런 실패 아님. 해당 단계를 건너뛰고 다음 단계/정직 폴백. 프로세스 누출 문구("이번 실행에서") 금지는 기존 계약 유지.
- 웹 뉴스: 타임아웃 짧게(~5초), 1회 시도, 재시도 없음, 오류는 사용자에게 보이지 않는 fail-open.
- 빈 결과: "카탈로그·피드에 없음"이 미확인 판정의 명시적 근거가 됨(스코프 문구로, 사실 부정이 아님).

## 7. 결정 기록

- **야후파이낸스 거부**: 비공식 엔드포인트(약관 회색지대·취약성) + 외부 장애면 추가. 사용자가 제안한 대안 중 FMP 채택.
- **FMP 엔진 채택 근거**: 이미 market snapshot이 FMP 소스로 동작 중(어댑터 매핑 존재), 키 프로비저닝됨, 공식 API, 원 매체 publisher 필드로 정직한 귀속 가능.
- **벤더 은닉**: 3겹(이름 중립 + 어휘 금지 + 원 매체 인용). 기존 FMP citation 문자열도 중립화.
- **fail-open 3단계 채택**: 최악의 경우가 현재와 동일(UX 역행 없음), 사용자에게 보이는 새 실패 모드 없음.

## 8. 테스트 (GLM-5.3-flash)

### 모델 교체
- deployments/{local,prod}/model-registry.glm.yaml: model_id glm-5.3 → glm-5.3-flash, 컨텍스트/출력 스펙 갱신(1M/131K — 현 204,800 캡 유지 여부는 능력 프로브 후 결정).
- crates/protocol/src/lib.rs: GLM_MODEL_ID/ALLOWED_MODEL_IDS 갱신. provider-wire SSE fixtures("glm-5.3" 참조) 동반 수정.
- 사전 능력 프로브: 코딩플랜 엔드포인트에서 glm-5.3-flash로 tool_choice=required·thinking·JSON 확인. 미지원 시 provider_wire_capabilities 플래그 조정.

### 품질 매트릭스 신설
- `lrcx_event_8k`(필수 통과): 8-K 인용 + 사건 확정 + 전이 경로 + 제 판단.
- `nvda_market_reaction`(필수 통과): 보도 기준 전제 확인(마이크론 등 숫자) + 공시 근거 분석 + "보도 기준" 라벨.
- `ctas_proxy`(필수 통과): "명세서 미제출" 정직 답변 유지 — 위양 금지.
- 통제 2건(일반 메트릭 질문): 사다리 도구 미호출 회귀 확인.
- 기존 50코퍼스에서 10케이스 서브셋 스타일 회귀(볼드 첫 문장·후속 3·라벨·프로세스 누출 0).

### 리스크 완화
- flash가 스타일 계약 통과율을 낮출 수 있음 → 실패 케이스는 glm-5.3(풀)로 재실행해 프롬프트 효과와 모델 효과 분리.
- 카탈로그 신선도는 폴링 주기에 종속(관찰상 시간 단위 — 채팅 제품 기준 충분).
- 로컬 dev-stack에서 1·2단 도구가 실 데이터를 보는 경로 확인(바인딩·자격증명 세팅) — 미비 시 fixtures 리플레이로 대체.

## 9. 출시 순서

1. GLM-5.3-flash dev-stack 검증(위 매트릭스)
2. 커밋 — 미공시→추정 3파일 + 본 설계 변경 동반(같은 프롬프트 파일, 한 배포)
3. DeepSeek 프로덕션 배포 (배포는 항상 DeepSeek)
4. 프로덕션 E2E: LRCX 질문 재현으로 8-K 인용 답변 확인
