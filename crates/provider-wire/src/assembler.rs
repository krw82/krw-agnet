//! `EpisodeAssembler`: consume [`AnthropicSseEvent`]s and produce a
//! [`ProviderEpisodeV1`].
//!
//! Anthropic streams content as a sequence of indexed content blocks:
//!
//! ```text
//! content_block_start (index=0, type=thinking|text|tool_use)
//! content_block_delta (index=0, text_delta|thinking_delta|signature_delta|input_json_delta)
//! content_block_stop  (index=0)
//! ```
//!
//! The assembler accumulates per-index state, then folds the blocks into the
//! internal flat [`AssistantMessage`] shape on [`EpisodeAssembler::finish`]:
//!
//! - `thinking` blocks  -> `reasoning_content` (concatenated in index order)
//! - `text` blocks      -> `content`           (concatenated in index order)
//! - `tool_use` blocks  -> `tool_calls[i]`     with `input` parsed as canonical JSON
//!
//! `stop_reason` is mapped to the run-engine-compatible `finish_reason`:
//!
//! | Anthropic `stop_reason` | Internal `finish_reason` |
//! |-------------------------|--------------------------|
//! | `end_turn`              | `stop`                   |
//! | `tool_use`              | `tool_calls`             |
//! | `max_tokens`            | `max_tokens`             |
//! | `stop_sequence`         | `stop`                   |
//! | (anything else)         | passed through verbatim  |

use std::collections::BTreeMap;

use krw_agent_protocol::ContentHash;
use serde_json::Value;

use crate::sse::{AnthropicSseEvent as SseEvent, ContentBlockDelta, ContentBlockStart};
use crate::{
    AssistantMessage, EpisodeContext, FunctionCall, ProviderEpisodeV1, ProviderFunctionName,
    TokenUsage, ToolCall, ToolCallKind, WireError,
};

/// Maximum number of tool calls allowed in a single assistant turn. Mirrors
/// the cap enforced by `deepseek-wire` for run-engine compatibility.
const MAX_TOOL_CALLS_PER_TURN: usize = 32;

/// Accumulator for one `tool_use` content block. The `input_json` field
/// collects `input_json_delta.partial_json` fragments; on finish the
/// concatenated text is parsed once as a JSON `Value` and re-serialised as
/// canonical JSON so that two providers streaming the same logical arguments
/// produce the same replay hash.
#[derive(Default)]
struct ToolUseBuilder {
    id: String,
    name: String,
    input_json: String,
}

/// Fold Anthropic SSE events into a [`ProviderEpisodeV1`]. One assembler per
/// HTTP response; do not reuse across streams.
pub struct EpisodeAssembler {
    request_hash: ContentHash,
    requested_model: String,
    context: EpisodeContext,
    observed_model: Option<String>,
    // Per-index accumulators. Using BTreeMap<u32, _> keeps block order stable
    // regardless of arrival order; Anthropic always emits blocks in index
    // order, but defending against a reordering proxy is cheap.
    text_blocks: BTreeMap<u32, String>,
    thinking_blocks: BTreeMap<u32, ThinkingBuilder>,
    tool_use_builders: BTreeMap<u32, ToolUseBuilder>,
    finish_reason: Option<String>,
    usage_input_tokens: u32,
    usage_output_tokens: u32,
    done: bool,
    max_buffer_bytes: usize,
    buffered_bytes: usize,
}

#[derive(Default)]
struct ThinkingBuilder {
    thinking: String,
    signature: String,
}

impl std::fmt::Debug for EpisodeAssembler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EpisodeAssembler")
            .field("request_hash", &self.request_hash)
            .field("requested_model", &self.requested_model)
            .field("observed_model", &self.observed_model)
            .field("text_block_count", &self.text_blocks.len())
            .field("thinking_block_count", &self.thinking_blocks.len())
            .field("tool_use_count", &self.tool_use_builders.len())
            .field("finish_reason", &self.finish_reason)
            .field("usage_input_tokens", &self.usage_input_tokens)
            .field("usage_output_tokens", &self.usage_output_tokens)
            .field("done", &self.done)
            .field("max_buffer_bytes", &self.max_buffer_bytes)
            .field("buffered_bytes", &self.buffered_bytes)
            .finish_non_exhaustive()
    }
}

impl EpisodeAssembler {
    /// Create a fresh assembler.
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
            text_blocks: BTreeMap::new(),
            thinking_blocks: BTreeMap::new(),
            tool_use_builders: BTreeMap::new(),
            finish_reason: None,
            usage_input_tokens: 0,
            usage_output_tokens: 0,
            done: false,
            max_buffer_bytes,
            buffered_bytes: 0,
        }
    }

    /// Feed one parsed SSE event. Returns an error on protocol violations
    /// (model changed mid-stream, event after `message_stop`, provider-side
    /// error frame).
    pub fn push_event(&mut self, event: SseEvent) -> Result<(), WireError> {
        if self.done {
            return Err(WireError::DataAfterDone);
        }
        match event {
            SseEvent::MessageStart {
                model,
                input_tokens,
            } => {
                if let Some(observed) = &self.observed_model {
                    if observed != &model {
                        return Err(WireError::ModelChangedMidStream {
                            first: observed.clone(),
                            later: model,
                        });
                    }
                } else {
                    self.observed_model = Some(model);
                }
                self.usage_input_tokens = input_tokens;
            }
            SseEvent::ContentBlockStart { index, block } => {
                self.handle_block_start(index, block)?;
            }
            SseEvent::ContentBlockDelta { index, delta } => {
                self.handle_block_delta(index, delta)?;
            }
            SseEvent::ContentBlockStop { index: _ } | SseEvent::Ping => {
                // No state change: block content was accumulated on the
                // start/delta events, and pings are keepalives.
            }
            SseEvent::MessageDelta {
                stop_reason,
                output_tokens,
            } => {
                if let Some(reason) = stop_reason {
                    self.finish_reason = Some(map_stop_reason(&reason));
                }
                if let Some(output) = output_tokens {
                    self.usage_output_tokens = output;
                }
            }
            SseEvent::MessageStop => {
                self.done = true;
            }
            SseEvent::Error {
                error_type,
                message,
            } => {
                return Err(WireError::StreamError {
                    error_type,
                    message,
                });
            }
        }
        Ok(())
    }

    /// Finalise the episode. Consumes `self` and validates that the stream
    /// terminated cleanly (`message_stop` observed, model and `stop_reason`
    /// present). Builds the [`AssistantMessage`] and computes the replay hash.
    pub fn finish(self) -> Result<ProviderEpisodeV1, WireError> {
        if !self.done {
            return Err(WireError::MissingMessageStop);
        }
        let observed_model = self.observed_model.ok_or(WireError::MissingObservedModel)?;
        let finish_reason = self.finish_reason.ok_or(WireError::MissingStopReason)?;
        if self.tool_use_builders.len() > MAX_TOOL_CALLS_PER_TURN {
            return Err(WireError::TooManyToolCalls(self.tool_use_builders.len()));
        }

        // Concatenate text blocks in index order. Empty strings are filtered
        // so that an all-empty text block does not become `content: Some("")`.
        let mut content_parts: Vec<String> = Vec::new();
        for text in self.text_blocks.into_values() {
            if !text.is_empty() {
                content_parts.push(text);
            }
        }
        let content = if content_parts.is_empty() {
            None
        } else {
            let joined = content_parts.concat();
            if joined.is_empty() {
                None
            } else {
                Some(joined)
            }
        };

        let mut reasoning_parts: Vec<String> = Vec::new();
        for thinking in self.thinking_blocks.into_values() {
            if !thinking.thinking.is_empty() {
                reasoning_parts.push(thinking.thinking);
            }
        }
        let reasoning_content = if reasoning_parts.is_empty() {
            None
        } else {
            let joined = reasoning_parts.concat();
            if joined.is_empty() {
                None
            } else {
                Some(joined)
            }
        };

        // Build tool calls. Each tool_use block's accumulated `input_json` is
        // parsed as a JSON Value (it must be complete and valid), then
        // re-serialised as canonical JSON (RFC 8785) so that replay hashes
        // are stable across providers.
        let mut tool_calls: Vec<ToolCall> = Vec::with_capacity(self.tool_use_builders.len());
        for (_index, builder) in self.tool_use_builders {
            let id = builder.id;
            let name = builder.name;
            let arguments_text = builder.input_json;
            if id.is_empty() {
                return Err(WireError::IncompleteToolCall("id"));
            }
            if name.is_empty() {
                return Err(WireError::IncompleteToolCall("name"));
            }
            // Parse the concatenated partial_json as a JSON value. Empty input
            // is treated as an empty object (`{}`) for robustness.
            let value: Value = if arguments_text.trim().is_empty() {
                Value::Object(serde_json::Map::default())
            } else {
                serde_json::from_str(&arguments_text).map_err(WireError::Json)?
            };
            let arguments = serde_jcs::to_string(&value)?;
            tool_calls.push(ToolCall {
                id,
                kind: ToolCallKind::Function,
                function: FunctionCall {
                    name: ProviderFunctionName::parse(name)?,
                    arguments,
                },
            });
        }

        let usage = TokenUsage {
            prompt_tokens: self.usage_input_tokens,
            completion_tokens: self.usage_output_tokens,
            total_tokens: self
                .usage_input_tokens
                .saturating_add(self.usage_output_tokens),
            prompt_cache_hit_tokens: 0,
            prompt_cache_miss_tokens: 0,
        };

        let assistant = AssistantMessage {
            content,
            reasoning_content,
            tool_calls,
        };

        let mut episode = ProviderEpisodeV1 {
            schema_version: 1,
            request_hash: self.request_hash,
            requested_model: self.requested_model,
            observed_model,
            api_version: self.context.api_version,
            assistant,
            tool_results: Vec::new(),
            tool_schema_hash: self.context.tool_schema_hash,
            agent_image_hash: self.context.agent_image_hash,
            finish_reason,
            usage,
            replay_hash: ContentHash::sha256("pending"),
        };
        episode.verify_model_identity()?;
        episode.verify_tool_call_structure()?;
        episode.replay_hash = episode.calculate_replay_hash()?;
        Ok(episode)
    }

    fn handle_block_start(
        &mut self,
        index: u32,
        block: ContentBlockStart,
    ) -> Result<(), WireError> {
        match block {
            ContentBlockStart::Text { text } => {
                self.charge(text.len())?;
                self.text_blocks.entry(index).or_default().push_str(&text);
            }
            ContentBlockStart::ToolUse { id, name, input } => {
                // Anthropic's streaming contract: the `content_block_start`
                // frame carries `input: {}` (placeholder) and the real
                // arguments arrive later as `input_json_delta` fragments.
                // Some providers, however, send the complete input object in
                // the start frame and emit no deltas. We distinguish the two
                // by checking whether `input` is a non-empty object: if so,
                // treat it as the complete arguments and seed `input_json`;
                // otherwise leave `input_json` empty and let deltas populate
                // it. This avoids concatenating `{}` with subsequent delta
                // fragments, which would yield invalid JSON like `{}{...}`.
                let seed = if input.is_object()
                    && !input.as_object().is_some_and(serde_json::Map::is_empty)
                {
                    let serialized = serde_jcs::to_string(&input)?;
                    self.charge(serialized.len())?;
                    serialized
                } else {
                    String::new()
                };
                let builder = self.tool_use_builders.entry(index).or_default();
                builder.id = id;
                builder.name = name;
                builder.input_json.push_str(&seed);
            }
            ContentBlockStart::Thinking {
                thinking,
                signature,
            } => {
                self.charge(thinking.len().saturating_add(signature.len()))?;
                let builder = self.thinking_blocks.entry(index).or_default();
                builder.thinking.push_str(&thinking);
                builder.signature.push_str(&signature);
            }
        }
        Ok(())
    }

    fn handle_block_delta(
        &mut self,
        index: u32,
        delta: ContentBlockDelta,
    ) -> Result<(), WireError> {
        match delta {
            ContentBlockDelta::TextDelta { text } => {
                self.charge(text.len())?;
                self.text_blocks.entry(index).or_default().push_str(&text);
            }
            ContentBlockDelta::ThinkingDelta { thinking } => {
                self.charge(thinking.len())?;
                self.thinking_blocks
                    .entry(index)
                    .or_default()
                    .thinking
                    .push_str(&thinking);
            }
            ContentBlockDelta::SignatureDelta { signature } => {
                self.charge(signature.len())?;
                self.thinking_blocks
                    .entry(index)
                    .or_default()
                    .signature
                    .push_str(&signature);
            }
            ContentBlockDelta::InputJsonDelta { partial_json } => {
                self.charge(partial_json.len())?;
                self.tool_use_builders
                    .entry(index)
                    .or_default()
                    .input_json
                    .push_str(&partial_json);
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

/// Map Anthropic `stop_reason` to the internal `finish_reason`. The internal
/// vocabulary is a superset that preserves the run-engine compatibility
/// invariant (`tool_calls` rather than `tool_use`).
fn map_stop_reason(reason: &str) -> String {
    match reason {
        // Anthropic's `end_turn` is the run-engine's `stop` — natural completion.
        "end_turn" | "stop_sequence" => "stop".to_string(),
        // Anthropic uses `tool_use`; run-engine expects `tool_calls`.
        "tool_use" => "tool_calls".to_string(),
        "max_tokens" => "max_tokens".to_string(),
        other => other.to_string(),
    }
}

// Note: `EpisodeAssembler` deliberately does not implement `Drop`. The
// sensitive accumulators (text, thinking, tool input) are moved into the
// `AssistantMessage` / `ProviderEpisodeV1` on `finish()`, and those types own
// the scrubbing responsibility via their own `Drop` impls. If `finish()` is
// never called, the buffers are still resident in memory until the
// (un-dropped) assembler is collected; this matches the deepseek-wire
// precedent where `EpisodeAssembler` likewise does not zeroize on drop.

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EpisodeContext, WireError};
    use krw_agent_protocol::{ContentHash, GLM_MODEL_ID};

    fn ctx() -> EpisodeContext {
        EpisodeContext {
            tool_schema_hash: ContentHash::sha256("tools"),
            agent_image_hash: ContentHash::sha256("image"),
            api_version: "messages-v1".to_string(),
        }
    }

    fn build_assembler(max_buffer_bytes: usize) -> EpisodeAssembler {
        EpisodeAssembler::new(
            ContentHash::sha256("request"),
            GLM_MODEL_ID.to_string(),
            ctx(),
            max_buffer_bytes,
        )
    }

    #[test]
    fn assembles_text_only_episode() {
        let mut asm = build_assembler(64 * 1024);
        asm.push_event(SseEvent::MessageStart {
            model: GLM_MODEL_ID.to_string(),
            input_tokens: 12,
        })
        .unwrap();
        asm.push_event(SseEvent::ContentBlockStart {
            index: 0,
            block: ContentBlockStart::Text {
                text: String::new(),
            },
        })
        .unwrap();
        asm.push_event(SseEvent::ContentBlockDelta {
            index: 0,
            delta: ContentBlockDelta::TextDelta {
                text: "Hello ".into(),
            },
        })
        .unwrap();
        asm.push_event(SseEvent::ContentBlockDelta {
            index: 0,
            delta: ContentBlockDelta::TextDelta {
                text: "world".into(),
            },
        })
        .unwrap();
        asm.push_event(SseEvent::ContentBlockStop { index: 0 })
            .unwrap();
        asm.push_event(SseEvent::MessageDelta {
            stop_reason: Some("end_turn".into()),
            output_tokens: Some(7),
        })
        .unwrap();
        asm.push_event(SseEvent::MessageStop).unwrap();

        let episode = asm.finish().expect("episode assembles");
        assert_eq!(episode.observed_model, GLM_MODEL_ID);
        assert_eq!(episode.finish_reason, "stop");
        assert_eq!(episode.assistant.content.as_deref(), Some("Hello world"));
        assert!(episode.assistant.tool_calls.is_empty());
        assert_eq!(episode.usage.prompt_tokens, 12);
        assert_eq!(episode.usage.completion_tokens, 7);
        assert_eq!(episode.usage.total_tokens, 19);
    }

    #[test]
    fn assembles_tool_use_episode_and_maps_finish_reason() {
        let mut asm = build_assembler(64 * 1024);
        asm.push_event(SseEvent::MessageStart {
            model: GLM_MODEL_ID.to_string(),
            input_tokens: 50,
        })
        .unwrap();
        // Text block at index 0.
        asm.push_event(SseEvent::ContentBlockStart {
            index: 0,
            block: ContentBlockStart::Text {
                text: String::new(),
            },
        })
        .unwrap();
        asm.push_event(SseEvent::ContentBlockDelta {
            index: 0,
            delta: ContentBlockDelta::TextDelta {
                text: "Let me query".into(),
            },
        })
        .unwrap();
        asm.push_event(SseEvent::ContentBlockStop { index: 0 })
            .unwrap();
        // Tool use block at index 1.
        asm.push_event(SseEvent::ContentBlockStart {
            index: 1,
            block: ContentBlockStart::ToolUse {
                id: "toolu_01".into(),
                name: "krw_query".into(),
                input: serde_json::json!({}),
            },
        })
        .unwrap();
        asm.push_event(SseEvent::ContentBlockDelta {
            index: 1,
            delta: ContentBlockDelta::InputJsonDelta {
                partial_json: r#"{"q":"한글"}"#.into(),
            },
        })
        .unwrap();
        asm.push_event(SseEvent::ContentBlockStop { index: 1 })
            .unwrap();
        asm.push_event(SseEvent::MessageDelta {
            stop_reason: Some("tool_use".into()),
            output_tokens: Some(20),
        })
        .unwrap();
        asm.push_event(SseEvent::MessageStop).unwrap();

        let episode = asm.finish().expect("episode assembles");
        assert_eq!(episode.finish_reason, "tool_calls");
        assert_eq!(episode.assistant.content.as_deref(), Some("Let me query"));
        assert_eq!(episode.assistant.tool_calls.len(), 1);
        let call = &episode.assistant.tool_calls[0];
        assert_eq!(call.id, "toolu_01");
        assert_eq!(call.function.name.as_str(), "krw_query");
        // Arguments must be valid canonical JSON.
        let parsed: Value = serde_json::from_str(&call.function.arguments).unwrap();
        assert_eq!(parsed, serde_json::json!({"q": "한글"}));
    }

    #[test]
    fn assembles_thinking_episode() {
        let mut asm = build_assembler(64 * 1024);
        asm.push_event(SseEvent::MessageStart {
            model: GLM_MODEL_ID.to_string(),
            input_tokens: 1,
        })
        .unwrap();
        asm.push_event(SseEvent::ContentBlockStart {
            index: 0,
            block: ContentBlockStart::Thinking {
                thinking: String::new(),
                signature: String::new(),
            },
        })
        .unwrap();
        asm.push_event(SseEvent::ContentBlockDelta {
            index: 0,
            delta: ContentBlockDelta::ThinkingDelta {
                thinking: "hmm".into(),
            },
        })
        .unwrap();
        asm.push_event(SseEvent::ContentBlockDelta {
            index: 0,
            delta: ContentBlockDelta::SignatureDelta {
                signature: "sig".into(),
            },
        })
        .unwrap();
        asm.push_event(SseEvent::ContentBlockStop { index: 0 })
            .unwrap();
        asm.push_event(SseEvent::ContentBlockStart {
            index: 1,
            block: ContentBlockStart::Text {
                text: String::new(),
            },
        })
        .unwrap();
        asm.push_event(SseEvent::ContentBlockDelta {
            index: 1,
            delta: ContentBlockDelta::TextDelta {
                text: "answer".into(),
            },
        })
        .unwrap();
        asm.push_event(SseEvent::ContentBlockStop { index: 1 })
            .unwrap();
        asm.push_event(SseEvent::MessageDelta {
            stop_reason: Some("end_turn".into()),
            output_tokens: Some(3),
        })
        .unwrap();
        asm.push_event(SseEvent::MessageStop).unwrap();

        let episode = asm.finish().expect("episode assembles");
        assert_eq!(episode.assistant.reasoning_content.as_deref(), Some("hmm"));
        assert_eq!(episode.assistant.content.as_deref(), Some("answer"));
    }

    #[test]
    fn finish_requires_message_stop() {
        let mut asm = build_assembler(64 * 1024);
        asm.push_event(SseEvent::MessageStart {
            model: GLM_MODEL_ID.to_string(),
            input_tokens: 1,
        })
        .unwrap();
        let err = asm.finish().expect_err("must require message_stop");
        assert!(matches!(err, WireError::MissingMessageStop));
    }

    #[test]
    fn finish_requires_observed_model() {
        let mut asm = build_assembler(64 * 1024);
        // Skip MessageStart; emit message_stop only.
        asm.push_event(SseEvent::MessageStop).unwrap();
        let err = asm.finish().expect_err("must require observed model");
        assert!(matches!(err, WireError::MissingObservedModel));
    }

    #[test]
    fn finish_requires_stop_reason() {
        let mut asm = build_assembler(64 * 1024);
        asm.push_event(SseEvent::MessageStart {
            model: GLM_MODEL_ID.to_string(),
            input_tokens: 1,
        })
        .unwrap();
        asm.push_event(SseEvent::MessageStop).unwrap();
        let err = asm.finish().expect_err("must require stop_reason");
        assert!(matches!(err, WireError::MissingStopReason));
    }

    #[test]
    fn rejects_event_after_message_stop() {
        let mut asm = build_assembler(64 * 1024);
        asm.push_event(SseEvent::MessageStop).unwrap();
        let err = asm
            .push_event(SseEvent::Ping)
            .expect_err("events after stop are illegal");
        assert!(matches!(err, WireError::DataAfterDone));
    }

    #[test]
    fn rejects_model_change_mid_stream() {
        let mut asm = build_assembler(64 * 1024);
        asm.push_event(SseEvent::MessageStart {
            model: GLM_MODEL_ID.to_string(),
            input_tokens: 1,
        })
        .unwrap();
        asm.push_event(SseEvent::MessageStop).unwrap();
        // The model change is detected at the next MessageStart, but
        // MessageStart after MessageStop is DataAfterDone; test directly:
        let mut asm2 = build_assembler(64 * 1024);
        asm2.push_event(SseEvent::MessageStart {
            model: "glm-5.2".into(),
            input_tokens: 1,
        })
        .unwrap();
        let err = asm2
            .push_event(SseEvent::MessageStart {
                model: "deepseek-v4-flash".into(),
                input_tokens: 1,
            })
            .expect_err("model change must error");
        assert!(matches!(err, WireError::ModelChangedMidStream { .. }));
    }

    #[test]
    fn surfaces_provider_error_event() {
        let mut asm = build_assembler(64 * 1024);
        let err = asm
            .push_event(SseEvent::Error {
                error_type: "overloaded_error".into(),
                message: "Overloaded".into(),
            })
            .expect_err("error events must surface");
        assert!(matches!(
            err,
            WireError::StreamError {
                error_type,
                message
            } if error_type == "overloaded_error" && message == "Overloaded"
        ));
    }

    #[test]
    fn enforces_episode_buffer_limit() {
        let mut asm = build_assembler(8);
        asm.push_event(SseEvent::MessageStart {
            model: GLM_MODEL_ID.to_string(),
            input_tokens: 1,
        })
        .unwrap();
        asm.push_event(SseEvent::ContentBlockStart {
            index: 0,
            block: ContentBlockStart::Text {
                text: String::new(),
            },
        })
        .unwrap();
        let err = asm
            .push_event(SseEvent::ContentBlockDelta {
                index: 0,
                delta: ContentBlockDelta::TextDelta {
                    text: "this is longer than eight bytes".into(),
                },
            })
            .expect_err("must hit buffer limit");
        assert!(matches!(err, WireError::EpisodeBufferLimit(8)));
    }

    #[test]
    fn empty_tool_input_is_object() {
        let mut asm = build_assembler(64 * 1024);
        asm.push_event(SseEvent::MessageStart {
            model: GLM_MODEL_ID.to_string(),
            input_tokens: 1,
        })
        .unwrap();
        asm.push_event(SseEvent::ContentBlockStart {
            index: 0,
            block: ContentBlockStart::ToolUse {
                id: "toolu_01".into(),
                name: "krw_noop".into(),
                input: serde_json::json!({}),
            },
        })
        .unwrap();
        // No input_json_delta: empty input.
        asm.push_event(SseEvent::ContentBlockStop { index: 0 })
            .unwrap();
        asm.push_event(SseEvent::MessageDelta {
            stop_reason: Some("tool_use".into()),
            output_tokens: Some(1),
        })
        .unwrap();
        asm.push_event(SseEvent::MessageStop).unwrap();
        let episode = asm.finish().expect("episode assembles");
        assert_eq!(episode.assistant.tool_calls.len(), 1);
        assert_eq!(episode.assistant.tool_calls[0].function.arguments, "{}");
    }

    #[test]
    fn tool_input_delta_is_canonicalised() {
        // If the upstream emits non-canonical JSON (key order), the replay
        // arguments must be canonical so the replay hash is stable.
        let mut asm = build_assembler(64 * 1024);
        asm.push_event(SseEvent::MessageStart {
            model: GLM_MODEL_ID.to_string(),
            input_tokens: 1,
        })
        .unwrap();
        asm.push_event(SseEvent::ContentBlockStart {
            index: 0,
            block: ContentBlockStart::ToolUse {
                id: "toolu_01".into(),
                name: "krw_query".into(),
                input: serde_json::json!({}),
            },
        })
        .unwrap();
        asm.push_event(SseEvent::ContentBlockDelta {
            index: 0,
            delta: ContentBlockDelta::InputJsonDelta {
                partial_json: r#"{"b":2,"a":1}"#.into(),
            },
        })
        .unwrap();
        asm.push_event(SseEvent::ContentBlockStop { index: 0 })
            .unwrap();
        asm.push_event(SseEvent::MessageDelta {
            stop_reason: Some("tool_use".into()),
            output_tokens: Some(1),
        })
        .unwrap();
        asm.push_event(SseEvent::MessageStop).unwrap();
        let episode = asm.finish().expect("episode assembles");
        let args = &episode.assistant.tool_calls[0].function.arguments;
        // Canonical JSON sorts keys alphabetically.
        assert_eq!(args, r#"{"a":1,"b":2}"#);
    }

    #[test]
    fn max_tokens_stop_reason_passes_through() {
        let mut asm = build_assembler(64 * 1024);
        asm.push_event(SseEvent::MessageStart {
            model: GLM_MODEL_ID.to_string(),
            input_tokens: 1,
        })
        .unwrap();
        asm.push_event(SseEvent::MessageDelta {
            stop_reason: Some("max_tokens".into()),
            output_tokens: None,
        })
        .unwrap();
        asm.push_event(SseEvent::MessageStop).unwrap();
        let episode = asm.finish().expect("episode assembles");
        assert_eq!(episode.finish_reason, "max_tokens");
    }

    #[test]
    fn multiple_text_blocks_are_concatenated_in_index_order() {
        let mut asm = build_assembler(64 * 1024);
        asm.push_event(SseEvent::MessageStart {
            model: GLM_MODEL_ID.to_string(),
            input_tokens: 1,
        })
        .unwrap();
        // Block at index 2 arrives first (defensive against proxy reordering).
        asm.push_event(SseEvent::ContentBlockStart {
            index: 2,
            block: ContentBlockStart::Text {
                text: String::new(),
            },
        })
        .unwrap();
        asm.push_event(SseEvent::ContentBlockDelta {
            index: 2,
            delta: ContentBlockDelta::TextDelta { text: "B".into() },
        })
        .unwrap();
        asm.push_event(SseEvent::ContentBlockStop { index: 2 })
            .unwrap();
        asm.push_event(SseEvent::ContentBlockStart {
            index: 5,
            block: ContentBlockStart::Text {
                text: String::new(),
            },
        })
        .unwrap();
        asm.push_event(SseEvent::ContentBlockDelta {
            index: 5,
            delta: ContentBlockDelta::TextDelta { text: "A".into() },
        })
        .unwrap();
        asm.push_event(SseEvent::ContentBlockStop { index: 5 })
            .unwrap();
        asm.push_event(SseEvent::MessageDelta {
            stop_reason: Some("end_turn".into()),
            output_tokens: Some(2),
        })
        .unwrap();
        asm.push_event(SseEvent::MessageStop).unwrap();
        let episode = asm.finish().expect("episode assembles");
        // BTreeMap iterates in ascending key order, so index 2 before 5.
        assert_eq!(episode.assistant.content.as_deref(), Some("BA"));
    }
}
