# OpenBB Workspace Copilot Bridge — 2026-08-31

## 목표

krw 엔진을 OpenBB Workspace 코파일럿의 커스텀 에이전트로 입점시킨다. 사용자가
Workspace 대시보드에서 위젯("Add to context" / `@`멘션)을 클릭한 뒤 질문하면,
그 위젯의 티커·파라미터·**실제 데이터**를 배경으로 krw 심층 리서치(국문
`company_research`)가 돌아가고, 진행 상황·최종 답변·차트·후속질문이 Workspace
SSE 계약으로 스트리밍된다.

## 확정된 설계 결정 (사용자 선택, 2026-08-31)

| 결정 | 선택 |
|---|---|
| 호스팅 | Mac 로컬 + cloudflared 터널 (`/opt/homebrew/bin/cloudflared`) |
| 답변 속도 | 전부 심층 리서치 (`company_research`, KO). 진행 배지로 대기 경험 관리 |
| 화면 컨텍스트 | 위젯 **실제 데이터까지** — 원격 함수호출 루프로 당겨옴 |
| 답변 언어 | 국문 고정 (게이트웨이 기본 `company_research` = KO) |

## 연결 대상 계약 (조사 완료)

### OpenBB Workspace ↔ 브리지 (openbb-ai, MIT)

- `GET /agents.json` — 에이전트 매니페스트 (`features.streaming` 필수,
  `widget-dashboard-select` = primary 위젯 접근, `widget-dashboard-search` =
  secondary 위젯 접근).
- `POST /query` — QueryRequest `{messages, widgets{primary,secondary,extra},
  context, urls(≤4), timezone, workspace_state, tools}` → 명명된 SSE.
- SSE 선(Wire) 형태 — `openbb_ai.models.BaseSSE.model_dump()`:
  `event: copilotMessageChunk` + `data: <compact JSON 문자열>`.
  이벤트 종류: `copilotMessageChunk{delta}`,
  `copilotStatusUpdate{eventType,message,group}`,
  `copilotFunctionCall{function,input_arguments}`,
  `copilotMessageArtifact{type,name,description,content,chart_params?}`,
  `copilotPromptSuggestions{suggestions}`,
  `copilotCitationCollection{citations}`.
- 원격 함수호출 루프 — 위젯 데이터가 필요하면 브리지가
  `copilotFunctionCall get_widget_data {data_sources:[{widget_uuid,origin,id,input_args}]}`
  를 뿌리고 **연결을 끊는다**. Workspace가 데이터를 조달해
  `messages` 뒤에 function_call + tool 결과를 붙여 **새로 POST /query** 한다.
  에이전트는 완전 상태less — 매 요청 전체 이력이 재전송된다.
- Widget 데이터 결과 형태 — tool 메시지 `data: [DataContent]`,
  `DataContent.items[].content`(문자열; JSON 행 배열 또는 텍스트) +
  `data_format.data_type`(`object` 등). 실패는 `ClientFunctionCallError`.

### 브리지 ↔ krw 로컬 게이트웨이 (변경 없음, 기존 계약 재사용)

- `POST {KRW_AGENT_GATEWAY_URL}/runs` — `{schema_version:1, question(≤64KB),
  ticker}` + `Authorization: Bearer` → `{run_id, state:"queued"}`.
- `GET /runs/{run_id}` — 폴링. `state`: queued/deferred/active/final/…
  final → `final_output.markdown`(≤64KB) + `final_output.visualizations`
  (krw-presentation 컴파일产物: `views[].chart_type`(line|bar),
  `series[].points[]{period, value}`, cap 16개).
- KO 답변 마크다운 꼬리: `### 이어서 볼 질문`(번호 목록), `### 출처`.

## 아키텍처

```
OpenBB Workspace (호스팅)                 Mac (로컬)
  │ 1. 위젯 클릭 → primary 컨텍스트        ┌─────────────────────────────┐
  │ 2. POST /query (질문+위젯 정의)  ────► │ cloudflared quick tunnel     │
  │                                        │  └─► workspace-bridge-ts     │
  │ ◄─── copilotFunctionCall(데이터 요청) │      :14790, 경로 비밀값      │
  │      (연결 종료)                       │        │ Bearer              │
  │ 3. POST /query (재호출+데이터)   ────► │        ▼                     │
  │ ◄─── copilotStatusUpdate(진행 배지)   │  로컬 게이트웨이 :14318       │
  │ ◄─── copilotMessageChunk(답변)        │   └─► agentd/GLM 심층 리서치  │
  │ ◄─── copilotMessageArtifact(차트)     │                               │
  │ ◄─── copilotPromptSuggestions(후속질문)└─────────────────────────────┘
```

- 신규 패키지 `packages/workspace-bridge-ts` — 런타임 의존 0(node stdlib +
  전역 fetch). 게이트웨이·릴리즈 이미지 변경 없음.
- 위젯 데이터 → 질문 합성: 64KB 질문 한도 내에서 관측 다이제스트(열·행
  요약, 4,000자 상한)를 `[화면 위젯 관측 — OpenBB Workspace 컨텍스트]`
  블록으로 question 뒤에 붙인다. 티커는 위젯 파라미터(`type:"ticker"`)에서
  추출(primary → secondary → 질문 정규식 폴백).
- 진행 경험: 런 상태 전이 + 90초 간격 하트비트를 `copilotStatusUpdate`로.
  게이트웨이 trace는 종단 상태에서만 열리므로 라이브 단계 스트림은 없다 —
  경과 시간 기반 정직한 배지만 제공한다(단계 날조 금지).
- 보안(데모 등급): 게이트웨이는 loopback+Bearer 유지. 브리지는 경로 비밀값
  (`/{secret}/agents.json`, `/{secret}/query`) + CORS 반영 + 분당 rate limit.
  터널 URL 자체가 2차 비밀. 프로덕션 승격 시 GCP + 실츠토큰 교체 필요.

## 이벤트 매핑 표

| krw 측 | Workspace SSE |
|---|---|
| 런 등록(queued) | `copilotStatusUpdate` "{ticker} 심층 리서치 시작(수 분 소요)" |
| 폴링 상태/경과 | `copilotStatusUpdate` 하트비트 |
| final_output.markdown | `copilotMessageChunk` (줄 단위 분할) |
| visualizations.views | `copilotMessageArtifact` type=chart (chart_params camelCase, 계열 병합 rows, 최근 60포인트) |
| `### 이어서 볼 질문` 섹션 | `copilotPromptSuggestions` (≤3) |
| 데이터를 당겨온 위젯 | `copilotCitationCollection` (type=widget) |
| failed / retry_message | `copilotStatusUpdate` ERROR + `copilotMessageChunk` |

## 스킬("/") 지원 — 2026-09-02 추가 (사용자 결정)

- Workspace 코파일럿의 "/" 피커가 보내는 `selected_skills[0]`(forced_slash,
  `contentMarkdown`)를 받아 **[사용자 지정 스킬 지시 — OpenBB Workspace /스킬]**
  블록(4,000자 상한)으로 정규화하고, 질문 합성 순서를
  **본 질문 → 스킬 지시 → 위젯 관측**으로 고정했다.
- `skills_catalog`(모델 자동 선택 핸드셰이크, `get_skill_content`)는 의도적으로
  무시 — 런은 항상 사용자의 명시적 의도에서 시작한다.
- **구루 렌즈는 비활성화**(사용자 결정 2026-09-02): 게이트웨이는 선택적
  `advisor_lens`를 받지만 브리지의 `gatewayRunBody`는 절대 발송하지 않으며
  테스트로 못 박았다. 스킬 지시는 오직 질문 텍스트로만 전달된다.
- 라이브 실증: dividend-conservative 스킬 페이로드 → 런 스냅샷
  `immutable_snapshot.request.question`에 스킬 블록이 그대로 반영 확인.

## 테스트 계획

1. 단위(node:test): SSE 직렬화 형태, 질문 추출, 위젯 티커 추출,
   함수호출-결과 감지, 다이제스트 생성(상한·생략 마커), 후속질문 파싱(KO/EN),
   viz→chart 아티팩트 변환.
2. 계약 e2e(로컬 curl, 시뮬레이티드 Workspace): 1차 POST → 함수호출 SSE +
   연결 종료 확인 → 2차 POST(모의 tool 결과) → 실제 GLM 런 → final
   마크다운·차트·후속질문 수신. dev-stack 게이트웨이(14318) 재사용.
3. 실전: cloudflared 터널 → Workspace 코파일럿에 agents.json URL 등록 →
   위젯 클릭 → 국문 답변 확인 (사용자 참여 단계).

## 운영

- `scripts/workspace-bridge.sh start|stop` — dev-stack 상태 디렉터리
  (`.local/agent-gateway`)의 `secrets.env` 재사용, 브리지 기동, quick tunnel
  URL 캡처, 등록용 agents.json URL 출력. 비밀 경로는 상태 디렉터리에 저장.
- 게이트웨이 헬스(`/healthz`) 확인 후 기동. 셧다운 시 터널·브리지 함께 정리.
