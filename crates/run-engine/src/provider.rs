//! Wire-failure classification retained for engine-side tests. The
//! [`Provider`] port itself and the HTTP `ProviderClient` adapter live in
//! `krw-agent-execution-contracts`; the engine re-exports them from its root.

#[cfg(test)]
use krw_agent_provider_wire::WireError;

#[cfg(test)]
use super::DeliveryCertainty;

#[cfg(test)]
pub(super) fn deepseek_failure_code(error: &WireError) -> String {
    match error {
        WireError::ApiStatus {
            status,
            provider_code,
            ..
        } => provider_code.as_ref().map_or_else(
            || format!("deepseek_http_{status}"),
            |code| format!("deepseek_http_{status}_{code}"),
        ),
        WireError::Http(_) => "deepseek_http_transport".into(),
        WireError::UnexpectedContentType => "deepseek_unexpected_content_type".into(),
        WireError::MissingMessageStop => "deepseek_missing_done_event".into(),
        WireError::MissingStopReason => "deepseek_missing_finish_reason".into(),
        WireError::IncompleteSseFrame => "deepseek_incomplete_sse_frame".into(),
        WireError::SseParseError(_) => "deepseek_sse_parse_error".into(),
        WireError::SseBufferLimit(_)
        | WireError::StreamLimit(_)
        | WireError::EpisodeBufferLimit(_) => "deepseek_response_too_large".into(),
        WireError::Json(_) => "deepseek_response_json_invalid".into(),
        WireError::InvalidThinkingToolReplay => "deepseek_thinking_tool_replay_invalid".into(),
        WireError::MissingObservedModel
        | WireError::ModelChangedMidStream { .. }
        | WireError::ObservedModelMismatch { .. } => "deepseek_model_stream_invalid".into(),
        WireError::InvalidEndpoint
        | WireError::InvalidAuthorization
        | WireError::UnknownModel(_)
        | WireError::InvalidRequest(_)
        | WireError::MissingMaxTokens
        | WireError::InvalidClientLimits
        | WireError::InvalidAllowedModel => "deepseek_configuration_invalid".into(),
        WireError::InvalidProviderFunctionName
        | WireError::InvalidJsonSchemaDocument
        | WireError::UnsupportedStructuredOutputSchema
        | WireError::RequestFootprintOverflow
        | WireError::CanonicalJsonNotUtf8
        | WireError::NonCanonicalJsonText
        | WireError::InvalidToolCallId
        | WireError::EmptyMessageContent
        | WireError::ToolResultInNonUserMessage
        | WireError::UnexpectedToolResultInAssistant => "deepseek_request_contract_invalid".into(),
        WireError::DataAfterDone
        | WireError::IncompleteToolCall(_)
        | WireError::TooManyToolCalls(_) => "deepseek_protocol_invalid".into(),
        WireError::StreamError { .. } => "deepseek_stream_error".into(),
    }
}

#[cfg(test)]
pub(super) fn classify_deepseek_failure(error: &WireError) -> (bool, DeliveryCertainty) {
    classify_wire_failure(error)
}

#[cfg(test)]
pub(super) fn glm_failure_code(error: &WireError) -> String {
    match error {
        WireError::ApiStatus {
            status,
            provider_code,
            ..
        } => provider_code.as_ref().map_or_else(
            || format!("glm_http_{status}"),
            |code| format!("glm_http_{status}_{code}"),
        ),
        WireError::Http(_) => "glm_http_transport".into(),
        WireError::UnexpectedContentType => "glm_unexpected_content_type".into(),
        WireError::MissingMessageStop => "glm_missing_done_event".into(),
        WireError::MissingStopReason => "glm_missing_finish_reason".into(),
        WireError::IncompleteSseFrame => "glm_incomplete_sse_frame".into(),
        WireError::SseParseError(_) => "glm_sse_parse_error".into(),
        WireError::SseBufferLimit(_)
        | WireError::StreamLimit(_)
        | WireError::EpisodeBufferLimit(_) => "glm_response_too_large".into(),
        WireError::Json(_) => "glm_response_json_invalid".into(),
        WireError::InvalidThinkingToolReplay => "glm_thinking_tool_replay_invalid".into(),
        WireError::MissingObservedModel => "glm_model_missing".into(),
        WireError::ModelChangedMidStream { .. } => "glm_model_changed_midstream".into(),
        WireError::ObservedModelMismatch { .. } => "glm_model_mismatch".into(),
        WireError::InvalidEndpoint
        | WireError::InvalidAuthorization
        | WireError::UnknownModel(_)
        | WireError::InvalidRequest(_)
        | WireError::MissingMaxTokens
        | WireError::InvalidClientLimits
        | WireError::InvalidAllowedModel => "glm_configuration_invalid".into(),
        WireError::InvalidProviderFunctionName
        | WireError::InvalidJsonSchemaDocument
        | WireError::UnsupportedStructuredOutputSchema
        | WireError::RequestFootprintOverflow
        | WireError::CanonicalJsonNotUtf8
        | WireError::NonCanonicalJsonText
        | WireError::InvalidToolCallId
        | WireError::EmptyMessageContent
        | WireError::ToolResultInNonUserMessage
        | WireError::UnexpectedToolResultInAssistant => "glm_request_contract_invalid".into(),
        WireError::DataAfterDone
        | WireError::IncompleteToolCall(_)
        | WireError::TooManyToolCalls(_) => "glm_protocol_invalid".into(),
        WireError::StreamError { .. } => "glm_stream_error".into(),
    }
}

#[cfg(test)]
pub(super) fn classify_glm_failure(error: &WireError) -> (bool, DeliveryCertainty) {
    classify_wire_failure(error)
}

#[cfg(test)]
fn classify_wire_failure(error: &WireError) -> (bool, DeliveryCertainty) {
    match error {
        WireError::InvalidEndpoint
        | WireError::InvalidAuthorization
        | WireError::UnknownModel(_)
        | WireError::InvalidRequest(_)
        | WireError::MissingMaxTokens
        | WireError::InvalidClientLimits
        | WireError::InvalidAllowedModel
        | WireError::InvalidProviderFunctionName
        | WireError::InvalidJsonSchemaDocument
        | WireError::CanonicalJsonNotUtf8
        | WireError::NonCanonicalJsonText
        | WireError::InvalidToolCallId
        | WireError::EmptyMessageContent
        | WireError::ToolResultInNonUserMessage
        | WireError::UnexpectedToolResultInAssistant => (false, DeliveryCertainty::NotDispatched),
        WireError::Http(error) => (
            error.is_timeout() || error.is_connect(),
            DeliveryCertainty::MayHaveDispatched,
        ),
        WireError::ApiStatus { status, .. } => (
            matches!(status, 429 | 500 | 503),
            DeliveryCertainty::MayHaveDispatched,
        ),
        WireError::IncompleteSseFrame
        | WireError::SseParseError(_)
        | WireError::SseBufferLimit(_)
        | WireError::StreamLimit(_)
        | WireError::MissingMessageStop
        | WireError::MissingStopReason
        | WireError::MissingObservedModel
        | WireError::Json(_)
        | WireError::UnexpectedContentType => (true, DeliveryCertainty::MayHaveDispatched),
        _ => (false, DeliveryCertainty::MayHaveDispatched),
    }
}
