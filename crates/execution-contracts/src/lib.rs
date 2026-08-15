//! Execution contracts shared by the run engine and its capability and
//! persistence runtimes.
//!
//! This crate is the inverted dependency boundary underneath the run engine:
//! the provider, capability, and persistence ports, the dependency failure
//! taxonomy they report through, the durable action/final DTOs exchanged with
//! persistence, the recovery snapshot DTOs restored after a failure, and the
//! canonical action key.  Capability and persistence runtimes implement these
//! ports without depending on the run engine itself.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use krw_agent_bounded_child::{
    CancelChildMutation, ChildExecutionReceipt, CompleteChildMutation, InvokeChildMutation,
    ReserveChildMutation,
};
use krw_agent_evidence::{Answerability, Calculation, EvidenceRecord};
use krw_agent_image::{CapabilitySpec, ResolvedCapabilityContracts};
use krw_agent_persistence::{
    ActionFinalizationReceipt, ActionReceipt, ActionStage, BeginActionMutation,
    CheckpointEpisodeMutation, FinalCommitMutation, FinalizeActionMutation, ObserveActionMutation,
};
use krw_agent_protocol::{BudgetUsage, CapabilityBinding, ContentHash};
use krw_agent_provider_wire::{
    EpisodeContext, MessagesRequest, PreparedMessagesRequest, ProviderEpisodeV1,
};
use krw_session_memory::SessionMemoryDeltaV3;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zeroize::Zeroize;

#[cfg(feature = "http")]
use krw_agent_protocol::GLM_MODEL_ID;
#[cfg(feature = "http")]
use krw_agent_provider_wire::{ProviderClient, WireError};

/// Redacted dependency error safe to retain in ordinary operational traces.
#[derive(Clone, PartialEq, Eq)]
pub struct DependencyFailure {
    pub code: String,
    pub diagnostic_hash: ContentHash,
    pub retryable: bool,
    pub delivery: DeliveryCertainty,
}

impl DependencyFailure {
    pub fn redacted(
        code: impl Into<String>,
        diagnostic: impl AsRef<[u8]>,
        retryable: bool,
        delivery: DeliveryCertainty,
    ) -> Self {
        Self {
            code: code.into(),
            diagnostic_hash: ContentHash::sha256(diagnostic),
            retryable,
            delivery,
        }
    }
}

impl fmt::Debug for DependencyFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DependencyFailure")
            .field("code", &self.code)
            .field("diagnostic_hash", &self.diagnostic_hash)
            .field("retryable", &self.retryable)
            .field("delivery", &self.delivery)
            .finish()
    }
}

impl fmt::Display for DependencyFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "dependency failure {} ({})",
            self.code, self.diagnostic_hash
        )
    }
}

impl std::error::Error for DependencyFailure {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryCertainty {
    NotDispatched,
    MayHaveDispatched,
}

/// Recursively zeroize every JSON string in place. Mirrors the run engine's
/// `validation::scrub_json` so the redacting `Drop` impls below keep their
/// zero-on-drop guarantees inside this crate.
fn scrub_json(value: &mut Value) {
    match value {
        Value::String(value) => value.zeroize(),
        Value::Array(values) => {
            for value in values {
                scrub_json(value);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                scrub_json(value);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityInvocation {
    pub run_id: String,
    pub action_key: String,
    pub capability_id: String,
    pub request_hash: ContentHash,
    pub input_schema_hash: ContentHash,
    pub output_schema_hash: ContentHash,
    pub normalized_output_contract_hash: ContentHash,
    pub arguments: Value,
    pub binding: CapabilityBinding,
}

impl fmt::Debug for CapabilityInvocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CapabilityInvocation")
            .field("run_id_hash", &ContentHash::sha256(&self.run_id))
            .field("action_key", &self.action_key)
            .field("capability_id", &self.capability_id)
            .field("request_hash", &self.request_hash)
            .field("input_schema_hash", &self.input_schema_hash)
            .field("output_schema_hash", &self.output_schema_hash)
            .field(
                "normalized_output_contract_hash",
                &self.normalized_output_contract_hash,
            )
            .field("arguments", &"[REDACTED]")
            .field("binding", &self.binding)
            .finish()
    }
}

impl Drop for CapabilityInvocation {
    fn drop(&mut self) {
        scrub_json(&mut self.arguments);
    }
}

/// A capability adapter returns both provider-visible content and its typed,
/// deterministic evidence projection.  The entire value is persisted before
/// any part is ingested into run memory.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityResult {
    pub provider_content: Value,
    #[serde(default)]
    pub evidence: Vec<EvidenceRecord>,
    pub answerability: Option<Answerability>,
    #[serde(default)]
    pub calculations: Vec<Calculation>,
    /// Private presentation data captured from the MCP `_meta` channel. It is
    /// durably carried with the action result but is never serialized into
    /// the provider-visible tool result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<Value>,
}

impl fmt::Debug for CapabilityResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CapabilityResult")
            .field("provider_content", &"[REDACTED]")
            .field("evidence_count", &self.evidence.len())
            .field("answerability", &self.answerability)
            .field("calculation_count", &self.calculations.len())
            .field("has_presentation", &self.presentation.is_some())
            .finish()
    }
}

impl Drop for CapabilityResult {
    fn drop(&mut self) {
        scrub_json(&mut self.provider_content);
        for record in &mut self.evidence {
            for fact in &mut record.facts {
                scrub_json(&mut fact.value);
            }
        }
        for calculation in &mut self.calculations {
            scrub_json(&mut calculation.output);
        }
        if let Some(presentation) = &mut self.presentation {
            scrub_json(presentation);
        }
    }
}

#[async_trait]
pub trait CapabilityRuntime: fmt::Debug + Send + Sync {
    async fn invoke(
        &self,
        invocation: &CapabilityInvocation,
    ) -> Result<CapabilityResult, DependencyFailure>;

    /// Legacy observability accessor for private presentation channels. The
    /// authoritative path is `CapabilityResult.presentation`, which is part
    /// of the durable action result and is ingested into `ActiveRun`; this
    /// accessor is retained only for isolated runtime diagnostics.
    fn presentation_packs(&self) -> Vec<Value> {
        Vec::new()
    }

    /// Restore run-local authorization state from a result that is already
    /// durably committed. Implementations must be deterministic and
    /// idempotent: recovery calls this in committed action order before the
    /// result is exposed to the provider conversation.
    async fn restore_committed_result(
        &self,
        _invocation: &CapabilityInvocation,
        _result: &CapabilityResult,
    ) -> Result<(), DependencyFailure> {
        Ok(())
    }
}

#[async_trait]
pub trait Provider: fmt::Debug + Send + Sync {
    async fn complete(
        &self,
        request: &MessagesRequest,
        context: &EpisodeContext,
    ) -> Result<ProviderEpisodeV1, DependencyFailure>;

    async fn complete_prepared(
        &self,
        prepared: &PreparedMessagesRequest<'_>,
        context: &EpisodeContext,
    ) -> Result<ProviderEpisodeV1, DependencyFailure> {
        self.complete(prepared.request(), context).await
    }
}

#[cfg(feature = "http")]
#[async_trait]
impl Provider for ProviderClient {
    async fn complete(
        &self,
        request: &MessagesRequest,
        context: &EpisodeContext,
    ) -> Result<ProviderEpisodeV1, DependencyFailure> {
        self.complete_stream(request, context)
            .await
            .map_err(|error| provider_failure(&request.model, &error))
    }

    async fn complete_prepared(
        &self,
        prepared: &PreparedMessagesRequest<'_>,
        context: &EpisodeContext,
    ) -> Result<ProviderEpisodeV1, DependencyFailure> {
        self.complete_stream_prepared(prepared, context)
            .await
            .map_err(|error| provider_failure(&prepared.request().model, &error))
    }
}

#[cfg(feature = "http")]
fn provider_failure(model: &str, error: &WireError) -> DependencyFailure {
    let is_glm = model == GLM_MODEL_ID;
    let (retryable, delivery) = if is_glm {
        classify_glm_failure(error)
    } else {
        classify_deepseek_failure(error)
    };
    let code = if is_glm {
        glm_failure_code(error)
    } else {
        deepseek_failure_code(error)
    };
    DependencyFailure::redacted(code, format!("{error:?}"), retryable, delivery)
}

#[cfg(feature = "http")]
fn deepseek_failure_code(error: &WireError) -> String {
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

#[cfg(feature = "http")]
fn classify_deepseek_failure(error: &WireError) -> (bool, DeliveryCertainty) {
    classify_wire_failure(error)
}

#[cfg(feature = "http")]
fn glm_failure_code(error: &WireError) -> String {
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

#[cfg(feature = "http")]
fn classify_glm_failure(error: &WireError) -> (bool, DeliveryCertainty) {
    classify_wire_failure(error)
}

#[cfg(feature = "http")]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunControl {
    Active {
        fencing_token: u64,
        cancel_generation: u64,
    },
    Cancelled,
    Finalized,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunIdentity {
    pub run_id: String,
    pub tenant_id: String,
    pub fencing_token: u64,
    pub expected_cancel_generation: u64,
}

#[derive(Clone)]
pub struct DurableEpisode {
    pub mutation: CheckpointEpisodeMutation,
    /// Implementations must store these opaque bytes encrypted and scoped to
    /// the run/principal; they can contain private reasoning.
    pub episode_bytes: Vec<u8>,
}

#[derive(Clone)]
pub struct DurableRunState {
    pub run_id: String,
    pub fencing_token: u64,
    pub recovery_schema_hash: ContentHash,
    pub state_hash: ContentHash,
    pub state_bytes: Vec<u8>,
    /// Optional product-facing lifecycle marker. It is not part of the
    /// encrypted recovery artifact and carries no user/provider content.
    pub lifecycle_stage: Option<RunLifecycleStage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunLifecycleStage {
    Composing,
}

impl fmt::Debug for DurableRunState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DurableRunState")
            .field("run_id_hash", &ContentHash::sha256(&self.run_id))
            .field("fencing_token", &self.fencing_token)
            .field("recovery_schema_hash", &self.recovery_schema_hash)
            .field("state_hash", &self.state_hash)
            .field("state_bytes", &"[REDACTED]")
            .field("state_byte_len", &self.state_bytes.len())
            .field("lifecycle_stage", &self.lifecycle_stage)
            .finish()
    }
}

impl Drop for DurableRunState {
    fn drop(&mut self) {
        self.state_bytes.zeroize();
    }
}

impl fmt::Debug for DurableEpisode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DurableEpisode")
            .field("episode_hash", &self.mutation.episode_hash)
            .field("episode_bytes", &"[REDACTED]")
            .field("episode_byte_len", &self.episode_bytes.len())
            .finish()
    }
}

impl Drop for DurableEpisode {
    fn drop(&mut self) {
        self.episode_bytes.zeroize();
    }
}

#[derive(Clone)]
pub struct ActionIntent {
    pub mutation: BeginActionMutation,
    pub episode_hash: ContentHash,
    pub tool_call_id: String,
    pub capability_id: String,
    pub input_contract: String,
    pub output_contracts: Vec<String>,
    pub input_schema_hash: ContentHash,
    /// Ordered composite hash of every declared remote/normalized output
    /// contract for this capability action.
    pub output_schema_hash: ContentHash,
    pub normalized_output_contract_hash: ContentHash,
    pub server_schema_bundle_hash: ContentHash,
    pub data_release_hash: ContentHash,
    /// May contain user or retrieved text. Never write it to ordinary logs.
    pub canonical_arguments: Vec<u8>,
}

impl fmt::Debug for ActionIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ActionIntent")
            .field("action_key", &self.mutation.action_key)
            .field("request_hash", &self.mutation.request_hash)
            .field("episode_hash", &self.episode_hash)
            .field("tool_call_id", &self.tool_call_id)
            .field("capability_id", &self.capability_id)
            .field("input_contract", &self.input_contract)
            .field("output_contracts", &self.output_contracts)
            .field("input_schema_hash", &self.input_schema_hash)
            .field("output_schema_hash", &self.output_schema_hash)
            .field(
                "normalized_output_contract_hash",
                &self.normalized_output_contract_hash,
            )
            .field("server_schema_bundle_hash", &self.server_schema_bundle_hash)
            .field("data_release_hash", &self.data_release_hash)
            .field("canonical_arguments", &"[REDACTED]")
            .field("canonical_arguments_len", &self.canonical_arguments.len())
            .finish()
    }
}

impl Drop for ActionIntent {
    fn drop(&mut self) {
        self.canonical_arguments.zeroize();
    }
}

#[derive(Clone)]
pub struct DurableActionObservation {
    pub mutation: ObserveActionMutation,
    /// Opaque result artifact. Persistence owns encryption and retention.
    pub result_bytes: Vec<u8>,
}

impl fmt::Debug for DurableActionObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DurableActionObservation")
            .field("action_key", &self.mutation.action_key)
            .field("result_hash", &self.mutation.result_hash)
            .field("result_bytes", &"[REDACTED]")
            .field("result_byte_len", &self.result_bytes.len())
            .finish()
    }
}

impl Drop for DurableActionObservation {
    fn drop(&mut self) {
        self.result_bytes.zeroize();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkActionAmbiguous {
    pub run_id: String,
    pub fencing_token: u64,
    pub mutation_id: String,
    pub action_key: String,
    pub reason_code: String,
}

#[derive(Clone, Serialize)]
pub struct DurableFinal {
    pub mutation: FinalCommitMutation,
    /// Hash of the exact canonical output value. For direct Markdown this is
    /// the JCS-encoded string; typed legacy outputs use their canonical JSON.
    pub final_output_hash: ContentHash,
    pub rendered_message_hash: ContentHash,
    pub answer_bundle: Value,
    pub usage: BudgetUsage,
    pub session_memory_delta: Option<SessionMemoryDeltaV3>,
    pub next_memory_frontier_hash: Option<ContentHash>,
}

impl fmt::Debug for DurableFinal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DurableFinal")
            .field("answer_bundle_hash", &self.mutation.answer_bundle_hash)
            .field("final_output_hash", &self.final_output_hash)
            .field("rendered_message_hash", &self.rendered_message_hash)
            .field("answer_bundle", &"[REDACTED]")
            .field("usage", &self.usage)
            .field(
                "session_memory_delta_hash",
                &self.mutation.session_memory_delta_hash,
            )
            .field("session_memory_delta", &"[REDACTED]")
            .field("next_memory_frontier_hash", &self.next_memory_frontier_hash)
            .finish()
    }
}

impl Drop for DurableFinal {
    fn drop(&mut self) {
        scrub_json(&mut self.answer_bundle);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalStatus {
    Committed,
    AlreadyCommitted,
    Cancelled,
}

/// Completion class for a research run's committed answer.
///
/// A run that passed admission must leave an answer behind in every case
/// where that is possible; only integrity violations may produce a terminal
/// research failure. This enum is the answer-side counterpart of
/// [`FinalStatus`]: `FinalStatus` describes the durability of the commit,
/// while `ResearchCompletion` describes how much verified material the
/// committed answer actually carries.
///
/// `Accepted` is the default so that answers produced before this field
/// existed (and answers that needed no degradation) deserialize identically.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResearchCompletion {
    /// Full evidence and claims for the question scope; the core final was
    /// committed without any sanitizer downgrade.
    #[default]
    Accepted,
    /// Some objectives, periods, documents, or citations were insufficient,
    /// but a useful verified-scope answer exists. Unsupported claims were
    /// removed or softened and the limitation is carried by the class.
    AcceptedWithWarnings,
    /// A provider/MCP/dependency problem was not resolved inside the bounded
    /// deadline; a deterministic unavailability notice was committed instead
    /// of a research answer.
    UnavailableButAnswerable,
    /// Reserved for the integrity-only failure list (ownership/pin/fence
    /// mismatch, forged ledger record, uncommitted calculation lineage,
    /// ambiguous re-dispatch, failed atomic commit).
    IntegrityFailure,
}

#[async_trait]
pub trait Persistence: fmt::Debug + Send + Sync {
    async fn load_recovery(&self, run: &RunIdentity)
    -> Result<RecoverySnapshot, DependencyFailure>;

    async fn inspect_run(&self, run: &RunIdentity) -> Result<RunControl, DependencyFailure>;

    async fn checkpoint_episode(&self, episode: &DurableEpisode) -> Result<(), DependencyFailure>;

    async fn checkpoint_run_state(&self, state: &DurableRunState) -> Result<(), DependencyFailure>;

    async fn reserve_child(
        &self,
        mutation: &ReserveChildMutation,
    ) -> Result<ChildExecutionReceipt, DependencyFailure>;

    async fn invoke_child(
        &self,
        mutation: &InvokeChildMutation,
    ) -> Result<ChildExecutionReceipt, DependencyFailure>;

    async fn complete_child(
        &self,
        mutation: &CompleteChildMutation,
    ) -> Result<ChildExecutionReceipt, DependencyFailure>;

    async fn cancel_child(
        &self,
        mutation: &CancelChildMutation,
    ) -> Result<ChildExecutionReceipt, DependencyFailure>;

    async fn begin_action(&self, intent: &ActionIntent)
    -> Result<ActionReceipt, DependencyFailure>;

    async fn observe_action(
        &self,
        observation: &DurableActionObservation,
    ) -> Result<ActionReceipt, DependencyFailure>;

    async fn finalize_action(
        &self,
        mutation: FinalizeActionMutation,
    ) -> Result<ActionFinalizationReceipt, DependencyFailure>;

    async fn mark_action_ambiguous(
        &self,
        mutation: MarkActionAmbiguous,
    ) -> Result<(), DependencyFailure>;

    async fn load_action_result(
        &self,
        run: &RunIdentity,
        action_key: &str,
        expected_hash: &ContentHash,
    ) -> Result<Option<Vec<u8>>, DependencyFailure>;

    async fn commit_final(
        &self,
        final_value: &DurableFinal,
    ) -> Result<FinalStatus, DependencyFailure>;
}

#[derive(Clone, PartialEq, Eq)]
pub struct RecoveredStateCheckpoint {
    pub recovery_schema_hash: ContentHash,
    pub provider_checkpoint_seq: u64,
    pub action_frontier_seq: u64,
    pub action_frontier_hash: ContentHash,
    pub state_hash: ContentHash,
    pub state_bytes: Vec<u8>,
}

impl fmt::Debug for RecoveredStateCheckpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RecoveredStateCheckpoint")
            .field("recovery_schema_hash", &self.recovery_schema_hash)
            .field("provider_checkpoint_seq", &self.provider_checkpoint_seq)
            .field("action_frontier_seq", &self.action_frontier_seq)
            .field("action_frontier_hash", &self.action_frontier_hash)
            .field("state_hash", &self.state_hash)
            .field("state_bytes", &"[REDACTED]")
            .field("state_byte_len", &self.state_bytes.len())
            .finish()
    }
}

impl Drop for RecoveredStateCheckpoint {
    fn drop(&mut self) {
        self.state_bytes.zeroize();
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct RecoveredEpisode {
    pub checkpoint_seq: u64,
    pub episode_hash: ContentHash,
    pub episode_bytes: Vec<u8>,
}

impl fmt::Debug for RecoveredEpisode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RecoveredEpisode")
            .field("checkpoint_seq", &self.checkpoint_seq)
            .field("episode_hash", &self.episode_hash)
            .field("episode_bytes", &"[REDACTED]")
            .field("episode_byte_len", &self.episode_bytes.len())
            .finish()
    }
}

impl Drop for RecoveredEpisode {
    fn drop(&mut self) {
        self.episode_bytes.zeroize();
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct RecoveredAction {
    pub action_key: String,
    pub request_hash: ContentHash,
    pub episode_hash: ContentHash,
    pub tool_call_id: String,
    pub capability_id: String,
    pub input_schema_hash: ContentHash,
    pub output_schema_hash: ContentHash,
    pub data_release_hash: ContentHash,
    pub retryable_read: bool,
    pub stage: ActionStage,
    pub result_hash: Option<ContentHash>,
    pub result_bytes: Option<Vec<u8>>,
}

impl fmt::Debug for RecoveredAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RecoveredAction")
            .field("action_key", &self.action_key)
            .field("request_hash", &self.request_hash)
            .field("episode_hash", &self.episode_hash)
            .field("tool_call_id", &self.tool_call_id)
            .field("capability_id", &self.capability_id)
            .field("input_schema_hash", &self.input_schema_hash)
            .field("output_schema_hash", &self.output_schema_hash)
            .field("data_release_hash", &self.data_release_hash)
            .field("retryable_read", &self.retryable_read)
            .field("stage", &self.stage)
            .field("result_hash", &self.result_hash)
            .field(
                "result_bytes",
                &self.result_bytes.as_ref().map(|_| "[REDACTED]"),
            )
            .field("result_byte_len", &self.result_bytes.as_ref().map(Vec::len))
            .finish()
    }
}

impl Drop for RecoveredAction {
    fn drop(&mut self) {
        if let Some(bytes) = &mut self.result_bytes {
            bytes.zeroize();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableRecoverySnapshot {
    pub state: Option<RecoveredStateCheckpoint>,
    pub episodes: Vec<RecoveredEpisode>,
    pub actions: Vec<RecoveredAction>,
    pub child: Option<ChildExecutionReceipt>,
    pub current_provider_checkpoint_seq: u64,
    pub current_action_frontier_seq: u64,
    pub current_action_frontier_hash: ContentHash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoverySnapshot {
    Fresh,
    Durable(Box<DurableRecoverySnapshot>),
}

#[derive(Serialize)]
struct ActionFingerprint<'a> {
    version: u16,
    run_id: &'a str,
    agent_image_hash: &'a ContentHash,
    capability_id: &'a str,
    input_contract_id: &'a str,
    input_schema_hash: &'a ContentHash,
    output_schema_hash: &'a ContentHash,
    server_build: &'a str,
    server_schema_bundle_hash: &'a ContentHash,
    data_release_hash: &'a ContentHash,
    arguments: &'a Value,
}

pub fn deterministic_action_key(
    run_id: &str,
    agent_image_hash: &ContentHash,
    capability: &CapabilitySpec,
    contracts: &ResolvedCapabilityContracts,
    binding: &CapabilityBinding,
    arguments: &Value,
) -> Result<String, DependencyFailure> {
    let fingerprint = ActionFingerprint {
        version: 2,
        run_id,
        agent_image_hash,
        capability_id: &capability.id,
        input_contract_id: &capability.input_contract,
        input_schema_hash: &contracts.input.content_hash,
        output_schema_hash: &contracts.output_contract_set_hash,
        server_build: &binding.server_build,
        server_schema_bundle_hash: &binding.server_schema_bundle_hash,
        data_release_hash: &binding.data_release_hash,
        arguments,
    };
    serde_jcs::to_vec(&fingerprint)
        .map(|bytes| ContentHash::sha256(bytes).to_string())
        .map_err(|error| {
            // Practically unreachable: the fingerprint is strings, hashes,
            // and a JSON value that already round-tripped through serde.
            // Classify as a non-retryable, never-dispatched failure.
            DependencyFailure::redacted(
                "action_key_fingerprint_serialization",
                format!("{error:?}"),
                false,
                DeliveryCertainty::NotDispatched,
            )
        })
}

/// Bounded, non-authoritative runtime timing counters.
///
/// These counters intentionally live outside the durable execution state while
/// a run is active.  They are diagnostics only: workflow transitions, leases,
/// and budget admission never depend on them.  Keeping them as atomics lets a
/// provider wrapper and the engine record the same run's local stages without
/// introducing a lock or an unbounded metrics label set.
#[derive(Debug, Default)]
pub struct RuntimeStageTimings {
    provider_queue_wait_ms: AtomicU64,
    session_memory_total_ms: AtomicU64,
    market_preflight_ms: AtomicU64,
    prompt_build_total_ms: AtomicU64,
    checkpoint_total_ms: AtomicU64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeStageTimingSnapshot {
    pub provider_queue_wait_ms: u64,
    pub session_memory_total_ms: u64,
    pub market_preflight_ms: u64,
    pub prompt_build_total_ms: u64,
    pub checkpoint_total_ms: u64,
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

fn add(counter: &AtomicU64, duration: Duration) {
    let amount = millis(duration);
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(amount))
    });
}

impl RuntimeStageTimings {
    pub fn add_provider_queue_wait(&self, duration: Duration) {
        add(&self.provider_queue_wait_ms, duration);
    }

    pub fn add_session_memory(&self, duration: Duration) {
        add(&self.session_memory_total_ms, duration);
    }

    pub fn add_market_preflight(&self, duration: Duration) {
        add(&self.market_preflight_ms, duration);
    }

    pub fn add_prompt_build(&self, duration: Duration) {
        add(&self.prompt_build_total_ms, duration);
    }

    pub fn add_checkpoint(&self, duration: Duration) {
        add(&self.checkpoint_total_ms, duration);
    }

    pub fn snapshot(&self) -> RuntimeStageTimingSnapshot {
        RuntimeStageTimingSnapshot {
            provider_queue_wait_ms: self.provider_queue_wait_ms.load(Ordering::Relaxed),
            session_memory_total_ms: self.session_memory_total_ms.load(Ordering::Relaxed),
            market_preflight_ms: self.market_preflight_ms.load(Ordering::Relaxed),
            prompt_build_total_ms: self.prompt_build_total_ms.load(Ordering::Relaxed),
            checkpoint_total_ms: self.checkpoint_total_ms.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_saturate_and_snapshot_without_a_lock() {
        let timings = RuntimeStageTimings::default();
        timings.add_provider_queue_wait(Duration::from_millis(7));
        timings.add_session_memory(Duration::from_millis(11));
        timings.add_market_preflight(Duration::from_millis(13));
        timings.add_prompt_build(Duration::from_millis(17));
        timings.add_checkpoint(Duration::from_millis(19));
        assert_eq!(
            timings.snapshot(),
            RuntimeStageTimingSnapshot {
                provider_queue_wait_ms: 7,
                session_memory_total_ms: 11,
                market_preflight_ms: 13,
                prompt_build_total_ms: 17,
                checkpoint_total_ms: 19,
            }
        );
    }
}
