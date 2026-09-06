# 티커 없는 자유 질문 — 오픈 리서치 비전 (Roadmap)

- 작성: 2026-09-06
- 상태: 승인됨 (2026-09-04) — 조각 1 구현 진행: `docs/superpowers/plans/2026-09-04-open-research-slice1.md`
- 계층: 이 문서는 **비전/로드맵**이다. 각 조각의 구현 명세는 별도 스펙(`docs/superpowers/specs/`)으로 작성하고, 각 스펙 → 플랜 → 구현 사이클을 따른다.

## 1. 문제

OpenBB Copilot 채팅 → krw-agent-service → 에이전트 게이트웨이 → 엔진으로 이어지는 질의 파이프라인은 "한 회사를 지정해서 조사"하는 **단일 문**으로 설계돼 있다.

- 게이트웨이 공개 계약이 `ticker`를 필수 문자열로 요구하고(`packages/host-ts/src/gateway.ts:141`), 컨텍스트는 항상 `company_ticker_set`으로 고정된다(`gateway.ts:73`).
- 티커는 정규식(`_TICKER_RE`, `krw_agent_service/runners.py:52`)으로 질문 텍스트에서 추출한다.
- 티커 없는 질문 → "티커를 언급해주세요" 안내문으로 거부(2026-09-04 도입, `flow.py`의 `NO_TICKER_MARKDOWN`).
- 미커버 티커 질문(온톨로지 355개 유니버스 밖) → 회사 문으로 들어가 온톨로지에서 근거를 찾지 못하고 ~10분을 소진한 뒤 정직한 거부로 종료한다.

그러나 실제 사용 패턴에서는 **티커 없는 질문이 더 많을 것으로 예상**된다. 예: (위젯을 가리키며) "이거 왜 이런거야?", "이거 관련된 회사 있나?", "이거 관련된 지표는 어떤 게 있나?", "인플레이션 오르면 어떤 종목이 좋을까?". 티커 질문에는 규칙이 많아 현재 회로로 충분하지만, 티커 없는 질문은 "에이전트가 조사하듯 자유롭게" 답해야 한다.

## 2. 비전 선언

> **모든 질문이 — 티커가 있든 없든, 커버드 유니버스 안이든 밖이든 — 증거에 근거한 인용 등급의 답변을 받는다.**
> 조사의 방향은 LLM이 자유롭게 정하고, 코드는 자료를 제공하고 검증만 한다.

## 3. 철학 (교리)

| 책임 | 주체 | 비고 |
|---|---|---|
| 질문 해석 (티커 유무, 위젯 맥락) | LLM — 에이전트 서비스 디스패처 | 정규식·규칙 코드 금지 |
| 조사 방향·계획 | LLM — 엔진 내 제공자(provider) | 결정론적 계층은 힌트 제공 가능, 방향 강제 불가 |
| 자료 접근 | 코드 — 온톨로지 MCP + OpenBB(뉴스·FMP 포함) 전면 개방 | 어떤 도구를 쓸지는 LLM이 고름 |
| 검증 | 코드 — 증거 원장 기반 발언, 인용 필수, 어드바이저리 라벨, 벤더 스크럽 | "자유로운 조사"와 "환각 없음"을 양립시키는 장치 |

세부 교리:

- **증거 평면 교리**: 증거는 온톨로지(공시+관측)와 OpenBB(플랫폼 엔드포인트·뉴스·FMP)에서만 나온다. 에이전트의 자율 웹 검색은 두지 않는다(1차 제외) — 외부 정보는 사용자가 큐레이션한 채널(예: RSS)을 통해 관측 평면에 들어온다(후기 과제, §9).
- **증거 등급 정직 — 내부 규칙 (2026-09-04 개정)**: 온톨로지 필링급 증거와 시장·뉴스 데이터급 증거는 **엔진·프롬프트 내부에서** 무엇을 사실로 주장할 수 있는지를 가르는 규칙으로 유지한다(사실 주장은 보증된 스코프의 인용에만, 시장 평면 수치는 관측으로 제시). 단, **답변 표면에는 등급 라벨(필링급/시장·뉴스급)이나 "커버리지 밖" 고지를 두지 않는다** — 출처는 문장 안에서 자연스럽게("실적발표 기준", "최근 시세 데이터 기준") 표기하고, 독자는 온톨로지를 썼는지 알 수 없어야 한다. 미커버 회사는 자기 수치를 시장 평면에서 가져와 다른 회사와 같은 방식으로 답한다.
- **단일 레인**: 모든 자유 질문은 엔진 심층 루프(~8-10분, SSE 진행 상황 표시)를 탄다. 빠른 조회 경로/질문 유형 분류기 이중화를 두지 않는다.
- **advisory_only 유지**: "어떤 종목이 좋을까?"형 질문도 투자 조언이 아니라 증거 기반 리서치로 답한다(기존 `idea_generation` 선례 준용).
- **티커 유무로 도구를 제한하지 않는다**: 도구 세트는 실행 종류별로 지정 가능하지만, "티커 없으면 온톨로지 금지" 같은 하드 룰은 두지 않는다. 온톨로지 발견이 얇으면 LLM이 스스로 OpenBB로 선회한다(룰 없이 자연히).

## 4. 목표 아키텍처

### 4.1 질문의 여정

1. OpenBB 채팅 → 질문(+위젯 정보 — 조각 2부터 수신)이 에이전트 서비스에 도착.
2. **에이전트 서비스 디스패처**: 가벼운 LLM 호출 1회 — "티커가 명시됐나? 위젯 맥락은?" (해석 실패 시 자유 문으로 폴백 — 티커 없는 질문은 자유 문에서 어차피 해결됨).
3. **문 선택** (4.2의 커버리지 기반).
4. 회사 문 = 기존 `company_research`/`company_research_en`/`guru_*` 회로 무손상. 자유 문 = `question_only` 컨텍스트 실행(신규 `open_research`/`open_research_en` 런카인드).
5. 엔진 루프: LLM이 온톨로지+OpenBB 도구를 자유롭게 호출하며 조사 → 증거 원장 → 검증 → 합성.
6. ~10분 후 SSE 스트리밍 답변(출처·어드바이저리 라벨 포함).

### 4.2 커버리지 기반 문 선택

```
LLM 해석 → 티커 T (또는 없음)
T 있음 + T ∈ 커버드 유니버스(355)  → 회사 문 (company_research)
T 있음 + T ∉ 커버드 유니버스      → 자유 문 (온톨로지 시도 → 자연히 OpenBB로 선회)
T 없음                            → 자유 문
```

- 커버리지 확인은 카탈로그 조회(집합 소속 판정)로 **결정론적으로** 수행한다. 문 선택은 인프라 판단이지 조사 방향 결정이 아니므로 철학과 충돌하지 않는다.
- 효과: 미커버 티커 질문이 더 이상 10분 낭비 후 거부로 끝나지 않고 즉시 자유 문으로 가서 OpenBB 데이터로 실제 답변을 만든다.

### 4.3 발견 사다리 (티커 없이 조사를 시작하는 법)

티커 없는 질문은 막힌 게 아니라 **입구가 다를 뿐**이다.

- **1단계 — 티커 불필요 도구로 후보 발견**: `krw_ontology_query_context`에 질문만 주고 `tickers: []` + `universe: "covered"`로 호출하면 사이드카 랭킹이 개념→회사 후보를 근거 행과 함께 반환한다. 커버리지 카드(`index_context`)·유니버스 목록(`catalog`)도 티커 불필요.
- **2단계 — 발견된 티커로 스코프 확장**: 발견된 티커가 DerivedTickerScope에 등록되면 그 순간부터 티커 필수 도구(회사 컨텍스트·토픽맵·비교·증거 추적)가 해당 회사에 한해 해금된다. 이 메커니즘은 `covered_universe` 실행(`idea_generation`, `wide_research`)이 이미 쓰는 것이다.
- **3단계 — 지표/매크로**: 지표 사전(한글 별칭 포함), 팩터 분류, 매크로 관측 계열은 애초에 티커가 필요 없다. 회사 발견이 0개여도 답이 성립하는 질문 유형이다 — `question_only` 문을 선택한 이유.
- **빈 사다리 — 정직한 거부**: 커버드 유니버스·OpenBB 어디에도 근거가 없으면 지어내지 않는다. 다만 부족함은 **데이터 갭**으로만 말한다("이 수치는 아직 확인된 데이터가 없다 + 제 판단은 ~") — 커버리지·온톨로지·시스템 범위 이야기는 답변에 넣지 않는다(2026-09-04 개정).

### 4.4 지표 관측 — 모든 문 공통 (미시는 동료 필링에서, 거시는 관측 평면에서)

"진짜 애널리스트" 조사의 불변식: **모든 질문 유형에서 관련 미시/거시 지표가 함께 관측되어 추론에 들어간다.**

- **미시(업황·산업) 지표 = 동료(동종·관련) 회사의 온톨로지 정보에서**: 10-K에는 업황·전망·리스크 힌트가 가장 많다. 회사 문에서는 대상 회사 조사에 동종 회사 스캔이 따라붙고(섹터 팩·회사 간 연결·팩터 노출로 피어 발견 → 피어들의 필링 유도 객체 집계), 자유 문은 발견 사다리가 이미 모은 관련 회사들에 동일한 집계를 적용한다. "동종 5개사 10-K에서 운임 상승 언급 증가" 같은 산업 신호가 필링 인용과 함께 나온다. 새로운 외부 데이터 인프라 없이 기존 색인으로 성립한다.
- **거시 지표 = 관측 평면에서 직접**: FRED 등 관측 계열·OpenBB 시세·뉴스. 피어 파생 같은 별도 메커니즘 불필요.
- **과거 계열**: 최신 값 스냅샷이 아니라 시계열로 관찰 — chart_series(매크로 20 패밀리)·관측 계열·과거 시세 도구를 frontier에 개방.
- **신선도 보완**: 동료 필링 신호는 분기 단위(필링 주기)가 한계 — 최신 사건은 OpenBB 뉴스가, 가격은 시장 데이터가 보완한다(이미 frontier 안).
- **되묻기(clarify)**: 조사 중 지표·데이터가 부족하면 답변에 그 갭을 명시하고(무엇을 확인해야 하는지) promptSuggestions로 후속 질문을 제안한다(1차 형태). 실행 일시정지·실시간 질의 같은 대화형 clarify는 게이트웨이 프로토콜 확장이 필요해 후기 과제다.

## 5. 증거 평면과 증거 등급

| 평면 | 내용 | 증거 등급 |
|---|---|---|
| 온톨로지 공시 | 355티커 SEC 필링 온톨로지(인용 가능 객체) | 필링급 |
| 온톨로지 관측 | 매크로 관측 계열(FRED 등, 관측 P1 평면) | 관측급 |
| OpenBB | 플랫폼 엔드포인트·시세·FMP 재무·뉴스 | 시장/뉴스급 |

- 벤더 스크럽(fmp/fred/polygon 비노출), 어드바이저리 라벨은 모든 평면에 동일하게 적용한다.
- 엔진의 capability frontier는 세 평면을 전부 포함한다: krw-ontology 어댑터 + OpenBB(뉴스·FMP 포함). 도구 선택은 LLM의 자유다.

## 6. 엔진 공사 범위 — `question_only` 활성화 (조각 1의 뼈대)

`question_only`는 프로토콜에 이미 존재하지만(`crates/protocol/src/lib.rs:559-568`, `:699-727`) 어떤 프로덕션 이미지도 사용하지 않으며, **무스코프(no-scope) 컨텍스트로서 도구 디스패치 권한이 전무**하다(`crates/run-engine/src/validation.rs:475` — 무스코프 컨텍스트는 capability 디스패치를 아예 허가하지 않음).

공사의 심장은 새 권한 규칙이다:

> **유니버스 구속 권한 규칙** — `question_only` 실행은 "커버드 유니버스 범위 내 조사"에 한해 온톨로지 도구를 허용한다. 티커 인자를 받는 도구는 실행 중 발견된 티커(DerivedTickerScope)에 한해 호출 가능하다. OpenBB/뉴스 도구는 유니버스 밖 회사도 허용하되, 해당 증거는 시장/뉴스급으로 등급 구분된다(내부 규칙 — 답변 표면에는 자연스러운 출처로만 드러난다, 2026-09-04 개정).

등록 지점(부록 A의 근거 위치 참조):

1. `crates/protocol` — `question_only` 변형의 검증 규칙 확장(현재 빈 변이 `{}`).
2. `crates/agent-image` — `validate_entrypoint_scope`에 `question_only` 정책 추가(카디널리티 0).
3. `crates/run-engine/src/validation.rs` — 6곳 매칭 확장: admission 규칙 입력, `trusted_scope_payload`(프롬프트 주입), `untrusted_task_payload`, `product_context_value`, **`validate_capability_run_scope`(유니버스 구속 규칙 — 심장부)**, `memory_tickers`.
4. `crates/research-planner/src/initial_plan.rs:1692` — `trusted_scope_values()`가 현재 `CompanyTickerSet`/`CoveredUniverse`만 받으므로 `question_only` 분기 추가(또는 유도 티커 경로 준용).
5. `crates/runtime-persistence/src/executor.rs:751-831` — 시장 스냅샷 사전필터가 `company_research|company_research_en`+단일 티커로 하드코딩됨 — 자유 문에서의 동작 결정(스킵 또는 발견 후 주입).
6. `packages/host-ts` — `contracts.ts`/`validation.ts`/`claim.ts`/`materialization.ts`의 컨텍스트 종류 갱신 + `prepareGatewayOpenResearch` 프렙 함수 신설.
7. 게이트웨이 라우트(`local-gateway.ts`) — 티커 없는 제출 경로(`ticker` 선택화; 디스패처가 티커를 보내면 회사 문, 없으면 자유 문).
8. 신규 엔트리포인트 `open_research`/`open_research_en`(scope: `allowed_context: question_only`) — 기존 `krw-ontology` 이미지 내 추가가 유력(`idea_generation` 선례; 별도 이미지 여부는 조각 1 스펙에서 결정) + 릴리스 재핀.
9. `krw-ontology`/`krw-ontology-en` 이미지의 capability frontier가 온톨로지+OpenBB 전면을 포함하는지 확인·확장.
10. 픽스처/인수테스트 갱신.

게이트웨이·에이전트 서비스 공사:

- 게이트웨이: 티커 없는 제출 허용(요청 스키마에서 `ticker` 선택화), `open_research` 런카인드 매핑(로케일 라우팅 재사용 — `gatewayLocaleForQuestion`).
- 에이전트 서비스: `_TICKER_RE` 정규식 제거 → **LLM 디스패처** 도입(티커 추출 + 조각 2부터 위젯 맥락 해석), `NO_TICKER_MARKDOWN` 안내문 폐기, 커버리지 기반 문 선택, 미커버 티커 즉시 자유 문 라우팅. 디스패처의 LLM 프로바이더·비용·타임아웃(1-3초 목표)·폴백 전략은 조각 1 스펙에서 결정.

## 7. 조각 분해와 로드맵

### 조각 1 — 자유 문 개통 (첫 별도 스펙의 대상)

- 엔진: `question_only` 활성화 + 유니버스 구속 권한 규칙 설계·구현
- 게이트웨이: 티커 없는 제출 경로 + `open_research` 런카인드
- 에이전트 서비스: LLM 디스패처(정규식 폐기) + 커버리지 기반 문 선택
- 회사 문 지표 관측 확대(시장 스냅샷 사전필터 확장 + 동료 스캔 힌트, §4.4)를 조각 1에 포함할지 별도 1b로 뗄지는 조각 1 스펙에서 결정
- 완료 기준: 성공 기준 질문 1, 2, 5가 증거 기반 답변을 냄

### 조각 2 — 위젯 수신 (OpenBB 플랫폼 위젯)

- 에이전트 매니페스트의 `widget-dashboard-select`/`widget-dashboard-search` 플래그 ON(`krw_agent_service/app.py:70-74`에서 현재 false)
- openbb-ai `QueryRequest`의 `context`(위젯 데이터+메타데이터)·`widgets`(widget_id·파라미터)·`workspace_state`(열린 위젯 목록) 파싱 — Copilot이 이미 보내는 데이터를 처음으로 사용함
- 디스패처 LLM이 위젯 맥락까지 해석해 제출에 반영. 위젯에 티커가 있으면(예: `RawContext.metadata`의 selected ticker) 회사 문으로 — 즉시 가치
- 완료 기준: 성공 기준 질문 3
- 주: krw 엔진 발행 위젯(create_widget 브리지)은 본 비전 범위 밖 — 엔진 개발 로드맵의 별도 항목(`docs/superpowers/specs/2026-08-31-engine-development-roadmap.md`)

### 조각 3 — 자료 품질

- 온톨로지: "개념→관련 회사" 1급 발견 도구, 지표 열거 도구, 한글 개념 정규화 서빙(현재는 발견이 증거 행의 부산물로만 나옴)
- OpenBB: 뉴스·FMP 포함 capability frontier 전면 개방 확인
- 시점: 조각 1 이후 언제든 끼워넣기 가능(조각 1은 기존 발견 도구로도 동작 — 이 조각은 답 품질 강화)

## 8. 성공 기준

1. "반도체 병목 관련된 회사 있나?" → 관련 회사 목록 + 회사별 필링 근거. (2026-09-04 개정: "커버드 유니버스 기준" 한계 고지는 표면에서 제거 — 발견 못한 회사는 시장 평면으로 보완하거나 데이터 갭으로만 언급)
2. "인플레이션 오르면 어떤 종목이 좋을까?" → 매크로 관측 증거 + 관련 노출 회사 + 어드바이저리 라벨.
3. (위젯 가리키며) "이거 왜 이런거야?" → 위젯 데이터 기반 설명 + 근거. [조각 2]
4. "SO 최근 실적은?" → 기존 회로와 동일하게 회사 문 (라우팅 회귀 무손상) + 관련 미시 지표가 동종 회사 온톨로지 정보에서 함께 관측됨(§4.4).
5. "PLTR 최근 실적은?" (미커버 티커) → 자유 문 → PLTR 자체 수치를 OpenBB(재무·뉴스)에서 가져온 답변. (2026-09-04 개정: "커버리지 밖" 고지·등급 라벨 없이 자연스러운 출처 표기)

## 9. 명시적 비-목표

- **빠른 조회 경로(30-90초 레인)·질문 유형 분류기** — 단일 레인 결정으로 배제. 재검토 트리거: 사용 중 "너무 느리다" 불만 발생 시.
- **엔진 발행 위젯(create_widget 브리지)** — 엔진 로드맵 별도 항목. 본 비전의 위젯은 OpenBB 플랫폼 위젯만.
- **에이전트 자율 웹 검색** — 1차 제외. 외부 양질 정보는 사용자 큐레이션 채널(예: RSS)로 관측 평면에 유입하는 구조를 후기 과제로 둔다. 모델 일반 지식 답변은 계속 배제.
- **멀티턴 세션 연속성(후속 질문)** — 후기 과제. 엔진 session-memory(티커 스코프)는 이미 존재하므로 확장 여지만 남긴다.
- **"티커 없으면 온톨로지 금지" 룰** — 검토 후 기각. 도구는 전면 개방하고 LLM이 고른다(3장 철학).

## 10. 리스크와 스펙에서 결정할 사항

- **매크로 단독 답변(티커 0개)**: research-planner의 `InitialPlanScope`에서 티커 0개 계획이 성립하는지 — 조각 1 스펙의 최우선 검증 항목. 성립하지 않으면 planner에 `question_only` 분기를 명시적으로 추가한다.
- **디스패처 LLM 의존**: 에이전트 서비스의 첫 LLM 의존. 프로바이더 선택, 비용, 타임아웃, 실패 시 자유 문 폴백 설계 필요.
- **커버리지 카탈로그 접근**: 에이전트 서비스가 유니버스 목록을 어디서 가져오나(온톨로지 MCP 직접 호출 vs 시작 시 로드+주기 갱신 캐시). 카탈로그는 릴리스 단위로만 바뀌므로 캐시가 유력.
- **자유 문 예산·카디널리티**: `covered_universe`의 max 12/20 선례 참고. 발견 상한·실행 예산·provider 호출 한도 설정.
- **미커버 회사의 OpenBB 증거 파이프라인**: FMP/뉴스 결과가 증거 원장·검증(compose/verify)에서 필링 증거와 동일하게 작동하는지 확인.
- **동료(피어) 발견·업황 객체 매핑(§4.4)**: "업황/전망"을 온톨로지 object types·조회 경로(팩터·합의·이벤트 조회 등)로 구체화 — 조각 1 스펙 항목.
- **clarify 1차 형태(§4.4)**: 답변 말미 갭 명시 + promptSuggestions 연결의 구현 위치(에이전트 서비스 vs 엔진 합성).
- **실행 시간**: ~10분 수용(단일 레인 결정). SSE statusUpdate로 진행 상황을 계속 보여 완화.

## 부록 A: 현재 구현 근거 (2026-09-06 탐색)

**엔진(krw-agnet)**

- `crates/protocol/src/lib.rs:559-568` — `RunContextKind` 8종(회사·유니버스·피드·필링·노트북·기존답변·라우팅·`QuestionOnly`), `:699-727` 변이 정의, `:756-767` `trusted_tickers()`, `:787-805` `validate()`
- `crates/agent-image/src/lib.rs:3232-3285` — 컨텍스트 종류별 스코프 정책표; `question_only`를 쓰는 프로덕션 이미지 없음(테스트만)
- `crates/run-engine/src/validation.rs:387-482` — capability 인가 매트릭스, `:475` 무스코프 컨텍스트 디스패치 불가; `:409-425` `covered_universe`의 DerivedTickerScope 인가 선례; `:810-852` admission, `:854-894` trusted_scope 주입, `:1140-1157` memory_tickers
- `crates/research-planner/src/initial_plan.rs:1678-1709` — `trusted_scope_values()`: `CompanyTickerSet`/`CoveredUniverse`만 허용, 나머지 `UnsupportedScope`
- `crates/run-engine/src/active_run.rs:83-93` — DerivedTickerScope(실행 중 발견 티커 등록)
- `crates/runtime-persistence/src/executor.rs:751-831` — 시장 스냅샷 사전필터(회사 런카인드+단일 티커 하드코딩)
- `packages/host-ts/src/gateway.ts:56-81,141` — 공개 계약(ticker 필수, `company_ticker_set` 고정, advisor_lens→guru 매핑), `contracts.ts:81-103`·`validation.ts:106-153` 미러

**온톨로지(krw-ontology)**

- `spine_router.py:820-845` — tickerless 후보 상한(환경변수 `KRW_ROUTER_TICKERLESS_QUERY_CONTEXT_MAX_TICKERS`, 기본 5), `:3283-3289` 상한 판정
- `mcp_server/contracts.py:366-415` — SearchPlan `tickers`(빈 배열=발견 필요), `universe: "covered"`, `limit_tickers`
- `mcp_server/server.py:801-851` — `catalog`/`index_context`(티커 불필요), `:880-912` `query_context`(발견 레인)
- `agent_index/factor_taxonomy.py`·`sector_packs.py`·`metric_dictionary.py`·`chart_series.py` — 팩터/섹터/지표/매크로(20 패밀리) 자료

**에이전트 서비스(krw-agent-service)**

- `runners.py:52-75` — `_TICKER_RE` 정규식·디니리스트·`extract_ticker`; `:240-255` — `TickerNotFound` + 게이트웨이 제출 body `{schema_version, question, ticker}`
- `flow.py:46-51` — `NO_TICKER_MARKDOWN`; `:64-73` — 마지막 human 메시지만 추출(위젯 컨텍스트 폐기)
- `app.py:70-74` — 매니페스트 `widget-dashboard-select/search: false`

**OpenBB 접점**

- openbb-ai `models.py:763-813` — `QueryRequest`(messages 외에 `context: list[RawContext]`, `widgets: WidgetCollection`, `workspace_state`); `:604-614` `RawContext`(위젯 데이터+메타데이터 "예: 선택된 티커"), `:380-469` `Widget`(widget_id·params)
- `krw-agnet/services/openbb-gateway` — `widget_publisher.py`(create_widget SSE 함수 호출 설계, 호스트 어댑터 미구현), `adapter.py`(openbb-mcp 리버스 프록시 :9444→:8001)

## 부록 B: 브레인스토밍 결정 기록 (2026-09-06)

| # | 결정 | 내용 |
|---|---|---|
| 1 | 산출 범위 | 전체 비전 문서(A+B+C) 먼저 작성, 첫 조각은 별도 스펙 사이클 |
| 2 | 응답 UX | 단일 레인 — 전부 엔진 심층 루프(~10분). 빠른 레인/분류기 배제 |
| 3 | 증거 평면 | 온톨로지(공시+관측) + OpenBB(뉴스·FMP 포함) 전부. 그 외 없음 |
| 4 | 위젯 앵커 | OpenBB 플랫폼 위젯만. krw 발행 위젯(브리지)은 범위 밖 |
| 5 | 엔진 경로 | 방법 2 — `question_only` 활성화 (covered_universe 재사용·팬아웃 방식 기각) |
| 6 | 진입 해석 | 티커 정규식 폐지 → 에이전트 서비스의 가벼운 LLM 디스패처 |
| 7 | 도구 개방 | 티커 유무로 도구 제한 안 함. 전면 개방 + LLM이 선택 |
| 8 | 문 선택 | 티커 유무 + 온톨로지 커버리지로 회사 문/자유 문 결정. 미커버 티커 → 자유 문 |
| 9 | 웹 검색 | 1차 제외 — 외부 정보는 사용자 큐레이션(RSS 등) 채널로 관측 평면 유입(후기 과제) |
| 10 | 지표 관측 | 모든 문 공통. 미시(업황)는 동종 회사 온톨로지 정보(10-K 힌트)에서, 거시는 관측 평면 직접 |
| 11 | 되묻기 | 지표·데이터 부족 시 답변에 갭 명시 + 후속 제안(promptSuggestions). 실시간 질의는 후기 |
