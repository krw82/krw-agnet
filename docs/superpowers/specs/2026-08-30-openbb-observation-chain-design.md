# OpenBB 기반 멀티 프로바이더 관측·체인 계층 설계 (거시·뉴스·미시 전 도메인)

- 날짜: 2026-08-30
- 상태: 사용자 리뷰 대기
- 브랜치: main (문서 커밋)
- 관련: [2026-08-28 파일링 이벤트 카탈로그·뉴스 폴백 설계](2026-08-28-filing-event-catalog-wiring-design.md), krw-ontology 저장소(스파인 v4·샤드 v2·차트 사이드카), krw-ontology-front(워크스페이스·valuation 도메인)

## 1. 배경

사용자 요청(2026-08-30): 리서치 엔진에 OpenBB식 멀티 프로바이더 데이터(FMP·FRED 등)를 붙여 ① 분석 데이터 확장·온톨로지화, ② 언뜻 다른 데이터(거시 지표·뉴스·미시 지표)의 체인 연결, ③ 장기적으로 프론트 투자 워크스페이스 + AI 차트. 전제 조건: 프로젝트는 오픈소스로 공개 예정(AGPL 부담 해소), OpenBB는 그대로 쓰지 않고 프로젝트에 맞게 변형, 과설계 금지(필요한 것만).

현 상태(2026-08-30 코드베이스·문헌 조사 완료):

- **FMP 시장 스냅샷 라우터 이미 존재** — `krw_capability_runtime/market/snapshot.py`. 폐쇄형 라우터(엔드포인트 2개·지표 6개 화이트리스트·60초 TTL·`advisory_only`). 모듈 주석에 "프로바이더 선택 노출이 아님"이 명시된 설계. Supabase `valuation_snapshots` 우선 → 직접 FMP 폴백 구조.
- **온톨로지**: 31개 오브젝트 타입, `(ticker, doc_type, period)` 아티팩트 → 텍스트 스팬 → 근거등급으로 역추적. 스파인·샤드 전역 `ticker NOT NULL`. 시세·밸류에이션 질문은 `research_router`의 `VALUATION_STOP`으로 라우팅 거부 중.
- **체인 기반 이미 존재**: `ExternalFactorExposure.factor`(+벤치마크), 전역 `factor/topic/metric/entity` 스파인, `global_chain_index`(티커 간 연결, link_type 체계).
- **차트 파이프라인 완비**: `chart_series.sqlite` → MCP `_meta` → Rust `krw-presentation`(line/bar/donut 생성) → `chat_message_visualizations` → 프론트 `KrwVisualizationCard`. **카드는 area·stacked·heatmap·scatter·waterfall까지 이미 렌더 가능** — 컴파일러가 앞서야 할 뿐.
- **프론트**: Next.js + Supabase. `valuation_snapshots` 도메인 + FMP 리프레시 워커 운영 중. 워크스페이스 페이지는 start·companies뿐(대시보드 없음).

## 2. 목표 / 비목표

**목표**:

1. 시세·밸류에이션 배수·거시 지표·추정치/컨센서스·발표 이벤트를 하나의 **관측 계층**으로 수집·저장한다.
2. 거시→팩터→기업, 발표→가격반응, 추정 개정→파일링 실적, 뉴스→관측 앵커의 **4종 체인 질의**가 가능하다.
3. 엔진이 관측 데이터를 **advisory 답변 + 차트**로 서빙한다(밴더 중립 유지).
4. 워크스페이스에서 사용자 직접 조작 질의(프론트 직접 쿼리)와 AI 차트(엔진 런)를 제공한다.

**비목표(명시적)**:

- OpenBB Workspace 제품(UI) 도입 — 참조 모델로만 활용, 자체 프론트 구축
- 관측 숫자의 1급 온톨로지 오브젝트화(접근법 B, §16 결정 기록 D1에서 거부)
- 실시간 스트리밍 시세 — 일별 마감 + 짧은 TTL 스냅샷(기존)으로 충분
- 관측 데이터의 추천·가격목표 근거화 — 영구 금지(기존 교리 유지)
- 온톨로지 스파인(v4)·샤드(v2) 스키마 변경 — 불변 유지
- OpenBB의 ~100 프로바이더 노출면 채택 — fetch 라이브러리로만 사용

## 3. 핵심 원칙: 증거 계층과 관측 계층의 분리

온톨로지 오브젝트의 증명 방식은 **텍스트 스팬 역추적**이다. 관측값(시세·거시 수치)의 증명 방식은 **출처(provider)·관측시점(phenomenon time)·산출시점(result time)·빈티지**다. 증명 방식이 다른 데이터는 저장 계층도 달라야 하며, **체인 레이어가 논리적으로 잇는다**.

| 계층 | 예 | 증명 방식 | 등급 상한 | 저장 |
|---|---|---|---|---|
| 증거(텍스트) | 파일링 인용구, 발표문 원문 스팬 | 스팬 역추적 | direct/strong | 온톨로지(기존) |
| 관측(숫자) | 종가, PE_TTM, CPI, 컨센서스 | 출처·시점·빈티지 | advisory(Unverified~medium) | observations.sqlite(신규) |
| 체인(링크) | CPI 서프라이즈→금리 팩터→MSFT 노출 | 결정론적 조인 규칙 | 참조용(인과 주장 불가) | chain 인덱스 확장 |

## 4. 아키텍처

```
[프로바이더]  FMP · FRED · Polygon  ← openbb-core 라이브러리 + 필요시 커스텀 프로바이더
                 │  정기 배치 수집 (일별 마감 후 + 거시 발표 캘린더 트리거)
                 ▼
[관측 스토어]  indexes/observations.sqlite  ← 불변 릴리스 아티팩트 (+.verify.json 씰)
                 │  프로젝션 3방향
     ┌───────────┼─────────────────────┐
     ▼           ▼                     ▼
[체인 레이어]  [엔진 서빙]           [Supabase 리드모델]
 팩터·발표·개정  krw_market_series     market_series_daily
 링크 생성      krw_macro_series      macro_series
     ▲           (고정 라우터 MCP)     estimate_consensus
     │
[텍스트 소스] 뉴스·거시발표·어닝발표 → 기존 온톨로지 파이프라인
              (BusinessEvent 서브타입, 근거 인용 유지)
```

**수집 파이프라인**: krw-ontology 저장소 내 신규 모듈(`src/krw_ontology/observation/`). 실행 단위는 배치(스케줄러는 기존 ops 관행 따름). OpenBB는 `openbb-core` + 선택 프로바이더 패키지(`openbb-fmp` 등)를 **라이브러리로 임베드**하되, 각 소스는 내부적으로 하나의 **프로바이더 포트**(인터페이스) 뒤에 격리한다 — openbb 경유가 불안정한 소스는 동일 포트의 직접 REST 구현체로 교체 가능(FMP는 기존 직접 구현체가 이미 존재). 빈티지·산출시점 표현이 부족한 지점은 커스텀 프로바이더/후처리로 보강한다(= "바꿔서 쓰기"). AGPL은 오픈소스 공개 전제로 무해.

**무결성 규칙**: 관측 스토어도 스파인과 동일한 릴리스 트랜잭션 — 임시 빌드 → 검증 → `manifest.json` → 원자 프로모트(`current` 심링크). 수집 실패 시 이전 릴리스 유지(불변성).

## 5. 데이터 도메인 큐레이션

| 도메인 | 소스 | 내용 | 단계 |
|---|---|---|---|
| 시세/거래 | FMP quote + Polygon 일별 OHLCV | 종가·거래량·일별 캔들 | P1 |
| 밸류에이션 배수 | FMP ratios-ttm(기존 연동 확장) | PE/PB/PS 역사 | P1 |
| 거시 지표 | FRED 핵심 ~40시리즈 + **ALFRED 빈티지** | 시계열 + 개정 이력 | P1 |
| 추정치/컨센서스 | FMP analyst-estimates | EPS/매출 컨센서스 + 개정 히스토리 | P2 |
| 발표 이벤트 | FRED release 캘린더 + FMP earnings calendar + 발표문 원문 | (실측, 컨센서스, 서프라이즈) | P2 |
| 기관지분 13F | FMP holdings | 분기 보유 변화 | P2 |

FRED 1차 시리스 세트(FRED-MD 8그룹 분류 참조): 물가(CPIAUCSL, PCEPILFE), 고용(UNRATE, PAYEMS, ICSA), 소득/생산(GDP, GDPC1, INDPRO, RSAFS), 금리(FEDFUNDS, DFEDTAR, T10Y2Y), 장단기 금리(DGS2, DGS10, DGS30), 주택(HOUST), 시장(VIXCLS, SP500). 목록은 `series_catalog` 시드 파일로 관리하고 코드 하드코딩하지 않는다.

## 6. 관측 스토어 스키마

`indexes/observations.sqlite`, 버전 상수 `krw-ontology-observations/v1`(+ 빌더 버전 상수). 테이블 5종:

1. **`series_catalog`** — qb(DataStructureDefinition) 역할. `series_key PK`, `domain(price|valuation|macro|estimates)`, `provider(fmp|fred|polygon)`, `provider_series_id`(예: `CPIAUCSL`), 정규 지표명, `unit`, `frequency`, `adjustment`, `dimensions_json`.
2. **`observations`** — SOSA 관측. 복합 PK `(series_key, phenomenon_time, vintage)`, `value`, **`result_time`**(산출시점), `provider_ref`, `provenance_json`(요청 URL·응답 해시·수집 배치 id). 관측시점/산출시점 분리가 ALFRED식 개정·빈티지를 표현하는 표준 방식(SOSA/SSN).
3. **`ohlcv_observations`** — 가격은 한 행(FIBO `InstrumentPricing` 명명). PK `(ticker, trade_date)`, open/high/low/close/volume, `provider`, `adjusted` 플래그.
4. **`observation_revisions`** — **supersede 엣지**: `(new_observation_id, prior_observation_id, revision_kind)` + `result_time`. 문헌 조사 결과 표준 미존재 → 본 프로젝트의 오픈소스 기여 포인트.
5. **`release_events`** — 발표 이벤트: `event_type(macro_release|earnings_release)`, `series_key` 또는 `ticker`, `release_time`, `actual`, `consensus`, `surprise`(= actual − consensus), 근거 `source_document_id`(발표문이 온톨로지에 있으면 연결).

**지표 사전 통합**: 시세·배수·거시·추정 지표를 기존 `metric_dictionary.yaml`의 `canonical_metrics`에 등록한다. `MetricDictionaryCatalog.canonicalize`가 이미 있으므로 SearchPlan의 `metrics` 검증을 그대로 통과하며, 차트 사이드카·질의 계약이 하나의 지표 어휘를 공유한다.

**용량 점검**: OHLCV 355티커 × 10년 ≈ 89만 행. SQLite 여유 범위. FRED 40시리즈 × 빈티지 포함 수십년치도 수백만 행 이하.

## 7. 체인 레이어 — 4종 링크

| 링크 | 정의 | 생성 시점 | 등급/제약 |
|---|---|---|---|
| `macro_factor_exposure` | 매크로 시리즈 → 팩터 → `ExternalFactorExposure.factor` 조인 | 관측 릴리스 빌드 시 결정론적 | 참조용, 인과 아님 |
| `event_reaction` | `release_events` × `ohlcv_observations` 전후 거래일 창(기본 ±5일, 창 크기 링크에 기록) | 빌드 시 결정론적 | `causal_inference_allowed: false` |
| `estimate_anchor` | 컨센서스 개정 ↔ 기간 매칭 `MetricObservation` | 빌드 시 결정론적 | SUE 등 파생치는 기존 `Calculation` + `calculated_from` 패턴 |
| `news_observation_anchor` | 뉴스/파일링 이벤트에 당시 관측 앵커(시세·컨센서스) 부착 | 빌드 시 결정론적 | 뉴스 등급(related/medium)은 기존 폴백 사다리 유지 |

**팩터 분류**: 신규 `ontology/schema/factor_taxonomy.yaml` — 매크로 시리즈를 FIBO IND 정렬 팩터(interest_rate, inflation, labor, growth, housing, market_volatility)로 매핑. `ExternalFactorExposure.factor` 값과 조인되는 정규 어휘를 제공한다. 매크로 시리즈는 ticker가 없으므로 **스파인에는 넣지 않고** 이 조인으로만 체인에 참여한다(스파인 `ticker NOT NULL` 유지 — 결정 D7).

**체인 서빙**: `krw_ontology_chain` 확장 — 관측 이웃 응답에 `kind: "observation_association"` 추가. 관측 연쇄로 답변할 때는 출처·시점 표기 의무 + strong_claim 금지.

예시 체인 질의: *"9월 CPI 서프라이즈 이후 MSFT 밸류에이션 압축?"* → CPI `release_events`(surprise) → `macro_factor_exposure`(inflation→interest_rate) → MSFT `ExternalFactorExposure`(interest_rate, 파일링 근거) + MSFT PE_TTM 시계열(관측) → `event_reaction` 창 내 압축 여부. 텍스트 근거와 관측 숫자가 한 체인에.

## 8. 텍스트 파이프라인 통합 (뉴스·발표문)

- 신규 `document_type`: `macro_release`(BLS·FRB 발표문), `earnings_release`(어닝 보도자료). `normalize_doc_type`(`config/constants.py`) + `doc_type_key` 추가. 발표문은 짧으므로 **경량 프로파일**: 인용구·이벤트·숫자 추출 위주, claims 단계 축약 옵션. 파이프라인 체크포인트·아티팩트 구조는 그대로 재사용.
- 이벤트 객체는 기존 `BusinessEvent`의 **서브타입**(`macro_release`, `earnings_announcement`) — ontology-layer-map의 호환명 방식이라 신규 오브젝트 타입 등록 불필요. 발표문 원문 스팬이 근거로 붙으므로 진짜 evidence grade 획득(관측 숫자와 대조되는 지점).
- **교차검증**: 발표문에서 추출된 숫자 ↔ `release_events.actual` 불일치 시 `quality_events` 기록(모니터링 지표).

## 9. 엔진 노출 (도구 2종 + 정책 1건)

| 도구 | 고정 쿼리면 | 등급 |
|---|---|---|
| `krw_market_series` | 티커 × 지표(가격·배수·컨센서스) × 기간 → 시계열 + 최근값 | advisory, strong_claim 금지 |
| `krw_macro_series` | 거시 시리즈 + 최근 발표(실측/컨센서스/서프라이즈, 발표문 근거 포함) | advisory(발표문 인용은 direct 가능) |

구현은 기존 6단계 체크리스트 그대로:

1. `contracts/kernel/v1/schemas/` 신규 입력·결과 JSON 스키마(content_hash 핀)
2. `descriptors.py` 등록 + `CapabilityRegistry` 도구 수 어설션 28 → 30 범프
3. `agents/krw-ontology/agent.yaml` 캐페빌리티·워크플로우 상태·제한(스냅샷과 동일한 `scope_binding: trusted_ticker_set` 패턴)
4. 어댑터 `map_market_series` / `map_macro_series`(`krw-ontology-adapter`) — 형식 검증 후 `advisory_only` 재기입, 밴더명 스크럽
5. 배포 바인딩(`deployments/*`) + 엔드포인트 레지스트리(기존 `krw-ontology-local` 재사용, 신규 엔드포인트 불필요 — 같은 Python 런타임이 서빙)
6. 시작 프리플라이트 `tools/list` 검증 통과

**정책 변경(명시적 결정, P2 시행)**: `research_router`의 `VALUATION_STOP` 완화 — *관측 사실(현재 밸류에이션·시세·거시 수치·컨센서스)은 출처·시점 표기 하에 답변 가능. 가격목표·매수추천의 근거로는 기존대로 금지.* 밴더 중립(`answer_policy.forbidden_user_terms`에 FMP 등 유지)과 "보도/관측 기준" 라벨 규칙은 그대로.

**컨텍스트 블록**: 신규 `TrustedMacroContext`(`ContextSegmentKind`/`LoadReason` 쌍 확장). 질문이 거시 팩터에 노출되면(플래너 팩터 투영 기반) run-start에서 최근 거시 발표 스냅샷 프리패치 — 기존 market snapshot 프리페치 패턴 재사용.

## 10. AI 차트 확장

- 관측 시리즈(가격·배수·거시·컨센서스)를 기존 `chart_series` 사이드카에 합류 — `source_class`에 관측 계열 추가, 빌더 버전 범프. `_meta` → Rust 컴파일러 → `chat_message_visualizations`까지 기존 파이프라인 전부 재사용.
- 컴파일러(`krw-presentation`) 신규 종류 — **프론트 카드가 이미 렌더 가능한 것만**: `area`(시계열 강조), `multi_line`(가격 vs 컨센서스 vs 실적), `scatter`(밸류에이션 매핑), `combo`(가격+거시 이중축), 신규 인텐트 `reaction`(발표 전후 창). `CHART_SAFE_CANONICAL_METRICS` 화이트리스트에 관측 지표 추가.
- 그라운딩 규칙 유지: 관측 포인트는 `evidence_ref: observation_id`(스토어 PK 문자열), 레저에 없는 참조 아티팩트는 기존 필터로 삭제.

## 11. 프론트/워크스페이스 (P3) — OpenBB 네이티브 통합 (2026-08-31 확정 v2, 심층 조사 기반)

**방향**: krw는 자체 프론트를 만들지 않고 **OpenBB 생태계에 1급 구성원으로 편입**된다 — krw 데이터는 OpenBB 데이터 프로바이더가 되고, 프론트는 OpenBB Workspace를 그대로 쓰며, 리서치 런은 OpenBB Copilot에서 호출 가능한 krw 에이전트로 제공된다. 조사 확정(2026-08-31): 워크스페이스 소스는 아직 비공개(2026-08-25 전체 제품 오픈소스 발표, 순서·시기 미정) → **포크는 트리거 조건부**, 그 전까지는 공개 인터페이스만으로 전부 구현 가능.

**산출물 4종** (의존 순서):

1. **`openbb-krw` 프로바이더 패키지** (pip) — openbb-core 프로바이더 인터페이스(QueryParams/Data/Fetcher, `require_credentials=False`), **커스텀 메타모델 허용 확인**: `KrwFilingFact`(근거등급 파일링 팩트), `KrwMacroVintage`(빈티지 거시 시리즈), `KrwMetricObservation` 등 + 표준 모델 매핑(시세 OHLCV 등은 표준 재사용). 라우터 확장으로 `obb.krw.*` 엔드포인트 → Python·REST·Excel·**openbb-mcp**(MCP 도구 자동 노출) 전 표면에서 krw 데이터 1급 시민. 참조 구현: openbb-sec, openbb-fred(무키 프로바이더 선례). 별도 pip 패키지로 배포(ODP 저장소 AGPLv3 임베드 회피 — 관대한 라이선스 전환 확정 전까지 번들 금지).
2. **krw 백엔드 서비스** (FastAPI, backends-for-openbb 패턴) — `/widgets.json` + 위젯: 파일링 팩트 테이블, 근거 체인 뷰, 빈티지 차트, **잡 큐 위젯 세트**. **큐 시스템은 OpenBB 제품군에 존재하지 않음을 확인** → 리서치 런 큐(`krw-agent run` 제출/상태/결과 엔드포인트 + 위젯 refreshInterval 폴링)는 krw 백엔드가 소유. 데이터 소스 = Supabase 리드모델(기존 P3 계획 유지) + 온톨로지/관측 스토어.
3. **krw 에이전트** (openbb-ai SDK, MIT) — 워크스페이스 `/agents.json` + `/query` SSE 등록. 내부적으로 우리 엔진을 호출(krw MCP 도구), **한국어 응답** + `citations`(근거 인용)·`chart`/`table` 이벤트 → 코파일럿 대화 안에서 한국어 심층 리서치. Copilot 자체를 프로그래밍 호출하는 API는 없음 — 에이전트 등록이 공식 경로(확인).
4. **포크 트리거** — 워크스페이스 저장소가 관대한 라이선스로 실제 공개되는 시점에 재평가. 한국어 크롬(메뉴·시스템 UI)이 필수라 판단될 때만 포크. 그 전까지 한국어는 **층위별 구현**: 위젯 이름·설명 한국어(코파일럿이 읽는 메타데이터), 에이전트 출력 한국어, 마크다운/HTML 위젯 콘텐츠 한국어.

**리스크 기록**: ① 스투워드십 공백(회사 운영 종료 중, 공개 일정·라이선스 명칭 미정) → 모든 로직은 프로바이더/백엔드/에이전트 층에 두고 폐쇄 프론트엔드와 결합하지 않는다 ② ODP AGPLv3 → openbb-krw는 독립 패키지, 전환 확정 전 번들 금지 ③ 동기 위젯 UX — 긴 런은 큐+폴링, 스트리밍은 에이전트 채널에서만(SSE).

**P3 착수 첫주 검증 스파이크(필수)**: ① cookiecutter로 프로바이더 스캐폴드 → 커스텀 엔드포인트 + `openbb-build` + `openbb-mcp` 노출 e2e ② 무료 pro.openbb.co 계정으로 에이전트 등록 → 한국어 SSE + citations 렌더 확인 ③ 무료 계정으로 로컬 백엔드 연결(CORS/HTTP) 확인. 세 가지가 통과하면 no-fork 경로 확정.

## 12. 단계화

| 단계 | 내용 | 완료 기준 |
|---|---|---|
| **P1 기반** | 수집 파이프라인 + `observations.sqlite`(시세·배수·FRED+빈티지) + `metric_dictionary` 확장 + 도구 2종 + 차트 사이드카 합류 | 도메인 3종 조회·차트 응답, 픽스처 테스트 녹색, 매트릭스 런 소수 케이스 통과 |
| **P2 체인** | `release_events` + 팩터 조인 + 반응 창 + 추정 개정 + `VALUATION_STOP` 완화 + `TrustedMacroContext` + 발표문 파이프라인 | "CPI→금리→기업" 체인 질의 골든 케이스 통과, 정책 완화 후 환각 0 매트릭스 |
| **P3 OpenBB 네이티브** | 검증 스파이크 3종 → `openbb-krw` 프로바이더 패키지 + krw 백엔드(위젯·잡큐) + krw 에이전트(한국어·citations) + Supabase 프로젝션 + 차트 종류 확장 | OpenBB Workspace에서 krw 위젯·프로바이더 데이터 렌더링 + 코파일럿에서 한국어 리서치 런 e2e |

각 단계는 독립 배포 가능(단계 간 계약은 스키마·도구 계약이 경계).

## 13. 에러 처리 (기존 교리 재사용)

- 프로바이더 실패 → `status: "unavailable"` 페이로드(에러 아님, 15초 TTL)
- 수집 실패 → 이전 릴리스 유지(불변성), 배치 재시도는 ops 관행
- 타임아웃·응답 바이트 상한(스냅샷 라우터 기준값 재사용)
- 관측값 서브셋 실패(예: ratios 누락) → 나머지 관측은 유효(기존 quote 우선 원칙 동일)
- openbb-core 의존성 장애 시 폴백: FMP는 이미 직접 구현체(`FmpResearchProvider`)가 있으므로 최소 시세·배수는 라이브러리와 무관하게 지속 가능

## 14. 테스트 전략

- **픽스처**: 프로바이더 응답 스냅샷(FMP/FRED/Polygon 각 도메인) — 기존 파이프라인 픽스처 관행
- **스키마**: `observations.sqlite` 검증기 + `.verify.json` 씰(스파인과 동일 규율: 테이블 집합·인덱스 허용목록·버전 상수 하드 실패)
- **체인 골든 테스트**: 실제 사례 기반(예: 특정 CPI 서프라이즈 → 금리 → MSFT exposure → PE 압축) 링크 생성·서빙 검증
- **엔진**: 어댑터 매핑 단위 테스트(형식 위조·밴더 누출·등급 위반 거부) + P1/P2마다 매트릭스 런 소수 케이스 추가(기존 `--case` 서브셋 관행)
- **정책 완화 검증**: P2에서 시세·밸류에이션 답변 케이스의 환각·볼드 부정형·프로세스 누출 검사 포함(기존 품질 스레드 기준)

## 15. 학술·표준 근거 (2026-08-30 문헌 조사)

채택 패턴과 근거(상세 조사 보고서는 세션 기록):

- **SOSA/SSN(W3C Rec)** — `phenomenonTime`/`resultTime` 분리로 개정·빈티지·as-of 표현. 관측 오브젝트의 뼈대.
- **RDF Data Cube(qb)·SDMX** — 시리즈 구조 정의(차원·측정·속성). `series_catalog`의 원형. 공식 통계계 표준.
- **FIBO(EDM Council, MIT)** — `InstrumentPricing`(OHLCV 명명), `IND`(지표·금리), `CAE`(기업행동 이벤트 유형). 명명 참조 모델. 최신 LLM 금융 KG 연구(FinKario 2025)도 FIBO 앵커링으로 수렴.
- **발표 서프라이즈 패턴** — Andersen·Bollerslev·Diebold·Vega(AER 2003): surprise = actual − consensus. 검증 데이터: ECB EA-MPD, FRBSF 이벤트 스터디 DB(공개).
- **FRED-MD(McCracken & Ng, JBES 2016)** — 128 월간 지표 8그룹 분류. 1차 시리스 세트 선정 기준.
- **거시 예측(nowcasting)** — Giannone-Reichlin-Small(2008): 발표 시점·빈티지가 수준보다 중요 → result_time/vintage 스키마 정당화.
- **이중 이벤트 계층** — 인스턴스 사변형(FinDKG, ICAIF 2024) + 추상 인과 그래프(ELG, ACL 2019) 분리. 본 설계는 인스턴스(증거 등급)만 1급으로, 추상 인과는 향후 추론 레이어 후보.
- **supersede/개정 엣지** — 표준 미존재 확인 → 본 프로젝트 기여 포인트.
- **AEVS(MDPI Computers 2025)** — 문자급 출처 추적을 1급 출력으로 하는 유일한 선행; 본 시스템의 스팬 근거 등급과 방향 일치.

## 16. 결정 기록

- **D1. 접근법 A(관측 레이어 분리) 채택** — 증명 방식이 다른 데이터는 계층을 분리하고 체인으로 잇는다. 대안 B(완전 온톨로지화)는 스키마 v5 전면 재빌드 + 스팬 없는 데이터에 근거등급 강제 = 의미론 오염으로 거부. 대안 C(OpenBB 사이드카 MCP)는 관측 스토어 부재로 체인·개정이력·워크스페이스 프로젝션 전부 불가하여 거부.
- **D2. OpenBB는 라이브러리로 임베드, 노출면은 미채택** — 오픈소스 공개 전제로 AGPL 무해. 단, 엔진에는 큐레이션된 고정 라우터만 노출(기존 "프로바이더 선택 노출 금지" 교리 유지).
- **D3. 관측 스토어 = 온톨로지와 동일한 불변 릴리스 아티팩트** — Supabase가 아니라 릴리스 트랜잭션/.verify.json 규율 적용. Supabase는 P3 리드모델 프로젝션 대상. (사용자 승인: 2026-08-30, 1부 리뷰)
- **D4. FRED 빈티지(ALFRED) 1차 포함** — 개정 이력이 체인 신뢰도의 뿌리. (사용자 승인: 그대로 진행)
- **D5. VALUATION_STOP 완화는 P2 시행** — 관측 사실 답변 허용 + 출처·시점 표기 + 추천 근거 금지 유지. (사용자 승인: 2부 "그대로 진행")
- **D6.~~자체 프론트~~ → ~~위젯 백엔드만~~ → 2026-08-31 v2 확정: OpenBB 네이티브 편입** — 프론트를 자체 제작하지 않고 OpenBB Workspace를 그대로 사용하되, 접점은 위젯 백엔드 한 개가 아니라 **프로바이더 패키지 + 백엔드 서비스 + 에이전트** 3개 산출물(§11). 사용자 결정, 심층 조사(2026-08-31) 뒷받침: 커스텀 프로바이더·비표준 엔드포인트·무키 프로바이더 전부 공식 지원, Copilot의 에이전트/MCP 확장 공식 경로, 워크스페이스 소스는 미공개 → 포크는 트리거 조건부.
- **D8. `openbb-krw`는 독립 pip 패키지** — ODP 저장소가 아직 AGPLv3이므로 같은 저장소/번들 배포 금지. 관대한 라이선스 전환 확정 전까지 분리 유지.
- **D9. 잡 큐는 krw 소유** — OpenBB 제품군 전반(ODP·Workspace·Copilot)에 잡 큐·스케줄러·비동기 시스템이 존재하지 않음을 확인(2026-08-31). 리서치 런 큐는 krw 백엔드가 제출/상태/결과 엔드포인트로 소유하고 위젯 refreshInterval로 폴링. 스트리밍 UX는 에이전트 SSE 채널.
- **D10. 한국어는 층위별 구현, 포크는 트리거** — 워크스페이스 i18n 미지원 확인. 위젯 메타데이터·에이전트 출력·콘텐츠의 한국어는 오늘 가능. 시스템 크롬 한국어는 워크스페이스 소스가 관대한 라이선스로 공개된 뒤에만(포크 트리거) 고려.
- **D7. 매크로 ticker 문제 — 스파인 미변경** — 매크로 시리즈는 스파인/샤드에 넣지 않고 `factor_taxonomy` 조인으로만 체인 참여. 스키마 대범프 회피.

## 17. 리스크

- **openbb-core 의존성 트리·버전 드리프트** — 임베드 범위를 최소 패키지로 제한, FMP 직접 구현체를 폴백 유지(§13).
- **FRED API 레이트리밋/키 관리** — 배치 수집 + 캐시(스토어 자체가 캐시), ALFRED는 대량 다운로드 엔드포인트 활용.
- **컨센서스 데이터 품질(FMP estimates 커버리지)** — P2에서 시리즈별 커버리지 리포트를 수집 배치 산출물로.
- **체인 오탐(상관↔인과 혼동)** — `event_reaction`은 창·수치만 제공, 인과 문구 금지는 프롬프트·등급 이중 방어.
- **P2 정책 완화 후 환각 위험** — 매트릭스 케이스로 검증(기존 품질 스레드 기준 적용), 필요 시 완화 범위 축소 복귀.
- **관측 스토어 용량·빌드 시간** — 1차 도메인 기준 SQLite 여유(§6), 빌드 시간은 배치 스케줄 여유로 흡수.
