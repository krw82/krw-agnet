# Agent Gateway: CLI·웹 호스트 단일 진입점

상태: **준비됨, 웹앱에는 아직 mount하지 않음.**

`krw-agent run`과 미래 웹앱은 provider SDK나 MCP를 직접 호출하지 않는다. 둘 다
호스트가 소유하는 작은 Agent Gateway에 **질문 intent만** 보내고, Gateway가 이미 있는
`@krw-agent/host` adapter로 immutable enqueue claim을 만든다.

```text
CLI / Browser
    │ question, ticker, optional session id
    ▼
Agent Gateway (authenticated server only)
    │ @krw-agent/host: ownership + pinned descriptor + enqueue
    ▼
PostgreSQL agent_v1.enqueue_run
    ▼
krw-agentd
    │ Selected GLM test or DeepSeek production lane + pooled immutable ontology MCP
    ▼
atomic final + host outbox
    ▼
Gateway status / existing product SSE
```

따라서 Gateway는 **모델 실행기나 두 번째 agent runtime이 아니다.** 다음을 절대 하지
않는다.

- `GLM_API_KEY`/`DEEPSEEK_API_KEY`를 읽거나 어느 provider API도 직접 호출하지 않음
- MCP 주소·credential·release를 browser/CLI request에서 받지 않음
- browser/CLI JSON에서 tenant, principal, run, immutable claim, session-memory carrier를 받지 않음
- `agent_store` table을 직접 읽거나 arbitrary SQL을 실행하지 않음

## 왜 이 방식인가

웹앱 API route에서 DeepSeek SDK를 직접 호출하면 queue, immutable release pin, EvidenceLedger,
cancel/fence, atomic final, session memory가 모두 우회된다. 반대로 Rust daemon에 browser를
직접 연결하면 사용자 인증·과금·presentation·SSE가 daemon에 섞인다.

Gateway + `@krw-agent/host`는 이 둘을 분리한다.

- 웹앱 서버는 인증·세션 ownership·과금 reservation·product presentation을 소유한다.
- `@krw-agent/host`는 서버 안에서 안전한 intent를 hash-bound `agent_v1.enqueue_run` 요청으로
  만든다.
- `krw-agentd`는 queue에서 claim한 뒤에만 선택된 provider와 MCP를 실행한다.
- browser와 CLI는 같은 작은 HTTP 계약만 안다.

`@krw-agent/host`가 여기서 말하는 SDK다. **provider SDK/키는 웹앱에 넣지 않는다.**

## v1 HTTP 계약

Gateway base URL은 예를 들어 `https://agent.example.com/v1/agent`다. 모든 endpoint는
인증된 server session 또는 CLI bearer token에서 tenant/principal을 결정한다.

### 새 대화의 첫 질문

```http
POST /v1/agent/runs
Authorization: Bearer <CLI token>
Content-Type: application/json

{
  "schema_version": 1,
  "question": "애플의 최근 3개 회계연도 매출 추이와 투자상 의미를 알려줘",
  "ticker": "AAPL"
}
```

```json
{
  "schema_version": 1,
  "session_id": "ses_01J...",
  "run_id": "run_01J...",
  "state": "queued"
}
```

새 session/run/mutation ID는 Gateway가 서버에서 생성한다. request body가 그 ID들을 정할 수 없다.

### 같은 대화의 후속 질문

```http
POST /v1/agent/sessions/ses_01J.../runs
Authorization: Bearer <CLI token>
Content-Type: application/json

{
  "schema_version": 1,
  "question": "FY2023 매출 감소의 주된 원인을 제품과 지역 기준으로 이어서 조사해줘",
  "ticker": "AAPL"
}
```

Gateway는 URL의 `session_id`가 현재 인증된 tenant/principal 소유인지 확인한 뒤, 새로운
`run_id`만 만들어 enqueue한다. session ID가 맞더라도 다른 사용자의 대화를 이어갈 수 없다.

### 상태와 최종 답변 조회

```http
GET /v1/agent/runs/run_01J...
Authorization: Bearer <CLI token>
```

진행 중에는 다음처럼 안전한 상태만 반환한다.

```json
{
  "schema_version": 1,
  "session_id": "ses_01J...",
  "run_id": "run_01J...",
  "state": "active",
  "final_output": null
}
```

`final` 뒤에만 product projection이 저장한 최종 Markdown을 반환한다.

```json
{
  "schema_version": 1,
  "session_id": "ses_01J...",
  "run_id": "run_01J...",
  "state": "final",
  "final_output": {
    "markdown": "## 결론\n...",
    "final_output_hash": "sha256:..."
  }
}
```

Provider episode, model reasoning, raw MCP response, EvidenceLedger body, recovery artifact,
session-memory carrier는 이 API로 반환하지 않는다. UI에 citation/evidence presentation이 필요하면
Gateway는 product DB의 committed projection에서 별도 read model을 만들어 제공한다.

## `krw-agent run`

CLI는 위 Gateway의 thin client다. daemon·DB·provider secret을 직접 만지지 않는다.

```bash
# 새 session을 만들고 최종 Markdown까지 기다린다.
export KRW_AGENT_GATEWAY_TOKEN='...'

cargo run -p krw-agent -- run \
  --gateway-url https://agent.example.com/v1/agent \
  --ticker AAPL \
  --question "애플의 최근 3개 회계연도 매출 추이와 투자상 의미를 알려줘" \
  --wait

# 위 출력의 session_id로 같은 대화를 이어간다.
cargo run -p krw-agent -- run \
  --gateway-url https://agent.example.com/v1/agent \
  --ticker AAPL \
  --session-id ses_01J... \
  --question "FY2023 감소 원인을 제품과 지역 기준으로 이어서 조사해줘" \
  --wait
```

`--wait`은 CLI process가 상태 endpoint를 poll할 뿐이다. queued/idle session마다 daemon task,
timer, client가 만들어지지 않는다. 웹 UI는 기존 host outbox → SSE projection을 사용한다.

## 후속 질문이 실제로 이어지는 방식

후속 질문은 Claude transcript resume도, 전체 대화 전문 재전송도 아니다.

1. 같은 `session_id`로 새 immutable run을 enqueue한다.
2. DB는 같은 session에서 active run을 하나만 허용한다. 첫 run이 실행 중이면 후속 run은 DB queue에
   머문다. 다른 session은 병렬로 실행된다.
3. 첫 run이 atomic final을 commit하면 direct Markdown, EvidenceLedger receipt, 사용량과
   SessionMemory v3 delta가 한 transaction으로 확정된다.
4. 후속 run이 자신의 fence를 얻은 뒤에만 daemon이 그 session의 최신 bounded memory를 읽고,
   현재 질문에 맞는 view를 만든다.
5. Flash는 **현재 질문 + 선택된 과거 문맥 + evidence-linked memory**를 보고, 필요하면 ontology를
   다시 조회한다. 과거 Markdown은 대화 문맥이지 검증된 새 사실의 권위가 아니다.

이 방식이면 첫 질문이 실패하거나 취소되어도 거짓 memory가 후속 질문에 들어가지 않는다. 후속 질문은
마지막으로 commit된 turn까지만 보고 독립적으로 조사한다.

## 웹앱 연결 시 실제 구현 위치

웹앱 적용 단계에서는 다음 네 군데만 추가한다.

1. 인증된 server route/handler가 v1 Gateway request를 parse한다.
2. server-owned conversation row에서 ownership을 만들고, `@krw-agent/host`의
   `prepareGatewayCompanyResearch` + `HostAgentClient.enqueue`를 호출한다.
3. host outbox worker가 `answer.committed`를 product answer row와 기존 SSE 이벤트로 projection한다.
4. authenticated status route가 product answer row를 읽어 위의 `final_output` 형태로 반환한다.

브라우저는 질문·ticker와 자신의 로그인 cookie만 보낸다. `krw-agentd`의 provider credential,
MCP binding, release authorization, PostgreSQL daemon role은 계속 서버 내부에 남는다.
