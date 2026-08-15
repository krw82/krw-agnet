use krw_agent_protocol::ContentHash;
use krw_agent_provider_wire::{
    AssistantMessage, CanonicalJsonText, ContentBlock, MessageRole, ProviderMessage, ToolCall,
    ToolResultMessage,
};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

/// Internal conversation representation used by the run engine while a run is
/// in flight. Conversion to the Anthropic wire shape happens only at the
/// provider request boundary.
#[derive(Clone, PartialEq)]
pub(super) enum RunEngineMessage {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        content: Option<String>,
        reasoning_content: Option<String>,
        reasoning_signature: Option<String>,
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        tool_call_id: String,
        content: CanonicalJsonText,
    },
}

impl RunEngineMessage {
    pub(super) fn system(content: impl Into<String>) -> Self {
        Self::System {
            content: content.into(),
        }
    }

    pub(super) fn user(content: impl Into<String>) -> Self {
        Self::User {
            content: content.into(),
        }
    }

    pub(super) fn from_assistant(mut assistant: AssistantMessage) -> Self {
        Self::Assistant {
            content: std::mem::take(&mut assistant.content),
            reasoning_content: std::mem::take(&mut assistant.reasoning_content),
            reasoning_signature: std::mem::take(&mut assistant.reasoning_signature),
            tool_calls: std::mem::take(&mut assistant.tool_calls),
        }
    }

    pub(super) fn from_tool_result(result: &ToolResultMessage) -> Self {
        Self::Tool {
            tool_call_id: result.tool_call_id.clone(),
            content: result.content.clone(),
        }
    }

    pub(super) fn scrub_sensitive(&mut self) {
        match self {
            Self::System { content } | Self::User { content } => content.zeroize(),
            Self::Assistant {
                content,
                reasoning_content,
                reasoning_signature,
                tool_calls,
            } => {
                if let Some(content) = content {
                    content.zeroize();
                }
                if let Some(reasoning) = reasoning_content {
                    reasoning.zeroize();
                }
                if let Some(signature) = reasoning_signature {
                    signature.zeroize();
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

    /// Convert into the provider wire shape used by durable replay and request
    /// construction. A system message is encoded as a user text block here;
    /// the request builder hoists it into the top-level system field.
    pub(super) fn to_provider_message_for_serialization(&self) -> ProviderMessage {
        match self {
            Self::System { content } | Self::User { content } => {
                ProviderMessage::user(content.clone())
            }
            Self::Assistant {
                content,
                reasoning_content,
                reasoning_signature,
                tool_calls,
            } => AssistantMessage {
                content: content.clone(),
                reasoning_content: reasoning_content.clone(),
                reasoning_signature: reasoning_signature.clone(),
                tool_calls: tool_calls.clone(),
            }
            .into_provider_message(),
            Self::Tool {
                tool_call_id,
                content,
            } => ToolResultMessage {
                tool_call_id: tool_call_id.clone(),
                content: content.clone(),
            }
            .into_provider_message(),
        }
    }

    fn try_from_provider_message(message: &ProviderMessage) -> Result<Self, String> {
        if message.role == MessageRole::User
            && message.content.len() == 1
            && let ContentBlock::Text { text } = &message.content[0]
        {
            return Ok(Self::User {
                content: text.clone(),
            });
        }
        if message.role == MessageRole::Assistant {
            let assistant = AssistantMessage::from_content_blocks(&message.content)
                .map_err(|error| format!("{error:?}"))?;
            return Ok(Self::from_assistant(assistant));
        }
        Err(format!(
            "unsupported provider message for run-engine replay: role={:?} blocks={}",
            message.role,
            message.content.len()
        ))
    }
}

impl std::fmt::Debug for RunEngineMessage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::System { content } => formatter
                .debug_struct("RunEngineMessage::System")
                .field("content_len", &content.len())
                .finish(),
            Self::User { content } => formatter
                .debug_struct("RunEngineMessage::User")
                .field("content_len", &content.len())
                .finish(),
            Self::Assistant {
                content,
                reasoning_content,
                reasoning_signature,
                tool_calls,
            } => formatter
                .debug_struct("RunEngineMessage::Assistant")
                .field("content_len", &content.as_ref().map(String::len))
                .field(
                    "reasoning_len",
                    &reasoning_content.as_ref().map(String::len),
                )
                .field(
                    "reasoning_signature_len",
                    &reasoning_signature.as_ref().map(String::len),
                )
                .field("tool_calls", tool_calls)
                .finish(),
            Self::Tool {
                tool_call_id,
                content,
            } => formatter
                .debug_struct("RunEngineMessage::Tool")
                .field("tool_call_id_hash", &ContentHash::sha256(tool_call_id))
                .field("content", content)
                .finish(),
        }
    }
}

impl Serialize for RunEngineMessage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.to_provider_message_for_serialization()
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RunEngineMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = ProviderMessage::deserialize(deserializer)?;
        Self::try_from_provider_message(&wire).map_err(serde::de::Error::custom)
    }
}
