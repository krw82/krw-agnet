//! Stable, implementation-neutral contracts shared by the daemon, host, and fixtures.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::Zeroize;

// v7 adds explicit constrained-output facts and pins provider context
// capacity into the execution contract. v6 separated a per-capability
// immutable data-release pin from the
// deployment-wide resolved binding fingerprint.  They are both hashes, but
// they prove different facts and must never be compared as one domain.
pub const PROTOCOL_VERSION: u16 = 7;
pub const CLAIM_PAYLOAD_SCHEMA_VERSION: u16 = 7;
pub const PUBLIC_RELEASE_DESCRIPTOR_SCHEMA_VERSION: u16 = 3;
pub const DEEPSEEK_MODEL_ID: &str = "deepseek-v4-flash";
pub const GLM_MODEL_ID: &str = "glm-5.3";
/// Closed set of model ids the protocol admits. Adding a new provider means
/// extending this slice, never bypassing it: every resolved profile must be
/// backed by one of these exact ids so that downstream codecs can dispatch on
/// `model_id` without a hidden fallback.
pub const ALLOWED_MODEL_IDS: &[&str] = &[DEEPSEEK_MODEL_ID, GLM_MODEL_ID];
pub const FLASH_HIGH_PROFILE_ID: &str = "flash_high";
pub const FLASH_MAX_PROFILE_ID: &str = "flash_max";
pub const FLASH_DIRECT_PROFILE_ID: &str = "flash_direct";
/// GLM-5.3 execution profile ids — the GLM mirror of the `flash_*` set.
pub const GLM_HIGH_PROFILE_ID: &str = "glm_high";
pub const GLM_MAX_PROFILE_ID: &str = "glm_max";
pub const GLM_DIRECT_PROFILE_ID: &str = "glm_direct";
/// Complete set of profile ids the runtime is permitted to accept. Pre
/// multi-provider this was exactly the three `flash_*` ids; GLM profiles are
/// optional (a deployment may omit GLM entirely) so the inventory check in
/// `prepare_globals` admits any subset that is contained in this list.
pub const ALLOWED_PROFILE_IDS: &[&str] = &[
    FLASH_HIGH_PROFILE_ID,
    FLASH_MAX_PROFILE_ID,
    FLASH_DIRECT_PROFILE_ID,
    GLM_HIGH_PROFILE_ID,
    GLM_MAX_PROFILE_ID,
    GLM_DIRECT_PROFILE_ID,
];
pub const MAX_RUN_CONTEXT_TICKERS: usize = 50;
pub const MAX_SELECTED_FEED_ITEM_IDS: usize = 8;
pub const MAX_NOTEBOOK_CONVERSATIONS: usize = 64;
pub const MAX_EXISTING_ANSWER_SOURCE_UNITS: usize = 64;
const MAX_ROUTING_INPUT_BYTES: usize = 256 * 1024;
const MAX_NOTEBOOK_INPUT_BYTES: usize = 1024 * 1024;
const MAX_COMMITTED_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_SESSION_MEMORY_VIEW_BYTES: usize = 256 * 1024;
const MAX_TYPED_CARRIER_ITEMS: usize = 16_384;
const MAX_TYPED_CARRIER_DEPTH: u8 = 32;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContentHash(String);

impl ContentHash {
    pub fn sha256(bytes: impl AsRef<[u8]>) -> Self {
        let digest = Sha256::digest(bytes.as_ref());
        Self(format!("sha256:{}", hex::encode(digest)))
    }

    pub fn parse(value: impl Into<String>) -> Result<Self, ContractError> {
        let value = value.into();
        let Some(hex_part) = value.strip_prefix("sha256:") else {
            return Err(ContractError::InvalidHash(value));
        };
        if hex_part.len() != 64 || !hex_part.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ContractError::InvalidHash(value));
        }
        Ok(Self(value.to_ascii_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ContentHash {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Versioned codec separating stable logical capability ids from provider
/// function-name grammar. `AgentSpec` ids may contain dots while `DeepSeek`
/// function names may not. Keep a bounded human-readable stem: a model has to
/// choose and fill these functions, so a completely opaque hash materially
/// harms tool-use quality. The digest suffix makes lossy punctuation
/// replacement unambiguous within a compiled tool frontier; the compiler also
/// rejects a collision before a run can start.
pub const PROVIDER_TOOL_NAME_CODEC_VERSION: &str = "krw-agent/provider-tool-name/v2";
const MAX_PROVIDER_TOOL_NAME_BYTES: usize = 64;
const PROVIDER_TOOL_PREFIX: &str = "krw_";
const PROVIDER_TOOL_DIGEST_BYTES: usize = 16;
const PROVIDER_TOOL_SEPARATOR: &str = "__";

pub fn provider_tool_name(capability_id: &str) -> String {
    let digest = ContentHash::sha256(format!(
        "{PROVIDER_TOOL_NAME_CODEC_VERSION}\0{capability_id}"
    ));
    let digest = digest
        .as_str()
        .strip_prefix("sha256:")
        .expect("ContentHash always has sha256 prefix");
    let maximum_stem = MAX_PROVIDER_TOOL_NAME_BYTES
        .checked_sub(
            PROVIDER_TOOL_PREFIX.len() + PROVIDER_TOOL_SEPARATOR.len() + PROVIDER_TOOL_DIGEST_BYTES,
        )
        .expect("fixed provider tool-name layout fits the provider limit");
    // Capability ids are already ASCII-constrained by AgentImage validation.
    // This nevertheless stays total so no caller can turn an unexpected byte
    // into an invalid provider function name.
    let mut stem = capability_id
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || byte == b'_' {
                char::from(byte)
            } else {
                '_'
            }
        })
        .collect::<String>();
    if stem.is_empty() {
        stem.push_str("capability");
    }
    stem.truncate(maximum_stem);
    format!(
        "{PROVIDER_TOOL_PREFIX}{stem}{PROVIDER_TOOL_SEPARATOR}{}",
        &digest[..PROVIDER_TOOL_DIGEST_BYTES]
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetLimits {
    pub max_provider_turns: u16,
    pub max_capability_calls: u16,
    pub max_replans: u8,
    pub max_repairs: u8,
    pub max_input_tokens: u32,
    pub max_output_tokens: u32,
    pub max_evidence_bytes: u64,
    pub deadline_ms: u64,
    #[serde(default)]
    pub capability_call_limits: BTreeMap<String, u16>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetUsage {
    pub provider_turns: u16,
    pub capability_calls: u16,
    pub replans: u8,
    pub repairs: u8,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub evidence_bytes: u64,
    /// Cumulative wall-clock time spent inside provider turns for the run.
    /// Defaults to 0 so persisted bundles written before this field existed
    /// still deserialize.
    #[serde(default)]
    pub provider_total_ms: u64,
    /// Cumulative wall-clock time spent in the capability lifecycle path
    /// (authorization, `begin_action`, tool dispatch, restore, and episode
    /// commit) — not just the MCP tool call itself. Useful for relative
    /// comparison across runs; the MCP-dispatch-only portion is observed by
    /// the `krw_capability_duration_seconds` histogram.
    #[serde(default)]
    pub capability_total_ms: u64,
    /// Cumulative wall-clock time spent inside phase compaction.
    #[serde(default)]
    pub compact_total_ms: u64,
    /// Time waiting for a model concurrency slot, excluding the model call.
    #[serde(default)]
    pub provider_queue_wait_ms: u64,
    /// Time spent reconstructing and validating same-room session context.
    #[serde(default)]
    pub session_memory_total_ms: u64,
    /// Time spent in the best-effort market preflight.
    #[serde(default)]
    pub market_preflight_ms: u64,
    /// Time spent assembling and canonicalizing a provider prompt.
    #[serde(default)]
    pub prompt_build_total_ms: u64,
    /// Time spent awaiting durable episode/run-state checkpoints.
    #[serde(default)]
    pub checkpoint_total_ms: u64,
}

impl BudgetUsage {
    pub fn ensure_within(&self, limits: &BudgetLimits) -> Result<(), ContractError> {
        let checks = [
            (
                u64::from(self.provider_turns),
                u64::from(limits.max_provider_turns),
                "provider_turns",
            ),
            (
                u64::from(self.capability_calls),
                u64::from(limits.max_capability_calls),
                "capability_calls",
            ),
            (
                u64::from(self.replans),
                u64::from(limits.max_replans),
                "replans",
            ),
            (
                u64::from(self.repairs),
                u64::from(limits.max_repairs),
                "repairs",
            ),
            (
                u64::from(self.input_tokens),
                u64::from(limits.max_input_tokens),
                "input_tokens",
            ),
            (
                u64::from(self.output_tokens),
                u64::from(limits.max_output_tokens),
                "output_tokens",
            ),
            (
                self.evidence_bytes,
                limits.max_evidence_bytes,
                "evidence_bytes",
            ),
        ];
        if let Some((used, limit, resource)) =
            checks.into_iter().find(|(used, limit, _)| used > limit)
        {
            return Err(ContractError::BudgetExceeded {
                resource,
                used,
                limit,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthScope {
    Public,
    Tenant,
    Principal,
    Run,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransportKind {
    McpHttp,
    McpStdio,
    Native,
}

/// Controls whether one initialized MCP tool session may cross a run
/// boundary. This is a deployment property, not an agent or prompt choice.
///
/// `AttestedStatelessV1` is intentionally versioned. Selecting it only makes
/// cross-run pooling eligible; the transport still has to verify the matching
/// readiness and MCP initialize attestations before admitting the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum McpToolSessionReuse {
    RunScoped,
    AttestedStatelessV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityBinding {
    /// Physical deployment key shared by one or more logical `AgentImage`
    /// capabilities. Logical capability ids never need duplicate endpoints,
    /// credentials, pools, or readiness state.
    pub binding_key: String,
    /// Exact physical MCP method selected by this deployment binding.  It is
    /// deliberately distinct from `binding_key`: the latter chooses an
    /// endpoint/pool/credential tuple, while this field is the ABI symbol on
    /// that endpoint.  Changing either changes the frozen execution contract.
    pub mcp_tool_name: String,
    pub transport: TransportKind,
    pub endpoint_ref: String,
    pub credential_ref: Option<String>,
    pub auth_scope: AuthScope,
    /// Required, fail-closed MCP session reuse policy. There is deliberately
    /// no serde default: every physical deployment must make this boundary
    /// explicit.
    pub tool_session_reuse: McpToolSessionReuse,
    /// Readiness fingerprint for the remote server's complete schema bundle.
    /// Tool input/output contract hashes are pinned independently in `AgentImage`.
    pub server_schema_bundle_hash: ContentHash,
    pub server_build: String,
    pub data_release_hash: ContentHash,
    pub max_connections: u16,
    pub request_timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentBinding {
    pub schema_version: u16,
    pub deployment_id: String,
    pub capabilities: Vec<CapabilityBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelDescriptor {
    pub model_id: String,
    pub api_base: String,
    pub api_version: String,
    pub max_context_tokens: u32,
    pub max_output_tokens: u32,
    pub max_in_flight: u16,
    /// Closed provider-wire feature matrix. This is deployment data, not a
    /// kernel assumption: a semantic state decision may be represented by
    /// different legal HTTP fields for different models or thinking modes.
    pub provider_wire_capabilities: ProviderWireCapabilities,
}

/// Features supported by one exact provider mode. `supported` is separate
/// from the output-channel flags because a model may support a mode while not
/// supporting tools or JSON output in that mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)]
pub struct ProviderWireModeCapabilities {
    pub supported: bool,
    pub supports_tools: bool,
    pub supports_tool_choice: bool,
    pub supports_json_object: bool,
    /// Exact JSON Schema output through the active provider wire protocol.
    #[serde(default)]
    pub supports_json_schema_output: bool,
    /// Provider-enforced input schema for an individual advertised tool.
    #[serde(default)]
    pub supports_strict_tool_input: bool,
}

/// Immutable, provider-facing feature facts for one exact model. The kernel
/// pins this matrix into every execution snapshot and asks the provider codec
/// to encode a semantic decision only through a legal wire channel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderWireCapabilities {
    pub thinking: ProviderWireModeCapabilities,
    pub non_thinking: ProviderWireModeCapabilities,
    /// Anthropic Messages API carries reasoning as a `thinking` content block;
    /// when set, the episode assembler must replay that block during recovery.
    pub requires_thinking_block_replay: bool,
    /// OpenAI-vocab legacy flag retained for run-engine `AssistantMessage`
    /// reconstruction compatibility: the provider may emit assistant content
    /// alongside tool calls (`DeepSeek` V4 does, GLM-5.3 does not).
    pub requires_assistant_content_for_tool_calls: bool,
}

impl ProviderWireCapabilities {
    /// `DeepSeek`'s compatibility profile is represented by explicit facts
    /// rather than scattered provider-specific conditionals. YAML must still
    /// carry this full matrix; this constant is used for validation and
    /// deterministic compatibility fixtures only.
    pub const fn deepseek_v4_flash() -> Self {
        Self {
            thinking: ProviderWireModeCapabilities {
                supported: true,
                supports_tools: true,
                // DeepSeek's Anthropic compatibility accepts `any`/`tool`;
                // emitting the semantic requirement avoids a free-text turn
                // when the workflow is at a capability frontier.
                supports_tool_choice: true,
                supports_json_object: true,
                supports_json_schema_output: false,
                supports_strict_tool_input: false,
            },
            non_thinking: ProviderWireModeCapabilities {
                supported: true,
                supports_tools: true,
                supports_tool_choice: true,
                supports_json_object: true,
                supports_json_schema_output: false,
                supports_strict_tool_input: false,
            },
            requires_thinking_block_replay: true,
            // DeepSeek's thinking-mode tool-call examples allow an empty
            // assistant content field; the reasoning/thinking block is the
            // replay requirement, not visible assistant text.
            requires_assistant_content_for_tool_calls: false,
        }
    }

    /// GLM-5.3 wire facts. Like `DeepSeek` V4 Flash it carries reasoning content
    /// on the thinking channel, but unlike `DeepSeek` its tool-channel accepts
    /// `tool_choice` in both thinking and non-thinking modes and keeps the
    /// same JSON-object support. Used for validation and deterministic test
    /// fixtures only; YAML must still carry the full matrix.
    pub const fn glm_5_3() -> Self {
        Self {
            thinking: ProviderWireModeCapabilities {
                supported: true,
                supports_tools: true,
                // GLM-5.3 accepts tool_choice=required in both modes, and
                // emitting it improves tool-call reliability for capability
                // states (the model is less likely to skip a required tool).
                supports_tool_choice: true,
                supports_json_object: true,
                supports_json_schema_output: false,
                supports_strict_tool_input: false,
            },
            non_thinking: ProviderWireModeCapabilities {
                supported: true,
                supports_tools: true,
                supports_tool_choice: true,
                supports_json_object: true,
                supports_json_schema_output: false,
                supports_strict_tool_input: false,
            },
            // GLM-5.3 may omit the thinking block on tool-only turns even when
            // thinking is enabled. The run engine still injects a bounded
            // wire-only replay placeholder before a later request, but the
            // provider episode itself must not be rejected for this omission.
            requires_thinking_block_replay: false,
            requires_assistant_content_for_tool_calls: false,
        }
    }

    pub const fn for_thinking(self, thinking: ThinkingMode) -> ProviderWireModeCapabilities {
        match thinking {
            ThinkingMode::Enabled => self.thinking,
            ThinkingMode::Disabled => self.non_thinking,
        }
    }

    /// Reject internally contradictory capability declarations at startup.
    /// The codec never treats a missing feature as permission to guess.
    pub const fn is_well_formed(self) -> bool {
        (!self.thinking.supports_tool_choice || self.thinking.supports_tools)
            && (!self.non_thinking.supports_tool_choice || self.non_thinking.supports_tools)
            && (!self.thinking.supports_strict_tool_input || self.thinking.supports_tools)
            && (!self.non_thinking.supports_strict_tool_input || self.non_thinking.supports_tools)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingMode {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    High,
    Max,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelExecutionProfile {
    pub profile_id: String,
    pub model_id: String,
    pub thinking: ThinkingMode,
    pub reasoning_effort: Option<ReasoningEffort>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRegistry {
    pub schema_version: u16,
    pub registry_id: String,
    pub models: Vec<ModelDescriptor>,
    pub profiles: Vec<ModelExecutionProfile>,
}

impl ModelRegistry {
    pub fn resolve_profile_exact(
        &self,
        profile_id: &str,
        requested: &str,
    ) -> Result<(&ModelExecutionProfile, &ModelDescriptor), ContractError> {
        if !ALLOWED_MODEL_IDS.contains(&requested) {
            return Err(ContractError::UnknownModel(requested.to_owned()));
        }
        let profile = self
            .profiles
            .iter()
            .find(|profile| profile.profile_id == profile_id)
            .ok_or_else(|| ContractError::UnknownModelProfile(profile_id.to_owned()))?;
        if !ALLOWED_MODEL_IDS
            .iter()
            .any(|allowed| *allowed == profile.model_id)
        {
            return Err(ContractError::UnknownModel(profile.model_id.clone()));
        }
        let model = self
            .models
            .iter()
            .find(|model| model.model_id == profile.model_id)
            .ok_or_else(|| ContractError::UnknownModel(profile.model_id.clone()))?;
        if profile.model_id != requested {
            return Err(ContractError::ModelMismatch {
                requested: requested.to_owned(),
                resolved: profile.model_id.clone(),
            });
        }
        Ok((profile, model))
    }

    pub fn model(&self, model_id: &str) -> Option<&ModelDescriptor> {
        self.models.iter().find(|model| model.model_id == model_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunContextKind {
    CompanyTickerSet,
    CoveredUniverse,
    SelectedFeedItems,
    SourceFiling,
    ResearchNotebook,
    ExistingAnswer,
    RoutingRequest,
    QuestionOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoveredUniverseMarker {
    Covered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NotebookUpdateMode {
    MergeResearchAnswer,
    ExtractOpenQuestions,
    AppendNote,
    ReorganizeNotebook,
}

/// Claim-pinned reference to the exact committed answer material from which a
/// display plan may be derived. `canonical_source` is only a bounded carrier:
/// the run engine must validate it against `canonical-display-source/v1`
/// before it can enter a provider prompt or accept a display plan.
/// The host must materialize this envelope from its durable final row; client
/// JSON is never an authority for `final_receipt_hash` or the source hashes.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommittedAnswerSourceV1 {
    pub schema_version: u16,
    pub final_receipt_hash: ContentHash,
    pub answer_bundle_hash: ContentHash,
    pub answer_ir_hash: ContentHash,
    pub canonical_source_hash: ContentHash,
    pub source_unit_hashes: BTreeMap<String, ContentHash>,
    pub canonical_source: serde_json::Value,
}

impl CommittedAnswerSourceV1 {
    pub fn validate_carrier(&self) -> Result<(), ContractError> {
        if self.schema_version != 1
            || self.source_unit_hashes.is_empty()
            || self.source_unit_hashes.len() > MAX_EXISTING_ANSWER_SOURCE_UNITS
            || self
                .source_unit_hashes
                .keys()
                .any(|id| !is_bounded_product_id(id))
        {
            return Err(ContractError::InvalidRunContext(
                "committed answer reference is outside the protocol bound",
            ));
        }
        validate_typed_carrier(&self.canonical_source, MAX_COMMITTED_SOURCE_BYTES)?;
        let canonical = serde_jcs::to_vec(&self.canonical_source).map_err(|_| {
            ContractError::InvalidRunContext("committed answer source is not canonicalizable")
        })?;
        if ContentHash::sha256(canonical) != self.canonical_source_hash {
            return Err(ContractError::InvalidRunContext(
                "committed answer source hash does not match its payload",
            ));
        }
        Ok(())
    }
}

impl std::fmt::Debug for CommittedAnswerSourceV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CommittedAnswerSourceV1")
            .field("schema_version", &self.schema_version)
            .field("final_receipt_hash", &self.final_receipt_hash)
            .field("answer_bundle_hash", &self.answer_bundle_hash)
            .field("answer_ir_hash", &self.answer_ir_hash)
            .field("canonical_source_hash", &self.canonical_source_hash)
            .field("source_unit_count", &self.source_unit_hashes.len())
            .field("canonical_source", &"[REDACTED]")
            .finish()
    }
}

/// Claim-pinned, context-only view of prior completed turns. The protocol owns
/// only the bounded carrier and hashes; `krw-session-memory` owns the exact
/// typed schema and relevance rules. The run engine must deserialize and
/// validate that schema before any byte enters a provider request. Session
/// memory never enters the evidence ledger and cannot authorize a claim.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionMemoryCarrierV3 {
    pub schema_version: u16,
    pub view_hash: ContentHash,
    pub source_frontier_hash: ContentHash,
    pub source_revision: u64,
    pub canonical_view: serde_json::Value,
}

impl SessionMemoryCarrierV3 {
    pub fn validate_carrier(&self) -> Result<(), ContractError> {
        if self.schema_version != 3 || self.source_revision == 0 {
            return Err(ContractError::InvalidSessionMemory(
                "unsupported session memory carrier",
            ));
        }
        validate_typed_carrier(&self.canonical_view, MAX_SESSION_MEMORY_VIEW_BYTES)?;
        let canonical = serde_jcs::to_vec(&self.canonical_view).map_err(|_| {
            ContractError::InvalidSessionMemory("session memory is not canonicalizable")
        })?;
        if ContentHash::sha256(canonical) != self.view_hash {
            return Err(ContractError::InvalidSessionMemory(
                "session memory view hash does not match its payload",
            ));
        }
        Ok(())
    }

    pub fn zeroize_sensitive(&mut self) {
        scrub_json(&mut self.canonical_view);
    }
}

impl std::fmt::Debug for SessionMemoryCarrierV3 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionMemoryCarrierV3")
            .field("schema_version", &self.schema_version)
            .field("view_hash", &self.view_hash)
            .field("source_frontier_hash", &self.source_frontier_hash)
            .field("source_revision", &self.source_revision)
            .field("canonical_view", &"[REDACTED]")
            .finish()
    }
}

/// Immutable, host-authored run scope. The internally tagged, closed shape is
/// deliberately distinct from the untrusted natural-language question.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunContextV1 {
    CompanyTickerSet {
        tickers: Vec<String>,
    },
    CoveredUniverse {
        universe: CoveredUniverseMarker,
    },
    SelectedFeedItems {
        feed_item_ids: Vec<String>,
    },
    SourceFiling {
        filing_event_id: String,
    },
    ResearchNotebook {
        ticker: String,
        input_hash: ContentHash,
        typed_input: serde_json::Value,
    },
    ExistingAnswer {
        committed_source: CommittedAnswerSourceV1,
    },
    RoutingRequest {
        input_hash: ContentHash,
        typed_input: serde_json::Value,
    },
    QuestionOnly {},
}

impl RunContextV1 {
    pub const fn kind(&self) -> RunContextKind {
        match self {
            Self::CompanyTickerSet { .. } => RunContextKind::CompanyTickerSet,
            Self::CoveredUniverse { .. } => RunContextKind::CoveredUniverse,
            Self::SelectedFeedItems { .. } => RunContextKind::SelectedFeedItems,
            Self::SourceFiling { .. } => RunContextKind::SourceFiling,
            Self::ResearchNotebook { .. } => RunContextKind::ResearchNotebook,
            Self::ExistingAnswer { .. } => RunContextKind::ExistingAnswer,
            Self::RoutingRequest { .. } => RunContextKind::RoutingRequest,
            Self::QuestionOnly {} => RunContextKind::QuestionOnly,
        }
    }

    /// Number governed by an entrypoint's exact/max cardinality policy.
    /// Covered-universe cardinality is enforced against the model's bounded
    /// discovery request, while the immutable context itself is only a marker.
    pub fn scoped_value_count(&self) -> usize {
        match self {
            Self::CompanyTickerSet { tickers } => tickers.len(),
            Self::SelectedFeedItems { feed_item_ids } => feed_item_ids.len(),
            Self::SourceFiling { .. } | Self::ResearchNotebook { .. } => 1,
            Self::ExistingAnswer { committed_source } => committed_source.source_unit_hashes.len(),
            Self::CoveredUniverse { .. } | Self::RoutingRequest { .. } | Self::QuestionOnly {} => 0,
        }
    }

    pub fn trusted_tickers(&self) -> &[String] {
        match self {
            Self::CompanyTickerSet { tickers } => tickers,
            Self::ResearchNotebook { ticker, .. } => std::slice::from_ref(ticker),
            Self::CoveredUniverse { .. }
            | Self::SelectedFeedItems { .. }
            | Self::SourceFiling { .. }
            | Self::ExistingAnswer { .. }
            | Self::RoutingRequest { .. }
            | Self::QuestionOnly {} => &[],
        }
    }

    pub fn selected_feed_item_ids(&self) -> &[String] {
        match self {
            Self::SelectedFeedItems { feed_item_ids } => feed_item_ids,
            _ => &[],
        }
    }

    pub fn filing_event_id(&self) -> Option<&str> {
        match self {
            Self::SourceFiling { filing_event_id } => Some(filing_event_id),
            _ => None,
        }
    }

    pub const fn is_covered_universe(&self) -> bool {
        matches!(self, Self::CoveredUniverse { .. })
    }

    pub fn validate(&self) -> Result<(), ContractError> {
        match self {
            Self::CompanyTickerSet { tickers } => {
                if tickers.is_empty() || tickers.len() > MAX_RUN_CONTEXT_TICKERS {
                    return Err(ContractError::InvalidRunContext(
                        "company ticker count is outside the protocol bound",
                    ));
                }
                if !all_unique(tickers) {
                    return Err(ContractError::InvalidRunContext(
                        "company ticker set contains duplicates",
                    ));
                }
                if !tickers.iter().all(|ticker| is_canonical_ticker(ticker)) {
                    return Err(ContractError::InvalidRunContext(
                        "company ticker is not canonical uppercase ASCII",
                    ));
                }
            }
            Self::CoveredUniverse { universe } => {
                if *universe != CoveredUniverseMarker::Covered {
                    return Err(ContractError::InvalidRunContext(
                        "covered-universe marker is invalid",
                    ));
                }
            }
            Self::SelectedFeedItems { feed_item_ids } => {
                if feed_item_ids.is_empty() || feed_item_ids.len() > MAX_SELECTED_FEED_ITEM_IDS {
                    return Err(ContractError::InvalidRunContext(
                        "selected feed item count is outside the protocol bound",
                    ));
                }
                if !all_unique(feed_item_ids) {
                    return Err(ContractError::InvalidRunContext(
                        "selected feed item IDs contain duplicates",
                    ));
                }
                if !feed_item_ids.iter().all(|value| is_canonical_uuid(value)) {
                    return Err(ContractError::InvalidRunContext(
                        "selected feed item ID is not a canonical UUID",
                    ));
                }
            }
            Self::SourceFiling { filing_event_id } => {
                if !is_canonical_uuid(filing_event_id) {
                    return Err(ContractError::InvalidRunContext(
                        "filing event ID is not a canonical UUID",
                    ));
                }
            }
            Self::ResearchNotebook {
                ticker,
                input_hash,
                typed_input,
            } => {
                if !is_canonical_ticker(ticker) {
                    return Err(ContractError::InvalidRunContext(
                        "notebook ticker is not canonical uppercase ASCII",
                    ));
                }
                validate_typed_carrier(typed_input, MAX_NOTEBOOK_INPUT_BYTES)?;
                let canonical = serde_jcs::to_vec(typed_input).map_err(|_| {
                    ContractError::InvalidRunContext("notebook input is not canonicalizable")
                })?;
                if ContentHash::sha256(canonical) != *input_hash {
                    return Err(ContractError::InvalidRunContext(
                        "notebook input hash does not match its payload",
                    ));
                }
            }
            Self::ExistingAnswer { committed_source } => {
                committed_source.validate_carrier()?;
            }
            Self::RoutingRequest {
                input_hash,
                typed_input,
            } => {
                validate_typed_carrier(typed_input, MAX_ROUTING_INPUT_BYTES)?;
                let canonical = serde_jcs::to_vec(typed_input).map_err(|_| {
                    ContractError::InvalidRunContext("routing input is not canonicalizable")
                })?;
                if ContentHash::sha256(canonical) != *input_hash {
                    return Err(ContractError::InvalidRunContext(
                        "routing input hash does not match its payload",
                    ));
                }
            }
            Self::QuestionOnly {} => {}
        }
        Ok(())
    }

    pub fn zeroize_sensitive(&mut self) {
        match self {
            Self::CompanyTickerSet { tickers } => tickers.zeroize(),
            Self::SelectedFeedItems { feed_item_ids } => feed_item_ids.zeroize(),
            Self::SourceFiling { filing_event_id } => filing_event_id.zeroize(),
            Self::ResearchNotebook {
                ticker,
                typed_input,
                ..
            } => {
                ticker.zeroize();
                scrub_json(typed_input);
            }
            Self::ExistingAnswer { committed_source } => {
                scrub_json(&mut committed_source.canonical_source);
                for id in std::mem::take(&mut committed_source.source_unit_hashes).into_keys() {
                    let mut id = id;
                    id.zeroize();
                }
            }
            Self::RoutingRequest { typed_input, .. } => scrub_json(typed_input),
            Self::CoveredUniverse { .. } | Self::QuestionOnly {} => {}
        }
    }
}

impl std::fmt::Debug for RunContextV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RunContextV1")
            .field("kind", &self.kind())
            .field("scoped_value_count", &self.scoped_value_count())
            .finish_non_exhaustive()
    }
}

impl Drop for RunContextV1 {
    fn drop(&mut self) {
        self.zeroize_sensitive();
    }
}

fn all_unique(values: &[String]) -> bool {
    let mut seen = std::collections::BTreeSet::new();
    values.iter().all(|value| seen.insert(value.as_str()))
}

fn is_bounded_product_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 128
        && bytes[0].is_ascii_alphanumeric()
        && bytes.iter().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'/' | b'-')
        })
}

fn validate_typed_carrier(
    value: &serde_json::Value,
    max_canonical_bytes: usize,
) -> Result<(), ContractError> {
    let canonical = serde_jcs::to_vec(value)
        .map_err(|_| ContractError::InvalidRunContext("typed input is not canonicalizable"))?;
    if canonical.len() > max_canonical_bytes {
        return Err(ContractError::InvalidRunContext(
            "typed input exceeds the canonical byte bound",
        ));
    }
    let mut stack = vec![(value, 0_u8)];
    let mut visited = 0_usize;
    while let Some((current, depth)) = stack.pop() {
        visited = visited.saturating_add(1);
        if visited > MAX_TYPED_CARRIER_ITEMS || depth > MAX_TYPED_CARRIER_DEPTH {
            return Err(ContractError::InvalidRunContext(
                "typed input exceeds the item or depth bound",
            ));
        }
        match current {
            serde_json::Value::String(text)
                if text.len() > MAX_COMMITTED_SOURCE_BYTES || text.contains('\0') =>
            {
                return Err(ContractError::InvalidRunContext(
                    "typed input string exceeds the protocol bound",
                ));
            }
            serde_json::Value::Array(values) => {
                stack.extend(values.iter().map(|item| (item, depth.saturating_add(1))));
            }
            serde_json::Value::Object(values) => {
                if values
                    .keys()
                    .any(|key| key.len() > 128 || key.contains('\0'))
                {
                    return Err(ContractError::InvalidRunContext(
                        "typed input key exceeds the protocol bound",
                    ));
                }
                stack.extend(values.values().map(|item| (item, depth.saturating_add(1))));
            }
            _ => {}
        }
    }
    Ok(())
}

fn scrub_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => text.zeroize(),
        serde_json::Value::Array(values) => values.iter_mut().for_each(scrub_json),
        serde_json::Value::Object(values) => {
            for (key, value) in values {
                let mut key = key.clone();
                key.zeroize();
                scrub_json(value);
            }
        }
        _ => {}
    }
    *value = serde_json::Value::Null;
}

pub fn is_canonical_ticker(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=32).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes.iter().all(|byte| {
            byte.is_ascii_uppercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
        })
}

pub fn is_canonical_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36
        || bytes[8] != b'-'
        || bytes[13] != b'-'
        || bytes[18] != b'-'
        || bytes[23] != b'-'
        || !matches!(bytes[14], b'1'..=b'5')
        || !matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
    {
        return false;
    }
    bytes.iter().enumerate().all(|(index, byte)| {
        matches!(index, 8 | 13 | 18 | 23) || byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')
    })
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRequest {
    pub run_id: String,
    pub session_id: String,
    pub tenant_id: String,
    pub principal_id: String,
    pub run_kind: String,
    pub locale: String,
    pub question: String,
    pub requested_model: String,
    pub model_profile: String,
    pub budget: BudgetLimits,
    pub context: RunContextV1,
    #[serde(default)]
    pub session_memory: Option<SessionMemoryCarrierV3>,
}

impl RunRequest {
    pub fn zeroize_sensitive(&mut self) {
        self.run_id.zeroize();
        self.session_id.zeroize();
        self.tenant_id.zeroize();
        self.principal_id.zeroize();
        self.question.zeroize();
        self.context.zeroize_sensitive();
        if let Some(memory) = &mut self.session_memory {
            memory.zeroize_sensitive();
        }
    }
}

impl std::fmt::Debug for RunRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RunRequest")
            .field("run_id_hash", &ContentHash::sha256(&self.run_id))
            .field("session_id_hash", &ContentHash::sha256(&self.session_id))
            .field("tenant_id_hash", &ContentHash::sha256(&self.tenant_id))
            .field(
                "principal_id_hash",
                &ContentHash::sha256(&self.principal_id),
            )
            .field("run_kind", &self.run_kind)
            .field("locale", &self.locale)
            .field("question", &"[REDACTED]")
            .field("requested_model", &self.requested_model)
            .field("model_profile", &self.model_profile)
            .field("budget", &self.budget)
            .field("context", &self.context)
            .field("session_memory", &self.session_memory)
            .finish()
    }
}

impl Drop for RunRequest {
    fn drop(&mut self) {
        self.zeroize_sensitive();
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedExecutionSnapshot {
    pub protocol_version: u16,
    pub run_id: String,
    pub fencing_token: u64,
    pub cancel_generation: u64,
    pub agent_image_hash: ContentHash,
    pub deployment_binding_hash: ContentHash,
    pub model_registry_hash: ContentHash,
    pub budget_registry_hash: ContentHash,
    pub model_profile: String,
    pub requested_model: String,
    pub resolved_model: String,
    pub provider_api_version: String,
    pub provider_max_context_tokens: u32,
    pub provider_wire_capabilities: ProviderWireCapabilities,
    pub thinking: ThinkingMode,
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Logical capability id -> immutable data release manifest hash.  The
    /// resolved endpoint/TLS/build/auth fingerprint is bound separately by
    /// `deployment_binding_hash`; this map intentionally does not duplicate
    /// that broader deployment identity.
    pub capability_release_hashes: BTreeMap<String, ContentHash>,
    pub budget: BudgetLimits,
}

/// Run-independent execution facts published by the daemon from the exact
/// immutable release set it loaded. The host copies this value; it never
/// resolves models, budgets, capabilities, or provider settings itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinnedExecutionContract {
    pub protocol_version: u16,
    pub agent_image_hash: ContentHash,
    pub deployment_binding_hash: ContentHash,
    pub model_registry_hash: ContentHash,
    pub budget_registry_hash: ContentHash,
    pub model_profile: String,
    pub requested_model: String,
    pub resolved_model: String,
    pub provider_api_version: String,
    pub provider_max_context_tokens: u32,
    pub provider_wire_capabilities: ProviderWireCapabilities,
    pub thinking: ThinkingMode,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub capability_release_hashes: BTreeMap<String, ContentHash>,
    pub budget: BudgetLimits,
}

impl From<&ResolvedExecutionSnapshot> for PinnedExecutionContract {
    fn from(snapshot: &ResolvedExecutionSnapshot) -> Self {
        Self {
            protocol_version: snapshot.protocol_version,
            agent_image_hash: snapshot.agent_image_hash.clone(),
            deployment_binding_hash: snapshot.deployment_binding_hash.clone(),
            model_registry_hash: snapshot.model_registry_hash.clone(),
            budget_registry_hash: snapshot.budget_registry_hash.clone(),
            model_profile: snapshot.model_profile.clone(),
            requested_model: snapshot.requested_model.clone(),
            resolved_model: snapshot.resolved_model.clone(),
            provider_api_version: snapshot.provider_api_version.clone(),
            provider_max_context_tokens: snapshot.provider_max_context_tokens,
            provider_wire_capabilities: snapshot.provider_wire_capabilities,
            thinking: snapshot.thinking,
            reasoning_effort: snapshot.reasoning_effort,
            capability_release_hashes: snapshot.capability_release_hashes.clone(),
            budget: snapshot.budget.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeCardinalityKind {
    Exact,
    Max,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntrypointScope {
    pub context_kind: RunContextKind,
    pub cardinality: ScopeCardinalityKind,
    pub value: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicReleaseEntrypoint {
    pub run_kind: String,
    pub locale: String,
    pub agent_image_hash: ContentHash,
    pub model_profile: String,
    pub scope: EntrypointScope,
    pub execution: PinnedExecutionContract,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicReleaseDescriptor {
    pub schema_version: u16,
    pub release_set_hash: ContentHash,
    pub runtime_version: String,
    pub entries: Vec<PublicReleaseEntrypoint>,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ContractError {
    #[error("invalid content hash: {0}")]
    InvalidHash(String),
    #[error("budget exceeded for {resource}: used {used}, limit {limit}")]
    BudgetExceeded {
        resource: &'static str,
        used: u64,
        limit: u64,
    },
    #[error("unknown model profile: {0}")]
    UnknownModelProfile(String),
    #[error("unknown model: {0}")]
    UnknownModel(String),
    #[error("requested model {requested} does not exactly match resolved model {resolved}")]
    ModelMismatch { requested: String, resolved: String },
    #[error("invalid run context: {0}")]
    InvalidRunContext(&'static str),
    #[error("invalid session memory: {0}")]
    InvalidSessionMemory(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable_and_validated() {
        let first = ContentHash::sha256(b"krw");
        let second = ContentHash::sha256(b"krw");
        assert_eq!(first, second);
        assert_eq!(ContentHash::parse(first.to_string()).unwrap(), first);
        assert!(ContentHash::parse("sha256:nope").is_err());
    }

    #[test]
    fn provider_tool_names_are_descriptive_bounded_and_disambiguated() {
        let context = provider_tool_name("ontology.query_context");
        let punctuation_collision = provider_tool_name("ontology-query_context");
        assert!(context.starts_with("krw_ontology_query_context__"));
        assert_ne!(context, punctuation_collision);
        assert!(context.len() <= MAX_PROVIDER_TOOL_NAME_BYTES);
        assert!(
            context
                .bytes()
                .all(|byte| { byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-') })
        );

        let long = provider_tool_name(&"a".repeat(256));
        assert!(long.len() <= MAX_PROVIDER_TOOL_NAME_BYTES);
        assert!(long.starts_with("krw_"));
    }

    #[test]
    fn missing_constrained_output_fields_default_to_false() {
        let mode: ProviderWireModeCapabilities = serde_json::from_value(serde_json::json!({
            "supported": true,
            "supports_tools": true,
            "supports_tool_choice": false,
            "supports_json_object": true
        }))
        .unwrap();
        assert!(!mode.supports_json_schema_output);
        assert!(!mode.supports_strict_tool_input);
    }

    #[test]
    fn strict_tool_input_requires_tools() {
        let capabilities = ProviderWireCapabilities {
            thinking: ProviderWireModeCapabilities {
                supported: true,
                supports_tools: false,
                supports_tool_choice: false,
                supports_json_object: true,
                supports_json_schema_output: false,
                supports_strict_tool_input: true,
            },
            non_thinking: ProviderWireModeCapabilities::default(),
            requires_thinking_block_replay: false,
            requires_assistant_content_for_tool_calls: false,
        };
        assert!(!capabilities.is_well_formed());
    }

    #[test]
    fn model_resolution_forbids_aliases_and_fallbacks() {
        let registry = ModelRegistry {
            schema_version: 2,
            registry_id: "fixture".into(),
            models: vec![ModelDescriptor {
                model_id: "deepseek-v4-flash".into(),
                api_base: "https://api.deepseek.com".into(),
                api_version: "anthropic-messages-v1".into(),
                max_context_tokens: 1_000_000,
                max_output_tokens: 384_000,
                max_in_flight: 64,
                provider_wire_capabilities: ProviderWireCapabilities::deepseek_v4_flash(),
            }],
            profiles: vec![ModelExecutionProfile {
                profile_id: "flash_high".into(),
                model_id: "deepseek-v4-flash".into(),
                thinking: ThinkingMode::Enabled,
                reasoning_effort: Some(ReasoningEffort::High),
            }],
        };

        assert!(
            registry
                .resolve_profile_exact("flash_high", "deepseek-v4-flash")
                .is_ok()
        );
        assert!(matches!(
            registry.resolve_profile_exact("flash_high", "claude-sonnet"),
            Err(ContractError::UnknownModel(_))
        ));

        let mut non_flash = registry.clone();
        non_flash.models[0].model_id = "forbidden-provider-model".into();
        non_flash.profiles[0].model_id = "forbidden-provider-model".into();
        assert!(matches!(
            non_flash.resolve_profile_exact("flash_high", "deepseek-v4-flash"),
            Err(ContractError::UnknownModel(_))
        ));
    }

    #[test]
    fn model_resolution_admits_glm_alongside_deepseek() {
        let registry = ModelRegistry {
            schema_version: 2,
            registry_id: "fixture".into(),
            models: vec![
                ModelDescriptor {
                    model_id: "deepseek-v4-flash".into(),
                    api_base: "https://api.deepseek.com".into(),
                    api_version: "anthropic-messages-v1".into(),
                    max_context_tokens: 1_000_000,
                    max_output_tokens: 384_000,
                    max_in_flight: 64,
                    provider_wire_capabilities: ProviderWireCapabilities::deepseek_v4_flash(),
                },
                ModelDescriptor {
                    model_id: "glm-5.3".into(),
                    api_base: "https://api.z.ai/api/anthropic".into(),
                    api_version: "anthropic-messages-v1".into(),
                    max_context_tokens: 128_000,
                    max_output_tokens: 16_384,
                    max_in_flight: 32,
                    provider_wire_capabilities: ProviderWireCapabilities::glm_5_3(),
                },
            ],
            profiles: vec![
                ModelExecutionProfile {
                    profile_id: "flash_high".into(),
                    model_id: "deepseek-v4-flash".into(),
                    thinking: ThinkingMode::Enabled,
                    reasoning_effort: Some(ReasoningEffort::High),
                },
                ModelExecutionProfile {
                    profile_id: "glm_high".into(),
                    model_id: "glm-5.3".into(),
                    thinking: ThinkingMode::Enabled,
                    reasoning_effort: Some(ReasoningEffort::High),
                },
            ],
        };

        let (deepseek_profile, deepseek_model) = registry
            .resolve_profile_exact("flash_high", "deepseek-v4-flash")
            .expect("deepseek still resolves");
        assert_eq!(deepseek_profile.model_id, "deepseek-v4-flash");
        assert_eq!(deepseek_model.model_id, "deepseek-v4-flash");

        let (glm_profile, glm_model) = registry
            .resolve_profile_exact("glm_high", "glm-5.3")
            .expect("glm-5.3 resolves under the generalized allow-list");
        assert_eq!(glm_profile.model_id, "glm-5.3");
        assert_eq!(glm_model.model_id, "glm-5.3");
        assert!(
            !glm_model
                .provider_wire_capabilities
                .requires_thinking_block_replay
        );

        // A profile that requests GLM through a deepseek-only profile id must
        // still surface the model mismatch rather than silently substituting.
        assert!(matches!(
            registry.resolve_profile_exact("flash_high", "glm-5.3"),
            Err(ContractError::ModelMismatch { .. })
        ));
    }

    #[test]
    fn run_context_is_closed_canonical_and_duplicate_free() {
        let context: RunContextV1 = serde_json::from_value(serde_json::json!({
            "kind": "company_ticker_set",
            "tickers": ["BRK.B", "005930"]
        }))
        .unwrap();
        context.validate().unwrap();

        for invalid in [
            serde_json::json!({"kind":"company_ticker_set","tickers":["aapl"]}),
            serde_json::json!({"kind":"company_ticker_set","tickers":["AAPL","AAPL"]}),
            serde_json::json!({"kind":"selected_feed_items","feed_item_ids":["550E8400-E29B-41D4-A716-446655440000"]}),
            serde_json::json!({"kind":"selected_feed_items","feed_item_ids":["550e8400-e29b-41d4-a716-446655440000","550e8400-e29b-41d4-a716-446655440000"]}),
        ] {
            let context: RunContextV1 = serde_json::from_value(invalid).unwrap();
            assert!(context.validate().is_err());
        }

        assert!(
            serde_json::from_value::<RunContextV1>(serde_json::json!({
                "kind": "question_only",
                "ticker": "AAPL"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<RunContextV1>(serde_json::json!({
                "kind": "covered_universe",
                "universe": "all"
            }))
            .is_err()
        );
    }

    #[test]
    fn typed_product_carriers_are_hash_bound_and_redacted() {
        let routing_value = serde_json::json!({
            "schema_version":1,
            "question":"AAPL을 분석해줘",
            "current_analysis_mode":"company"
        });
        let routing = RunContextV1::RoutingRequest {
            input_hash: ContentHash::sha256(serde_jcs::to_vec(&routing_value).unwrap()),
            typed_input: routing_value.clone(),
        };
        routing.validate().unwrap();
        assert!(!format!("{routing:?}").contains("AAPL"));

        let tampered = RunContextV1::RoutingRequest {
            input_hash: ContentHash::sha256("different"),
            typed_input: routing_value,
        };
        assert!(tampered.validate().is_err());

        let canonical_source = serde_json::json!({"schema_version":1,"units":[]});
        let committed = CommittedAnswerSourceV1 {
            schema_version: 1,
            final_receipt_hash: ContentHash::sha256("final"),
            answer_bundle_hash: ContentHash::sha256("bundle"),
            answer_ir_hash: ContentHash::sha256("answer"),
            canonical_source_hash: ContentHash::sha256(
                serde_jcs::to_vec(&canonical_source).unwrap(),
            ),
            source_unit_hashes: BTreeMap::from([("unit-1".into(), ContentHash::sha256("unit"))]),
            canonical_source,
        };
        committed.validate_carrier().unwrap();
        assert!(!format!("{committed:?}").contains("units"));
    }

    #[test]
    fn session_memory_carrier_is_bounded_hash_bound_and_redacted() {
        let canonical_view = serde_json::json!({
            "schema_version": 3,
            "authority": "context_only",
            "source_revision": 7,
            "recent_turns": [{"user_content":"비밀 질문","answer_content":"비밀 답변"}]
        });
        let memory = SessionMemoryCarrierV3 {
            schema_version: 3,
            view_hash: ContentHash::sha256(serde_jcs::to_vec(&canonical_view).unwrap()),
            source_frontier_hash: ContentHash::sha256("frontier"),
            source_revision: 7,
            canonical_view: canonical_view.clone(),
        };
        memory.validate_carrier().unwrap();
        let debug = format!("{memory:?}");
        assert!(!debug.contains("비밀 질문"));
        assert!(!debug.contains("비밀 답변"));

        let tampered = SessionMemoryCarrierV3 {
            canonical_view: serde_json::json!({"tampered": true}),
            ..memory
        };
        assert!(tampered.validate_carrier().is_err());
    }

    #[test]
    fn context_debug_redacts_tickers_and_ids() {
        let context = RunContextV1::SelectedFeedItems {
            feed_item_ids: vec!["550e8400-e29b-41d4-a716-446655440000".into()],
        };
        let rendered = format!("{context:?}");
        assert!(!rendered.contains("550e8400"));
        assert!(rendered.contains("SelectedFeedItems"));
    }

    #[test]
    fn budget_usage_roundtrips_duration_fields() {
        // New bundles with explicit duration values must round-trip exactly.
        let usage = BudgetUsage {
            provider_turns: 3,
            capability_calls: 7,
            replans: 1,
            repairs: 0,
            input_tokens: 12_345,
            output_tokens: 6_789,
            evidence_bytes: 4_567,
            provider_total_ms: 18_200,
            capability_total_ms: 9_400,
            compact_total_ms: 250,
            provider_queue_wait_ms: 120,
            session_memory_total_ms: 34,
            market_preflight_ms: 8,
            prompt_build_total_ms: 75,
            checkpoint_total_ms: 19,
        };
        let encoded = serde_json::to_string(&usage).unwrap();
        let decoded: BudgetUsage = serde_json::from_str(&encoded).unwrap();
        assert_eq!(usage, decoded);
    }

    #[test]
    fn budget_usage_old_bundle_without_duration_fields_defaults_to_zero() {
        // A bundle persisted before the duration fields existed must still
        // deserialize: `deny_unknown_fields` is satisfied because the new
        // fields are `#[serde(default)]`, and missing values become 0.
        let legacy = serde_json::json!({
            "provider_turns": 2,
            "capability_calls": 4,
            "replans": 0,
            "repairs": 0,
            "input_tokens": 1000,
            "output_tokens": 500,
            "evidence_bytes": 200,
        });
        let decoded: BudgetUsage = serde_json::from_value(legacy).unwrap();
        assert_eq!(decoded.provider_total_ms, 0);
        assert_eq!(decoded.capability_total_ms, 0);
        assert_eq!(decoded.compact_total_ms, 0);
        assert_eq!(decoded.provider_queue_wait_ms, 0);
        assert_eq!(decoded.session_memory_total_ms, 0);
        assert_eq!(decoded.market_preflight_ms, 0);
        assert_eq!(decoded.prompt_build_total_ms, 0);
        assert_eq!(decoded.checkpoint_total_ms, 0);
        assert_eq!(decoded.capability_calls, 4);
    }
}
