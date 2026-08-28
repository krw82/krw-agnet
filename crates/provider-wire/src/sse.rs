//! Anthropic Messages API streaming SSE decoder.
//!
//! Anthropic's streaming format uses named `event:` + `data:` pairs separated
//! by blank lines, terminated by a `message_stop` event (not `OpenAI`'s
//! `[DONE]` sentinel). This decoder is chunk-boundary independent: callers can
//! feed arbitrarily split byte slices and the decoder accumulates incomplete
//! frames until a `\n\n` (or `\r\n\r\n`) delimiter is observed.
//!
//! Wire layout:
//!
//! ```text
//! event: message_start
//! data: {"type":"message_start","message":{...,"usage":{"input_tokens":25}}}
//!
//! event: content_block_delta
//! data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}
//!
//! ...
//!
//! event: message_stop
//! data: {"type":"message_stop"}
//! ```
//!
//! The decoder parses the `data:` JSON, dispatches on the `event:` type when
//! present (otherwise falls back to the `"type"` field inside `data:`), and
//! emits strongly-typed [`AnthropicSseEvent`] values.

use serde::Deserialize;
use serde_json::Value;

use crate::WireError;

// ---------------------------------------------------------------------------
// Typed SSE event payload.
// ---------------------------------------------------------------------------

/// A parsed Anthropic streaming event. Mirrors the named `event:` line plus
/// the discriminant field inside the `data:` JSON.
#[derive(Clone, PartialEq)]
pub enum AnthropicSseEvent {
    /// `event: message_start`. `input_tokens` come from
    /// `message.usage.input_tokens`.
    MessageStart { model: String, input_tokens: u32 },
    /// `event: content_block_start`. The block payload is dispatched on
    /// `content_block.type` (`text`, `tool_use`, `thinking`).
    ContentBlockStart {
        index: u32,
        block: ContentBlockStart,
    },
    /// `event: content_block_delta`. The delta payload is dispatched on
    /// `delta.type` (`text_delta`, `input_json_delta`, `thinking_delta`,
    /// `signature_delta`).
    ContentBlockDelta {
        index: u32,
        delta: ContentBlockDelta,
    },
    /// `event: content_block_stop`.
    ContentBlockStop { index: u32 },
    /// `event: message_delta`. `stop_reason` lives in `delta.stop_reason`;
    /// `output_tokens` (cumulative) lives in `usage.output_tokens`. Z.AI's
    /// Anthropic-compatible endpoint reports `message_start.usage.input_tokens: 0`
    /// and delivers the real input count only in this final frame, so
    /// `input_tokens` here is authoritative when present.
    MessageDelta {
        stop_reason: Option<String>,
        output_tokens: Option<u32>,
        input_tokens: Option<u32>,
    },
    /// `event: message_stop`. Stream terminator.
    MessageStop,
    /// `event: ping`. Keepalive; no payload.
    Ping,
    /// `event: error` (or an inline `{"type":"error",...}` data frame).
    Error { error_type: String, message: String },
}

impl std::fmt::Debug for AnthropicSseEvent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MessageStart {
                model,
                input_tokens,
            } => formatter
                .debug_struct("AnthropicSseEvent::MessageStart")
                .field("model", model)
                .field("input_tokens", input_tokens)
                .finish(),
            Self::ContentBlockStart { index, block } => formatter
                .debug_struct("AnthropicSseEvent::ContentBlockStart")
                .field("index", index)
                .field("block", block)
                .finish(),
            Self::ContentBlockDelta { index, delta } => formatter
                .debug_struct("AnthropicSseEvent::ContentBlockDelta")
                .field("index", index)
                .field("delta_len", &delta.text_len())
                .field("delta", &"[REDACTED]")
                .finish(),
            Self::ContentBlockStop { index } => formatter
                .debug_struct("AnthropicSseEvent::ContentBlockStop")
                .field("index", index)
                .finish(),
            Self::MessageDelta {
                stop_reason,
                output_tokens,
                input_tokens,
            } => formatter
                .debug_struct("AnthropicSseEvent::MessageDelta")
                .field("stop_reason", stop_reason)
                .field("output_tokens", output_tokens)
                .field("input_tokens", input_tokens)
                .finish(),
            Self::MessageStop => formatter.write_str("AnthropicSseEvent::MessageStop"),
            Self::Ping => formatter.write_str("AnthropicSseEvent::Ping"),
            Self::Error {
                error_type,
                message,
                ..
            } => formatter
                .debug_struct("AnthropicSseEvent::Error")
                .field("error_type", error_type)
                .field("message_len", &message.len())
                .field(
                    "message_hash",
                    &krw_agent_protocol::ContentHash::sha256(message),
                )
                .finish(),
        }
    }
}

/// `content_block` payload from a `content_block_start` frame. Dispatched on
/// `"type"`; the remaining variants (`text_delta`, etc.) live in
/// [`ContentBlockDelta`].
#[derive(Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlockStart {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
    Thinking {
        thinking: String,
        #[serde(default)]
        signature: String,
    },
}

impl std::fmt::Debug for ContentBlockStart {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text { text } => formatter
                .debug_struct("ContentBlockStart::Text")
                .field("text_len", &text.len())
                .field("text", &"[REDACTED]")
                .finish(),
            Self::ToolUse { id, name, .. } => formatter
                .debug_struct("ContentBlockStart::ToolUse")
                .field("id_hash", &krw_agent_protocol::ContentHash::sha256(id))
                .field("name", name)
                .field("input", &"[REDACTED]")
                .finish(),
            Self::Thinking { thinking, .. } => formatter
                .debug_struct("ContentBlockStart::Thinking")
                .field("thinking_len", &thinking.len())
                .field("thinking", &"[REDACTED]")
                .finish(),
        }
    }
}

/// `delta` payload from a `content_block_delta` frame. Dispatched on `"type"`.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlockDelta {
    TextDelta { text: String },
    InputJsonDelta { partial_json: String },
    ThinkingDelta { thinking: String },
    SignatureDelta { signature: String },
}

impl std::fmt::Debug for ContentBlockDelta {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TextDelta { text } => formatter
                .debug_struct("ContentBlockDelta::TextDelta")
                .field("text_len", &text.len())
                .field("text", &"[REDACTED]")
                .finish(),
            Self::InputJsonDelta { partial_json } => formatter
                .debug_struct("ContentBlockDelta::InputJsonDelta")
                .field("partial_json_len", &partial_json.len())
                .field("partial_json", &"[REDACTED]")
                .finish(),
            Self::ThinkingDelta { thinking } => formatter
                .debug_struct("ContentBlockDelta::ThinkingDelta")
                .field("thinking_len", &thinking.len())
                .field("thinking", &"[REDACTED]")
                .finish(),
            Self::SignatureDelta { signature } => formatter
                .debug_struct("ContentBlockDelta::SignatureDelta")
                .field("signature_len", &signature.len())
                .field("signature", &"[REDACTED]")
                .finish(),
        }
    }
}

impl ContentBlockDelta {
    /// Length of the carry-over text (for redacted Debug output).
    fn text_len(&self) -> usize {
        match self {
            Self::TextDelta { text } => text.len(),
            Self::InputJsonDelta { partial_json } => partial_json.len(),
            Self::ThinkingDelta { thinking } => thinking.len(),
            Self::SignatureDelta { signature } => signature.len(),
        }
    }
}

// ---------------------------------------------------------------------------
// Internal serde helpers for the JSON envelopes.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct MessageStartEnvelope {
    message: MessageStartInner,
}

#[derive(Deserialize)]
struct MessageStartInner {
    model: String,
    #[serde(default)]
    usage: UsageInput,
}

#[derive(Default, Deserialize)]
struct UsageInput {
    #[serde(default)]
    input_tokens: u32,
}

#[derive(Deserialize)]
struct ContentBlockStartEnvelope {
    index: u32,
    content_block: ContentBlockStart,
}

#[derive(Deserialize)]
struct ContentBlockDeltaEnvelope {
    index: u32,
    delta: ContentBlockDelta,
}

#[derive(Deserialize)]
struct ContentBlockStopEnvelope {
    index: u32,
}

#[derive(Deserialize)]
struct MessageDeltaEnvelope {
    #[serde(default)]
    delta: MessageDeltaInner,
    #[serde(default)]
    usage: MessageDeltaUsage,
}

#[derive(Default, Deserialize)]
struct MessageDeltaInner {
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Default, Deserialize)]
struct MessageDeltaUsage {
    #[serde(default)]
    output_tokens: Option<u32>,
    #[serde(default)]
    input_tokens: Option<u32>,
}

// ---------------------------------------------------------------------------
// Decoder.
// ---------------------------------------------------------------------------

/// Chunk-boundary-independent SSE decoder for the Anthropic streaming wire
/// format. Bytes accumulate in `buffer`; complete `\n\n`-delimited frames are
/// extracted and parsed on each [`SseDecoder::push`].
#[derive(Debug)]
pub struct AnthropicSseDecoder {
    buffer: Vec<u8>,
    max_buffer_bytes: usize,
}

impl AnthropicSseDecoder {
    /// Create a decoder. `max_buffer_bytes` bounds the unprocessed byte
    /// backlog and guards against a runaway upstream.
    pub fn new(max_buffer_bytes: usize) -> Self {
        Self {
            buffer: Vec::new(),
            max_buffer_bytes,
        }
    }

    /// Feed a raw byte chunk. Returns the fully-parsed events that became
    /// complete as a result. Partial frames remain buffered until the next
    /// call (or [`AnthropicSseDecoder::finish`]).
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<AnthropicSseEvent>, WireError> {
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

    /// Drain the decoder at end-of-stream. Returns an error if a non-whitespace
    /// frame remains unparsed (the stream ended mid-frame).
    pub fn finish(self) -> Result<(), WireError> {
        if self.buffer.iter().all(u8::is_ascii_whitespace) {
            Ok(())
        } else {
            Err(WireError::IncompleteSseFrame)
        }
    }
}

/// Locate the next frame delimiter. Returns `(end_index, delimiter_len)`.
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

/// Parse one complete frame (no trailing delimiter) into a typed event.
/// Returns `Ok(None)` for ignorable frames (comments, empty data).
fn parse_frame(frame: &[u8]) -> Result<Option<AnthropicSseEvent>, WireError> {
    let text = std::str::from_utf8(frame)
        .map_err(|err| WireError::SseParseError(format!("frame is not UTF-8: {err}")))?;

    let mut event_type: Option<&str> = None;
    let mut data_lines: Vec<&str> = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            let value = value.strip_prefix(' ').unwrap_or(value);
            event_type = Some(value);
        } else if let Some(value) = line.strip_prefix("data:") {
            let value = value.strip_prefix(' ').unwrap_or(value);
            data_lines.push(value);
        }
        // Anthropic does not emit `id:` or `retry:` lines; any other field is
        // ignored to stay permissive about upstream proxies.
    }

    if data_lines.is_empty() {
        return Ok(None);
    }
    let data = data_lines.join("\n");

    // The event type comes from the `event:` line; fall back to the `type`
    // field inside the data JSON (Anthropic always emits both, but a robust
    // parser should not require the named-event line).
    let owned_data_value: Option<Value> = if event_type.is_some() {
        None
    } else {
        Some(
            serde_json::from_str::<Value>(&data)
                .map_err(|err| WireError::SseParseError(format!("data is not JSON: {err}")))?,
        )
    };
    let discriminator: &str = match event_type {
        Some(name) => name,
        None => match owned_data_value
            .as_ref()
            .and_then(|value| value.get("type"))
            .and_then(Value::as_str)
        {
            Some(name) => name,
            None => return Ok(None),
        },
    };

    parse_event(discriminator, &data)
}

fn parse_event(discriminator: &str, data: &str) -> Result<Option<AnthropicSseEvent>, WireError> {
    match discriminator {
        "message_start" => {
            let envelope: MessageStartEnvelope = serde_json::from_str(data).map_err(|err| {
                WireError::SseParseError(format!("message_start body invalid: {err}"))
            })?;
            Ok(Some(AnthropicSseEvent::MessageStart {
                model: envelope.message.model,
                input_tokens: envelope.message.usage.input_tokens,
            }))
        }
        "content_block_start" => {
            let envelope: ContentBlockStartEnvelope =
                serde_json::from_str(data).map_err(|err| {
                    WireError::SseParseError(format!("content_block_start body invalid: {err}"))
                })?;
            Ok(Some(AnthropicSseEvent::ContentBlockStart {
                index: envelope.index,
                block: envelope.content_block,
            }))
        }
        "content_block_delta" => {
            let envelope: ContentBlockDeltaEnvelope =
                serde_json::from_str(data).map_err(|err| {
                    WireError::SseParseError(format!("content_block_delta body invalid: {err}"))
                })?;
            Ok(Some(AnthropicSseEvent::ContentBlockDelta {
                index: envelope.index,
                delta: envelope.delta,
            }))
        }
        "content_block_stop" => {
            let envelope: ContentBlockStopEnvelope = serde_json::from_str(data).map_err(|err| {
                WireError::SseParseError(format!("content_block_stop body invalid: {err}"))
            })?;
            Ok(Some(AnthropicSseEvent::ContentBlockStop {
                index: envelope.index,
            }))
        }
        "message_delta" => {
            let envelope: MessageDeltaEnvelope = serde_json::from_str(data).map_err(|err| {
                WireError::SseParseError(format!("message_delta body invalid: {err}"))
            })?;
            Ok(Some(AnthropicSseEvent::MessageDelta {
                stop_reason: envelope.delta.stop_reason,
                output_tokens: envelope.usage.output_tokens,
                input_tokens: envelope.usage.input_tokens,
            }))
        }
        "message_stop" => {
            // Body should be `{"type":"message_stop"}` but we do not require
            // any fields; just consume and ignore.
            Ok(Some(AnthropicSseEvent::MessageStop))
        }
        "ping" => Ok(Some(AnthropicSseEvent::Ping)),
        "error" => {
            let value: Value = serde_json::from_str(data)
                .map_err(|err| WireError::SseParseError(format!("error body invalid: {err}")))?;
            // Tolerate both nested `error:{type,message}` and flat shapes.
            let (error_type, message) =
                if let Some(error_obj) = value.get("error").filter(|v| v.is_object()) {
                    let t = error_obj
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("error")
                        .to_string();
                    let m = error_obj
                        .get("message")
                        .and_then(Value::as_str)
                        .or_else(|| value.get("message").and_then(Value::as_str))
                        .unwrap_or("")
                        .to_string();
                    (t, m)
                } else {
                    let t = value
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("error")
                        .to_string();
                    let m = value
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    (t, m)
                };
            Ok(Some(AnthropicSseEvent::Error {
                error_type,
                message,
            }))
        }
        // Unknown event types are ignored rather than erroring, so forward
        // compatibility with new Anthropic events does not require a release.
        _ => {
            // Unknown event types are tolerated when their payload is valid
            // JSON, so we validate and ignore the frame.
            let _ = serde_json::from_str::<Value>(data).map_err(|err| {
                WireError::SseParseError(format!("unknown event body invalid: {err}"))
            })?;
            Ok(None)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_error_debug_redacts_message_content() {
        let event = AnthropicSseEvent::Error {
            error_type: "invalid_request".into(),
            message: "private provider message".into(),
        };

        let rendered = format!("{event:?}");
        assert!(rendered.contains("message_hash"));
        assert!(!rendered.contains("private provider message"));
    }

    /// Concatenate a list of byte slices and feed them through the decoder.
    fn decode_chunks(
        chunks: &[&[u8]],
        max_buffer_bytes: usize,
    ) -> Result<(Vec<AnthropicSseEvent>, Result<(), WireError>), WireError> {
        let mut decoder = AnthropicSseDecoder::new(max_buffer_bytes);
        let mut events = Vec::new();
        for chunk in chunks {
            events.extend(decoder.push(chunk)?);
        }
        Ok((events, decoder.finish()))
    }

    fn full_stream() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"event: message_start\n");
        bytes.extend_from_slice(b"data: {\"type\":\"message_start\",\"message\":{\"model\":\"glm-5.3-flash\",\"usage\":{\"input_tokens\":25}}}\n\n");
        bytes.extend_from_slice(b"event: content_block_start\n");
        bytes.extend_from_slice(b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n");
        bytes.extend_from_slice(b"event: content_block_delta\n");
        bytes.extend_from_slice(b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\n");
        bytes.extend_from_slice(b"event: content_block_stop\n");
        bytes.extend_from_slice(b"data: {\"type\":\"content_block_stop\",\"index\":0}\n\n");
        bytes.extend_from_slice(b"event: message_delta\n");
        bytes.extend_from_slice(b"data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":15}}\n\n");
        bytes.extend_from_slice(b"event: message_stop\n");
        bytes.extend_from_slice(b"data: {\"type\":\"message_stop\"}\n\n");
        bytes
    }

    #[test]
    fn parses_full_stream_in_one_chunk() {
        let stream = full_stream();
        let (events, finish) =
            decode_chunks(&[stream.as_slice()], 64 * 1024).expect("decode succeeds");
        assert!(finish.is_ok(), "stream ended cleanly: {finish:?}");
        assert_eq!(events.len(), 6);

        assert!(matches!(
            &events[0],
            AnthropicSseEvent::MessageStart {
                model,
                input_tokens: 25,
            } if model == "glm-5.3-flash"
        ));
        assert!(matches!(
            &events[1],
            AnthropicSseEvent::ContentBlockStart {
                index: 0,
                block: ContentBlockStart::Text { text }
            } if text.is_empty()
        ));
        assert!(matches!(
            &events[2],
            AnthropicSseEvent::ContentBlockDelta {
                index: 0,
                delta: ContentBlockDelta::TextDelta { text }
            } if text == "hello"
        ));
        assert!(matches!(
            &events[3],
            AnthropicSseEvent::ContentBlockStop { index: 0 }
        ));
        assert!(matches!(
            &events[4],
            AnthropicSseEvent::MessageDelta {
                stop_reason: Some(reason),
                output_tokens: Some(15),
                ..
            } if reason == "end_turn"
        ));
        assert!(matches!(&events[5], AnthropicSseEvent::MessageStop));
    }

    #[test]
    fn parses_stream_split_at_every_byte_boundary() {
        let stream = full_stream();
        // Feed one byte at a time. Every push after the first should still
        // eventually produce the full event set in order.
        let mut decoder = AnthropicSseDecoder::new(64 * 1024);
        let mut events = Vec::new();
        for byte in &stream {
            let produced = decoder.push(std::slice::from_ref(byte)).expect("push ok");
            events.extend(produced);
        }
        decoder.finish().expect("clean finish");
        assert_eq!(events.len(), 6);
        assert!(matches!(
            events.first(),
            Some(AnthropicSseEvent::MessageStart {
                input_tokens: 25,
                ..
            })
        ));
        assert!(matches!(
            events.last(),
            Some(AnthropicSseEvent::MessageStop)
        ));
    }

    #[test]
    fn splits_frame_arbitrarily_mid_payload() {
        let stream = full_stream();
        // Cut inside the second event's JSON, then again mid-delimiter.
        let cut_a = 80;
        let cut_b = 220;
        let part1 = &stream[..cut_a];
        let part2 = &stream[cut_a..cut_b];
        let part3 = &stream[cut_b..];
        let (events, finish) = decode_chunks(&[part1, part2, part3], 64 * 1024).expect("decode ok");
        assert!(finish.is_ok());
        assert_eq!(events.len(), 6);
    }

    #[test]
    fn handles_crlf_line_endings() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"event: ping\r\n");
        bytes.extend_from_slice(b"data: {\"type\":\"ping\"}\r\n\r\n");
        bytes.extend_from_slice(b"event: message_stop\r\n");
        bytes.extend_from_slice(b"data: {\"type\":\"message_stop\"}\r\n\r\n");
        let (events, finish) = decode_chunks(&[bytes.as_slice()], 64 * 1024).expect("decode ok");
        assert!(finish.is_ok());
        assert!(matches!(
            &events[..],
            [AnthropicSseEvent::Ping, AnthropicSseEvent::MessageStop]
        ));
    }

    #[test]
    fn handles_ping_event() {
        let frame = b"event: ping\ndata: {\"type\":\"ping\"}\n\n";
        let (events, _) = decode_chunks(&[frame.as_slice()], 64 * 1024).expect("decode ok");
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], AnthropicSseEvent::Ping));
    }

    #[test]
    fn handles_error_event_nested_shape() {
        // Standard Anthropic error: data envelope wraps an `error` object.
        let frame = b"event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
        let (events, _) = decode_chunks(&[frame.as_slice()], 64 * 1024).expect("decode ok");
        assert_eq!(events.len(), 1);
        match &events[0] {
            AnthropicSseEvent::Error {
                error_type,
                message,
            } => {
                assert_eq!(error_type, "overloaded_error");
                assert_eq!(message, "Overloaded");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn handles_error_event_flat_shape() {
        let frame =
            b"event: error\ndata: {\"type\":\"api_error\",\"message\":\"something broke\"}\n\n";
        let (events, _) = decode_chunks(&[frame.as_slice()], 64 * 1024).expect("decode ok");
        assert_eq!(events.len(), 1);
        match &events[0] {
            AnthropicSseEvent::Error {
                error_type,
                message,
            } => {
                assert_eq!(error_type, "api_error");
                assert_eq!(message, "something broke");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parses_tool_use_block_start_and_input_json_delta() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(
            b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_01\",\"name\":\"krw_query\",\"input\":{}}}\n\n",
        );
        bytes.extend_from_slice(
            b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"q\\\":\\\"hi\\\"}\"}}\n\n",
        );
        bytes.extend_from_slice(
            b"event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        );
        let (events, _) = decode_chunks(&[bytes.as_slice()], 64 * 1024).expect("decode ok");
        assert_eq!(events.len(), 3);
        match &events[0] {
            AnthropicSseEvent::ContentBlockStart {
                index: 1,
                block: ContentBlockStart::ToolUse { id, name, input },
            } => {
                assert_eq!(id, "toolu_01");
                assert_eq!(name, "krw_query");
                assert_eq!(input, &serde_json::json!({}));
            }
            other => panic!("expected tool_use start, got {other:?}"),
        }
        match &events[1] {
            AnthropicSseEvent::ContentBlockDelta {
                index: 1,
                delta: ContentBlockDelta::InputJsonDelta { partial_json },
            } => {
                assert_eq!(partial_json, "{\"q\":\"hi\"}");
            }
            other => panic!("expected input_json_delta, got {other:?}"),
        }
        assert!(matches!(
            &events[2],
            AnthropicSseEvent::ContentBlockStop { index: 1 }
        ));
    }

    #[test]
    fn parses_thinking_block_and_signature_delta() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(
            b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"\"}}\n\n",
        );
        bytes.extend_from_slice(
            b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"hm\"}}\n\n",
        );
        bytes.extend_from_slice(
            b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig\"}}\n\n",
        );
        let (events, _) = decode_chunks(&[bytes.as_slice()], 64 * 1024).expect("decode ok");
        assert_eq!(events.len(), 3);
        assert!(matches!(
            &events[0],
            AnthropicSseEvent::ContentBlockStart {
                index: 0,
                block: ContentBlockStart::Thinking { .. }
            }
        ));
        assert!(matches!(
            &events[1],
            AnthropicSseEvent::ContentBlockDelta {
                index: 0,
                delta: ContentBlockDelta::ThinkingDelta { thinking }
            } if thinking == "hm"
        ));
        assert!(matches!(
            &events[2],
            AnthropicSseEvent::ContentBlockDelta {
                index: 0,
                delta: ContentBlockDelta::SignatureDelta { signature }
            } if signature == "sig"
        ));
    }

    #[test]
    fn message_delta_without_usage_is_tolerated() {
        let frame =
            b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"}}\n\n";
        let (events, _) = decode_chunks(&[frame.as_slice()], 64 * 1024).expect("decode ok");
        assert_eq!(events.len(), 1);
        match &events[0] {
            AnthropicSseEvent::MessageDelta {
                stop_reason,
                output_tokens,
                ..
            } => {
                assert_eq!(stop_reason.as_deref(), Some("max_tokens"));
                assert!(output_tokens.is_none());
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn buffer_limit_is_enforced() {
        // Use a tiny cap; the first push exceeds it.
        let frame = b"event: ping\ndata: {\"type\":\"ping\"}\n\n";
        let mut decoder = AnthropicSseDecoder::new(4);
        let err = decoder.push(frame).expect_err("must hit buffer limit");
        assert!(matches!(err, WireError::SseBufferLimit(4)));
    }

    #[test]
    fn finish_rejects_partial_frame() {
        let mut decoder = AnthropicSseDecoder::new(64 * 1024);
        decoder
            .push(b"event: ping\ndata: {\"type\":\"ping\"}") // no trailing delimiter
            .expect("push ok");
        let err = decoder.finish().expect_err("must be incomplete");
        assert!(matches!(err, WireError::IncompleteSseFrame));
    }

    #[test]
    fn finish_accepts_trailing_whitespace() {
        let mut decoder = AnthropicSseDecoder::new(64 * 1024);
        decoder
            .push(b"event: ping\ndata: {\"type\":\"ping\"}\n\n")
            .expect("push ok");
        decoder
            .push(b"\n  \n") // stray whitespace after final delimiter
            .expect("push ok");
        assert!(decoder.finish().is_ok());
    }

    #[test]
    fn ignores_comment_and_unknown_field_lines() {
        let frame = b": this is a comment\nevent: ping\nid: 42\ndata: {\"type\":\"ping\"}\n\n";
        let (events, _) = decode_chunks(&[frame.as_slice()], 64 * 1024).expect("decode ok");
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], AnthropicSseEvent::Ping));
    }

    #[test]
    fn joins_multi_line_data_payload() {
        // SSE spec: consecutive `data:` lines are joined with `\n`. Anthropic
        // does not normally split, but be robust.
        let frame = b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\ndata: \"delta\":{\"type\":\"text_delta\",\"text\":\"x\"}}\n\n";
        let (events, _) = decode_chunks(&[frame.as_slice()], 64 * 1024).expect("decode ok");
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            AnthropicSseEvent::ContentBlockDelta {
                index: 0,
                delta: ContentBlockDelta::TextDelta { text }
            } if text == "x"
        ));
    }

    #[test]
    fn falls_back_to_type_field_when_event_line_missing() {
        // Some proxies strip the `event:` line. The decoder must still recover
        // via the inner `type` discriminant.
        let frame = b"data: {\"type\":\"message_stop\"}\n\n";
        let (events, _) = decode_chunks(&[frame.as_slice()], 64 * 1024).expect("decode ok");
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], AnthropicSseEvent::MessageStop));
    }

    #[test]
    fn malformed_data_json_yields_sse_parse_error() {
        let frame = b"event: message_start\ndata: {not valid json}\n\n";
        let err = decode_chunks(&[frame.as_slice()], 64 * 1024).expect_err("must fail");
        assert!(matches!(err, WireError::SseParseError(_)));
    }

    #[test]
    fn unknown_event_type_is_ignored() {
        let frame = b"event: future_event\ndata: {\"type\":\"future_event\"}\n\n";
        let (events, _) = decode_chunks(&[frame.as_slice()], 64 * 1024).expect("decode ok");
        assert!(events.is_empty());
    }
}
