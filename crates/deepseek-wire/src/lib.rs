//! DeepSeek-native wire types and a chunk-boundary-independent SSE decoder.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use futures_util::StreamExt;
use krw_agent_protocol::{ContentHash, DEEPSEEK_MODEL_ID};
pub use krw_agent_protocol::{ReasoningEffort, ThinkingMode};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use reqwest::redirect::Policy as RedirectPolicy;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use thiserror::Error;
use zeroize::Zeroize;

const MAX_PROVIDER_FUNCTION_NAME_BYTES: usize = 64;
const MAX_TOOL_CALL_ID_BYTES: usize = 256;

/// A provider-visible function identifier.  Logical capability IDs never
/// cross this boundary directly: the compiler maps them to this deliberately
/// narrow, DeepSeek-compatible grammar.
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

/// Canonical JSON encoded as a provider-required text field.  A tool result
/// cannot accidentally be represented as a JSON object once it reaches the
/// `DeepSeek` request type.
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
/// non-object `parameters` field by accident.
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

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantMessage {
    pub content: Option<String>,
    pub reasoning_content: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
}

impl AssistantMessage {
    /// Validate the provider-neutral structure of a tool decision. Direct
    /// Flash tool turns legally omit both `content` and `reasoning_content`,
    /// so mode-specific replay requirements are checked separately from this
    /// basic envelope validation.
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

    pub fn into_provider_message(mut self) -> ProviderMessage {
        ProviderMessage::Assistant {
            content: self.content.take(),
            reasoning_content: self.reasoning_content.take(),
            tool_calls: std::mem::take(&mut self.tool_calls),
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

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResultMessage {
    pub tool_call_id: String,
    /// `DeepSeek`'s tool-result wire contract requires canonical JSON text.
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

    pub fn into_provider_message(mut self) -> ProviderMessage {
        ProviderMessage::Tool {
            tool_call_id: std::mem::take(&mut self.tool_call_id),
            content: std::mem::replace(&mut self.content, CanonicalJsonText(String::new())),
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

/// The only outbound provider-message variants.  This deliberately removes
/// the generic JSON escape hatch for transcripts: a tool result is always
/// canonical JSON text, and role-specific fields are unrepresentable on the
/// wrong message kind.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase", deny_unknown_fields)]
pub enum ProviderMessage {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        content: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning_content: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        tool_call_id: String,
        content: CanonicalJsonText,
    },
}

impl std::fmt::Debug for ProviderMessage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::System { content } => formatter
                .debug_struct("ProviderMessage::System")
                .field("content_len", &content.len())
                .field("content", &"[REDACTED]")
                .finish(),
            Self::User { content } => formatter
                .debug_struct("ProviderMessage::User")
                .field("content_len", &content.len())
                .field("content", &"[REDACTED]")
                .finish(),
            Self::Assistant {
                content,
                reasoning_content,
                tool_calls,
            } => formatter
                .debug_struct("ProviderMessage::Assistant")
                .field("content_len", &content.as_ref().map(String::len))
                .field(
                    "reasoning_len",
                    &reasoning_content.as_ref().map(String::len),
                )
                .field("tool_calls", tool_calls)
                .finish(),
            Self::Tool {
                tool_call_id,
                content,
            } => formatter
                .debug_struct("ProviderMessage::Tool")
                .field("tool_call_id_hash", &ContentHash::sha256(tool_call_id))
                .field("content", content)
                .finish(),
        }
    }
}

impl ProviderMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self::System {
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::User {
            content: content.into(),
        }
    }

    pub fn role(&self) -> &'static str {
        match self {
            Self::System { .. } => "system",
            Self::User { .. } => "user",
            Self::Assistant { .. } => "assistant",
            Self::Tool { .. } => "tool",
        }
    }

    pub fn content(&self) -> Option<&str> {
        match self {
            Self::System { content } | Self::User { content } => Some(content),
            Self::Assistant { content, .. } => content.as_deref(),
            Self::Tool { content, .. } => Some(content.as_str()),
        }
    }

    pub fn replace_user_content(&mut self, content: impl Into<String>) -> Result<(), WireError> {
        match self {
            Self::User { content: existing } => {
                existing.zeroize();
                *existing = content.into();
                Ok(())
            }
            _ => Err(WireError::ExpectedUserMessage),
        }
    }

    fn validate(&self) -> Result<(), WireError> {
        match self {
            Self::System { content } | Self::User { content } if content.is_empty() => {
                Err(WireError::EmptyMessageContent)
            }
            Self::System { .. } | Self::User { .. } => Ok(()),
            Self::Assistant {
                content,
                reasoning_content,
                tool_calls,
            } => AssistantMessage {
                content: content.clone(),
                reasoning_content: reasoning_content.clone(),
                tool_calls: tool_calls.clone(),
            }
            .validate_tool_call_structure(),
            Self::Tool { tool_call_id, .. } => {
                if tool_call_id.is_empty() || tool_call_id.len() > MAX_TOOL_CALL_ID_BYTES {
                    Err(WireError::InvalidToolCallId)
                } else {
                    Ok(())
                }
            }
        }
    }

    pub fn scrub_sensitive(&mut self) {
        match self {
            Self::System { content } | Self::User { content } => content.zeroize(),
            Self::Assistant {
                content,
                reasoning_content,
                tool_calls,
            } => {
                if let Some(content) = content {
                    content.zeroize();
                }
                if let Some(reasoning_content) = reasoning_content {
                    reasoning_content.zeroize();
                }
                for call in tool_calls {
                    call.id.zeroize();
                    call.function.arguments.zeroize();
                }
            }
            Self::Tool {
                tool_call_id,
                content,
            } => {
                tool_call_id.zeroize();
                content.scrub_sensitive();
            }
        }
    }
}

/// Provider function definitions are strongly typed so an invalid schema or
/// a dotted logical capability ID cannot leak into `DeepSeek`'s API payload.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderFunctionDefinition {
    pub description: String,
    pub name: ProviderFunctionName,
    pub parameters: JsonSchemaDocument,
}

impl std::fmt::Debug for ProviderFunctionDefinition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderFunctionDefinition")
            .field("name", &self.name)
            .field("description_len", &self.description.len())
            .field("parameters", &self.parameters)
            .finish()
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderToolDefinition {
    #[serde(rename = "type")]
    pub kind: ToolCallKind,
    pub function: ProviderFunctionDefinition,
}

impl std::fmt::Debug for ProviderToolDefinition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderToolDefinition")
            .field("kind", &self.kind)
            .field("function", &self.function)
            .finish()
    }
}

impl ProviderToolDefinition {
    pub fn function(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
    ) -> Result<Self, WireError> {
        Ok(Self {
            kind: ToolCallKind::Function,
            function: ProviderFunctionDefinition {
                description: description.into(),
                name: ProviderFunctionName::parse(name)?,
                parameters: JsonSchemaDocument::from_value(parameters)?,
            },
        })
    }

    pub fn function_name(&self) -> &str {
        self.function.name.as_str()
    }

    pub fn replace_description(&mut self, description: impl Into<String>) {
        self.function.description.zeroize();
        self.function.description = description.into();
    }

    pub fn scrub_sensitive(&mut self) {
        self.function.description.zeroize();
        self.function.parameters.scrub_sensitive();
    }
}

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
        if self.requested_model != DEEPSEEK_MODEL_ID {
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

    /// Validate only the exact tool-call envelope. This accepts direct Flash
    /// tool turns, whose assistant message may intentionally have null
    /// `content` and no `reasoning_content`.
    pub fn verify_tool_call_structure(&self) -> Result<(), WireError> {
        self.assistant.validate_tool_call_structure()
    }

    /// Validate the replay fields required by the *specific provider mode*
    /// that generated this episode. The caller owns that mode through the
    /// pinned request/snapshot; it must not be inferred from response shape.
    pub fn verify_tool_call_requirements(
        &self,
        require_content: bool,
        require_reasoning_content: bool,
    ) -> Result<(), WireError> {
        self.assistant
            .validate_tool_call_requirements(require_content, require_reasoning_content)
    }

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

/// Streaming SSE decoder. It retains incomplete UTF-8 and frame bytes until a complete event exists.
#[derive(Debug, Default)]
pub struct SseDecoder {
    buffer: Vec<u8>,
    max_buffer_bytes: usize,
}

impl SseDecoder {
    pub fn new(max_buffer_bytes: usize) -> Self {
        Self {
            buffer: Vec::new(),
            max_buffer_bytes,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseEvent>, WireError> {
        if self.buffer.len().saturating_add(chunk.len()) > self.max_buffer_bytes {
            return Err(WireError::SseBufferLimit(self.max_buffer_bytes));
        }
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some((frame_end, delimiter_len)) = find_frame(&self.buffer) {
            let frame = self.buffer[..frame_end].to_vec();
            self.buffer.drain(..frame_end + delimiter_len);
            if let Some(event) = parse_frame(&frame)? {
                events.push(event);
            }
        }
        Ok(events)
    }

    pub fn finish(self) -> Result<(), WireError> {
        if self.buffer.iter().all(u8::is_ascii_whitespace) {
            Ok(())
        } else {
            Err(WireError::IncompleteSseFrame)
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum SseEvent {
    Data(String),
    Done,
}

impl std::fmt::Debug for SseEvent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Data(data) => formatter
                .debug_struct("SseEvent::Data")
                .field("byte_len", &data.len())
                .field("payload", &"[REDACTED]")
                .finish(),
            Self::Done => formatter.write_str("SseEvent::Done"),
        }
    }
}

fn find_frame(bytes: &[u8]) -> Option<(usize, usize)> {
    for index in 0..bytes.len() {
        if bytes.get(index..index + 2) == Some(b"\n\n") {
            return Some((index, 2));
        }
        if bytes.get(index..index + 4) == Some(b"\r\n\r\n") {
            return Some((index, 4));
        }
    }
    None
}

fn parse_frame(frame: &[u8]) -> Result<Option<SseEvent>, WireError> {
    let text = std::str::from_utf8(frame).map_err(WireError::Utf8)?;
    let mut data = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("data:") {
            data.push(value.strip_prefix(' ').unwrap_or(value));
        }
    }
    if data.is_empty() {
        return Ok(None);
    }
    let joined = data.join("\n");
    if joined == "[DONE]" {
        Ok(Some(SseEvent::Done))
    } else {
        Ok(Some(SseEvent::Data(joined)))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatCompletionChunk {
    pub id: String,
    pub model: String,
    #[serde(default)]
    pub choices: Vec<ChunkChoice>,
    #[serde(default)]
    pub usage: Option<TokenUsage>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChunkChoice {
    pub index: u16,
    pub delta: Value,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThinkingConfig {
    #[serde(rename = "type")]
    pub kind: ThinkingMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamOptions {
    pub include_usage: bool,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ProviderMessage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ProviderToolDefinition>,
    pub stream: bool,
    pub stream_options: StreamOptions,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    pub thinking: ThinkingConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// Optional provider-level preference for a function call. The semantic
    /// decision contract lives in the AgentImage/state program; this field is
    /// emitted only when the pinned DeepSeek mode explicitly supports it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
}

/// Closed subset of DeepSeek's tool-choice surface used by the kernel. A
/// named function is deliberately unnecessary: autonomous assessment states
/// must be able to choose among the image-declared action frontier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    None,
    Auto,
    Required,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseFormatKind {
    JsonObject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseFormat {
    #[serde(rename = "type")]
    pub kind: ResponseFormatKind,
}

impl ChatCompletionRequest {
    /// Validates the provider-specific thinking contract before hashing or I/O.
    ///
    /// `DeepSeek` requires an explicit effort for thinking mode. Non-thinking
    /// requests must omit that field entirely; accepting it would make the
    /// effective execution profile ambiguous.
    pub fn validate(&self) -> Result<(), WireError> {
        if !self.stream || self.messages.is_empty() {
            return Err(WireError::InvalidRequest(
                "stream=true and at least one message are required".into(),
            ));
        }
        match (self.thinking.kind, self.reasoning_effort) {
            (ThinkingMode::Enabled, None) => return Err(WireError::MissingReasoningEffort),
            (ThinkingMode::Disabled, Some(_)) => {
                return Err(WireError::UnexpectedReasoningEffort);
            }
            (ThinkingMode::Enabled, Some(_)) | (ThinkingMode::Disabled, None) => {}
        }
        // DeepSeek V4 accepts tools in thinking mode, but rejects the entire
        // `tool_choice` parameter in that mode. Keep this native wire rule as
        // a last line of defence even though the runtime resolves the same
        // fact from its immutable provider capability matrix.
        if self.thinking.kind == ThinkingMode::Enabled && self.tool_choice.is_some() {
            return Err(WireError::ThinkingToolChoiceUnsupported);
        }
        if matches!(self.tool_choice, Some(ToolChoice::Required)) && self.tools.is_empty() {
            return Err(WireError::InvalidRequest(
                "tool_choice=required requires at least one tool".into(),
            ));
        }
        // The kernel deliberately uses one output channel per state: either
        // a typed function call or one JSON response. Combining both would
        // reintroduce the ambiguous free-text protocol this wire layer closes.
        if self.response_format.is_some() && !self.tools.is_empty() {
            return Err(WireError::InvalidRequest(
                "response_format cannot be combined with tools".into(),
            ));
        }
        let mut pending_tool_calls = BTreeSet::new();
        for message in &self.messages {
            message.validate()?;
            match message {
                ProviderMessage::Assistant { tool_calls, .. } => {
                    if !pending_tool_calls.is_empty() {
                        return Err(WireError::UnresolvedToolCalls);
                    }
                    for call in tool_calls {
                        if !pending_tool_calls.insert(call.id.as_str()) {
                            return Err(WireError::InvalidToolCallId);
                        }
                    }
                }
                ProviderMessage::Tool { tool_call_id, .. } => {
                    if !pending_tool_calls.remove(tool_call_id.as_str()) {
                        return Err(WireError::UnexpectedToolResult(tool_call_id.clone()));
                    }
                }
                ProviderMessage::System { .. } | ProviderMessage::User { .. }
                    if !pending_tool_calls.is_empty() =>
                {
                    return Err(WireError::UnresolvedToolCalls);
                }
                ProviderMessage::System { .. } | ProviderMessage::User { .. } => {}
            }
        }
        if !pending_tool_calls.is_empty() {
            return Err(WireError::UnresolvedToolCalls);
        }
        Ok(())
    }
}

impl std::fmt::Debug for ChatCompletionRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChatCompletionRequest")
            .field("model", &self.model)
            .field("message_count", &self.messages.len())
            .field("tool_count", &self.tools.len())
            .field("stream", &self.stream)
            .field("thinking", &self.thinking.kind)
            .field("reasoning_effort", &self.reasoning_effort)
            .field("max_tokens", &self.max_tokens)
            .field("user_id", &self.user_id.as_ref().map(|_| "[REDACTED]"))
            .field("tool_choice", &self.tool_choice)
            .field(
                "response_format",
                &self.response_format.as_ref().map(|_| "[REDACTED]"),
            )
            .finish_non_exhaustive()
    }
}

impl Drop for ChatCompletionRequest {
    fn drop(&mut self) {
        self.model.zeroize();
        for message in &mut self.messages {
            message.scrub_sensitive();
        }
        for tool in &mut self.tools {
            tool.scrub_sensitive();
        }
        if let Some(user_id) = &mut self.user_id {
            user_id.zeroize();
        }
    }
}

#[derive(Debug, Clone)]
pub struct EpisodeContext {
    pub tool_schema_hash: ContentHash,
    pub agent_image_hash: ContentHash,
    pub api_version: String,
}

pub struct DeepSeekClientConfig {
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

impl DeepSeekClientConfig {
    pub fn production(
        api_base: impl Into<String>,
        allowed_models: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            api_base: api_base.into(),
            allowed_models: allowed_models.into_iter().collect(),
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(130),
            max_error_body_bytes: 64 * 1024,
            max_sse_frame_bytes: 2 * 1024 * 1024,
            max_stream_bytes: 8 * 1024 * 1024,
            max_episode_bytes: 4 * 1024 * 1024,
            max_idle_per_host: 8,
        }
    }
}

impl std::fmt::Debug for DeepSeekClientConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeepSeekClientConfig")
            .field("api_base_hash", &ContentHash::sha256(&self.api_base))
            .field("allowed_models", &self.allowed_models)
            .field("connect_timeout", &self.connect_timeout)
            .field("request_timeout", &self.request_timeout)
            .field("max_error_body_bytes", &self.max_error_body_bytes)
            .field("max_sse_frame_bytes", &self.max_sse_frame_bytes)
            .field("max_stream_bytes", &self.max_stream_bytes)
            .field("max_episode_bytes", &self.max_episode_bytes)
            .field("max_idle_per_host", &self.max_idle_per_host)
            .finish()
    }
}

pub struct DeepSeekClient {
    http: reqwest::Client,
    endpoint: String,
    allowed_models: BTreeSet<String>,
    request_timeout: Duration,
    max_error_body_bytes: usize,
    max_sse_frame_bytes: usize,
    max_stream_bytes: usize,
    max_episode_bytes: usize,
}

impl std::fmt::Debug for DeepSeekClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeepSeekClient")
            .field("endpoint_hash", &ContentHash::sha256(&self.endpoint))
            .field("allowed_models", &self.allowed_models)
            .field("request_timeout", &self.request_timeout)
            .field("authorization", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl DeepSeekClient {
    pub fn new(config: DeepSeekClientConfig, api_key: &str) -> Result<Self, WireError> {
        let url = reqwest::Url::parse(&config.api_base).map_err(|_| WireError::InvalidEndpoint)?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(WireError::InvalidEndpoint);
        }
        if api_key.is_empty() {
            return Err(WireError::InvalidAuthorization);
        }
        if config.request_timeout.is_zero()
            || config.max_error_body_bytes == 0
            || config.max_sse_frame_bytes == 0
            || config.max_stream_bytes < config.max_sse_frame_bytes
            || config.max_episode_bytes == 0
            || config.max_idle_per_host == 0
        {
            return Err(WireError::InvalidClientLimits);
        }
        if config.allowed_models.len() != 1 || !config.allowed_models.contains(DEEPSEEK_MODEL_ID) {
            return Err(WireError::InvalidAllowedModel);
        }
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut bearer = zeroize::Zeroizing::new(String::with_capacity(7 + api_key.len()));
        bearer.push_str("Bearer ");
        bearer.push_str(api_key);
        let mut authorization =
            HeaderValue::from_str(&bearer).map_err(|_| WireError::InvalidAuthorization)?;
        authorization.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, authorization);
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .redirect(RedirectPolicy::none())
            .connect_timeout(config.connect_timeout)
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(config.max_idle_per_host)
            .tcp_keepalive(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            http,
            endpoint: format!("{}/chat/completions", config.api_base.trim_end_matches('/')),
            allowed_models: config.allowed_models,
            request_timeout: config.request_timeout,
            max_error_body_bytes: config.max_error_body_bytes,
            max_sse_frame_bytes: config.max_sse_frame_bytes,
            max_stream_bytes: config.max_stream_bytes,
            max_episode_bytes: config.max_episode_bytes,
        })
    }

    pub async fn complete_stream(
        &self,
        request: &ChatCompletionRequest,
        context: &EpisodeContext,
    ) -> Result<ProviderEpisodeV1, WireError> {
        if request.model != DEEPSEEK_MODEL_ID || !self.allowed_models.contains(&request.model) {
            return Err(WireError::UnknownModel(request.model.clone()));
        }
        request.validate()?;
        let request_hash = ContentHash::sha256(serde_jcs::to_vec(request)?);
        if std::env::var("KRW_DEBUG_PROVIDER").is_ok() {
            eprintln!(
                "[KRW_DEBUG_PROVIDER] request model={} thinking={:?} tool_choice={:?} stream={} tools={} messages={} request_body={}",
                request.model,
                request.thinking,
                request.tool_choice,
                request.stream,
                request.tools.len(),
                request.messages.len(),
                serde_json::to_string(request).unwrap_or_else(|_| "<serialize-failed>".into())
            );
        }
        let response = self
            .http
            .post(&self.endpoint)
            .timeout(self.request_timeout)
            .json(request)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let mut bytes = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                let remaining = self.max_error_body_bytes.saturating_sub(bytes.len());
                bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                if bytes.len() >= self.max_error_body_bytes {
                    break;
                }
            }
            if std::env::var("KRW_DEBUG_PROVIDER").is_ok() {
                eprintln!(
                    "[KRW_DEBUG_PROVIDER] error status={} body={}",
                    status.as_u16(),
                    String::from_utf8_lossy(&bytes)
                );
            }
            let provider_code = safe_api_error_code(&bytes);
            return Err(WireError::ApiStatus {
                status: status.as_u16(),
                body_prefix_hash: ContentHash::sha256(bytes),
                provider_code,
            });
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if !content_type.starts_with("text/event-stream") {
            return Err(WireError::UnexpectedContentType);
        }

        let mut sse = SseDecoder::new(self.max_sse_frame_bytes);
        let mut assembler = EpisodeAssembler::new(
            request_hash,
            request.model.clone(),
            context.clone(),
            self.max_episode_bytes,
        );
        let mut stream = response.bytes_stream();
        let mut streamed_bytes = 0_usize;
        let mut done = false;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            streamed_bytes = streamed_bytes
                .checked_add(chunk.len())
                .ok_or(WireError::StreamLimit(self.max_stream_bytes))?;
            if streamed_bytes > self.max_stream_bytes {
                return Err(WireError::StreamLimit(self.max_stream_bytes));
            }
            for event in sse.push(&chunk)? {
                if done {
                    return Err(WireError::DataAfterDone);
                }
                match event {
                    SseEvent::Data(data) => assembler.push_chunk(serde_json::from_str(&data)?)?,
                    SseEvent::Done => {
                        assembler.mark_done()?;
                        done = true;
                    }
                }
            }
            if done {
                break;
            }
        }
        sse.finish()?;
        assembler.finish()
    }
}

/// Extract only a tightly bounded machine error code from a provider error
/// response.  Human-readable error text is deliberately never retained: it
/// can contain echoed request material or account information.
fn safe_api_error_code(bytes: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let candidate = value
        .get("error")
        .and_then(|error| error.get("code"))
        .or_else(|| value.get("code"))
        .and_then(Value::as_str)?;
    (1..=64)
        .contains(&candidate.len())
        .then_some(candidate)
        .filter(|candidate| {
            candidate
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        })
        .map(ToOwned::to_owned)
}

pub struct EpisodeAssembler {
    request_hash: ContentHash,
    requested_model: String,
    context: EpisodeContext,
    observed_model: Option<String>,
    reasoning_content: String,
    content: String,
    reasoning_seen: bool,
    content_seen: bool,
    tool_calls: BTreeMap<u16, ToolCallBuilder>,
    finish_reason: Option<String>,
    usage: TokenUsage,
    done: bool,
    max_buffer_bytes: usize,
    buffered_bytes: usize,
}

impl std::fmt::Debug for EpisodeAssembler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EpisodeAssembler")
            .field("request_hash", &self.request_hash)
            .field("requested_model", &self.requested_model)
            .field("observed_model", &self.observed_model)
            .field("reasoning_len", &self.reasoning_content.len())
            .field("content_len", &self.content.len())
            .field("tool_call_count", &self.tool_calls.len())
            .field("finish_reason", &self.finish_reason)
            .field("done", &self.done)
            .field("max_buffer_bytes", &self.max_buffer_bytes)
            .field("buffered_bytes", &self.buffered_bytes)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Default)]
struct ToolCallBuilder {
    id: Option<String>,
    kind: Option<String>,
    name: Option<String>,
    arguments: String,
}

impl EpisodeAssembler {
    pub fn new(
        request_hash: ContentHash,
        requested_model: String,
        context: EpisodeContext,
        max_buffer_bytes: usize,
    ) -> Self {
        Self {
            request_hash,
            requested_model,
            context,
            observed_model: None,
            reasoning_content: String::new(),
            content: String::new(),
            reasoning_seen: false,
            content_seen: false,
            tool_calls: BTreeMap::new(),
            finish_reason: None,
            usage: TokenUsage::default(),
            done: false,
            max_buffer_bytes,
            buffered_bytes: 0,
        }
    }

    pub fn push_chunk(&mut self, chunk: ChatCompletionChunk) -> Result<(), WireError> {
        if let Some(observed) = &self.observed_model {
            if observed != &chunk.model {
                return Err(WireError::ModelChangedMidStream {
                    first: observed.clone(),
                    later: chunk.model,
                });
            }
        } else {
            self.observed_model = Some(chunk.model);
        }
        if let Some(usage) = chunk.usage {
            self.usage = usage;
        }
        for choice in chunk.choices {
            if choice.index != 0 {
                return Err(WireError::UnexpectedChoiceIndex(choice.index));
            }
            if let Some(reason) = choice.finish_reason {
                self.finish_reason = Some(reason);
            }
            if let Some(reasoning) = choice
                .delta
                .get("reasoning_content")
                .and_then(Value::as_str)
            {
                self.append_text(reasoning, true)?;
            }
            if let Some(content) = choice.delta.get("content").and_then(Value::as_str) {
                self.append_text(content, false)?;
            }
            if let Some(tool_calls) = choice.delta.get("tool_calls").and_then(Value::as_array) {
                for call in tool_calls {
                    self.push_tool_delta(call)?;
                }
            }
        }
        Ok(())
    }

    pub fn mark_done(&mut self) -> Result<(), WireError> {
        if self.done {
            return Err(WireError::DataAfterDone);
        }
        self.done = true;
        Ok(())
    }

    pub fn finish(self) -> Result<ProviderEpisodeV1, WireError> {
        if !self.done {
            return Err(WireError::MissingDoneEvent);
        }
        let observed_model = self.observed_model.ok_or(WireError::MissingObservedModel)?;
        let finish_reason = self.finish_reason.ok_or(WireError::MissingFinishReason)?;
        if self.tool_calls.len() > 32 {
            return Err(WireError::TooManyToolCalls(self.tool_calls.len()));
        }
        let tool_calls = self
            .tool_calls
            .into_values()
            .map(|call| {
                let kind = call.kind.ok_or(WireError::IncompleteToolCall("type"))?;
                if kind != "function" {
                    return Err(WireError::UnsupportedToolCallType(kind));
                }
                let arguments = call.arguments;
                let _: Value = serde_json::from_str(&arguments)?;
                Ok(ToolCall {
                    id: call.id.ok_or(WireError::IncompleteToolCall("id"))?,
                    kind: ToolCallKind::Function,
                    function: FunctionCall {
                        name: ProviderFunctionName::parse(
                            call.name.ok_or(WireError::IncompleteToolCall("name"))?,
                        )?,
                        arguments,
                    },
                })
            })
            .collect::<Result<Vec<_>, WireError>>()?;
        let mut episode = ProviderEpisodeV1 {
            schema_version: 1,
            request_hash: self.request_hash,
            requested_model: self.requested_model,
            observed_model,
            api_version: self.context.api_version,
            assistant: AssistantMessage {
                content: self.content_seen.then_some(self.content),
                reasoning_content: self.reasoning_seen.then_some(self.reasoning_content),
                tool_calls,
            },
            tool_results: Vec::new(),
            tool_schema_hash: self.context.tool_schema_hash,
            agent_image_hash: self.context.agent_image_hash,
            finish_reason,
            usage: self.usage,
            replay_hash: ContentHash::sha256("pending"),
        };
        episode.verify_model_identity()?;
        episode.verify_tool_call_structure()?;
        episode.replay_hash = episode.calculate_replay_hash()?;
        Ok(episode)
    }

    fn append_text(&mut self, text: &str, reasoning: bool) -> Result<(), WireError> {
        self.charge(text.len())?;
        if reasoning {
            self.reasoning_seen = true;
            self.reasoning_content.push_str(text);
        } else {
            self.content_seen = true;
            self.content.push_str(text);
        }
        Ok(())
    }

    fn push_tool_delta(&mut self, delta: &Value) -> Result<(), WireError> {
        let index = delta
            .get("index")
            .and_then(Value::as_u64)
            .ok_or(WireError::IncompleteToolCall("index"))?;
        let index = u16::try_from(index).map_err(|_| WireError::IncompleteToolCall("index"))?;
        let builder = self.tool_calls.entry(index).or_default();
        if let Some(id) = delta.get("id").and_then(Value::as_str) {
            merge_once(&mut builder.id, id, "id")?;
        }
        if let Some(kind) = delta.get("type").and_then(Value::as_str) {
            merge_once(&mut builder.kind, kind, "type")?;
        }
        if let Some(function) = delta.get("function") {
            if let Some(name) = function.get("name").and_then(Value::as_str) {
                merge_once(&mut builder.name, name, "name")?;
            }
            if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                self.charge(arguments.len())?;
                self.tool_calls
                    .get_mut(&index)
                    .expect("inserted above")
                    .arguments
                    .push_str(arguments);
            }
        }
        Ok(())
    }

    fn charge(&mut self, bytes: usize) -> Result<(), WireError> {
        self.buffered_bytes = self
            .buffered_bytes
            .checked_add(bytes)
            .ok_or(WireError::EpisodeBufferLimit(self.max_buffer_bytes))?;
        if self.buffered_bytes > self.max_buffer_bytes {
            return Err(WireError::EpisodeBufferLimit(self.max_buffer_bytes));
        }
        Ok(())
    }
}

fn merge_once(
    target: &mut Option<String>,
    value: &str,
    field: &'static str,
) -> Result<(), WireError> {
    match target {
        Some(existing) if existing != value => Err(WireError::ConflictingToolCallField(field)),
        Some(_) => Ok(()),
        None => {
            *target = Some(value.to_owned());
            Ok(())
        }
    }
}

fn scrub_json(value: &mut Value) {
    match value {
        Value::String(text) => text.zeroize(),
        Value::Array(values) => values.iter_mut().for_each(scrub_json),
        Value::Object(values) => values.values_mut().for_each(scrub_json),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDisposition {
    Never,
    FullJitterBeforeOutput,
    RestartIncompleteStep,
    ReadbackCommittedAction,
}

pub fn classify_retry(
    status: Option<u16>,
    emitted_output: bool,
    action_dispatched: bool,
) -> RetryDisposition {
    if action_dispatched {
        return RetryDisposition::ReadbackCommittedAction;
    }
    if emitted_output {
        return RetryDisposition::RestartIncompleteStep;
    }
    match status {
        Some(429 | 500 | 503) | None => RetryDisposition::FullJitterBeforeOutput,
        Some(_) => RetryDisposition::Never,
    }
}

#[derive(Debug, Error)]
pub enum WireError {
    #[error("provider function name violates the closed DeepSeek grammar")]
    InvalidProviderFunctionName,
    #[error("provider JSON Schema parameters must be an object")]
    InvalidJsonSchemaDocument,
    #[error("canonical JSON serialization was not UTF-8")]
    CanonicalJsonNotUtf8,
    #[error("provider tool result text is valid JSON but not RFC 8785 canonical JSON")]
    NonCanonicalJsonText,
    #[error("provider tool-call ID is empty, too long, or duplicated")]
    InvalidToolCallId,
    #[error("a provider user message was required at this position")]
    ExpectedUserMessage,
    #[error("provider system/user message content cannot be empty")]
    EmptyMessageContent,
    #[error("provider transcript has unresolved tool calls")]
    UnresolvedToolCalls,
    #[error("provider transcript includes unexpected tool result {0}")]
    UnexpectedToolResult(String),
    #[error("observed model {observed} differs from requested model {requested}")]
    ObservedModelMismatch { requested: String, observed: String },
    #[error("SSE buffer exceeded {0} bytes")]
    SseBufferLimit(usize),
    #[error("incomplete SSE frame at end of stream")]
    IncompleteSseFrame,
    #[error("invalid UTF-8 in complete SSE frame: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("HTTP transport failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("DeepSeek endpoint must use HTTPS")]
    InvalidEndpoint,
    #[error("invalid authorization header")]
    InvalidAuthorization,
    #[error("model is not in the exact allowlist: {0}")]
    UnknownModel(String),
    #[error("invalid DeepSeek request: {0}")]
    InvalidRequest(String),
    #[error("thinking=enabled requires reasoning_effort")]
    MissingReasoningEffort,
    #[error("thinking=disabled requires reasoning_effort to be omitted")]
    UnexpectedReasoningEffort,
    #[error("DeepSeek thinking mode does not support the tool_choice parameter")]
    ThinkingToolChoiceUnsupported,
    #[error("invalid DeepSeek client resource limits")]
    InvalidClientLimits,
    #[error("DeepSeek allowlist contains an unsupported model id")]
    InvalidAllowedModel,
    #[error("DeepSeek API returned status {status}; redacted body prefix hash {body_prefix_hash}")]
    ApiStatus {
        status: u16,
        body_prefix_hash: ContentHash,
        /// A bounded, syntax-checked provider error code if the body exposed
        /// one. Never contains the provider's human-readable message.
        provider_code: Option<String>,
    },
    #[error("stream model changed from {first} to {later}")]
    ModelChangedMidStream { first: String, later: String },
    #[error("DeepSeek response was not an SSE stream")]
    UnexpectedContentType,
    #[error("DeepSeek stream exceeded {0} bytes")]
    StreamLimit(usize),
    #[error("DeepSeek stream emitted data after its terminal event")]
    DataAfterDone,
    #[error("unexpected streamed choice index: {0}")]
    UnexpectedChoiceIndex(u16),
    #[error("stream ended without data: [DONE]")]
    MissingDoneEvent,
    #[error("stream contained no observed model")]
    MissingObservedModel,
    #[error("stream contained no finish reason")]
    MissingFinishReason,
    #[error("incomplete tool call field: {0}")]
    IncompleteToolCall(&'static str),
    #[error("unsupported tool call type: {0}")]
    UnsupportedToolCallType(String),
    #[error("provider emitted too many tool calls in one turn: {0}")]
    TooManyToolCalls(usize),
    #[error("conflicting streamed tool call field: {0}")]
    ConflictingToolCallField(&'static str),
    #[error("provider episode buffer exceeded {0} bytes")]
    EpisodeBufferLimit(usize),
    #[error("thinking tool call lacks non-null content or complete non-empty reasoning_content")]
    InvalidThinkingToolReplay,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_is_independent_of_single_byte_chunk_boundaries() {
        let input = "data: {\"delta\":\"한글\"}\r\n\r\n: keep-alive\n\ndata: [DONE]\n\n".as_bytes();
        let mut decoder = SseDecoder::new(4096);
        let mut events = Vec::new();
        for byte in input {
            events.extend(decoder.push(std::slice::from_ref(byte)).unwrap());
        }
        decoder.finish().unwrap();
        assert_eq!(
            events,
            vec![
                SseEvent::Data("{\"delta\":\"한글\"}".into()),
                SseEvent::Done
            ]
        );
    }

    #[test]
    fn exact_reasoning_and_tool_argument_strings_survive_replay() {
        let episode = ProviderEpisodeV1 {
            schema_version: 1,
            request_hash: ContentHash::sha256("request"),
            requested_model: "deepseek-v4-flash".into(),
            observed_model: "deepseek-v4-flash".into(),
            api_version: "chat-completions-v1".into(),
            assistant: AssistantMessage {
                content: Some(String::new()),
                reasoning_content: Some("private reasoning 한글".into()),
                tool_calls: vec![ToolCall {
                    id: "call-1".into(),
                    kind: ToolCallKind::Function,
                    function: FunctionCall {
                        name: ProviderFunctionName::parse("ontology_query_context").unwrap(),
                        arguments: "{\"search_plan\":{\"b\":2,\"a\":1}}".into(),
                    },
                }],
            },
            tool_results: vec![],
            tool_schema_hash: ContentHash::sha256("tools"),
            agent_image_hash: ContentHash::sha256("image"),
            finish_reason: "tool_calls".into(),
            usage: TokenUsage::default(),
            replay_hash: ContentHash::sha256("placeholder"),
        };
        let replay = episode.replay_messages();
        let ProviderMessage::Assistant {
            reasoning_content,
            tool_calls,
            ..
        } = &replay[0]
        else {
            panic!("replay must begin with the assistant message");
        };
        assert_eq!(reasoning_content.as_deref(), Some("private reasoning 한글"));
        assert_eq!(
            tool_calls[0].function.arguments,
            "{\"search_plan\":{\"b\":2,\"a\":1}}"
        );
        assert!(episode.verify_model_identity().is_ok());
    }

    #[test]
    fn equal_non_flash_episode_identity_is_still_rejected() {
        let mut episode = ProviderEpisodeV1 {
            schema_version: 1,
            request_hash: ContentHash::sha256("request"),
            requested_model: "forbidden-provider-model".into(),
            observed_model: "forbidden-provider-model".into(),
            api_version: "chat-completions-v1".into(),
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
        episode.replay_hash = episode.calculate_replay_hash().unwrap();
        assert!(matches!(
            episode.verify_model_identity(),
            Err(WireError::UnknownModel(model)) if model == "forbidden-provider-model"
        ));
    }

    #[test]
    fn thinking_tool_replay_rejects_missing_content_or_reasoning_but_direct_is_structurally_valid()
    {
        let episode =
            |content: Option<String>, reasoning_content: Option<String>| ProviderEpisodeV1 {
                schema_version: 1,
                request_hash: ContentHash::sha256("request"),
                requested_model: "deepseek-v4-flash".into(),
                observed_model: "deepseek-v4-flash".into(),
                api_version: "chat-completions-v1".into(),
                assistant: AssistantMessage {
                    content,
                    reasoning_content,
                    tool_calls: vec![ToolCall {
                        id: "call-1".into(),
                        kind: ToolCallKind::Function,
                        function: FunctionCall {
                            name: ProviderFunctionName::parse("ontology_query_context").unwrap(),
                            arguments: "{}".into(),
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

        for invalid in [
            episode(None, Some("reasoning".into())),
            episode(Some(String::new()), None),
            episode(Some(String::new()), Some(String::new())),
            episode(Some(String::new()), Some("   \n".into())),
        ] {
            assert!(matches!(
                invalid.verify_tool_call_requirements(true, true),
                Err(WireError::InvalidThinkingToolReplay)
            ));
        }
        assert!(
            episode(Some(String::new()), Some("complete reasoning".into()))
                .verify_tool_call_requirements(true, true)
                .is_ok()
        );
        let direct = episode(None, None);
        assert!(direct.verify_tool_call_structure().is_ok());
        assert!(direct.verify_tool_call_requirements(false, false).is_ok());
    }

    #[test]
    fn assembler_reconstructs_fragmented_reasoning_and_tool_arguments() {
        let context = EpisodeContext {
            tool_schema_hash: ContentHash::sha256("tools"),
            agent_image_hash: ContentHash::sha256("image"),
            api_version: "chat-completions-v1".into(),
        };
        let mut assembler = EpisodeAssembler::new(
            ContentHash::sha256("request"),
            "deepseek-v4-flash".into(),
            context,
            4096,
        );
        for delta in [
            serde_json::json!({
                "content": "",
                "reasoning_content": "reason ",
                "tool_calls": [{
                    "index": 0,
                    "id": "call-1",
                    "type": "function",
                    "function": {"name": "ontology_query_context", "arguments": "{\"search"}
                }]
            }),
            serde_json::json!({
                "reasoning_content": "continued",
                "tool_calls": [{
                    "index": 0,
                    "function": {"arguments": "_plan\":{}}"}
                }]
            }),
        ] {
            assembler
                .push_chunk(ChatCompletionChunk {
                    id: "response-1".into(),
                    model: "deepseek-v4-flash".into(),
                    choices: vec![ChunkChoice {
                        index: 0,
                        delta,
                        finish_reason: None,
                    }],
                    usage: None,
                    extra: BTreeMap::new(),
                })
                .unwrap();
        }
        assembler
            .push_chunk(ChatCompletionChunk {
                id: "response-1".into(),
                model: "deepseek-v4-flash".into(),
                choices: vec![ChunkChoice {
                    index: 0,
                    delta: serde_json::json!({}),
                    finish_reason: Some("tool_calls".into()),
                }],
                usage: Some(TokenUsage::default()),
                extra: BTreeMap::new(),
            })
            .unwrap();
        assembler.mark_done().unwrap();
        let episode = assembler.finish().unwrap();
        assert_eq!(
            episode.assistant.reasoning_content.as_deref(),
            Some("reason continued")
        );
        assert_eq!(
            episode.assistant.tool_calls[0].function.arguments,
            "{\"search_plan\":{}}"
        );
        assert_eq!(episode.finish_reason, "tool_calls");
    }

    #[test]
    fn retry_matrix_never_blindly_retries_after_dispatch() {
        assert_eq!(
            classify_retry(Some(503), false, false),
            RetryDisposition::FullJitterBeforeOutput
        );
        assert_eq!(
            classify_retry(None, true, false),
            RetryDisposition::RestartIncompleteStep
        );
        assert_eq!(
            classify_retry(Some(503), false, true),
            RetryDisposition::ReadbackCommittedAction
        );
        assert_eq!(
            classify_retry(Some(400), false, false),
            RetryDisposition::Never
        );
    }

    #[test]
    fn client_rejects_model_aliases_before_network_io() {
        let config = DeepSeekClientConfig::production(
            "https://api.deepseek.com",
            ["claude-sonnet".to_owned()],
        );
        assert!(matches!(
            DeepSeekClient::new(config, "not-a-live-key"),
            Err(WireError::InvalidAllowedModel)
        ));

        let config = DeepSeekClientConfig::production(
            "https://api.deepseek.com",
            [
                DEEPSEEK_MODEL_ID.to_owned(),
                "forbidden-provider-model".to_owned(),
            ],
        );
        assert!(matches!(
            DeepSeekClient::new(config, "not-a-live-key"),
            Err(WireError::InvalidAllowedModel)
        ));
    }

    fn request_with_thinking(
        thinking: ThinkingMode,
        reasoning_effort: Option<ReasoningEffort>,
    ) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "deepseek-v4-flash".into(),
            messages: vec![ProviderMessage::user("question")],
            tools: Vec::new(),
            stream: true,
            stream_options: StreamOptions {
                include_usage: true,
            },
            reasoning_effort,
            thinking: ThinkingConfig { kind: thinking },
            max_tokens: Some(512),
            user_id: None,
            tool_choice: None,
            response_format: None,
        }
    }

    #[test]
    fn thinking_profiles_serialize_to_the_exact_provider_contract() {
        let enabled = request_with_thinking(ThinkingMode::Enabled, Some(ReasoningEffort::Max));
        enabled.validate().unwrap();
        let enabled_json = serde_json::to_value(&enabled).unwrap();
        assert_eq!(
            enabled_json["thinking"],
            serde_json::json!({"type": "enabled"})
        );
        assert_eq!(enabled_json["reasoning_effort"], "max");

        let disabled = request_with_thinking(ThinkingMode::Disabled, None);
        disabled.validate().unwrap();
        let disabled_json = serde_json::to_value(&disabled).unwrap();
        assert_eq!(
            disabled_json["thinking"],
            serde_json::json!({"type": "disabled"})
        );
        assert!(disabled_json.get("reasoning_effort").is_none());
    }

    #[test]
    fn typed_output_lanes_require_one_unambiguous_provider_channel() {
        let mut request = request_with_thinking(ThinkingMode::Disabled, None);
        request.tool_choice = Some(ToolChoice::Required);
        assert!(matches!(
            request.validate(),
            Err(WireError::InvalidRequest(message)) if message.contains("tool_choice=required")
        ));

        request.tools = vec![
            ProviderToolDefinition::function(
                "typed_transition",
                "choose a typed transition",
                serde_json::json!({
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["event"],
                    "properties": {"event": {"type": "string"}}
                }),
            )
            .unwrap(),
        ];
        request.validate().unwrap();
        assert_eq!(
            serde_json::to_value(&request).unwrap()["tool_choice"],
            serde_json::json!("required")
        );

        request.response_format = Some(ResponseFormat {
            kind: ResponseFormatKind::JsonObject,
        });
        assert!(matches!(
            request.validate(),
            Err(WireError::InvalidRequest(message)) if message.contains("response_format")
        ));
    }

    #[test]
    fn thinking_profile_validation_rejects_ambiguous_combinations() {
        let missing_effort = request_with_thinking(ThinkingMode::Enabled, None);
        assert!(matches!(
            missing_effort.validate(),
            Err(WireError::MissingReasoningEffort)
        ));

        let unexpected_effort =
            request_with_thinking(ThinkingMode::Disabled, Some(ReasoningEffort::High));
        assert!(matches!(
            unexpected_effort.validate(),
            Err(WireError::UnexpectedReasoningEffort)
        ));

        let mut tool_choice_in_thinking =
            request_with_thinking(ThinkingMode::Enabled, Some(ReasoningEffort::High));
        tool_choice_in_thinking.tools = vec![
            ProviderToolDefinition::function(
                "typed_transition",
                "choose a typed transition",
                serde_json::json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {},
                    "required": []
                }),
            )
            .unwrap(),
        ];
        tool_choice_in_thinking.tool_choice = Some(ToolChoice::Required);
        assert!(matches!(
            tool_choice_in_thinking.validate(),
            Err(WireError::ThinkingToolChoiceUnsupported)
        ));
    }

    #[test]
    fn typed_tool_result_is_canonical_text_and_cannot_be_an_object() {
        let result = ToolResultMessage::from_value(
            "call-1",
            &serde_json::json!({"private": "typed capability output"}),
        )
        .unwrap();
        let ProviderMessage::Tool { content, .. } = result.into_provider_message() else {
            panic!("tool result must build a tool message");
        };
        assert_eq!(
            content.as_str(),
            "{\"private\":\"typed capability output\"}"
        );
        assert!(matches!(
            CanonicalJsonText::parse("{\"b\":2,\"a\":1}"),
            Err(WireError::NonCanonicalJsonText)
        ));
    }

    #[test]
    fn tool_result_wire_message_serializes_text_content() {
        let result =
            ToolResultMessage::from_value("call-1", &serde_json::json!({"status":"ok"})).unwrap();
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["content"], "{\"status\":\"ok\"}");
    }

    #[test]
    fn sensitive_request_and_tool_result_debug_are_redacted() {
        let secret = "private user evidence";
        let request = ChatCompletionRequest {
            model: "deepseek-v4-flash".into(),
            messages: vec![ProviderMessage::user(secret)],
            tools: vec![
                ProviderToolDefinition::function(
                    "private_tool",
                    secret,
                    serde_json::json!({"type": "object"}),
                )
                .unwrap(),
            ],
            stream: true,
            stream_options: StreamOptions {
                include_usage: true,
            },
            reasoning_effort: Some(ReasoningEffort::High),
            thinking: ThinkingConfig {
                kind: ThinkingMode::Enabled,
            },
            max_tokens: Some(512),
            user_id: Some(secret.into()),
            tool_choice: None,
            response_format: Some(ResponseFormat {
                kind: ResponseFormatKind::JsonObject,
            }),
        };
        let result =
            ToolResultMessage::from_value("call-secret", &serde_json::json!({"private": secret}))
                .unwrap();
        assert!(!format!("{request:?}").contains(secret));
        assert!(!format!("{result:?}").contains(secret));
    }

    #[test]
    fn terminal_stream_event_is_unique() {
        let context = EpisodeContext {
            tool_schema_hash: ContentHash::sha256("tools"),
            agent_image_hash: ContentHash::sha256("image"),
            api_version: "chat-completions-v1".into(),
        };
        let mut assembler = EpisodeAssembler::new(
            ContentHash::sha256("request"),
            "deepseek-v4-flash".into(),
            context,
            1024,
        );
        assembler.mark_done().unwrap();
        assert!(matches!(
            assembler.mark_done(),
            Err(WireError::DataAfterDone)
        ));
    }
}
