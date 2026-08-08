# Anthropic Messages API Wire Layer 전면 전환

## 배경

현재 시스템은 OpenAI Chat Completions 형식의 `deepseek-wire` crate를 사용하여
DeepSeek 및 GLM-5.2와 통신한다. GLM-5.2 50문제 동시 테스트에서 38% 성공률에
머물렀으며, 주된 실패 원인은 GLM이 OpenAI 프로토콜에서 도구 호출을 신뢰성 있게
수행하지 못하는 것이다.

커뮤니티 실증 데이터(Reddit r/ZaiGLM)에 따르면 z.ai의 Anthropic 호환
엔드포인트가 OpenAI 호환 엔드포인트보다 도구 호출 신뢰성이 현저히 높다.
DeepSeek 역시 `https://api.deepseek.com/anthropic`에서 Anthropic 호환
엔드포인트를 제공하므로, 전체 프로바이더를 Anthropic Messages API로 전환한다.

## 결정사항

- **전면 재작성**: 기존 `deepseek-wire` crate를 `provider-wire`로 대체
- **DeepSeek + GLM 모두 Anthropic**: 두 프로바이더 모두 Anthropic 호환 엔드포인트 사용
- **3단계 점진적 구현**: crate 생성 → run-engine 연결 → 통합 테스트

## 엔드포인트

| 프로바이더 | 엔드포인트 | 모델 |
|---|---|---|
| DeepSeek | `https://api.deepseek.com/anthropic` | `deepseek-v4-flash` |
| GLM (z.ai) | `https://api.z.ai/api/anthropic` | `glm-5.2` |

인증: `x-api-key` 헤더 + `anthropic-version: 2023-06-01`

## 구조적 차이점

### 1. 시스템 메시지

```
[기존 OpenAI]
messages: [{role: "system", content: "..."}, {role: "user", ...}]

[신규 Anthropic]
system: "..."           // top-level 필드
messages: [{role: "user", ...}]
```

### 2. 메시지 Content

```
[기존 OpenAI]
{role: "assistant", content: "텍스트", reasoning_content: "추론", tool_calls: [...]}

[신규 Anthropic]
{role: "assistant", content: [
  {type: "thinking", thinking: "추론", signature: "..."},
  {type: "text", text: "텍스트"},
  {type: "tool_use", id: "toolu_xxx", name: "함수명", input: {...}}
]}
```

### 3. 도구 정의

```
[기존 OpenAI]
{type: "function", function: {name, description, parameters: {...JSON Schema...}}}

[신규 Anthropic]
{name, description, input_schema: {...JSON Schema...}}
```

### 4. 도구 결과

```
[기존 OpenAI]
{role: "tool", tool_call_id: "call_xxx", content: "{결과 JSON}"}

[신규 Anthropic]
{role: "user", content: [
  {type: "tool_result", tool_use_id: "toolu_xxx", content: "결과"}
]}
```

### 5. SSE 스트리밍

```
[기존 OpenAI]
data: {"choices":[{"delta":{"content":"...","tool_calls":[...]}}]}
data: [DONE]

[신규 Anthropic]
event: message_start
data: {"type":"message_start","message":{...}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use",...}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"..."}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":15}}

event: message_stop
data: {"type":"message_stop"}
```

### 6. Thinking

```
[기존 OpenAI]
thinking: {type: "enabled"}
reasoning_effort: "high"
→ 응답: delta.reasoning_content 필드

[신규 Anthropic]
thinking: {type: "enabled", budget_tokens: 16384}
→ 응답: {type: "thinking", thinking: "...", signature: "..."} content block
→ replay: thinking block을 그대로 다음 요청에 포함 (signature 포함)
```

### 7. Stop 사유 매핑

| Anthropic `stop_reason` | 의미 | 기존 `finish_reason` 대응 |
|---|---|---|
| `end_turn` | 자연 종료 | `stop` |
| `tool_use` | 도구 호출 | `tool_calls` |
| `max_tokens` | 토큰 한계 | `length` |
| `stop_sequence` | stop 문자열 | `stop` |

## 컴포넌트 설계

### `crates/provider-wire/src/lib.rs`

**공개 타입:**

```rust
// 요청
pub struct MessagesRequest {
    pub model: String,
    pub messages: Vec<ProviderMessage>,
    pub system: String,                    // top-level
    pub max_tokens: u32,                   // 필수
    pub tools: Vec<ProviderToolDefinition>,
    pub tool_choice: Option<ToolChoice>,
    pub thinking: ThinkingConfig,
    pub stream: bool,
    pub metadata: Option<RequestMetadata>,
}

// 메시지
pub struct ProviderMessage {
    pub role: MessageRole,                 // User, Assistant
    pub content: Vec<ContentBlock>,
}

pub enum MessageRole { User, Assistant }

// 콘텐츠 블록
pub enum ContentBlock {
    Text { text: String },
    ToolUse { id: String, name: ProviderFunctionName, input: Value },
    ToolResult { tool_use_id: String, content: String, is_error: bool },
    Thinking { thinking: String, signature: String },
}

// 도구 정의
pub struct ProviderToolDefinition {
    pub name: ProviderFunctionName,
    pub description: String,
    pub input_schema: JsonSchemaDocument,
}

// 도구 선택
pub enum ToolChoice {
    Auto,
    Any,                                    // = 기존 Required
    Tool { name: ProviderFunctionName },
    None,
}

// Thinking
pub struct ThinkingConfig {
    pub kind: ThinkingMode,
    pub budget_tokens: Option<u32>,
}

// 에피소드 (run-engine 호환)
pub struct ProviderEpisodeV1 {
    pub schema_version: u16,
    pub request_hash: ContentHash,
    pub requested_model: String,
    pub observed_model: String,
    pub api_version: String,
    pub assistant: AssistantMessage,        // 내부 표현 유지
    pub tool_results: Vec<ToolResultMessage>,
    pub tool_schema_hash: ContentHash,
    pub agent_image_hash: ContentHash,
    pub finish_reason: String,              // "tool_use", "end_turn" 등
    pub usage: TokenUsage,
    pub replay_hash: ContentHash,
}

// HTTP 클라이언트
pub struct ProviderClient {
    http: reqwest::Client,
    endpoint: String,                       // {api_base}/v1/messages
    api_key: Arc<Zeroizing<String>>,
    request_timeout: Duration,
    // ...
}
```

**SSE 디코더:**

```rust
pub struct AnthropicSseDecoder {
    buffer: Vec<u8>,
    max_buffer_bytes: usize,
}

pub enum AnthropicSseEvent {
    MessageStart { message: MessageStartBody },
    ContentBlockStart { index: u32, content_block: ContentBlockStart },
    ContentBlockDelta { index: u32, delta: ContentBlockDelta },
    ContentBlockStop { index: u32 },
    MessageDelta { stop_reason: Option<String>, output_tokens: Option<u32> },
    MessageStop,
    Ping,
    Error { error: ErrorBody },
}
```

**EpisodeAssembler:**

```rust
pub struct EpisodeAssembler {
    request_hash: ContentHash,
    model: String,
    context: EpisodeContext,
    // 인덱스별 콘텐츠 블록 누적
    text_blocks: BTreeMap<u32, String>,
    thinking_blocks: BTreeMap<u32, (String, String)>,  // (thinking, signature)
    tool_use_builders: BTreeMap<u32, ToolUseBuilder>,
    finish_reason: Option<String>,
    usage: Option<TokenUsage>,
}
```

### run-engine 변경사항

1. **import 교체**: `krw_agent_deepseek_wire` → `krw_agent_provider_wire`
2. **`build_provider_request`**: `ChatCompletionRequest` → `MessagesRequest`
   - system 메시지를 top-level `system` 필드로 추출
   - content를 ContentBlock 배열로 빌드
3. **`Provider` trait**: `DeepSeekClient` → `ProviderClient`
4. **메시지 빌딩**: `build_trusted_messages`가 ContentBlock 기반으로 동작
5. **도구 결과**: `append_capability_tool_result`가 `tool_result` block 생성

### model-registry.yaml 변경

```yaml
- model_id: deepseek-v4-flash
  api_base: https://api.deepseek.com/anthropic    # 변경
  api_version: anthropic-messages-v1               # 변경
  # ...

- model_id: glm-5.2
  api_base: https://api.z.ai/api/anthropic          # 변경
  api_version: anthropic-messages-v1                 # 변경
  # ...
```

### protocol crate 변경

- `ProviderWireCapabilities` 필드를 Anthropic 개념에 맞게 조정
- `requires_reasoning_content_replay` → `requires_thinking_block_replay`
- `requires_assistant_content_for_tool_calls` → Anthropic에서는 항상 content blocks 사용

## 3단계 구현 순서

### 1단계: provider-wire crate 생성

1. `crates/provider-wire/` 디렉토리 및 Cargo.toml 생성
2. Anthropic Messages API 타입 정의
3. SSE 디코더 (event-type-aware, `message_stop` 종료)
4. EpisodeAssembler (content block 단위)
5. ProviderClient (`x-api-key`, `/v1/messages`)
6. crate 내 단위 테스트

### 2단계: run-engine 연결

1. `run-engine/Cargo.toml`: `deepseek-wire` → `provider-wire`
2. import 및 타입 참조 전면 교체
3. `build_provider_request` 재작성 (system top-level, ContentBlock)
4. 메시지 빌딩 / 도구 결과 로직 재작성
5. `Provider` trait 구현체 교체
6. 워크스페이스 빌드 통과

### 3단계: 엔드포인트 전환 + 통합 테스트

1. model-registry.yaml 업데이트
2. protocol crate capability matrix 업데이트
3. budget-registry.yaml 확인 (토큰 제한 유지)
4. `.env.local` API 키 확인
5. 로컬 스택 시작 + 단일 GLM 테스트
6. GLM 50문제 동시 테스트

## 호환성 유지

- `ProviderEpisodeV1` 구조 유지: replay_hash, tool_schema_hash 등
  체크포인트 호환성 보존
- `ContentHash`, `ProviderFunctionName`, `CanonicalJsonText`,
  `JsonSchemaDocument` 재사용
- `EpisodeContext` 구조 유지
- `WireError` 에러 타입 유지 (variant 추가/제거 가능)
- `AssistantMessage` 내부 표현: `content`, `reasoning_content`, `tool_calls` 필드를
  유지하되, Anthropic 응답에서 이 필드들을 채우는 매핑 로직만 변경.
  `reasoning_content` ← thinking block의 `thinking` 텍스트,
  `content` ← text block의 `text`,
  `tool_calls` ← tool_use block의 `{id, name, input}`.
  이렇게 하면 run-engine의 episode 처리 로직(검증, replay, recovery)이
  변경 없이 호환됨.

## 예상 효과

- GLM 도구 호출 신뢰성 향상 (Anthropic 프로토콜 최적화)
- 50문제 동시 테스트 성공률 38% → 60%+ 예상
- DeepSeek도 동일한 wire layer 사용 (코드 단순화)
- 향후 다른 Anthropic 호환 프로바이더 쉽게 추가 가능
