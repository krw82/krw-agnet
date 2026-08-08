# Anthropic Messages API Wire Layer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the OpenAI Chat Completions wire layer (`deepseek-wire`) with an Anthropic Messages API wire layer (`provider-wire`) so both DeepSeek and GLM-5.2 use Anthropic-native tool calling for better agentic reliability.

**Architecture:** Full rewrite of the wire crate. The new `provider-wire` crate speaks Anthropic Messages API directly to both `api.deepseek.com/anthropic` and `api.z.ai/api/anthropic`. `ProviderEpisodeV1`, `EpisodeContext`, `ContentHash` types stay unchanged for checkpoint compatibility. The `Provider` trait signature changes from `ChatCompletionRequest` to `MessagesRequest`.

**Tech Stack:** Rust 1.97.1, reqwest, serde_jcs, tokio, zeroize, rustls

## Global Constraints

- Rust toolchain: 1.97.1 (from `rust-toolchain.toml`)
- All code: `--forbid=unsafe_code`, `--warn=clippy::pedantic` (workspace lints)
- Secrets: zeroized on drop
- Replay hashing: `serde_jcs` canonical JSON
- Auth: `x-api-key` header + `anthropic-version: 2023-06-01`
- Endpoints: `https://api.deepseek.com/anthropic`, `https://api.z.ai/api/anthropic`
- Constraint: ONLY modify `~/krw-agnet`

---

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `crates/provider-wire/Cargo.toml` | Create | Crate manifest |
| `crates/provider-wire/src/lib.rs` | Create | All Anthropic wire types, SSE decoder, HTTP client, episode assembler |
| `crates/provider-wire/src/assembler.rs` | Create | EpisodeAssembler (content block accumulation) |
| `crates/provider-wire/src/sse.rs` | Create | Anthropic SSE event decoder |
| `crates/run-engine/Cargo.toml` | Modify | `deepseek-wire` → `provider-wire` |
| `crates/run-engine/src/lib.rs` | Modify | Import swap + `build_provider_request` rewrite |
| `deployments/local/model-registry.yaml` | Modify | Anthropic endpoints |
| `deployments/prod/model-registry.yaml` | Modify | Anthropic endpoints |
| `crates/protocol/src/lib.rs` | Modify | Capability matrix Anthropic vocab |
| `Cargo.toml` (workspace) | Modify | Replace member + dep |
| Other consumers' Cargo.toml | Modify | Dependency swap |

---

## Task 1: Create `provider-wire` crate scaffold + core types

**Files:**
- Create: `crates/provider-wire/Cargo.toml`
- Create: `crates/provider-wire/src/lib.rs`

**Interfaces:**
- Produces: `MessagesRequest`, `ProviderMessage`, `ContentBlock`, `MessageRole`, `ProviderToolDefinition`, `ToolChoice`, `ThinkingConfig`, `ProviderEpisodeV1`, `TokenUsage`, `EpisodeContext`, `AssistantMessage`, `ToolResultMessage`, `ToolCall`, `ToolCallKind`, `FunctionCall`, `WireError`, `ProviderFunctionName`, `CanonicalJsonText`, `JsonSchemaDocument`

- [ ] **Step 1: Create Cargo.toml**

Create `crates/provider-wire/Cargo.toml`:

```toml
[package]
name = "krw-agent-provider-wire"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
bytes.workspace = true
futures-util.workspace = true
krw-agent-protocol = { path = "../protocol" }
reqwest.workspace = true
rustls.workspace = true
serde.workspace = true
serde_jcs.workspace = true
serde_json.workspace = true
thiserror.workspace = true
tokio.workspace = true
zeroize.workspace = true

[lints]
workspace = true
```

- [ ] **Step 2: Add crate to workspace Cargo.toml**

In `Cargo.toml` (workspace root), replace `crates/deepseek-wire` with `crates/provider-wire` in the `members` list.

- [ ] **Step 3: Write `lib.rs` core types — header + newtypes**

Create `crates/provider-wire/src/lib.rs`. Start with module doc, imports, and the three newtypes (`ProviderFunctionName`, `CanonicalJsonText`, `JsonSchemaDocument`) copied verbatim from `deepseek-wire/src/lib.rs` lines 16-168. Change import line 10 from `use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, ...}` to `use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue}` (no `AUTHORIZATION`).

- [ ] **Step 4: Write message types**

Write the Anthropic-format message types:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole { User, Assistant }

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContentBlock {
    Text { text: String },
    ToolUse {
        id: String,
        name: ProviderFunctionName,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
    Thinking {
        thinking: String,
        #[serde(default)]
        signature: String,
    },
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderMessage {
    pub role: MessageRole,
    pub content: Vec<ContentBlock>,
}
```

- [ ] **Step 5: Write request types**

```rust
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessagesRequest {
    pub model: String,
    pub messages: Vec<ProviderMessage>,
    pub system: String,
    pub max_tokens: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ProviderToolDefinition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    pub thinking: ThinkingConfig,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<RequestMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestMetadata {
    pub user_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    Any,
    Tool { name: ProviderFunctionName },
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThinkingConfig {
    #[serde(rename = "type")]
    pub kind: ThinkingMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<u32>,
}
```

- [ ] **Step 6: Write tool definition, assistant message, tool result, episode, usage, context**

Copy from `deepseek-wire` and adapt:
- `ProviderToolDefinition`: unwrap from `{type:"function", function:{...}}` to flat `{name, description, input_schema}`:
```rust
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderToolDefinition {
    pub name: ProviderFunctionName,
    pub description: String,
    pub input_schema: JsonSchemaDocument,
}
```
- `AssistantMessage` (keep internal fields `content`, `reasoning_content`, `tool_calls` for run-engine compat)
- `ToolResultMessage` (keep `tool_call_id`, `content: CanonicalJsonText`)
- `ToolCall`, `FunctionCall`, `ToolCallKind` — copy verbatim
- `ProviderEpisodeV1`, `TokenUsage`, `EpisodeContext` — copy verbatim
- `WireError` — copy, remove `MissingReasoningEffort`/`UnexpectedReasoningEffort`, add `MissingMaxTokens`

- [ ] **Step 7: Write validation functions**

Write `MessagesRequest::validate()`:
- `max_tokens > 0`
- `!messages.is_empty()`
- thinking config: if enabled, `budget_tokens` must be `Some` and `>= 1024` and `< max_tokens`
- tool_result blocks must appear in `user` messages only

- [ ] **Step 8: Write ProviderEpisodeV1 methods**

Copy `verify_model_identity`, `verify_tool_call_structure`, `verify_tool_call_requirements`, `replay_messages`, `calculate_replay_hash` verbatim from deepseek-wire lines 633-680.

- [ ] **Step 9: Build the crate standalone**

Run: `KRW_ONTOLOGY_ROOT=$HOME/krw-ontology cargo build -p krw-agent-provider-wire`
Expected: compiled successfully (it won't link to run-engine yet, that's fine)

- [ ] **Step 10: Commit**

```bash
git add crates/provider-wire/ Cargo.toml
git commit -m "feat(provider-wire): Anthropic Messages API core types"
```

---

## Task 2: Anthropic SSE decoder

**Files:**
- Create: `crates/provider-wire/src/sse.rs`
- Modify: `crates/provider-wire/src/lib.rs` (add `mod sse;`)

**Interfaces:**
- Produces: `AnthropicSseDecoder`, `AnthropicSseEvent`

- [ ] **Step 1: Write SSE event enum**

Create `crates/provider-wire/src/sse.rs`:

```rust
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, PartialEq)]
pub enum AnthropicSseEvent {
    MessageStart { model: String, input_tokens: u32 },
    ContentBlockStart { index: u32, block: ContentBlockStart },
    ContentBlockDelta { index: u32, delta: ContentBlockDelta },
    ContentBlockStop { index: u32 },
    MessageDelta { stop_reason: Option<String>, output_tokens: Option<u32> },
    MessageStop,
    Ping,
    Error { error_type: String, message: String },
}

#[derive(Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlockStart {
    Text { text: String },
    ToolUse { id: String, name: String, input: Value },
    Thinking { thinking: String, signature: String },
}

#[derive(Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlockDelta {
    TextDelta { text: String },
    InputJsonDelta { partial_json: String },
    ThinkingDelta { thinking: String },
    SignatureDelta { signature: String },
}
```

- [ ] **Step 2: Write the SSE decoder**

Write a chunk-boundary-independent decoder that parses `event:` + `data:` pairs:

```rust
pub struct AnthropicSseDecoder {
    buffer: Vec<u8>,
    max_buffer_bytes: usize,
}

impl AnthropicSseDecoder {
    pub fn new(max_buffer_bytes: usize) -> Self { ... }
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<AnthropicSseEvent>, WireError> { ... }
    pub fn finish(self) -> Result<(), WireError> { ... }
}
```

The decoder:
1. Accumulates bytes in `buffer`
2. Finds `\n\n` delimited frames
3. For each frame, extracts `event:` line and `data:` line
4. Parses `data:` JSON based on event type
5. Returns typed `AnthropicSseEvent` vector

- [ ] **Step 3: Write unit tests for SSE decoder**

Test: feed a multi-event stream in chunks (split mid-frame), verify correct event sequence. Test: `message_stop` event. Test: error event. Test: buffer limit.

- [ ] **Step 4: Build + test**

Run: `KRW_ONTOLOGY_ROOT=$HOME/krw-ontology cargo test -p krw-agent-provider-wire --lib`
Expected: all tests pass

- [ ] **Step 5: Commit**

```bash
git add crates/provider-wire/src/sse.rs crates/provider-wire/src/lib.rs
git commit -m "feat(provider-wire): Anthropic SSE event decoder"
```

---

## Task 3: EpisodeAssembler + ProviderClient

**Files:**
- Create: `crates/provider-wire/src/assembler.rs`
- Modify: `crates/provider-wire/src/lib.rs` (add `mod assembler;`, write `ProviderClient`)

**Interfaces:**
- Produces: `EpisodeAssembler`, `ProviderClient`, `ProviderClientConfig`
- Consumes: `AnthropicSseDecoder`, `AnthropicSseEvent`, `MessagesRequest`, `ProviderEpisodeV1`

- [ ] **Step 1: Write EpisodeAssembler**

Create `crates/provider-wire/src/assembler.rs`. The assembler consumes `AnthropicSseEvent`s and produces a `ProviderEpisodeV1`:

```rust
pub struct EpisodeAssembler {
    request_hash: ContentHash,
    requested_model: String,
    context: EpisodeContext,
    observed_model: Option<String>,
    // Per-index accumulators
    text_blocks: BTreeMap<u32, String>,
    thinking_blocks: BTreeMap<u32, (String, String)>, // (thinking, signature)
    tool_use_builders: BTreeMap<u32, ToolUseBuilder>,
    finish_reason: Option<String>,
    usage_input_tokens: u32,
    usage_output_tokens: u32,
    done: bool,
    max_buffer_bytes: usize,
    buffered_bytes: usize,
}

struct ToolUseBuilder {
    id: String,
    name: String,
    input_json: String, // accumulated partial_json fragments
}
```

Methods: `new()`, `push_event(event: AnthropicSseEvent) -> Result<(), WireError>`, `finish() -> Result<ProviderEpisodeV1, WireError>`.

On `finish()`, map assembled data to `AssistantMessage`:
- `content` ← concatenate all `text_blocks` values
- `reasoning_content` ← concatenate all `thinking_blocks` thinking texts
- `tool_calls` ← parse each `ToolUseBuilder.input_json` as JSON Value, construct `ToolCall { id, kind: Function, function: FunctionCall { name, arguments: canonical_json_string } }`

Map `stop_reason` → `finish_reason`:
- `end_turn` → `"end_turn"`
- `tool_use` → `"tool_calls"` (keep for run-engine compat)
- `max_tokens` → `"max_tokens"`

- [ ] **Step 2: Write ProviderClientConfig + ProviderClient**

In `lib.rs`, write:

```rust
pub struct ProviderClientConfig {
    pub api_base: String,
    pub allowed_models: BTreeSet<String>,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub max_error_body_bytes: usize,
    pub max_sse_frame_bytes: usize,
    pub max_stream_bytes: usize,
    pub max_episode_bytes: usize,
    pub max_idle_per_host: usize,
}
// production() factory — same pattern as DeepSeekClientConfig

pub struct ProviderClient {
    http: reqwest::Client,
    endpoint: String,        // {api_base}/v1/messages
    allowed_models: BTreeSet<String>,
    request_timeout: Duration,
    max_error_body_bytes: usize,
    max_sse_frame_bytes: usize,
    max_stream_bytes: usize,
    max_episode_bytes: usize,
}
```

- [ ] **Step 3: Write ProviderClient::new()**

Same URL validation as `DeepSeekClient::new`. Key differences:
- Headers: `x-api-key: <key>` (sensitive) + `anthropic-version: 2023-06-01`
- Endpoint: `format!("{}/v1/messages", api_base.trim_end_matches('/'))`
- No Bearer auth

```rust
let mut headers = HeaderMap::new();
let mut key_value = HeaderValue::from_str(api_key)
    .map_err(|_| WireError::InvalidAuthorization)?;
key_value.set_sensitive(true);
headers.insert("x-api-key", key_value);
headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
```

- [ ] **Step 4: Write complete_stream()**

```rust
pub async fn complete_stream(
    &self,
    request: &MessagesRequest,
    context: &EpisodeContext,
) -> Result<ProviderEpisodeV1, WireError>
```

Flow:
1. Validate request model is in `allowed_models`
2. `request.validate()`
3. Compute `request_hash = ContentHash::sha256(serde_jcs::to_vec(request)?)`
4. HTTP POST to `self.endpoint` with `.json(request)`, bounded retry on 429/500/503
5. Check `Content-Type: text/event-stream`
6. Stream response bytes through `AnthropicSseDecoder`
7. Feed events to `EpisodeAssembler`
8. Return `ProviderEpisodeV1`

- [ ] **Step 5: Build + test**

Run: `KRW_ONTOLOGY_ROOT=$HOME/krw-ontology cargo test -p krw-agent-provider-wire --lib`
Expected: all tests pass

- [ ] **Step 6: Commit**

```bash
git add crates/provider-wire/
git commit -m "feat(provider-wire): EpisodeAssembler + ProviderClient with Anthropic Messages API"
```

---

## Task 4: Wire run-engine to provider-wire

**Files:**
- Modify: `crates/run-engine/Cargo.toml`
- Modify: `crates/run-engine/src/lib.rs`

**Interfaces:**
- Consumes: all public types from `provider-wire`
- Produces: updated `Provider` trait, `build_provider_request` with `MessagesRequest`

- [ ] **Step 1: Swap dependency in Cargo.toml**

In `crates/run-engine/Cargo.toml`, replace:
```toml
krw-agent-deepseek-wire = { path = "../deepseek-wire" }
```
with:
```toml
krw-agent-provider-wire = { path = "../provider-wire" }
```

- [ ] **Step 2: Update import block in run-engine lib.rs**

Replace lines 44-48:
```rust
use krw_agent_provider_wire::{
    ContentBlock, EpisodeContext, MessageRole, MessagesRequest, ProviderClient,
    ProviderEpisodeV1, ProviderMessage, ProviderToolDefinition, ThinkingConfig,
    ToolChoice, WireError,
};
```

Note: `ChatCompletionRequest` → `MessagesRequest`, `DeepSeekClient` → `ProviderClient`, `ToolCallKind`/`ToolResultMessage`/`ResponseFormat`/`ResponseFormatKind`/`StreamOptions` removed (not needed in Anthropic format). Add `ContentBlock` and `MessageRole` imports.

- [ ] **Step 3: Update Provider trait**

Change trait signature:
```rust
#[async_trait]
pub trait Provider: fmt::Debug + Send + Sync {
    async fn complete(
        &self,
        request: &MessagesRequest,
        context: &EpisodeContext,
    ) -> Result<ProviderEpisodeV1, DependencyFailure>;
}
```

Update impl:
```rust
#[async_trait]
impl Provider for ProviderClient {
    async fn complete(
        &self,
        request: &MessagesRequest,
        context: &EpisodeContext,
    ) -> Result<ProviderEpisodeV1, DependencyFailure> {
        self.complete_stream(request, context)
            .await
            .map_err(|error| {
                let is_glm = request.model == GLM_MODEL_ID;
                let (retryable, delivery) = if is_glm {
                    classify_glm_failure(&error)
                } else {
                    classify_deepseek_failure(&error)
                };
                let code = if is_glm { glm_failure_code(&error) } else { deepseek_failure_code(&error) };
                DependencyFailure::redacted(code, format!("{error:?}"), retryable, delivery)
            })
    }
}
```

- [ ] **Step 4: Rewrite build_provider_request**

Change the function to produce `MessagesRequest` instead of `ChatCompletionRequest`. Key changes:
- Extract first system message → top-level `system` field
- Convert remaining messages to `ProviderMessage` with `Vec<ContentBlock>`
- `max_tokens` is required (from `turn_policy.max_output_tokens`)
- `tool_choice`: `ToolChoice::Any` instead of `ToolChoice::Required`
- `thinking`: `ThinkingConfig { kind, budget_tokens: Some(max_output_tokens) }`
- Remove `stream_options`, `response_format`, `user_id` → `metadata: Some(RequestMetadata { user_id })`

- [ ] **Step 5: Update all message-building helpers**

Update `build_trusted_messages`, `append_assistant`, `append_capability_tool_result`, `append_workflow_transition_result`, `normalize_reasoning_content_for_thinking` to work with `ContentBlock` arrays instead of flat strings.

- [ ] **Step 6: Update all references throughout run-engine**

Search-and-replace remaining type references. ~150 references to update. Key patterns:
- `ChatCompletionRequest` → `MessagesRequest`
- `DeepSeekClient` → `ProviderClient`
- `StreamOptions { include_usage: true }` → removed
- `ToolChoice::Required` → `ToolChoice::Any`

- [ ] **Step 7: Build workspace**

Run: `KRW_ONTOLOGY_ROOT=$HOME/krw-ontology cargo build --workspace`
Expected: compiled successfully

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat(run-engine): wire to provider-wire (Anthropic Messages API)"
```

---

## Task 5: Update model-registry + protocol + runtime-config

**Files:**
- Modify: `deployments/local/model-registry.yaml`
- Modify: `deployments/prod/model-registry.yaml`
- Modify: `crates/protocol/src/lib.rs`
- Modify: `crates/runtime-config/src/lib.rs`

- [ ] **Step 1: Update model-registry.yaml endpoints**

```yaml
# DeepSeek
api_base: https://api.deepseek.com/anthropic
api_version: anthropic-messages-v1

# GLM
api_base: https://api.z.ai/api/anthropic
api_version: anthropic-messages-v1
```

- [ ] **Step 2: Update protocol capability matrix**

In `crates/protocol/src/lib.rs`, update `ProviderWireCapabilities`:
- `requires_reasoning_content_replay` → `requires_thinking_block_replay`
- `requires_assistant_content_for_tool_calls` → keep (run-engine AssistantMessage compat)
- Update `deepseek_v4_flash()` and `glm_5_2()` const fns

- [ ] **Step 3: Update runtime-config validation**

In `crates/runtime-config/src/lib.rs`:
- `DEEPSEEK_API_BASE` → `"https://api.deepseek.com/anthropic"`
- `GLM_API_BASE` → `"https://api.z.ai/api/anthropic"`
- `DEEPSEEK_API_VERSION` → `"anthropic-messages-v1"`
- `GLM_API_VERSION` → `"anthropic-messages-v1"`
- Update `validate_deepseek_model` / `validate_glm_model` endpoint checks

- [ ] **Step 4: Update remaining consumer Cargo.toml files**

Replace `krw-agent-deepseek-wire` with `krw-agent-provider-wire` in:
- `crates/test-support/Cargo.toml`
- `crates/context-compaction/Cargo.toml`
- `crates/perf-harness/Cargo.toml`
- `crates/research-quality/Cargo.toml`
- `crates/runtime-persistence/Cargo.toml`
- `crates/context-planner/Cargo.toml`

- [ ] **Step 5: Remove deepseek-wire crate**

Delete `crates/deepseek-wire/` directory and remove from workspace `Cargo.toml`.

- [ ] **Step 6: Build + clippy**

Run: `KRW_ONTOLOGY_ROOT=$HOME/krw-ontology cargo build --workspace && cargo clippy --workspace -- -D warnings`
Expected: clean

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat: switch all endpoints to Anthropic Messages API, remove deepseek-wire"
```

---

## Task 6: Integration test — GLM 50-question concurrent

**Files:**
- No code changes; test execution only

- [ ] **Step 1: Update .env.local**

Ensure `DEEPSEEK_API_KEY` and `GLM_API_KEY` are set (keys unchanged, just different endpoints).

- [ ] **Step 2: Start local stack**

```bash
export PATH="$HOME/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin:$PATH"
export KRW_ONTOLOGY_ROOT="$HOME/krw-ontology"
psql "postgresql://127.0.0.1:55432/postgres?user=$(id -un)" -c "DROP SCHEMA public CASCADE; CREATE SCHEMA public;"
rm -rf .local/agent-gateway/artifacts/*
bash scripts/start_local_agent_gateway_stack.sh
```

- [ ] **Step 3: Run single GLM test**

```bash
source .local/agent-gateway/secrets.env
# Submit single run, verify it reaches `final` state
```

Expected: single run succeeds.

- [ ] **Step 4: Run 50-question concurrent test**

```bash
bash scripts/run_concurrent50.sh
```

Expected: pass rate ≥ 50% (up from 38%).

- [ ] **Step 5: Commit test results**

```bash
git add -A
git commit -m "test: GLM 50-question concurrent test with Anthropic Messages API"
```
