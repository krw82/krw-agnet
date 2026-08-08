//! Anthropic Messages API wire types for the KRW agent runtime.
//!
//! This crate defines the request, message, content-block, and episode types
//! used to communicate with Anthropic-compatible chat completion endpoints.
//! It replaces the `OpenAI`-format `deepseek-wire` crate with a native
//! Anthropic Messages API surface: `MessagesRequest`, `ProviderMessage` as a
//! `{role, content: Vec<ContentBlock>}` struct, block-level `ContentBlock`
//! variants (`Text`, `ToolUse`, `ToolResult`, `Thinking`), and the
//! `ProviderEpisodeV1` replay-hash envelope shared with `run-engine`.

use std::collections::BTreeSet;

use krw_agent_protocol::{ContentHash, ThinkingMode, ALLOWED_MODEL_IDS};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use thiserror::Error;
use zeroize::Zeroize;

const MAX_PROVIDER_FUNCTION_NAME_BYTES: usize = 64;
const MAX_TOOL_CALL_ID_BYTES: usize = 256;

// ---------------------------------------------------------------------------
// Newtypes copied from deepseek-wire for provider compatibility.
// ---------------------------------------------------------------------------

/// A provider-visible function identifier. Logical capability IDs never cross
/// this boundary directly: the compiler maps them to this deliberately narrow,
/// provider-compatible grammar.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderFunctionName(String);

impl ProviderFunctionName {
    pub fn parse(value: impl Into<String>) -> Result<Self, WireError> {
        let value = value.into();
        let mut bytes = value.bytes();
        let Some(first) = bytes.next() else {
            return Err(WireError::InvalidProviderFunctionName);
        };
        if value.len() > MAX_PROVIDER_FUNCTION_NAME_BYTES
            || !(first.is_ascii_alphabetic() || first == b'_')
            || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(WireError::InvalidProviderFunctionName);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ProviderFunctionName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for ProviderFunctionName {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ProviderFunctionName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::parse(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Canonical JSON encoded as a provider-required text field. A tool result
/// cannot accidentally be represented as a JSON object once it reaches the
/// provider request type.
#[derive(Clone, PartialEq, Eq)]
pub struct CanonicalJsonText(String);

impl CanonicalJsonText {
    pub fn from_value(value: &Value) -> Result<Self, WireError> {
        let bytes = serde_jcs::to_vec(value)?;
        let text = String::from_utf8(bytes).map_err(|_| WireError::CanonicalJsonNotUtf8)?;
        Ok(Self(text))
    }

    pub fn parse(text: impl Into<String>) -> Result<Self, WireError> {
        let text = text.into();
        let value: Value = serde_json::from_str(&text)?;
        let canonical = Self::from_value(&value)?;
        if canonical.0 != text {
            return Err(WireError::NonCanonicalJsonText);
        }
        Ok(canonical)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn scrub_sensitive(&mut self) {
        self.0.zeroize();
    }
}

impl std::fmt::Debug for CanonicalJsonText {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CanonicalJsonText")
            .field("byte_len", &self.0.len())
            .field("content", &"[REDACTED]")
            .finish()
    }
}

impl Serialize for CanonicalJsonText {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for CanonicalJsonText {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::parse(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// A JSON Schema document used only in a typed provider function definition.
/// The contained value is intentionally private so callers cannot construct a
/// non-object `input_schema` field by accident.
#[derive(Clone, PartialEq)]
pub struct JsonSchemaDocument(Value);

impl JsonSchemaDocument {
    pub fn from_value(value: Value) -> Result<Self, WireError> {
        if !value.is_object() {
            return Err(WireError::InvalidJsonSchemaDocument);
        }
        Ok(Self(value))
    }

    pub fn as_value(&self) -> &Value {
        &self.0
    }

    pub fn scrub_sensitive(&mut self) {
        scrub_json(&mut self.0);
    }
}

impl std::fmt::Debug for JsonSchemaDocument {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JsonSchemaDocument")
            .field(
                "content_hash",
                &ContentHash::sha256(serde_jcs::to_vec(&self.0).unwrap_or_default()),
            )
            .finish()
    }
}

impl Serialize for JsonSchemaDocument {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for JsonSchemaDocument {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::from_value(Value::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Tool call primitives (copied from deepseek-wire).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallKind {
    Function,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FunctionCall {
    pub name: ProviderFunctionName,
    /// Provider-emitted JSON argument text, preserved without parse/reserialize.
    pub arguments: String,
}

impl std::fmt::Debug for FunctionCall {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FunctionCall")
            .field("name", &self.name)
            .field("arguments", &"[REDACTED]")
            .field("arguments_len", &self.arguments.len())
            .finish()
    }
}

impl Drop for FunctionCall {
    fn drop(&mut self) {
        self.arguments.zeroize();
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: ToolCallKind,
    pub function: FunctionCall,
}

// ---------------------------------------------------------------------------
// Internal assistant message (kept for run-engine compatibility).
// ---------------------------------------------------------------------------

/// Internal representation of an assistant turn. The flat `content`,
/// `reasoning_content`, and `tool_calls` fields are preserved so `run-engine`
/// can continue to build episodes without changes. Conversion to and from the
/// Anthropic `ContentBlock` array is handled by `into_content_blocks` and
/// `from_content_blocks`.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantMessage {
    pub content: Option<String>,
    pub reasoning_content: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
}

impl AssistantMessage {
    /// Validate the provider-neutral structure of a tool decision.
    fn validate_tool_call_structure(&self) -> Result<(), WireError> {
        let mut ids = BTreeSet::new();
        for call in &self.tool_calls {
            if call.id.is_empty()
                || call.id.len() > MAX_TOOL_CALL_ID_BYTES
                || !ids.insert(call.id.as_str())
            {
                return Err(WireError::InvalidToolCallId);
            }
            let _: Value = serde_json::from_str(&call.function.arguments)?;
        }
        Ok(())
    }

    fn validate_tool_call_requirements(
        &self,
        require_content: bool,
        require_reasoning_content: bool,
    ) -> Result<(), WireError> {
        self.validate_tool_call_structure()?;
        if self.tool_calls.is_empty() {
            return Ok(());
        }
        if (require_content && self.content.is_none())
            || (require_reasoning_content
                && self
                    .reasoning_content
                    .as_deref()
                    .is_none_or(|reasoning| reasoning.trim().is_empty()))
        {
            return Err(WireError::InvalidThinkingToolReplay);
        }
        Ok(())
    }

    /// Convert the internal assistant representation into the Anthropic
    /// `ContentBlock` array. The canonical order is: thinking (if present),
    /// text (if present), then one `ToolUse` block per tool call.
    ///
    /// Takes `self` by value but cannot use partial moves because this type
    /// implements `Drop`; `mem::take` extracts each field safely and leaves
    /// `Drop` to scrub the now-empty husk.
    pub fn into_content_blocks(mut self) -> Vec<ContentBlock> {
        let mut blocks = Vec::new();
        let reasoning = std::mem::take(&mut self.reasoning_content);
        if let Some(reasoning) = reasoning
            && !reasoning.is_empty()
        {
            blocks.push(ContentBlock::Thinking {
                thinking: reasoning,
                signature: String::new(),
            });
        }
        let content = std::mem::take(&mut self.content);
        if let Some(content) = content
            && !content.is_empty()
        {
            blocks.push(ContentBlock::Text { text: content });
        }
        let tool_calls = std::mem::take(&mut self.tool_calls);
        for call in tool_calls {
            let input: Value = serde_json::from_str(&call.function.arguments).unwrap_or(Value::Null);
            // `FunctionCall` implements `Drop`, so the name and arguments
            // cannot be partially moved out of `call.function`. Clone the
            // name (a small bounded string) and let `Drop` scrub the husk.
            blocks.push(ContentBlock::ToolUse {
                id: call.id,
                name: call.function.name.clone(),
                input,
            });
        }
        blocks
    }

    /// Reconstruct an internal `AssistantMessage` from an Anthropic
    /// `ContentBlock` array. `ToolResult` blocks are rejected: they only
    /// appear in `user` messages.
    pub fn from_content_blocks(blocks: &[ContentBlock]) -> Result<Self, WireError> {
        let mut content = None;
        let mut reasoning_content = None;
        let mut tool_calls = Vec::new();
        for block in blocks {
            match block {
                ContentBlock::Text { text } => {
                    content = Some(text.clone());
                }
                ContentBlock::Thinking { thinking, .. } => {
                    reasoning_content = Some(thinking.clone());
                }
                ContentBlock::ToolUse { id, name, input } => {
                    let arguments = serde_jcs::to_string(input)?;
                    tool_calls.push(ToolCall {
                        id: id.clone(),
                        kind: ToolCallKind::Function,
                        function: FunctionCall {
                            name: name.clone(),
                            arguments,
                        },
                    });
                }
                ContentBlock::ToolResult { .. } => {
                    return Err(WireError::UnexpectedToolResultInAssistant);
                }
            }
        }
        Ok(Self {
            content,
            reasoning_content,
            tool_calls,
        })
    }

    /// Build an Anthropic `ProviderMessage` with role `assistant`.
    pub fn into_provider_message(self) -> ProviderMessage {
        ProviderMessage {
            role: MessageRole::Assistant,
            content: self.into_content_blocks(),
        }
    }
}

impl std::fmt::Debug for AssistantMessage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AssistantMessage")
            .field("content_len", &self.content.as_ref().map(String::len))
            .field(
                "reasoning_content",
                &self.reasoning_content.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "reasoning_len",
                &self.reasoning_content.as_ref().map(String::len),
            )
            .field("tool_calls", &self.tool_calls)
            .finish()
    }
}

impl Drop for AssistantMessage {
    fn drop(&mut self) {
        if let Some(content) = &mut self.content {
            content.zeroize();
        }
        if let Some(reasoning) = &mut self.reasoning_content {
            reasoning.zeroize();
        }
    }
}

// ---------------------------------------------------------------------------
// Tool result message (internal representation, kept for run-engine compat).
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResultMessage {
    pub tool_call_id: String,
    /// Provider tool-result wire contract requires canonical JSON text.
    pub content: CanonicalJsonText,
}

impl ToolResultMessage {
    pub fn from_value(tool_call_id: impl Into<String>, value: &Value) -> Result<Self, WireError> {
        let tool_call_id = tool_call_id.into();
        if tool_call_id.is_empty() || tool_call_id.len() > MAX_TOOL_CALL_ID_BYTES {
            return Err(WireError::InvalidToolCallId);
        }
        Ok(Self {
            tool_call_id,
            content: CanonicalJsonText::from_value(value)?,
        })
    }

    /// Convert into a single Anthropic `ContentBlock::ToolResult`.
    pub fn into_content_block(mut self) -> ContentBlock {
        let tool_use_id = std::mem::take(&mut self.tool_call_id);
        let content = self.content.as_str().to_owned();
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error: false,
        }
    }

    /// Build an Anthropic `ProviderMessage` with role `user` wrapping the tool
    /// result in a single `ContentBlock::ToolResult`.
    pub fn into_provider_message(self) -> ProviderMessage {
        ProviderMessage {
            role: MessageRole::User,
            content: vec![self.into_content_block()],
        }
    }
}

impl std::fmt::Debug for ToolResultMessage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolResultMessage")
            .field(
                "tool_call_id_hash",
                &ContentHash::sha256(&self.tool_call_id),
            )
            .field("content", &"[REDACTED]")
            .finish()
    }
}

impl Drop for ToolResultMessage {
    fn drop(&mut self) {
        self.tool_call_id.zeroize();
        self.content.scrub_sensitive();
    }
}

// ---------------------------------------------------------------------------
// Anthropic Messages API types (new).
// ---------------------------------------------------------------------------

/// Anthropic message role. `System` is deliberately absent: the system prompt
/// is a top-level field on `MessagesRequest`, never a message variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
}

/// Anthropic content block, tagged on `"type"`.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContentBlock {
    Text {
        text: String,
    },
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

impl std::fmt::Debug for ContentBlock {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text { text } => formatter
                .debug_struct("ContentBlock::Text")
                .field("text_len", &text.len())
                .field("text", &"[REDACTED]")
                .finish(),
            Self::ToolUse { id, name, .. } => formatter
                .debug_struct("ContentBlock::ToolUse")
                .field("id_hash", &ContentHash::sha256(id))
                .field("name", name)
                .field("input", &"[REDACTED]")
                .finish(),
            Self::ToolResult {
                tool_use_id,
                is_error,
                ..
            } => formatter
                .debug_struct("ContentBlock::ToolResult")
                .field("tool_use_id_hash", &ContentHash::sha256(tool_use_id))
                .field("content", &"[REDACTED]")
                .field("is_error", is_error)
                .finish(),
            Self::Thinking { thinking, .. } => formatter
                .debug_struct("ContentBlock::Thinking")
                .field("thinking_len", &thinking.len())
                .field("thinking", &"[REDACTED]")
                .finish(),
        }
    }
}

/// Anthropic provider message: a role plus a list of content blocks.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderMessage {
    pub role: MessageRole,
    pub content: Vec<ContentBlock>,
}

impl ProviderMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: vec![ContentBlock::Text {
                text: content.into(),
            }],
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: vec![ContentBlock::Text {
                text: content.into(),
            }],
        }
    }

    pub fn scrub_sensitive(&mut self) {
        for block in &mut self.content {
            block.scrub_sensitive();
        }
    }
}

impl ContentBlock {
    pub fn scrub_sensitive(&mut self) {
        match self {
            Self::Text { text } => text.zeroize(),
            Self::ToolUse { id, input, .. } => {
                id.zeroize();
                scrub_json(input);
            }
            Self::ToolResult {
                tool_use_id,
                content,
                ..
            } => {
                tool_use_id.zeroize();
                content.zeroize();
            }
            Self::Thinking { thinking, signature } => {
                thinking.zeroize();
                signature.zeroize();
            }
        }
    }
}

impl std::fmt::Debug for ProviderMessage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderMessage")
            .field("role", &self.role)
            .field("block_count", &self.content.len())
            .field("content", &"[REDACTED]")
            .finish()
    }
}

impl Drop for ProviderMessage {
    fn drop(&mut self) {
        self.scrub_sensitive();
    }
}

/// Anthropic tool definition. Unlike `OpenAI`'s `{type:"function",
/// function:{...}}` wrapper, this is a flat `{name, description,
/// input_schema}` structure matching the Anthropic Messages API.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderToolDefinition {
    pub name: ProviderFunctionName,
    pub description: String,
    pub input_schema: JsonSchemaDocument,
}

impl std::fmt::Debug for ProviderToolDefinition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderToolDefinition")
            .field("name", &self.name)
            .field("description_len", &self.description.len())
            .field("input_schema", &self.input_schema)
            .finish()
    }
}

impl ProviderToolDefinition {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Result<Self, WireError> {
        Ok(Self {
            name: ProviderFunctionName::parse(name)?,
            description: description.into(),
            input_schema: JsonSchemaDocument::from_value(input_schema)?,
        })
    }

    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    pub fn replace_description(&mut self, description: impl Into<String>) {
        self.description.zeroize();
        self.description = description.into();
    }

    pub fn scrub_sensitive(&mut self) {
        self.description.zeroize();
        self.input_schema.scrub_sensitive();
    }
}

impl Drop for ProviderToolDefinition {
    fn drop(&mut self) {
        self.description.zeroize();
        self.input_schema.scrub_sensitive();
    }
}

/// Anthropic `tool_choice`. Internally tagged on `"type"`. `Any` replaces
/// `OpenAI`'s `Required`; `Tool{name}` selects a specific tool; `None`
/// suppresses tool use entirely.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolChoice {
    Auto,
    Any,
    Tool { name: ProviderFunctionName },
    None,
}

/// Anthropic extended-thinking configuration. Uses `budget_tokens` instead of
/// `OpenAI`'s `reasoning_effort`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThinkingConfig {
    #[serde(rename = "type")]
    pub kind: ThinkingMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<u32>,
}

/// Anthropic request-level metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestMetadata {
    pub user_id: String,
}

/// Anthropic Messages API request. Replaces the `OpenAI` `ChatCompletionRequest`.
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

impl MessagesRequest {
    /// Validate the Anthropic-specific request contract before hashing or I/O.
    pub fn validate(&self) -> Result<(), WireError> {
        if self.max_tokens == 0 {
            return Err(WireError::MissingMaxTokens);
        }
        if self.messages.is_empty() {
            return Err(WireError::InvalidRequest(
                "at least one message is required".into(),
            ));
        }
        if self.system.is_empty() {
            return Err(WireError::InvalidRequest(
                "system prompt must not be empty".into(),
            ));
        }
        // Thinking budget validation: when enabled, budget_tokens is required,
        // must be >= 1024 (Anthropic minimum), and must be < max_tokens.
        if self.thinking.kind == ThinkingMode::Enabled {
            let budget = self.thinking.budget_tokens.ok_or_else(|| {
                WireError::InvalidRequest("thinking=enabled requires budget_tokens".into())
            })?;
            if budget < 1024 {
                return Err(WireError::InvalidRequest(
                    "thinking budget_tokens must be >= 1024".into(),
                ));
            }
            if budget >= self.max_tokens {
                return Err(WireError::InvalidRequest(
                    "thinking budget_tokens must be < max_tokens".into(),
                ));
            }
        }
        // ToolResult blocks may only appear in `user` messages.
        for message in &self.messages {
            if message.role != MessageRole::User {
                for block in &message.content {
                    if matches!(block, ContentBlock::ToolResult { .. }) {
                        return Err(WireError::ToolResultInNonUserMessage);
                    }
                }
            }
        }
        // If a specific tool is requested via tool_choice, the tools list must
        // not be empty.
        if matches!(self.tool_choice, Some(ToolChoice::Tool { .. })) && self.tools.is_empty() {
            return Err(WireError::InvalidRequest(
                "tool_choice=tool requires at least one tool".into(),
            ));
        }
        if matches!(self.tool_choice, Some(ToolChoice::Any)) && self.tools.is_empty() {
            return Err(WireError::InvalidRequest(
                "tool_choice=any requires at least one tool".into(),
            ));
        }
        Ok(())
    }
}

impl std::fmt::Debug for MessagesRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MessagesRequest")
            .field("model", &self.model)
            .field("message_count", &self.messages.len())
            .field("system_len", &self.system.len())
            .field("max_tokens", &self.max_tokens)
            .field("tool_count", &self.tools.len())
            .field("thinking", &self.thinking.kind)
            .field("stream", &self.stream)
            .field("tool_choice", &self.tool_choice)
            .field(
                "metadata",
                &self.metadata.as_ref().map(|_| "[REDACTED]"),
            )
            .finish_non_exhaustive()
    }
}

impl Drop for MessagesRequest {
    fn drop(&mut self) {
        self.model.zeroize();
        self.system.zeroize();
        for message in &mut self.messages {
            message.scrub_sensitive();
        }
        for tool in &mut self.tools {
            tool.scrub_sensitive();
        }
        if let Some(metadata) = &mut self.metadata {
            metadata.user_id.zeroize();
        }
    }
}

// ---------------------------------------------------------------------------
// Episode envelope (copied from deepseek-wire for run-engine compat).
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderEpisodeV1 {
    pub schema_version: u16,
    pub request_hash: ContentHash,
    pub requested_model: String,
    pub observed_model: String,
    pub api_version: String,
    pub assistant: AssistantMessage,
    #[serde(default)]
    pub tool_results: Vec<ToolResultMessage>,
    pub tool_schema_hash: ContentHash,
    pub agent_image_hash: ContentHash,
    pub finish_reason: String,
    pub usage: TokenUsage,
    /// Hash of the canonical replay object stored in encrypted durable storage.
    pub replay_hash: ContentHash,
}

impl std::fmt::Debug for ProviderEpisodeV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderEpisodeV1")
            .field("request_hash", &self.request_hash)
            .field("requested_model", &self.requested_model)
            .field("observed_model", &self.observed_model)
            .field("finish_reason", &self.finish_reason)
            .field("tool_call_count", &self.assistant.tool_calls.len())
            .field("usage", &self.usage)
            .field("replay_hash", &self.replay_hash)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub prompt_cache_hit_tokens: u32,
    pub prompt_cache_miss_tokens: u32,
}

impl ProviderEpisodeV1 {
    pub fn verify_model_identity(&self) -> Result<(), WireError> {
        if !ALLOWED_MODEL_IDS.contains(&self.requested_model.as_str()) {
            return Err(WireError::UnknownModel(self.requested_model.clone()));
        }
        if self.requested_model != self.observed_model {
            return Err(WireError::ObservedModelMismatch {
                requested: self.requested_model.clone(),
                observed: self.observed_model.clone(),
            });
        }
        Ok(())
    }

    /// Validate only the exact tool-call envelope.
    pub fn verify_tool_call_structure(&self) -> Result<(), WireError> {
        self.assistant.validate_tool_call_structure()
    }

    /// Validate the replay fields required by the specific provider mode that
    /// generated this episode.
    pub fn verify_tool_call_requirements(
        &self,
        require_content: bool,
        require_reasoning_content: bool,
    ) -> Result<(), WireError> {
        self.assistant
            .validate_tool_call_requirements(require_content, require_reasoning_content)
    }

    /// Produce the Anthropic-format replay message sequence. The assistant
    /// turn comes first, followed by one `user` message per tool result.
    pub fn replay_messages(&self) -> Vec<ProviderMessage> {
        let mut messages = vec![self.assistant.clone().into_provider_message()];
        messages.extend(
            self.tool_results
                .iter()
                .cloned()
                .map(ToolResultMessage::into_provider_message),
        );
        messages
    }

    pub fn calculate_replay_hash(&self) -> Result<ContentHash, WireError> {
        let replay = self.replay_messages();
        Ok(ContentHash::sha256(serde_jcs::to_vec(&replay)?))
    }
}

#[derive(Debug, Clone)]
pub struct EpisodeContext {
    pub tool_schema_hash: ContentHash,
    pub agent_image_hash: ContentHash,
    pub api_version: String,
}

// ---------------------------------------------------------------------------
// Error type (adapted from deepseek-wire).
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum WireError {
    #[error("provider function name violates the closed grammar")]
    InvalidProviderFunctionName,
    #[error("provider JSON Schema parameters must be an object")]
    InvalidJsonSchemaDocument,
    #[error("canonical JSON serialization was not UTF-8")]
    CanonicalJsonNotUtf8,
    #[error("provider tool result text is valid JSON but not RFC 8785 canonical JSON")]
    NonCanonicalJsonText,
    #[error("provider tool-call ID is empty, too long, or duplicated")]
    InvalidToolCallId,
    #[error("provider system/user message content cannot be empty")]
    EmptyMessageContent,
    #[error("observed model {observed} differs from requested model {requested}")]
    ObservedModelMismatch {
        requested: String,
        observed: String,
    },
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("HTTP transport failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("model is not in the exact allowlist: {0}")]
    UnknownModel(String),
    #[error("invalid provider request: {0}")]
    InvalidRequest(String),
    #[error("max_tokens is required and must be greater than zero")]
    MissingMaxTokens,
    #[error("tool_result content block appeared in a non-user message")]
    ToolResultInNonUserMessage,
    #[error("tool_result content block appeared in an assistant message")]
    UnexpectedToolResultInAssistant,
    #[error("thinking tool call lacks non-null content or complete non-empty reasoning_content")]
    InvalidThinkingToolReplay,
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn scrub_json(value: &mut Value) {
    match value {
        Value::String(text) => text.zeroize(),
        Value::Array(values) => values.iter_mut().for_each(scrub_json),
        Value::Object(values) => values.values_mut().for_each(scrub_json),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use krw_agent_protocol::{DEEPSEEK_MODEL_ID, GLM_MODEL_ID};

    #[test]
    fn content_block_roundtrips_through_serde() {
        let block = ContentBlock::Text {
            text: "hello 한글".into(),
        };
        let json = serde_json::to_string(&block).unwrap();
        assert_eq!(
            json,
            r#"{"type":"text","text":"hello 한글"}"#
        );
        let decoded: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(block, decoded);
    }

    #[test]
    fn tool_use_block_serializes_anthropic_format() {
        let block = ContentBlock::ToolUse {
            id: "toolu_01".into(),
            name: ProviderFunctionName::parse("krw_query").unwrap(),
            input: serde_json::json!({"q": "test"}),
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains(r#""type":"tool_use""#));
        assert!(json.contains(r#""id":"toolu_01""#));
        assert!(json.contains(r#""name":"krw_query""#));
        assert!(json.contains(r#""input":{"q":"test"}"#));
    }

    #[test]
    fn tool_choice_serializes_internally_tagged() {
        let json = serde_json::to_string(&ToolChoice::Auto).unwrap();
        assert_eq!(json, r#"{"type":"auto"}"#);

        let json = serde_json::to_string(&ToolChoice::Any).unwrap();
        assert_eq!(json, r#"{"type":"any"}"#);

        let json = serde_json::to_string(&ToolChoice::None).unwrap();
        assert_eq!(json, r#"{"type":"none"}"#);

        let choice = ToolChoice::Tool {
            name: ProviderFunctionName::parse("krw_query").unwrap(),
        };
        let json = serde_json::to_string(&choice).unwrap();
        assert_eq!(json, r#"{"type":"tool","name":"krw_query"}"#);
    }

    #[test]
    fn thinking_config_serializes_with_budget() {
        let config = ThinkingConfig {
            kind: ThinkingMode::Enabled,
            budget_tokens: Some(10_000),
        };
        let json = serde_json::to_string(&config).unwrap();
        assert_eq!(
            json,
            r#"{"type":"enabled","budget_tokens":10000}"#
        );
    }

    #[test]
    fn thinking_config_omits_budget_when_none() {
        let config = ThinkingConfig {
            kind: ThinkingMode::Disabled,
            budget_tokens: None,
        };
        let json = serde_json::to_string(&config).unwrap();
        assert_eq!(json, r#"{"type":"disabled"}"#);
    }

    #[test]
    fn provider_tool_definition_is_flat_anthropic_format() {
        let tool = ProviderToolDefinition::new(
            "krw_query",
            "Query the ontology",
            serde_json::json!({"type": "object", "properties": {}}),
        )
        .unwrap();
        let json = serde_json::to_string(&tool).unwrap();
        assert!(json.contains(r#""name":"krw_query""#));
        assert!(json.contains(r#""description":"Query the ontology""#));
        // The input_schema is serialized by serde_json, which preserves the
        // insertion order of serde_json::Value objects. The shape must be a
        // flat Anthropic tool def, NOT the OpenAI `{type:"function",...}` wrapper.
        assert!(json.contains(r#""input_schema":"#));
        assert!(json.contains(r#""type":"object""#));
        assert!(!json.contains(r#""type":"function""#));
        assert!(!json.contains(r#""function":"#));
    }

    #[test]
    fn messages_request_validates_basic_contract() {
        fn build() -> MessagesRequest {
            MessagesRequest {
                model: GLM_MODEL_ID.into(),
                messages: vec![ProviderMessage::user("hello")],
                system: "You are helpful".into(),
                max_tokens: 1024,
                tools: Vec::new(),
                tool_choice: None,
                thinking: ThinkingConfig {
                    kind: ThinkingMode::Disabled,
                    budget_tokens: None,
                },
                stream: true,
                metadata: None,
            }
        }

        assert!(build().validate().is_ok());

        let mut zero_tokens = build();
        zero_tokens.max_tokens = 0;
        assert!(matches!(
            zero_tokens.validate(),
            Err(WireError::MissingMaxTokens)
        ));

        let mut empty_messages = build();
        empty_messages.messages = Vec::new();
        assert!(empty_messages.validate().is_err());

        let mut empty_system = build();
        empty_system.system = String::new();
        assert!(empty_system.validate().is_err());
    }

    #[test]
    fn thinking_enabled_requires_valid_budget() {
        fn build(budget: Option<u32>, max_tokens: u32) -> MessagesRequest {
            MessagesRequest {
                model: GLM_MODEL_ID.into(),
                messages: vec![ProviderMessage::user("hello")],
                system: "You are helpful".into(),
                max_tokens,
                tools: Vec::new(),
                tool_choice: None,
                thinking: ThinkingConfig {
                    kind: ThinkingMode::Enabled,
                    budget_tokens: budget,
                },
                stream: true,
                metadata: None,
            }
        }

        assert!(build(Some(5_000), 10_000).validate().is_ok());
        assert!(build(None, 10_000).validate().is_err());
        assert!(build(Some(512), 10_000).validate().is_err());
        assert!(build(Some(10_000), 10_000).validate().is_err());
    }

    #[test]
    fn tool_result_block_only_in_user_messages() {
        let assistant_with_tool_result = MessagesRequest {
            model: GLM_MODEL_ID.into(),
            messages: vec![ProviderMessage {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "toolu_01".into(),
                    content: "{}".into(),
                    is_error: false,
                }],
            }],
            system: "system".into(),
            max_tokens: 1024,
            tools: Vec::new(),
            tool_choice: None,
            thinking: ThinkingConfig {
                kind: ThinkingMode::Disabled,
                budget_tokens: None,
            },
            stream: true,
            metadata: None,
        };
        assert!(matches!(
            assistant_with_tool_result.validate(),
            Err(WireError::ToolResultInNonUserMessage)
        ));
    }

    #[test]
    fn assistant_message_roundtrips_through_content_blocks() {
        let original = AssistantMessage {
            content: Some("answer".into()),
            reasoning_content: Some("private reasoning".into()),
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                kind: ToolCallKind::Function,
                function: FunctionCall {
                    name: ProviderFunctionName::parse("krw_query").unwrap(),
                    arguments: r#"{"q":"한글"}"#.into(),
                },
            }],
        };
        let blocks = original.clone().into_content_blocks();
        // thinking first, then text, then tool_use
        assert_eq!(blocks.len(), 3);
        assert!(matches!(&blocks[0], ContentBlock::Thinking { .. }));
        assert!(matches!(&blocks[1], ContentBlock::Text { .. }));
        assert!(matches!(&blocks[2], ContentBlock::ToolUse { .. }));

        let reconstructed = AssistantMessage::from_content_blocks(&blocks).unwrap();
        assert_eq!(reconstructed.content, original.content);
        assert_eq!(reconstructed.reasoning_content, original.reasoning_content);
        assert_eq!(reconstructed.tool_calls.len(), original.tool_calls.len());
        assert_eq!(
            reconstructed.tool_calls[0].function.name.as_str(),
            original.tool_calls[0].function.name.as_str()
        );
        // Arguments should be valid JSON with the same canonical content.
        let orig_val: Value =
            serde_json::from_str(&original.tool_calls[0].function.arguments).unwrap();
        let recon_val: Value =
            serde_json::from_str(&reconstructed.tool_calls[0].function.arguments).unwrap();
        assert_eq!(orig_val, recon_val);
    }

    #[test]
    fn from_content_blocks_rejects_tool_result() {
        let blocks = vec![ContentBlock::ToolResult {
            tool_use_id: "toolu_01".into(),
            content: "{}".into(),
            is_error: false,
        }];
        assert!(matches!(
            AssistantMessage::from_content_blocks(&blocks),
            Err(WireError::UnexpectedToolResultInAssistant)
        ));
    }

    #[test]
    fn episode_replay_hash_is_stable() {
        let episode = ProviderEpisodeV1 {
            schema_version: 1,
            request_hash: ContentHash::sha256("request"),
            requested_model: DEEPSEEK_MODEL_ID.into(),
            observed_model: DEEPSEEK_MODEL_ID.into(),
            api_version: "messages-v1".into(),
            assistant: AssistantMessage {
                content: Some("answer".into()),
                reasoning_content: Some("private reasoning 한글".into()),
                tool_calls: vec![ToolCall {
                    id: "call_1".into(),
                    kind: ToolCallKind::Function,
                    function: FunctionCall {
                        name: ProviderFunctionName::parse("ontology_query_context").unwrap(),
                        arguments: r#"{"search_plan":{"b":2,"a":1}}"#.into(),
                    },
                }],
            },
            tool_results: Vec::new(),
            tool_schema_hash: ContentHash::sha256("tools"),
            agent_image_hash: ContentHash::sha256("image"),
            finish_reason: "tool_calls".into(),
            usage: TokenUsage::default(),
            replay_hash: ContentHash::sha256("placeholder"),
        };
        let hash_a = episode.calculate_replay_hash().unwrap();
        let hash_b = episode.calculate_replay_hash().unwrap();
        assert_eq!(hash_a, hash_b);

        let replay = episode.replay_messages();
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].role, MessageRole::Assistant);
        // thinking + text + tool_use = 3 blocks
        assert_eq!(replay[0].content.len(), 3);
    }

    #[test]
    fn episode_replay_includes_tool_results_as_user_messages() {
        let episode = ProviderEpisodeV1 {
            schema_version: 1,
            request_hash: ContentHash::sha256("request"),
            requested_model: GLM_MODEL_ID.into(),
            observed_model: GLM_MODEL_ID.into(),
            api_version: "messages-v1".into(),
            assistant: AssistantMessage {
                content: Some("answer".into()),
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: "call_1".into(),
                    kind: ToolCallKind::Function,
                    function: FunctionCall {
                        name: ProviderFunctionName::parse("krw_query").unwrap(),
                        arguments: r#"{"q":"test"}"#.into(),
                    },
                }],
            },
            tool_results: vec![ToolResultMessage::from_value(
                "call_1",
                &serde_json::json!({"result": "ok"}),
            )
            .unwrap()],
            tool_schema_hash: ContentHash::sha256("tools"),
            agent_image_hash: ContentHash::sha256("image"),
            finish_reason: "tool_calls".into(),
            usage: TokenUsage::default(),
            replay_hash: ContentHash::sha256("placeholder"),
        };
        let replay = episode.replay_messages();
        assert_eq!(replay.len(), 2);
        assert_eq!(replay[0].role, MessageRole::Assistant);
        assert_eq!(replay[1].role, MessageRole::User);
        assert!(matches!(
            &replay[1].content[0],
            ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "call_1"
        ));
    }

    #[test]
    fn episode_verify_model_identity_rejects_unknown() {
        let episode = ProviderEpisodeV1 {
            schema_version: 1,
            request_hash: ContentHash::sha256("request"),
            requested_model: "forbidden-model".into(),
            observed_model: "forbidden-model".into(),
            api_version: "messages-v1".into(),
            assistant: AssistantMessage {
                content: Some("answer".into()),
                reasoning_content: None,
                tool_calls: Vec::new(),
            },
            tool_results: Vec::new(),
            tool_schema_hash: ContentHash::sha256("tools"),
            agent_image_hash: ContentHash::sha256("image"),
            finish_reason: "stop".into(),
            usage: TokenUsage::default(),
            replay_hash: ContentHash::sha256("placeholder"),
        };
        assert!(matches!(
            episode.verify_model_identity(),
            Err(WireError::UnknownModel(m)) if m == "forbidden-model"
        ));
    }

    #[test]
    fn full_messages_request_roundtrips_through_serde() {
        let request = MessagesRequest {
            model: GLM_MODEL_ID.into(),
            messages: vec![
                ProviderMessage::user("What is the weather?"),
                ProviderMessage {
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::ToolUse {
                        id: "toolu_01".into(),
                        name: ProviderFunctionName::parse("krw_weather").unwrap(),
                        input: serde_json::json!({"city": "Seoul"}),
                    }],
                },
                ProviderMessage {
                    role: MessageRole::User,
                    content: vec![ContentBlock::ToolResult {
                        tool_use_id: "toolu_01".into(),
                        content: r#"{"temp":22}"#.into(),
                        is_error: false,
                    }],
                },
            ],
            system: "You are a weather assistant".into(),
            max_tokens: 4096,
            tools: vec![ProviderToolDefinition::new(
                "krw_weather",
                "Get weather",
                serde_json::json!({"type":"object","properties":{"city":{"type":"string"}}}),
            )
            .unwrap()],
            tool_choice: Some(ToolChoice::Auto),
            thinking: ThinkingConfig {
                kind: ThinkingMode::Enabled,
                budget_tokens: Some(2048),
            },
            stream: true,
            metadata: Some(RequestMetadata {
                user_id: "user-123".into(),
            }),
        };
        let json = serde_json::to_string(&request).unwrap();
        let decoded: MessagesRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(request, decoded);
    }
}
