# OpenBB 위젯 파이프라인 라인별 분석 및 구현 계획 — 2026-09-02 (조사·계획 단계)

/goal: agents-for-openbb·openbb-ai·OpenBB-finance org의 **위젯 처리 방식**을
코드 라인 단위로 엄격 분석하고, 계획을 세워 **검증 루프 3회**를 실측으로
반복한다. 개발 미착수.

## §1 조사 범위·방법 (전부 실제 클론 후 직독)

| 리포 | 핵심 파일 (줄수) | 역할 |
|---|---|---|
| backends-for-openbb | reference-backend/main.py(80)·core.py(166)·widgets_aggrid_table.py(337)·widgets_plotly_chart.py(609)·widgets_input_params.py·scripts/validate_widgets.py | 커스텀 백엔드 계약의 정본 + 공식 검증기 |
| openbb-platform-pro-backend | openbb_platform_pro_backend/main.py(245)·utils.py(202) | **ODP 위젯 자동 생성기 본체** (OpenAPI→widgets.json) |
| openbb-ai (기존 심층 분석 재검증) | models.py(1000)·helpers.py(379) | 에이전트 측 위젯 흐름 계약 |
| agents-for-openbb | testing/test_payloads/*.json | 공식 위젯 요청/응답 실측 표본 |
| 실자산 대조 | krw-backend widgets.py + 우리 브리지 contract.ts | 계약 준수 실측 (§4) |

## §2 위젯 처리 방식 — 3층 파이프라인 (라인 인용)

### A. 커스텀 백엔드 계약 (backends-for-openbb)

1. **등록**: `core.py:134-166` `@register_widget(config)` 데코레이터가
   `WIDGETS[widgetId] = config` 등록. `endpoint` 있으면 widgetId 기본값 =
   endpoint (core.py:155-160). `GET /widgets.json`이 이 dict를 그대로 반환
   (main.py:44-47).
2. **필수 필드**: 검증기 `validate_widgets.py:133` — `name, type, endpoint`
   3종. `VALID_WIDGET_TYPES`(19-31) = table·chart·table_ssrm·markdown·
   metric·note·multi_file_viewer·live_grid·newsfeed·advanced-chart·
   chart-highcharts·youtube. `VALID_PARAM_TYPES`(33-42) = text·number·
   boolean·date·endpoint·**ticker**·tabs·form.
3. **데이터 엔드포인트 반환형**:
   - table → **행 객체 JSON 배열** 그대로 (widgets_aggrid_table.py:46-50).
   - chart(type:"chart") → **Plotly figure JSON** (`fig.to_json()`,
     widgets_plotly_chart.py:139). 라인+바 이중축, 테마(다크 #151518) 예제
     구비.
   - 컬럼 정의는 `data.table.columnsDefs[]`에 AG Grid 형식으로 선언:
     field·headerName·cellDataType(text/number)·chartDataType(category/series)
     ·formatterFn(int/percent)·renderFn(greenRed/columnColor/titleCase)+
     renderFnParams.colorRules (widgets_aggrid_table.py:15-44, 60-116).
4. **파라미터**: `params[]` 항목 = paramName·description·label·type·value(기본)
   ·options[]·optionsEndpoint(동적 옵션)+optionsParams·multiSelect
   (widgets_input_params.py:125-131, 214-215). 쿼리 파라미터가 곧 엔드포인트
   인자.
5. **CORS**: `core.py:112` `pro.openbb.co`, `pro.openbb.dev`,
   `localhost:1420` 허용 (krw-backend가 실측한 것과 동일).
6. **검증기가 허용하는 `mcp_tool` 필드** (validate_widgets.py:361): 커스텀
   백엔드 위젯도 MCP 도구 매핑을 선언할 수 있다 — 코파일럿이 커스텀 위젯
   데이터를 표준 경로로 인용/조회하는 공식 수단.

### B. ODP 플랫폼 위젯 자동 생성기 (openbb-platform-pro-backend/main.py)

1. `openapi["paths"]`에서 `/api` 시작 + GET 라우트 전수 추출 (main.py:29-31).
2. `widget_id = operationId` — **라우트의 operationId가 곧 위젯/MCP 도구 ID**.
3. 쿼리 파라미터 → `params` 변환 (utils.py:14-60): sort/limit/order 제외,
   `chart` 파라미터 존재 시 차트 위젯 파생 플래그, provider 단일 enum이면
   고정, enum→options, anyOf enum 병합.
4. 응답 스키마 `results` → `columnsDefs` **자동 생성**
   (main.py:44-49, dataKey="results"). `date`/`period` 컬럼 있으면 index로.
5. `chart` 파라미터 있는 라우트는 **같은 엔드포인트로 차트 위젯 자동 파생**:
   `widgetId_chart`, defaultViz="chart", data.chart.type="line",
   dataKey="chart.content" (main.py:87-94).
6. **프로덕션 실측**: ODP 카탈로그에서 `_chart` 파생 위젯 51개, 타입 분포
   table 337·chart 51·markdown 8 (metric 0 — 생성기는 metric 안 만듦).

핵심 결론: **커스텀 프로바이더가 플랫폼에 마운트되면(=신규 GET 라우트가
생기면) 테이블 위젯+columnsDefs+차트 위젯까지 전부 자동**. 위젯을 손으로
쓰는 게 아니라 Data 모델 필드 설계가 곧 위젯이 된다.

### C. 에이전트 측 위젯 흐름 (openbb-ai + 공식 페이로드, 재검증)

- `QueryRequest.widgets{primary,secondary,extra}` (models.py:482-492) —
  primary=사용자가 "Add to context"로 고정, secondary=활성 대시보드.
- 위젯 **정의+파라미터**만 오고 데이터는 별도: 에이전트가
  `copilotFunctionCall get_widget_data{data_sources[{widget_uuid,origin,id,input_args}]}`
  뿌리고 연결 종료 → Workspace가 실행해 재POST (README 타이밍도).
- 공식 테스트 페이로드 실측
  (message_with_primary_widget_and_tool_call.json): ai 메시지 content는
  **JSON 문자열**, tool 메시지 `data[0] = {items:[{content, data_format}]}`,
  data_format은 **null일 수 있음**.
- **우리 브리지 구현과 대조: 전 필드 일치** (emit/read 양방향). 유일 예외:
  파라미터 `type:"string"` + `name:"ticker"` 조합이 공식 표본에 존재 —
  우리 `pickTicker`는 `type==="ticker"`만 보므로 이 경우 질문 정규식으로
  폴백됨(동작하나 우회). → 계획 P5 보강.

## §3 위젯 구현 경로 비교 (DCF·커모디티 관점)

| 경로 | 위젯 | MCP 도구 | 공수 | 비고 |
|---|---|---|---|---|
| A. 확장 프로바이더 (플랫폼 마운트) | **자동 생성** (테이블+차트) | **자동 노출** | 중 | ODP 재기동 필요; columnsDefs 자동; metric은 불가 |
| B. krw-backend widgets.json (직접) | 수동 정의 (테이블·Plotly 차트·metric 전부 가능) | mcp_tool 필드로 선언 가능 | 소 | 이미 검증된 패턴(매크로 위젯 5종); 히트맵 등 자유도 최대 |
| C. FMP 공식 MCP (직전 조사 옵션 A) | 없음 | 전 엔드포인트 즉시 | 최소 | 위젯 목적엔 부적합, 엔진 조사용 |

## §4 검증 루프 (3회, 전 회차 실측 수행)

### 1회차 — 계약 준수 (공식 검증기 + 공식 페이로드로 실측)

- krw-backend widgets.json(5종)을 공식 `validate_widgets.py`로 검증 →
  **위반 5건: `source`가 문자열인데 계약은 문자열 배열** (ODP 실측
  widgets.json도 배열). 수정 필요 (P5).
- 브리지 위젯 파싱/발신 ↔ 공식 페이로드 대조 → 일치. 예외 1건: 위 §2C의
  ticker 파라미터 타입 폴백.
- **판정: 조건부 통과** — source 필드 5건 수정 + ticker 이름 매칭 보강을
  계획에 반영.

### 2회차 — 위험·타당성 (플랫폼 실측 데이터로 재검)

- 차트 자동 파생 51건 실측 → DCF 라우트에 `chart` 파라미터 제공 시 라인
  차트 위젯까지 자동 확인. 단 **히트맵은 표준 차트엔진 밖** → 민감도
  히트맵은 경로 B(krw-backend Plotly)로만 가능. DCF는 이원화: 요약 테이블+
  라인(자동, 경로 A) / 히트맵(경로 B).
- ODP 데스크톱 env 오염 리스크: 확장 프로바이더를 ODP 내부 env에 설치하면
  앱 업데이트 시 소실 가능 → **별도 인스턴스 옵션**(openbb+확장+pro-backend
  직접 기동, 백엔드로 추가 등록)을 병행 제시. 워크스페이스는 멀티 백엔드
  등록 지원(실측: krw+ODP 동시 등록했음).
- **판정: 통과(수정 반영)** — DCF 위젯 이원화, 배포 옵션 2안 명시.

### 3회차 — 목적 완전성 (사용자 목표 전수 대조)

- "8-K든 뭐든 전부 FMP로" → FMP filings 라우트(이미 존재) + P1 큐레이션
  흡수로 충족. "없는 FMP 위젯 싹다" → P2 확장 프로바이더(커모디티·시장
  시간·M&A·펀드·DCF) + 롱테일은 FMP 공식 MCP(직전 조사 옵션 C와 정합).
  "DCF 위젯" → §3 이원화로 충족. "/dcf" → P4. **누락 발견: 없음.**
- 추가 편익 발견: krw-backend 위젯에 `mcp_tool` 선언 시 코파일럿이 krw
  위젯 데이터를 표준 경로로 인용 가능(검증기가 공식 지원) → P5에 포함.
- **판정: 통과** — 계획 확정.

## §5 실행 계획 (승인 대기; 직전 FMP 조사 107c771과 통합)

| 단계 | 내용 | 경로 | 공수 |
|---|---|---|---|
| P0 | ~~FMP 공식 MCP 등록 스파이크~~ → **2026-09-03 판정: 불가 폐기** (Bearer 401·`?apikey=`만 200 vs 엔진 쿼리 금지 `tool-mcp/src/lib.rs:626`; 상세 근거는 FMP 문서 §6). 스타터 게이트는 ODP 경유로 P1에 흡수 | — | 완료 |
| P1 | 엔진 openbb 바인딩 병합(완료 확인) + FMP 필링 큐레이션 + 스타터 게이트 ODP 실측 | — | 0.5일 |
| P2 | ~~`openbb-fmp-extra` 확장 프로바이더~~ → **2026-09-03 완료(f7aedb2, 자체 저장소 `~/krw-ontology-v2/openbb-fmp-extra`)**: 6명령(commodity list/quote/EOD·dcf·market_hours·mergers_latest), 단위 10/10, REST 마운트 6라우트 + `obb.fmpextra.*` 빌드 실증. 설계 변경 3건(모두 근본 원인): ① provider명 `fmpextra`(레지스트리가 진입점 이름으로 키 → fmp 재등록은 섀도잉) ② 자격증명 `fmpextra_api_key`(openbb-core가 provider명 접두 — 기존 fmp 키 값 재사용 안내) ③ 커모디티는 커스텀 라우트(표준 CommoditySpotPrices 모델에 symbol 차원 없음). 펀드/ETF 보유내역은 스타터 402 실측으로 의도적 제외. 라이브 데이터는 P6 마운트 시 측정 | A | 완료 |
| P3 | ~~DCF 위젯 이원화 마무리: krw-backend에 민감도 히트맵(Plotly)+metric 카드~~ → **2026-09-03 완료(13a06fe)**: 히트맵+기저 요약 2위젯, 58/58 테스트, 라이브 실증(AAPL 기저 $100.84, MSFT 요약 실데이터). metric **타입**은 공식 소스 전무(페이로드 계약 미검증)로 보류 — 검증 가능한 계약(chart/table)만 선적 | B | 완료 |
| P4 | ~~"/dcf" 코파일럿 스킬~~ → **2026-09-03 완료(bddfa55)**: `skills/dcf-valuation.md`(quant.dcf·민감도·위젯 숫자 정합 지시) + 합성 테스트(브리지 20/20). 워크스페이스 "/" 등록은 사용자 액션(스킬은 워크스페이스 소유 — 에이전트는 selected_skills 수신만 계약상 가능) | — | 완료 |
| P5 | ~~계약 정합 수리~~ → **2026-09-03 완료(13a06fe·8f87e45)**: source 배열화 7위젯 → 공식 검증기 **7/7 통과**(기존 5위반 소멸) · pickTicker 이름 매칭(공식 페이로드 `{name:"ticker",type:"string"}` 대응, 19→20/20) · mcp_tool은 불선언 판정(워크스페이스 MCP 서버 이름 전제 + 우리 코파일럿은 자체 citation 경로 — README D11 기록) | B | 완료 |
| P6 | ~~배포 경로 확정~~ → **2026-09-03 확정: 별도 인스턴스 채택**. 실측: 스파이크 venv+fmpextra에서 `openbb_platform_api` 부팅 → widgets.json 319개 중 **fmpextra 6위젯이 깔끔한 ID**(`fmpextra_dcf_fmpextra_obb` 등)로 자동 서빙(200). ODP env 직접 설치는 기각(앱 업데이트 시 env 소실 + 데스크톱 앱 내부 침입). 참고: pro-backend 생성기는 이 venv의 legacy util 라우트(no-ref)에서 깨져 /tmp 검증 사본에 no-ref 스킵 가드를 했고, 원시 `openbb_core` rest_api로 서빙하면 ID가 `fmp_extra_routers_…`로丑化되므로 **platform-api 경로가 정석**. 사용자 액션 2건: ① `~/.openbb_platform/user_settings.json`에 `fmpextra_api_key` 1행 추가(기존 fmp_api_key와 동일 값 — 값 복사는 사용자만) ② 워크스페이스 Add data에 `http://127.0.0.1:<port>/widgets.json` 등록 | — | 완료 |
| P7 | 품질 루프: 커버 밖 티커 FMP 폴백 + "/dcf" + 위젯 렌더 실측 판정서 | — | 루프 1회 |

리스크 롤업: AGPL 격리(독립 pip)·FMP 스타터 한도(큐레이션 상한+캐시)·
ODP env 소실(P6 별도 인스턴스로 회피 가능)·openbb-core <2.0 핀.
