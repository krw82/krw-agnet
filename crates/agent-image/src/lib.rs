//! Canonical `AgentSpec` parser, bounded validator ISA, and immutable `AgentImage` compiler.
#![allow(clippy::format_push_string)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use krw_agent_bounded_child::ChildBudgetLimits;
use krw_agent_contracts::{FINAL_MARKDOWN_V1, GURU_QUERY_REQUEST_V1, verify_pin};
use krw_agent_protocol::{ContentHash, RunContextKind, RunContextV1};
use krw_agent_state_artifact::{
    ArtifactGuard, ArtifactTransition, BuiltinHandler, ContractPin, StateNode, StateProgram,
    TerminalDisposition as OperationTerminalDisposition,
};
pub use krw_agent_state_artifact::{ModelOutputMode, StateOperation};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const AGENT_SPEC_API_VERSION: &str = "krw.agent/spec-v3";
pub const AGENT_IMAGE_FORMAT_VERSION: u16 = 14;
pub const RULE_ISA_VERSION: u16 = 1;
pub const COMPILER_VERSION: &str = env!("CARGO_PKG_VERSION");

const MAX_STATES: usize = 128;
const MAX_TRANSITIONS: usize = 512;
const MAX_CAPABILITIES: usize = 64;
const MAX_PROMPT_SEGMENTS: usize = 64;
const MAX_SPEC_BYTES: usize = 2 * 1024 * 1024;
const MAX_MANIFEST_BYTES: usize = 2 * 1024 * 1024;
const MAX_PROMPT_BLOB_BYTES: usize = 256 * 1024;
const MAX_TOTAL_PROMPT_BYTES: usize = 1024 * 1024;
const MAX_STATE_VISITS: u16 = 256;
const MAX_TOTAL_STATE_VISITS: u64 = 4_096;
const MAX_RULES_PER_PROGRAM: usize = 256;
const MAX_RULE_FUEL: u32 = 65_536;
const MAX_PATH_DEPTH: usize = 12;
const MAX_CONSTANT_BYTES: usize = 64 * 1024;
const MAX_COLLECTION_ITEMS: usize = 4_096;
const MAX_VIOLATIONS: usize = 64;
const STATE_OPERATION_OUTPUT_CONTRACT_ID: &str = "state-operation-output/v1";
const STATE_FACTS_CONTRACT_ID: &str = "state-facts/v1";
static IMAGE_WRITE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSpec {
    pub api_version: String,
    pub kind: String,
    pub metadata: AgentMetadata,
    pub entrypoints: BTreeMap<String, EntrypointSpec>,
    pub contracts: Vec<ContractSpec>,
    pub roles: Vec<RoleSpec>,
    pub prompt_segments: Vec<PromptSegmentSource>,
    pub capabilities: Vec<CapabilitySpec>,
    pub workflows: Vec<WorkflowSpec>,
    pub planning_policy: PlanningPolicy,
    pub evidence_policy: EvidencePolicy,
    pub period_policy: PeriodPolicy,
    pub answer_policy: AnswerPolicySpec,
    pub security_policy: SecurityPolicy,
    pub validators: Vec<RuleProgram>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentMetadata {
    pub id: String,
    pub version: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntrypointSpec {
    pub run_kind: String,
    pub locale: String,
    pub workflow: String,
    pub required_budget_profile: String,
    pub required_model_profile: String,
    pub scope: InputScope,
    pub constants: EntrypointConstants,
    /// Skill segment IDs force-loaded into every model state in this run
    /// kind, in addition to the role's own `prompt_segments`. Tier 2 (pinned)
    /// skills guarantee that critical analysis frameworks (e.g., the
    /// earnings 5-stage causal chain) are always in the system prompt
    /// regardless of whether the model calls `skill.load`. Tier 1 skills
    /// (security, kernel, ontology, catalog) are role-level and need not be
    /// repeated here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pinned_skills: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputScope {
    pub allowed_context: RunContextKind,
    pub cardinality: ScopeCardinality,
    pub ticker_canonicalization: TickerCanonicalizationPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScopeCardinality {
    Exact { value: u8 },
    Max { value: u8 },
}

impl ScopeCardinality {
    pub const fn value(self) -> u8 {
        match self {
            Self::Exact { value } | Self::Max { value } => value,
        }
    }

    pub fn permits(self, observed: usize) -> bool {
        match self {
            Self::Exact { value } => observed == usize::from(value),
            Self::Max { value } => observed <= usize::from(value),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TickerCanonicalizationPolicy {
    RequireUppercase,
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntrypointConstants {
    pub fixed_guru_author: Option<GuruAuthor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuruAuthor {
    Ackman,
    Buffett,
    Flatt,
    Marks,
    TerrySmith,
}

impl GuruAuthor {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ackman => "ackman",
            Self::Buffett => "buffett",
            Self::Flatt => "flatt",
            Self::Marks => "marks",
            Self::TerrySmith => "terry_smith",
        }
    }

    fn expected_for_run_kind(run_kind: &str) -> Option<Self> {
        match run_kind {
            "guru_ackman" => Some(Self::Ackman),
            "guru_buffett" => Some(Self::Buffett),
            "guru_flatt" => Some(Self::Flatt),
            "guru_marks" => Some(Self::Marks),
            "guru_terry_smith" => Some(Self::TerrySmith),
            _ => None,
        }
    }
}

impl EntrypointSpec {
    pub fn validate_run_context(&self, context: &RunContextV1) -> Result<(), ContextPolicyError> {
        context
            .validate()
            .map_err(|_| ContextPolicyError::InvalidContextShape)?;
        if context.kind() != self.scope.allowed_context {
            return Err(ContextPolicyError::ContextKindMismatch);
        }
        if !self.scope.cardinality.permits(context.scoped_value_count()) {
            return Err(ContextPolicyError::CardinalityMismatch);
        }
        if self.scope.ticker_canonicalization == TickerCanonicalizationPolicy::RequireUppercase
            && context.trusted_tickers().is_empty()
        {
            return Err(ContextPolicyError::TickerPolicyMismatch);
        }
        Ok(())
    }
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum ContextPolicyError {
    #[error("run context contains invalid or noncanonical values")]
    InvalidContextShape,
    #[error("run context kind is not allowed by the selected entrypoint")]
    ContextKindMismatch,
    #[error("run context cardinality violates the selected entrypoint policy")]
    CardinalityMismatch,
    #[error("run ticker canonicalization policy is not satisfied")]
    TickerPolicyMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractSpec {
    pub id: String,
    pub schema_ref: String,
    /// Exact RFC 8785 canonical schema artifact hash. A contract identity is
    /// the `(id, schema_ref, content_hash)` triple, never its name alone.
    pub content_hash: ContentHash,
    pub semantic_authority: String,
    pub max_items: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedCapabilityContracts {
    pub input: ContractSpec,
    /// Kept in the exact order declared by the capability. The order is part
    /// of the composite output contract identity.
    pub outputs: Vec<ContractSpec>,
    pub output_contract_set_hash: ContentHash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleSpec {
    pub id: String,
    pub prompt_segments: Vec<String>,
    pub deterministic: bool,
    /// Image-owned execution policy for a model role. This expresses whether
    /// the role needs deliberation; it never names a provider model or puts a
    /// deployment-owned model profile into the `AgentImage`.
    #[serde(default, skip_serializing_if = "RoleExecutionPolicy::is_default")]
    pub execution: RoleExecutionPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounded_child: Option<BoundedChildSpec>,
}

/// Small, closed per-role provider policy. A role can elect direct synthesis
/// after research is complete, while the entrypoint's deployment-owned model
/// profile remains the source of model identity and default reasoning mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RoleExecutionPolicy {
    #[serde(default)]
    pub reasoning: RoleReasoningMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

impl RoleExecutionPolicy {
    #[must_use]
    pub const fn is_default(&self) -> bool {
        matches!(self.reasoning, RoleReasoningMode::Inherit) && self.max_output_tokens.is_none()
    }
}

/// Whether a role inherits the entrypoint's deliberation mode or performs a
/// bounded direct transformation of already admitted evidence.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleReasoningMode {
    #[default]
    Inherit,
    Direct,
}

/// Closed child execution declaration. It is intentionally not a recursive
/// graph or arbitrary scheduler hook: the only supported child consumes one
/// reservation from its parent and cannot create a child of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundedChildSpec {
    pub max_children: u8,
    pub max_depth: u8,
    pub budget_inheritance: ChildBudgetInheritance,
    pub reservation: ChildBudgetLimits,
    pub allowed_capabilities: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildBudgetInheritance {
    ParentReservation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptSegmentSource {
    pub id: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub ontology_schema: Option<OntologySchemaSource>,
    /// Auto-generate a compact skill catalog from explicitly registered,
    /// loadable Markdown prompt segments. The rendered blob replaces optional
    /// full skill bodies in the system prompt so the model can discover them
    /// by description and load one on demand via `skill.load`.
    #[serde(default)]
    pub skill_catalog: Option<SkillCatalogSource>,
    pub stable_prefix: bool,
    pub private: bool,
    /// Whether this immutable prompt body is an explicitly registered skill
    /// that the model may retrieve through `skill.load`. Internal policy
    /// segments remain unavailable even when they are present in the image.
    #[serde(default, skip_serializing_if = "is_false")]
    pub loadable: bool,
}

/// Declares that a prompt segment is generated at build time from explicitly
/// registered, `loadable: true` prompt segments under these directories. Each
/// registered skill contributes its immutable segment ID and frontmatter
/// description. The catalog is a compact discovery index; the full skill body
/// is fetched on demand via `skill.load`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillCatalogSource {
    /// One or more directories under the agent root containing registered
    /// skill files. Typically `["skills", "prompts", "references"]`. Only
    /// declared prompt segments marked `loadable: true` are included.
    pub skills_dirs: Vec<String>,
}

/// Declares that a prompt segment is generated at build time from ontology
/// schema YAML files, rather than read from a static Markdown file. The
/// source is either an explicitly supplied ontology root or the immutable
/// runtime schema snapshot bundled with this workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OntologySchemaSource {
    /// Optional environment variable holding the absolute ontology root path
    /// (e.g. `"KRW_ONTOLOGY_ROOT"`). This is retained for standalone image
    /// builds, but must not be combined with `bundled_runtime_schema`.
    #[serde(default)]
    pub root_env: Option<String>,
    /// Resolve the schema from the checked-in runtime snapshot under
    /// `services/krw-ontology-runtime`. This makes the image and the runtime
    /// validate against the same immutable schema without ambient filesystem
    /// state.
    #[serde(default)]
    pub bundled_runtime_schema: bool,
    /// Schema file basenames under `ontology/schema/` to include, without the
    /// `.yaml` extension (e.g. `["metric_dictionary", "quote_types"]`).
    pub schemas: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdempotencyPolicy {
    CanonicalArgs,
    RemoteKey,
    None,
}

/// Closed, deterministic derivations from a provider proposal to canonical
/// capability input. Transport is deliberately not represented here: semantic
/// input construction and MCP encoding have independent invariants.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputDerivation {
    #[default]
    Identity,
    /// Expand the small provider-visible company-context request into the
    /// physical ontology MCP input. The kernel fixes internal-ID visibility
    /// and leaves unsupported transport knobs absent, so the model only
    /// chooses a trusted ticker and optional retrieval filters.
    CompanyContextRequestV1,
    /// Start a fixed-author Guru retrieval with an empty provider-visible
    /// trigger. The kernel injects the immutable run question, fixed author,
    /// and trusted singleton ticker before MCP dispatch.
    SealedGuruQueryContextV1,
    /// The provider proposes evidence needs; the kernel deterministically
    /// constructs the private goal graph and minimum sufficient root
    /// `SearchPlan` before direct MCP dispatch.
    ResearchProposalToSearchPlanV4,
    /// Assemble a typed Guru company brief from a retained, typed Guru query
    /// input and result. The source is image data, not a kernel capability-ID
    /// convention.
    SealedGuruCompanyBriefV1 { query_context_capability: String },
    /// Assemble a typed Guru review from a retained Guru query, a sealed
    /// company brief, and the declared family of evidence capabilities.
    /// Every source is pinned by the image and verified before use.
    SealedGuruEvidenceReviewV1 {
        query_context_capability: String,
        company_brief_capability: String,
        evidence_capabilities: Vec<String>,
    },
}

impl InputDerivation {
    // `serde(skip_serializing_if)` requires a `&T -> bool` predicate. Keep
    // this reference receiver even though the enum is `Copy`.
    #[allow(clippy::trivially_copy_pass_by_ref)]
    const fn is_identity(&self) -> bool {
        matches!(self, Self::Identity)
    }
}

/// Trusted text which may anchor a model-authored `ResearchProposal`. The
/// provider cannot choose this authority. A sealed investigation question is
/// available only after its explicitly declared, typed producer capability
/// has completed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResearchProposalAnchor {
    RunQuestion,
    SealedGuruInvestigationBriefV1 { source_capability: String },
}

/// Closed semantic class for an action managed by the generic research
/// planner. This is deliberately independent of capability id, MCP tool name,
/// and provider tool name: an `AgentImage` declares the behavior once and the
/// kernel consumes the declaration without string matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResearchActionKind {
    Context,
    Targeted,
    Trace,
}

/// Fixed-point, image-pinned fallback estimate used until a signed deployment
/// telemetry registry supersedes it. Keeping the fallback in the image makes
/// its cost/benefit trade-off reviewable and removes per-capability Rust
/// conditionals from the hot path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchActionEstimate {
    pub historical_success_lower_ppm: u32,
    pub expected_duplicate_ppm: u32,
    pub failure_risk_upper_ppm: u32,
    pub expected_latency_ms: u32,
    pub expected_tokens: u32,
    pub expected_tool_cost_micros: u32,
    pub expected_result_bytes: u32,
}

/// Declarative planner behavior for one read capability. The conflict domain
/// is a stable logical resource, not a capability id, so related physical
/// tools can be serialized without hard-coded kernel knowledge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchActionPolicy {
    pub kind: ResearchActionKind,
    pub estimate: ResearchActionEstimate,
    pub conflict_domain: String,
}

/// Closed, typed projection from a capability's validated physical result to
/// the kernel's evidence/control artifacts. This replaces an open mapping
/// string: adding a capability may reuse an existing projection without
/// kernel changes, while a genuinely new result semantic requires an explicit
/// reviewed enum variant and adapter implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityResultIngest {
    ResearchStateV2,
    /// Sanitized, orientation-only company topic context. It may guide a
    /// later query but never grants a factual or strong-claim permission.
    CompanyContextV1,
    /// Timestamped, research-only current-market context. It is deliberately
    /// advisory and never enters the filing-evidence or recommendation path.
    MarketSnapshotV1,
    TargetedEvidenceV1,
    TraceLineageV1,
    FrontFeedListItemsV1,
    FrontFeedGetItemsV1,
    FrontFeedContextV2,
    FrontFilingSearchV1,
    FrontFilingMetadataV1,
    FrontFilingBriefV1,
    FrontFilingSectionsV1,
    FrontFilingSectionTextV1,
    FrontFilingDocumentsV1,
    FrontFilingDocumentTextV1,
    FrontForm4TransactionsV1,
    GuruQueryContextV1,
    GuruCompanyBriefV1,
    GuruEvidenceReviewV1,
    /// Local skill body loaded on demand from the immutable image blob store.
    /// No MCP round trip; the result is a passthrough of the pinned Markdown.
    SkillContentV1,
}

impl CapabilityResultIngest {
    /// Provider-visible semantic hint. It is selected by the same typed
    /// result ABI that governs post-tool validation, so prompt text cannot
    /// drift from the actual capability behavior.
    pub const fn provider_tool_description(self) -> &'static str {
        match self {
            Self::ResearchStateV2 => {
                "Retrieve filing research context for the authenticated in-scope company before drafting an answer. Use the exact provider input schema as the root arguments object; never add a transport wrapper. Use the least sufficient evidence request and never broaden the authenticated scope."
            }
            Self::CompanyContextV1 => {
                "Retrieve a compact, orientation-only topic map for the already in-scope company only when it can improve the next evidence query. Use it to narrow follow-up research, never as factual support or a final-answer claim. The kernel removes internal routing data and keeps the result advisory."
            }
            Self::MarketSnapshotV1 => {
                "Retrieve compact current price and valuation context only when it materially improves a current-price or valuation question. It is timestamped advisory research data, not filing evidence and not support for a target price, recommendation, or factual filing claim."
            }
            Self::TargetedEvidenceV1 => {
                "Retrieve one precise fact only for an unresolved research clause. Use the pinned input schema and do not broaden the authenticated scope."
            }
            Self::TraceLineageV1 => {
                "Retrieve source lineage or a bounded relationship chain only for an observed in-scope record. Use the pinned input schema and do not invent an object identifier."
            }
            Self::FrontFeedListItemsV1
            | Self::FrontFeedGetItemsV1
            | Self::FrontFeedContextV2
            | Self::FrontFilingSearchV1
            | Self::FrontFilingMetadataV1
            | Self::FrontFilingBriefV1
            | Self::FrontFilingSectionsV1
            | Self::FrontFilingSectionTextV1
            | Self::FrontFilingDocumentsV1
            | Self::FrontFilingDocumentTextV1
            | Self::FrontForm4TransactionsV1
            | Self::GuruQueryContextV1
            | Self::GuruCompanyBriefV1
            | Self::GuruEvidenceReviewV1 => {
                "Invoke the pinned read capability allowed in this workflow."
            }
            Self::SkillContentV1 => {
                "Load the full body of a skill listed in the skill catalog. Call this only for skills you intend to follow, then act on the loaded instructions. The body is resolved locally — no external lookup."
            }
        }
    }
}

/// A closed, result-derived run scope. The producing capability remains a
/// normal read adapter; only this bounded projection may influence what a
/// later capability is authorized to inspect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeProjectionKind {
    TickerSet,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeProjectionSpec {
    pub kind: ScopeProjectionKind,
    /// Declared remote result contract from which the projection is read.
    pub output_contract: String,
    /// RFC 6901 JSON pointer into the typed output. The runtime accepts only
    /// a bounded array of canonical tickers for `ticker_set`.
    pub source_pointer: String,
    pub max_items: u8,
    pub require_nonempty: bool,
}

/// How a capability input binds to the authenticated run scope. This is a
/// closed declarative ABI, not a capability-name convention: the kernel may
/// validate a new input surface without gaining a new `if capability == ...`
/// branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CapabilityScopeBinding {
    /// A company/notebook ticker set, or the ticker set durably projected by
    /// a selected-feed context result. Every declared ticker value must be a
    /// canonical member of that immutable set.
    TrustedTickerSet {
        ticker_references: Vec<TickerReferenceSpec>,
        /// Reference IDs of which at least one must contain a ticker. This
        /// distinguishes a required top-level `SearchPlan` ticker set from an
        /// optional nested clause ticker annotation.
        require_any_of: Vec<String>,
        /// Fields whose non-null value would widen a trusted company scope,
        /// such as a `SearchPlan` `universe` selector.
        #[serde(default)]
        reject_non_null_pointers: Vec<String>,
    },
    /// A closed covered-universe execution. Explicit tickers must be absent;
    /// optional literals and a bounded discovery limit are declared here
    /// rather than inferred from a capability ID.
    CoveredUniverse {
        ticker_references: Vec<TickerReferenceSpec>,
        #[serde(default)]
        required_string_values: Vec<RequiredStringValue>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bounded_integer_pointer: Option<String>,
    },
    /// Only the immutable selected feed-item IDs may be read. A capability
    /// can separately emit a `TickerSet` result projection, after which a
    /// later `TrustedTickerSet` capability may use the derived scope.
    SelectedFeedItems {
        issue_ids_pointer: String,
        #[serde(default)]
        ticker_references: Vec<TickerReferenceSpec>,
        forbid_ticker_references: bool,
    },
    /// A source-filing workflow may only access the immutable filing event
    /// received in its host `RunRequest`.
    SourceFiling { filing_event_id_pointer: String },
    /// A capability whose input carries no authenticated run scope at all —
    /// for example a local skill-body lookup keyed only by a skill name. The
    /// kernel imposes no ticker/filing binding because the result is a static
    /// prompt artifact already pinned inside the immutable image.
    Unscoped,
}

/// A bounded JSON-pointer pattern used to locate an input ticker without
/// baking an input shape into the kernel. `*` is permitted only as a complete
/// segment and expands over an array, e.g. `/clauses/*/tickers`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TickerReferenceSpec {
    pub id: String,
    pub pointer_pattern: String,
    pub value_kind: TickerReferenceValueKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TickerReferenceValueKind {
    String,
    StringArray,
}

/// A literal required by a closed scope binding, for example
/// `/universe == "covered"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequiredStringValue {
    pub pointer: String,
    pub value: String,
}

/// Physical encoding for a canonical capability input. The first canonical
/// runtime intentionally has one codec: direct JSON MCP arguments. Keeping
/// it explicit stops a future transport convention from leaking back into
/// domain input derivation.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportCodec {
    #[default]
    CanonicalMcpV1,
}

/// Provider-facing encoding for a capability's model-authored semantic input.
///
/// This is intentionally separate from [`TransportCodec`]: the former only
/// describes `DeepSeek` function arguments, while the latter describes the
/// canonical arguments that the kernel sends to a real capability.  A model
/// may naturally use a named argument such as `proposal`, even when the
/// kernel's canonical contract is a root object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderInputCodec {
    /// Expose and consume the canonical model-input contract as the function
    /// arguments root object.
    #[default]
    CanonicalRootV1,
    /// Expose one named function argument whose value is the canonical
    /// model-input contract. The kernel validates and unwraps that field
    /// before running any domain compiler or physical capability.
    SingleFieldEnvelopeV1 { field: String },
}

impl ProviderInputCodec {
    #[must_use]
    pub const fn is_canonical_root(&self) -> bool {
        matches!(self, Self::CanonicalRootV1)
    }

    /// Build the exact JSON Schema advertised to the provider. Local `$defs`
    /// are lifted to the new root so the canonical contract's local `$ref`
    /// values retain their meaning inside the envelope.
    pub fn provider_parameters(&self, canonical_parameters: Value) -> Result<Value, ImageError> {
        let canonical_parameters = expand_provider_root_schema(canonical_parameters)?;
        let canonical = canonical_parameters.as_object().ok_or_else(|| {
            ImageError::InvalidSpec("provider input contract schema must be an object".into())
        })?;
        match self {
            Self::CanonicalRootV1 => Ok(canonical_parameters),
            Self::SingleFieldEnvelopeV1 { field } => {
                if !valid_provider_envelope_field(field) {
                    return Err(ImageError::InvalidSpec(
                        "provider envelope field must be a lowercase ASCII identifier".into(),
                    ));
                }
                let mut nested = canonical.clone();
                let definitions = nested.remove("$defs");
                let schema = nested.remove("$schema");
                let mut wrapper = serde_json::Map::new();
                if let Some(schema) = schema {
                    wrapper.insert("$schema".into(), schema);
                }
                if let Some(definitions) = definitions {
                    wrapper.insert("$defs".into(), definitions);
                }
                wrapper.insert("type".into(), Value::String("object".into()));
                wrapper.insert("additionalProperties".into(), Value::Bool(false));
                wrapper.insert(
                    "required".into(),
                    Value::Array(vec![Value::String(field.clone())]),
                );
                wrapper.insert(
                    "properties".into(),
                    Value::Object(serde_json::Map::from_iter([(
                        field.clone(),
                        Value::Object(nested),
                    )])),
                );
                Ok(Value::Object(wrapper))
            }
        }
    }

    /// Convert provider function arguments into the canonical model-input
    /// value. This is a closed syntax adaptation, not a legacy fallback: an
    /// envelope codec accepts exactly its one declared field and nothing else.
    pub fn decode(&self, provider_arguments: Value) -> Result<Value, ProviderInputCodecError> {
        match self {
            Self::CanonicalRootV1 => Ok(provider_arguments),
            Self::SingleFieldEnvelopeV1 { field } => {
                let object = provider_arguments
                    .as_object()
                    .ok_or(ProviderInputCodecError::EnvelopeInvalid)?;
                if object.len() != 1 {
                    return Err(ProviderInputCodecError::EnvelopeInvalid);
                }
                object
                    .get(field)
                    .cloned()
                    .ok_or(ProviderInputCodecError::EnvelopeInvalid)
            }
        }
    }
}

/// Some canonical contracts intentionally keep their reusable definition under
/// `$defs` and make the document root only a local `$ref`. Several
/// Anthropic-compatible tool callers (including GLM) do not reliably expose
/// that indirection to the model, which can result in an empty `{}` tool
/// argument despite a valid contract. Expand only a local root reference for
/// the provider schema; the canonical contract bytes and runtime validation
/// remain unchanged.
fn expand_provider_root_schema(canonical_parameters: Value) -> Result<Value, ImageError> {
    let Some(canonical) = canonical_parameters.as_object() else {
        return Err(ImageError::InvalidSpec(
            "provider input contract schema must be an object".into(),
        ));
    };
    let Some(reference) = canonical.get("$ref").and_then(Value::as_str) else {
        return Ok(canonical_parameters);
    };
    let Some(definition_name) = reference.strip_prefix("#/$defs/") else {
        return Ok(canonical_parameters);
    };
    let Some(definitions) = canonical.get("$defs").and_then(Value::as_object) else {
        return Ok(canonical_parameters);
    };
    let Some(definition) = definitions.get(definition_name).and_then(Value::as_object) else {
        return Ok(canonical_parameters);
    };
    let mut expanded = definition.clone();
    if let Some(schema) = canonical.get("$schema") {
        expanded.insert("$schema".into(), schema.clone());
    }
    if let Some(identifier) = canonical.get("$id") {
        expanded.insert("$id".into(), identifier.clone());
    }
    expanded.insert("$defs".into(), Value::Object(definitions.clone()));
    Ok(Value::Object(expanded))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderInputCodecError {
    EnvelopeInvalid,
}

fn valid_provider_envelope_field(field: &str) -> bool {
    let bytes = field.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_lowercase)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilitySpec {
    pub id: String,
    /// Symbolic key resolved only by `DeploymentBinding`. Never a URL.
    pub binding_key: String,
    pub permission: Permission,
    /// Canonical contract sent to the physical capability implementation.
    pub input_contract: String,
    /// Narrow provider-authored proposal contract. Omitted when the provider
    /// supplies the physical capability input directly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_input_contract: Option<String>,
    /// Provider-wire shape for the model input. This does not alter the
    /// canonical model contract or the physical capability transport.
    #[serde(default, skip_serializing_if = "ProviderInputCodec::is_canonical_root")]
    pub provider_input_codec: ProviderInputCodec,
    #[serde(default, skip_serializing_if = "InputDerivation::is_identity")]
    pub input_derivation: InputDerivation,
    /// Retain the canonical argument bytes only when a later closed input
    /// derivation explicitly declares this capability as its source. This is
    /// a durable, image-owned decision; the kernel never retains an input by
    /// capability name convention.
    #[serde(default, skip_serializing_if = "is_false")]
    pub retain_canonical_input: bool,
    /// Required by the `ResearchProposal` compiler. It declares the trusted
    /// question authority instead of allowing a model to restate scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research_proposal_anchor: Option<ResearchProposalAnchor>,
    #[serde(default)]
    pub transport_codec: TransportCodec,
    pub output_contracts: Vec<String>,
    pub idempotency: IdempotencyPolicy,
    pub result_ingest: CapabilityResultIngest,
    /// Present only for capabilities which participate in value-based
    /// evidence acquisition. Other read capabilities remain typed, but do
    /// not enter the research planner merely because their name resembles an
    /// ontology operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research_action: Option<ResearchActionPolicy>,
    /// Optional authenticated scope produced by this capability's typed
    /// result. It is never model-authored and does not alter the MCP input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_projection: Option<ScopeProjectionSpec>,
    /// Mandatory binding from this capability's canonical input to one closed
    /// run-context authority. The absence of a scope binding is never an
    /// implicit authorization.
    pub scope_binding: CapabilityScopeBinding,
    /// Declares a capability eligible for in-run parallel dispatch. Currently
    /// **not enforced** by the run engine or capability runtime — all
    /// capabilities execute sequentially regardless of this flag. Retained in
    /// the ABI for forward compatibility; do not author `parallel_safe: true`
    /// expecting concurrent execution today. Wiring this flag to real
    /// parallel dispatch requires careful handling of ordering, budget, and
    /// MCP-tail semantics (see architecture review).
    pub parallel_safe: bool,
    pub prerequisites: Vec<String>,
}

impl CapabilitySpec {
    pub fn model_input_contract_id(&self) -> &str {
        self.model_input_contract
            .as_deref()
            .unwrap_or(&self.input_contract)
    }

    /// Provider-facing input guidance is derived from the model-input ABI,
    /// not from the result type. A capability may return `ResearchStateV2`
    /// while accepting a narrower `ResearchProposal` that the kernel compiles
    /// into `SearchPlan`; describing the physical input to the provider would
    /// make the model violate its own tool schema.
    pub fn provider_tool_description(&self) -> &'static str {
        match &self.input_derivation {
            InputDerivation::SealedGuruQueryContextV1 => {
                "Start the fixed-author Guru research flow. Call this function with an empty object `{}` only. The kernel attaches the current user question, the host-selected Guru lens, and the authenticated company ticker. Do not provide an author, ticker, company context, limits, or a conclusion."
            }
            InputDerivation::ResearchProposalToSearchPlanV4 => {
                "Call this function with exactly one top-level `proposal` field. Its value is one complete ResearchProposal v4, not a SearchPlan. Each objective declares whether evidence is required now or deliberately deferred, its proof quality, interchangeable retrieval alternatives, and exactly one tagged semantic goal such as metric_time_series, metric_change, or qualitative_evidence. Do not create goal IDs, candidate IDs, graph dependencies, retrieval_query, tickers, universe, limits, clauses, comparison axes, calculation windows outside a metric_change goal, or MCP encoding: the kernel constructs and validates all of them. Mark only evidence that can materially change this answer as required; deferred objectives do not expand the initial plan."
            }
            InputDerivation::CompanyContextRequestV1 => {
                "Call this function exactly once with only the trusted ticker. The kernel fetches a bounded company ontology map using the current document landscape; treat it as orientation-only, never as factual support. Do not set periods, document types, limits, internal IDs, or a conclusion. Exact filing scope belongs in the later ResearchProposal."
            }
            InputDerivation::Identity | InputDerivation::SealedGuruEvidenceReviewV1 { .. } => {
                self.result_ingest.provider_tool_description()
            }
            InputDerivation::SealedGuruCompanyBriefV1 { .. } => {
                "After guru.query_context, draft exactly one central company tension and call this function with one JSON object containing exactly these keys: question, guru_principle_ids, company_context_anchor_ids, hypothesis, counter_hypothesis, evidence_needed, strengthens_if, weakens_if, why_material, decision_role. Use arrays for the two ID fields and evidence_needed; copy principle reviewed_id and company anchor_id values verbatim from guru.query_context; set decision_role to main_tension. Do not call with an empty object, wrapper, ticker, author, proposal, or any extra field."
            }
        }
    }
}

// `serde(skip_serializing_if)` requires a `&T -> bool` predicate.
#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_false(value: &bool) -> bool {
    !*value
}

/// Provider tools use the exact model-input contract schema. Provider-specific
/// function naming and JSON-text result encoding live in the provider codec,
/// never in an individual capability's schema projection.
pub fn provider_input_parameters(
    codec: &ProviderInputCodec,
    canonical_parameters: Value,
) -> Result<Value, ImageError> {
    codec.provider_parameters(canonical_parameters)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateKind {
    Start,
    Plan,
    Validate,
    Capability,
    Ingest,
    Assess,
    Compose,
    Verify,
    Render,
    Commit,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalDisposition {
    Succeeded,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateSpec {
    pub id: String,
    pub kind: StateKind,
    #[serde(default)]
    pub capability_id: Option<String>,
    /// The exact prompt role used when this state requires a model decision.
    /// Rust-owned deterministic states deliberately have no role.
    #[serde(default)]
    pub role_id: Option<String>,
    #[serde(default)]
    pub terminal: Option<TerminalDisposition>,
    pub max_visits: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TransitionGuard {
    #[default]
    Always,
    FieldPresent {
        pointer: String,
    },
    FieldAbsent {
        pointer: String,
    },
    FieldEquals {
        pointer: String,
        value: Value,
    },
    FieldNotEquals {
        pointer: String,
        value: Value,
    },
}

impl TransitionGuard {
    pub fn matches(&self, facts: &Value) -> bool {
        match self {
            Self::Always => true,
            Self::FieldPresent { pointer } => facts.pointer(pointer).is_some(),
            Self::FieldAbsent { pointer } => facts.pointer(pointer).is_none(),
            Self::FieldEquals { pointer, value } => facts.pointer(pointer) == Some(value),
            Self::FieldNotEquals { pointer, value } => facts.pointer(pointer) != Some(value),
        }
    }

    fn pointer(&self) -> Option<&str> {
        match self {
            Self::Always => None,
            Self::FieldPresent { pointer }
            | Self::FieldAbsent { pointer }
            | Self::FieldEquals { pointer, .. }
            | Self::FieldNotEquals { pointer, .. } => Some(pointer),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransitionSpec {
    pub from: String,
    pub on: String,
    pub to: String,
    #[serde(default)]
    pub guard: TransitionGuard,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowSpec {
    pub id: String,
    pub initial: String,
    pub states: Vec<StateSpec>,
    pub transitions: Vec<TransitionSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)]
pub struct PlanningPolicy {
    pub internal_brief_language: String,
    pub expose_internal_brief: bool,
    pub query_context_first: bool,
    pub required_clause_min: u8,
    pub clause_max: u8,
    pub require_unique_clause_ids: bool,
    pub require_ticker_subset: bool,
    pub split_metric_and_qualitative_clauses: bool,
    pub correction_field: String,
    pub gap_priority: Vec<String>,
    pub stop_conditions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)]
pub struct EvidencePolicy {
    pub directness_values: Vec<String>,
    pub grade_values: Vec<String>,
    pub grade_never_upgrades_directness: bool,
    pub direct_required_accepts: Vec<String>,
    pub numeric_lineage_accepts: Vec<String>,
    pub strong_claim_global_flag: String,
    pub strong_claim_clause_status: String,
    pub require_load_bearing_direct_premise: bool,
    pub require_calculation_coverage: bool,
    pub require_counter_signal_for_inference: bool,
    pub non_evidence_kinds: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeriodPolicy {
    pub current_driver_order: Vec<String>,
    pub annual_baseline: String,
    pub forbid_unconfirmed_future_labels: bool,
    pub allowed_older_filing_uses: Vec<String>,
    pub disclose_period_mismatch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)]
pub struct AnswerPolicySpec {
    pub internal_format: String,
    /// Output tokens held back from research turns for the terminal answer.
    /// This is part of the immutable `AgentImage` because it protects answer
    /// completion quality, not a deployment-specific model selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_output_reserve_tokens: Option<u32>,
    /// Smallest viable research-decision turn. Once admitted evidence exists
    /// and this much room is no longer available outside the final reserve,
    /// the kernel enters the image-declared composition fallback instead of
    /// asking a thinking model to produce a predictably truncated decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_research_turn_tokens: Option<u32>,
    /// Hard ceiling for one non-answer model decision. This prevents a single
    /// planning or assessment turn from consuming the entire research slice
    /// before the agent has had a chance to execute, observe, and react to a
    /// capability result. It is image-owned rather than a kernel constant so
    /// each agent program can balance deliberation against completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_research_turn_tokens: Option<u32>,
    pub locale: String,
    pub conclusion_first: bool,
    pub exact_follow_up_count: u8,
    pub target_price_forbidden: bool,
    pub definitive_rating_forbidden: bool,
    pub personalized_instruction_forbidden: bool,
    pub raw_internal_ids_visible: bool,
    pub forbidden_user_terms: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)]
pub struct SecurityPolicy {
    pub user_and_retrieved_text_untrusted: bool,
    pub reveal_hidden_prompt: bool,
    pub filesystem_access: bool,
    pub web_access: bool,
    pub mutation_capabilities: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RulePhase {
    Admission,
    Plan,
    PreAction,
    PostAction,
    Answer,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleProgram {
    pub id: String,
    pub phase: RulePhase,
    /// An empty scope applies to every payload in the phase. A non-empty
    /// scope is valid only for `post_action`, where the result ABI is known
    /// before policy evaluation. This keeps result-contract rules attached to
    /// their declared semantic output rather than accidentally applying a
    /// `ResearchState` assertion to an unrelated trace or targeted result.
    #[serde(default)]
    pub result_ingest_scope: Vec<CapabilityResultIngest>,
    pub fuel: u32,
    pub instructions: Vec<RuleInstruction>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuleInstruction {
    RequireField {
        path: String,
        code: String,
    },
    Equals {
        path: String,
        value: Value,
        code: String,
    },
    InSet {
        path: String,
        values: Vec<Value>,
        code: String,
    },
    MaxCalls {
        capability_id: String,
        max: u16,
        code: String,
    },
    StatePrecedes {
        before: String,
        after: String,
        code: String,
    },
    ClaimHasEvidence {
        except_kinds: Vec<String>,
        code: String,
    },
    NoInternalTerm {
        terms: Vec<String>,
        code: String,
    },
}

impl RuleInstruction {
    fn constant_bytes(&self) -> usize {
        serde_jcs::to_vec(self).map_or(MAX_CONSTANT_BYTES + 1, |bytes| bytes.len())
    }

    fn paths(&self) -> impl Iterator<Item = &str> {
        let path = match self {
            Self::RequireField { path, .. }
            | Self::Equals { path, .. }
            | Self::InSet { path, .. } => Some(path.as_str()),
            _ => None,
        };
        path.into_iter()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleViolation {
    pub instruction_index: u16,
    pub path: String,
    pub code: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleEvaluation {
    pub fuel_used: u32,
    pub violations: Vec<RuleViolation>,
}

/// Execute a total, bounded validator program. There are no jumps, recursion, regexes, or host calls.
pub fn evaluate_rule_program(
    program: &RuleProgram,
    input: &Value,
) -> Result<RuleEvaluation, ImageError> {
    validate_rule_program(program)?;
    let mut machine = RuleMachine {
        remaining_fuel: program.fuel,
        initial_fuel: program.fuel,
        visited_items: 0,
        violations: Vec::new(),
    };
    for (instruction_index, instruction) in program.instructions.iter().enumerate() {
        machine.charge(1)?;
        let instruction_index = u16::try_from(instruction_index)
            .map_err(|_| ImageError::Limit("validator instruction index"))?;
        match instruction {
            RuleInstruction::RequireField { path, code } => {
                if input.pointer(path).is_none() {
                    machine.violate(instruction_index, path, code)?;
                }
            }
            RuleInstruction::Equals { path, value, code } => {
                if input.pointer(path) != Some(value) {
                    machine.violate(instruction_index, path, code)?;
                }
            }
            RuleInstruction::InSet { path, values, code } => {
                machine.charge(u32::try_from(values.len()).unwrap_or(u32::MAX))?;
                if input
                    .pointer(path)
                    .is_none_or(|actual| !values.contains(actual))
                {
                    machine.violate(instruction_index, path, code)?;
                }
            }
            RuleInstruction::MaxCalls {
                capability_id,
                max,
                code,
            } => {
                let path = format!(
                    "/usage/capability_calls/{}",
                    capability_id.replace('~', "~0").replace('/', "~1")
                );
                let used = input.pointer(&path).and_then(Value::as_u64).unwrap_or(0);
                if used > u64::from(*max) {
                    machine.violate(instruction_index, &path, code)?;
                }
            }
            RuleInstruction::StatePrecedes {
                before,
                after,
                code,
            } => {
                let trace = input
                    .pointer("/state_trace")
                    .and_then(Value::as_array)
                    .ok_or_else(|| ImageError::RuleInput("state_trace must be an array".into()))?;
                machine.visit(trace.len())?;
                let before_index = trace
                    .iter()
                    .position(|value| value.as_str() == Some(before));
                let after_index = trace.iter().position(|value| value.as_str() == Some(after));
                if after_index.is_some() && (before_index.is_none() || before_index >= after_index)
                {
                    machine.violate(instruction_index, "/state_trace", code)?;
                }
            }
            RuleInstruction::ClaimHasEvidence { except_kinds, code } => {
                let claims = input
                    .pointer("/claims")
                    .and_then(Value::as_array)
                    .ok_or_else(|| ImageError::RuleInput("claims must be an array".into()))?;
                machine.visit(claims.len())?;
                for (index, claim) in claims.iter().enumerate() {
                    machine.charge(1)?;
                    let kind = claim.get("kind").and_then(Value::as_str).unwrap_or("");
                    let exempt = except_kinds.iter().any(|value| value == kind);
                    let evidence_count = claim
                        .get("evidence_ids")
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len);
                    if !exempt && evidence_count == 0 {
                        machine.violate(
                            instruction_index,
                            &format!("/claims/{index}/evidence_ids"),
                            code,
                        )?;
                    }
                }
            }
            RuleInstruction::NoInternalTerm { terms, code } => {
                machine.scan_forbidden(instruction_index, input, terms, code)?;
            }
        }
    }
    machine.violations.sort_by(|left, right| {
        (left.instruction_index, &left.path, &left.code).cmp(&(
            right.instruction_index,
            &right.path,
            &right.code,
        ))
    });
    Ok(RuleEvaluation {
        fuel_used: machine.initial_fuel - machine.remaining_fuel,
        violations: machine.violations,
    })
}

#[derive(Debug)]
struct RuleMachine {
    remaining_fuel: u32,
    initial_fuel: u32,
    visited_items: usize,
    violations: Vec<RuleViolation>,
}

impl RuleMachine {
    fn charge(&mut self, amount: u32) -> Result<(), ImageError> {
        self.remaining_fuel = self
            .remaining_fuel
            .checked_sub(amount)
            .ok_or(ImageError::RuleFuelExhausted)?;
        Ok(())
    }

    fn visit(&mut self, count: usize) -> Result<(), ImageError> {
        self.visited_items = self
            .visited_items
            .checked_add(count)
            .ok_or(ImageError::Limit("validator visited collection items"))?;
        if self.visited_items > MAX_COLLECTION_ITEMS {
            return Err(ImageError::Limit("validator collection items"));
        }
        self.charge(u32::try_from(count).unwrap_or(u32::MAX))
    }

    fn violate(
        &mut self,
        instruction_index: u16,
        path: &str,
        code: &str,
    ) -> Result<(), ImageError> {
        if self.violations.len() >= MAX_VIOLATIONS {
            return Err(ImageError::Limit("validator violations"));
        }
        self.violations.push(RuleViolation {
            instruction_index,
            path: path.to_owned(),
            code: code.to_owned(),
        });
        Ok(())
    }

    fn scan_forbidden(
        &mut self,
        instruction_index: u16,
        input: &Value,
        terms: &[String],
        code: &str,
    ) -> Result<(), ImageError> {
        let normalized_terms = terms
            .iter()
            .map(|term| term.to_lowercase())
            .collect::<Vec<_>>();
        let mut stack = vec![(String::new(), input)];
        while let Some((path, value)) = stack.pop() {
            self.visit(1)?;
            match value {
                Value::String(text) => {
                    let normalized = text.to_lowercase();
                    if normalized_terms
                        .iter()
                        .any(|term| normalized.contains(term))
                    {
                        self.violate(instruction_index, &path, code)?;
                    }
                }
                Value::Array(values) => {
                    for (index, value) in values.iter().enumerate().rev() {
                        stack.push((format!("{path}/{index}"), value));
                    }
                }
                Value::Object(values) => {
                    for (key, value) in values.iter().rev() {
                        let escaped = key.replace('~', "~0").replace('/', "~1");
                        stack.push((format!("{path}/{escaped}"), value));
                    }
                }
                Value::Null | Value::Bool(_) | Value::Number(_) => {}
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptBlob {
    pub id: String,
    pub content_hash: ContentHash,
    pub byte_len: u64,
    pub stable_prefix: bool,
    pub private: bool,
    /// Explicit local-skill allowlist bit. This is separate from `private`:
    /// a skill body may be private to the end user while still being safe for
    /// the model to load during a run.
    #[serde(default, skip_serializing_if = "is_false")]
    pub loadable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledState {
    pub numeric_id: u16,
    pub stable_id: String,
    pub kind: StateKind,
    pub capability_id: Option<String>,
    pub role_id: Option<String>,
    pub terminal: Option<TerminalDisposition>,
    pub operation: StateOperation,
    pub max_visits: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledTransition {
    pub from: u16,
    pub event: String,
    pub to: u16,
    pub guard: TransitionGuard,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledWorkflow {
    pub id: String,
    pub initial: u16,
    pub states: Vec<CompiledState>,
    pub transitions: Vec<CompiledTransition>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentImageBody {
    pub format_version: u16,
    pub rule_isa_version: u16,
    pub compiler_version: String,
    pub source_hash: ContentHash,
    pub metadata: AgentMetadata,
    pub entrypoints: BTreeMap<String, EntrypointSpec>,
    pub contracts: Vec<ContractSpec>,
    pub roles: Vec<RoleSpec>,
    pub prompt_blobs: Vec<PromptBlob>,
    pub capabilities: Vec<CapabilitySpec>,
    pub workflows: Vec<CompiledWorkflow>,
    pub planning_policy: PlanningPolicy,
    pub evidence_policy: EvidencePolicy,
    pub period_policy: PeriodPolicy,
    pub answer_policy: AnswerPolicySpec,
    pub security_policy: SecurityPolicy,
    pub validators: Vec<RuleProgram>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentImageManifest {
    pub content_hash: ContentHash,
    pub body: AgentImageBody,
}

#[derive(Debug, Clone)]
pub struct CompiledImage {
    pub manifest: AgentImageManifest,
    pub blobs: BTreeMap<ContentHash, Vec<u8>>,
}

impl CompiledImage {
    /// Verify and materialize a freshly compiled image without a filesystem
    /// round trip. This follows the same hash and size boundary as `load_image`.
    pub fn into_loaded(self) -> Result<LoadedImage, ImageError> {
        self.into_loaded_with_interner(&mut PromptBlobInterner::default())
    }

    /// Verify and materialize a compiled image while sharing exact prompt
    /// bytes with every other image loaded through the same startup interner.
    pub fn into_loaded_with_interner(
        self,
        interner: &mut PromptBlobInterner,
    ) -> Result<LoadedImage, ImageError> {
        validate_loaded_manifest(&self.manifest)?;
        let mut loaded = BTreeMap::new();
        let mut total_bytes = 0_usize;
        for descriptor in &self.manifest.body.prompt_blobs {
            let blob = self
                .blobs
                .get(&descriptor.content_hash)
                .ok_or_else(|| ImageError::MissingBlob(descriptor.content_hash.clone()))?;
            validate_prompt_blob(descriptor, blob, &mut total_bytes)?;
            loaded
                .entry(descriptor.content_hash.clone())
                .or_insert(interner.intern(&descriptor.content_hash, blob)?);
        }
        let image = LoadedImage {
            manifest: self.manifest,
            blobs: loaded,
        };
        image.verify()?;
        Ok(image)
    }
}

/// Startup-only content-addressed prompt store. It contains no image-specific
/// aliases, so equal hashes safely share one immutable allocation.
#[derive(Debug, Default)]
pub struct PromptBlobInterner {
    blobs: BTreeMap<ContentHash, Arc<[u8]>>,
}

impl PromptBlobInterner {
    fn intern(&mut self, hash: &ContentHash, bytes: &[u8]) -> Result<Arc<[u8]>, ImageError> {
        if let Some(existing) = self.blobs.get(hash) {
            if existing.as_ref() != bytes {
                return Err(ImageError::InternedBlobConflict(hash.clone()));
            }
            return Ok(Arc::clone(existing));
        }
        let shared: Arc<[u8]> = Arc::from(bytes);
        self.blobs.insert(hash.clone(), Arc::clone(&shared));
        Ok(shared)
    }

    pub fn unique_blob_count(&self) -> usize {
        self.blobs.len()
    }
}

#[derive(Debug, Clone)]
pub struct LoadedImage {
    pub manifest: AgentImageManifest,
    blobs: BTreeMap<ContentHash, Arc<[u8]>>,
}

impl std::ops::Deref for LoadedImage {
    type Target = AgentImageManifest;

    fn deref(&self) -> &Self::Target {
        &self.manifest
    }
}

impl LoadedImage {
    /// Revalidate the manifest hash and every retained prompt blob. Release-set
    /// construction calls this even for already-loaded inputs so a mutable
    /// pre-startup staging value cannot bypass content identity checks.
    pub fn verify(&self) -> Result<(), ImageError> {
        validate_loaded_manifest(&self.manifest)?;
        let mut total_bytes = 0_usize;
        for descriptor in &self.manifest.body.prompt_blobs {
            let bytes = self
                .blobs
                .get(&descriptor.content_hash)
                .ok_or_else(|| ImageError::MissingBlob(descriptor.content_hash.clone()))?;
            validate_prompt_blob(descriptor, bytes, &mut total_bytes)?;
        }
        Ok(())
    }

    pub fn prompt(&self, prompt_id: &str) -> Result<&str, ImageError> {
        let descriptor = self
            .manifest
            .body
            .prompt_blobs
            .iter()
            .find(|prompt| prompt.id == prompt_id)
            .ok_or_else(|| ImageError::UnknownReference {
                kind: "loaded prompt",
                id: prompt_id.to_owned(),
            })?;
        let bytes = self
            .blobs
            .get(&descriptor.content_hash)
            .ok_or_else(|| ImageError::MissingBlob(descriptor.content_hash.clone()))?;
        std::str::from_utf8(bytes).map_err(|_| ImageError::InvalidPromptUtf8(prompt_id.to_owned()))
    }

    pub fn role_prompt_segments(&self, role_id: &str) -> Result<Vec<&str>, ImageError> {
        let role = self
            .manifest
            .body
            .roles
            .iter()
            .find(|role| role.id == role_id)
            .ok_or_else(|| ImageError::UnknownReference {
                kind: "loaded role",
                id: role_id.to_owned(),
            })?;
        role.prompt_segments
            .iter()
            .map(|prompt_id| self.prompt(prompt_id))
            .collect()
    }

    /// Shared immutable bytes for diagnostics and startup catalog tests. Run
    /// code normally uses [`Self::prompt`] and never clones the allocation.
    pub fn prompt_blob_arc(&self, prompt_id: &str) -> Result<Arc<[u8]>, ImageError> {
        let descriptor = self
            .manifest
            .body
            .prompt_blobs
            .iter()
            .find(|prompt| prompt.id == prompt_id)
            .ok_or_else(|| ImageError::UnknownReference {
                kind: "loaded prompt",
                id: prompt_id.to_owned(),
            })?;
        self.blobs
            .get(&descriptor.content_hash)
            .cloned()
            .ok_or_else(|| ImageError::MissingBlob(descriptor.content_hash.clone()))
    }
}

impl AgentImageManifest {
    pub fn contract(&self, contract_id: &str) -> Option<&ContractSpec> {
        self.body
            .contracts
            .iter()
            .find(|contract| contract.id == contract_id)
    }

    pub fn resolve_capability_contracts(
        &self,
        capability: &CapabilitySpec,
    ) -> Result<ResolvedCapabilityContracts, ImageError> {
        let input = self
            .contract(&capability.input_contract)
            .ok_or_else(|| ImageError::UnknownReference {
                kind: "capability input contract",
                id: capability.input_contract.clone(),
            })?
            .clone();
        if capability.output_contracts.is_empty() {
            return Err(ImageError::InvalidSpec(format!(
                "capability {} has no output contract",
                capability.id
            )));
        }
        let outputs = capability
            .output_contracts
            .iter()
            .map(|contract_id| {
                self.contract(contract_id)
                    .cloned()
                    .ok_or_else(|| ImageError::UnknownReference {
                        kind: "capability output contract",
                        id: contract_id.clone(),
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let output_contract_set_hash = composite_contract_hash(&outputs)?;
        Ok(ResolvedCapabilityContracts {
            input,
            outputs,
            output_contract_set_hash,
        })
    }

    /// Resolve the schema exposed to the model for a capability proposal. A
    /// closed kernel assembler may deliberately expose a narrower schema than
    /// the physical capability ABI.
    pub fn resolve_capability_model_input_contract(
        &self,
        capability: &CapabilitySpec,
    ) -> Result<ContractSpec, ImageError> {
        let contract_id = capability
            .model_input_contract
            .as_deref()
            .unwrap_or(&capability.input_contract);
        self.contract(contract_id)
            .cloned()
            .ok_or_else(|| ImageError::UnknownReference {
                kind: "capability model input contract",
                id: contract_id.to_owned(),
            })
    }

    /// Return the output budget held back while a particular workflow is still
    /// researching.  The image-wide policy is a floor, while the workflow's
    /// largest composer determines the extra room needed for one complete
    /// retry.  This keeps a long-form company answer from being cut off by the
    /// cap chosen for a short-form workflow in the same image.
    pub fn effective_final_output_reserve_tokens(
        &self,
        workflow_id: &str,
    ) -> Result<Option<u32>, ImageError> {
        let Some(base_reserve) = self.body.answer_policy.final_output_reserve_tokens else {
            return Ok(None);
        };
        let workflow = self
            .body
            .workflows
            .iter()
            .find(|workflow| workflow.id == workflow_id)
            .ok_or_else(|| ImageError::UnknownReference {
                kind: "compiled workflow",
                id: workflow_id.to_owned(),
            })?;
        let max_composer_cap = workflow
            .states
            .iter()
            .filter(|state| state.kind == StateKind::Compose)
            .filter_map(|state| state.role_id.as_deref())
            .map(|role_id| {
                self.body
                    .roles
                    .iter()
                    .find(|role| role.id == role_id)
                    .ok_or_else(|| ImageError::UnknownReference {
                        kind: "compose role",
                        id: role_id.to_owned(),
                    })?
                    .execution
                    .max_output_tokens
                    .ok_or_else(|| {
                        ImageError::InvalidSpec(format!(
                            "compose role {role_id} must declare an output token cap when final output is reserved"
                        ))
                    })
            })
            .collect::<Result<Vec<_>, ImageError>>()?
            .into_iter()
            .max()
            .unwrap_or_default();
        let retry_reserve = max_composer_cap
            .checked_mul(2)
            .ok_or_else(|| ImageError::InvalidSpec("compose retry reserve overflow".into()))?;
        Ok(Some(base_reserve.max(retry_reserve)))
    }

    /// Materialize the closed typed-state program pinned by this immutable
    /// image. Runtime code interprets `operation` directly and never infers an
    /// executable handler from a legacy state name or `StateKind`.
    pub fn state_program(&self, workflow_id: &str) -> Result<StateProgram, ImageError> {
        let workflow = self
            .body
            .workflows
            .iter()
            .find(|workflow| workflow.id == workflow_id)
            .ok_or_else(|| ImageError::UnknownReference {
                kind: "compiled workflow",
                id: workflow_id.to_owned(),
            })?;
        let states = workflow
            .states
            .iter()
            .map(|state| StateNode {
                id: state.stable_id.clone(),
                operation: state.operation.clone(),
                max_visits: state.max_visits,
            })
            .collect();
        let transitions = workflow
            .transitions
            .iter()
            .map(|transition| {
                let from = workflow
                    .states
                    .iter()
                    .find(|state| state.numeric_id == transition.from)
                    .ok_or(ImageError::InvalidCompiledWorkflow)?;
                let to = workflow
                    .states
                    .iter()
                    .find(|state| state.numeric_id == transition.to)
                    .ok_or(ImageError::InvalidCompiledWorkflow)?;
                Ok(ArtifactTransition {
                    from: from.stable_id.clone(),
                    event: Some(transition.event.clone()),
                    guard: artifact_guard(&transition.guard),
                    to: to.stable_id.clone(),
                })
            })
            .collect::<Result<Vec<_>, ImageError>>()?;
        let initial_state = workflow
            .states
            .iter()
            .find(|state| state.numeric_id == workflow.initial)
            .ok_or(ImageError::InvalidCompiledWorkflow)?
            .stable_id
            .clone();
        let max_fuel = workflow.states.iter().try_fold(0_u32, |fuel, state| {
            fuel.checked_add(u32::from(state.max_visits))
                .ok_or(ImageError::Limit("typed workflow fuel"))
        })?;
        let program = StateProgram {
            image_hash: self.content_hash.clone(),
            workflow_id: workflow.id.clone(),
            initial_state,
            states,
            transitions,
            max_fuel,
        };
        program.validate()?;
        Ok(program)
    }
}

fn artifact_guard(guard: &TransitionGuard) -> ArtifactGuard {
    match guard {
        TransitionGuard::Always => ArtifactGuard::Always,
        TransitionGuard::FieldPresent { pointer } => ArtifactGuard::FieldPresent {
            pointer: pointer.clone(),
        },
        TransitionGuard::FieldAbsent { pointer } => ArtifactGuard::FieldAbsent {
            pointer: pointer.clone(),
        },
        TransitionGuard::FieldEquals { pointer, value } => ArtifactGuard::FieldEquals {
            pointer: pointer.clone(),
            value: value.clone(),
        },
        TransitionGuard::FieldNotEquals { pointer, value } => ArtifactGuard::FieldNotEquals {
            pointer: pointer.clone(),
            value: value.clone(),
        },
    }
}

#[derive(Serialize)]
struct CompositeContractHash<'a> {
    format: &'static str,
    contracts: Vec<CompositeContractEntry<'a>>,
}

#[derive(Serialize)]
struct CompositeContractEntry<'a> {
    id: &'a str,
    schema_ref: &'a str,
    content_hash: &'a ContentHash,
}

pub fn composite_contract_hash(contracts: &[ContractSpec]) -> Result<ContentHash, ImageError> {
    if contracts.is_empty() {
        return Err(ImageError::InvalidSpec(
            "composite contract set must not be empty".into(),
        ));
    }
    let envelope = CompositeContractHash {
        format: "krw.agent/composite-contract-v1",
        contracts: contracts
            .iter()
            .map(|contract| CompositeContractEntry {
                id: &contract.id,
                schema_ref: &contract.schema_ref,
                content_hash: &contract.content_hash,
            })
            .collect(),
    };
    Ok(ContentHash::sha256(serde_jcs::to_vec(&envelope)?))
}

pub fn parse_spec(bytes: &[u8]) -> Result<AgentSpec, ImageError> {
    let spec: AgentSpec = serde_yaml_ng::from_slice(bytes)?;
    Ok(spec)
}

/// Resolve a single prompt segment to its byte content. Static segments are
/// read from `path`; ontology-catalog segments are generated from YAML at
/// build time. Exactly one source must be present.
fn load_prompt_segment_bytes(
    root: &Path,
    segment: &PromptSegmentSource,
    all_segments: &[PromptSegmentSource],
) -> Result<Vec<u8>, ImageError> {
    let source_count = [
        segment.path.is_some(),
        segment.ontology_schema.is_some(),
        segment.skill_catalog.is_some(),
    ]
    .iter()
    .filter(|&&flag| flag)
    .count();
    if source_count != 1 {
        return Err(ImageError::InvalidSpec(format!(
            "prompt segment `{}` must declare exactly one of path, ontology_schema, or skill_catalog (found {source_count})",
            segment.id
        )));
    }
    if let Some(path) = &segment.path {
        let resolved = safe_source_path(root, path)?;
        Ok(fs::read(resolved)?)
    } else if let Some(source) = &segment.ontology_schema {
        let markdown = render_ontology_catalog(root, source, &segment.id)?;
        Ok(markdown.into_bytes())
    } else if let Some(source) = &segment.skill_catalog {
        let markdown = render_skill_catalog(source, root, &segment.id, all_segments)?;
        Ok(markdown.into_bytes())
    } else {
        unreachable!("source_count == 1 guard guarantees one branch is taken")
    }
}

/// Render an AI-facing ontology catalog Markdown blob from the declared schema
/// YAML files. Each schema contributes a section: metrics, quote-type search
/// hints, claim types, risk categories, and searchable object types.
fn render_ontology_catalog(
    root: &Path,
    source: &OntologySchemaSource,
    segment_id: &str,
) -> Result<String, ImageError> {
    let (schema_dir, source_label) = resolve_ontology_schema_dir(root, source, segment_id)?;
    let mut out = String::new();
    out.push_str("<!-- AUTO-GENERATED from ontology schema YAML. Do not edit by hand.\n");
    out.push_str("     Source: ");
    out.push_str(&source_label);
    out.push_str(" -->\n\n");
    out.push_str("# Ontology catalog\n\n");
    out.push_str(
        "This catalog is generated at image build time from the immutable ontology \
         schema. Use the metric identifiers, filing-language aliases, search hints, \
         and object types listed here when authoring a ResearchProposal or deciding \
         what evidence to request. The kernel validates submitted values against \
         these same schemas.\n\n",
    );
    for schema in &source.schemas {
        let path = schema_dir.join(format!("{schema}.yaml"));
        let text = fs::read_to_string(&path).map_err(|_| {
            ImageError::InvalidSpec(format!(
                "ontology_schema segment `{segment_id}` cannot read {}",
                path.display()
            ))
        })?;
        let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(&text).map_err(|err| {
            ImageError::InvalidSpec(format!(
                "ontology_schema segment `{segment_id}` failed to parse {}: {err}",
                path.display()
            ))
        })?;
        match schema.as_str() {
            "metric_dictionary" => render_metric_section(&value, &mut out),
            "quote_types" => render_quote_types_section(&value, &mut out),
            "claim_types" => render_claim_types_section(&value, &mut out),
            "risk_categories" => render_risk_categories_section(&value, &mut out),
            "objects" => render_objects_section(&value, &mut out),
            other => {
                out.push_str(&format!("## {other}\n\n(No renderer for this schema.)\n\n"));
            }
        }
    }
    Ok(out)
}

fn resolve_ontology_schema_dir(
    root: &Path,
    source: &OntologySchemaSource,
    segment_id: &str,
) -> Result<(PathBuf, String), ImageError> {
    match (source.root_env.as_deref(), source.bundled_runtime_schema) {
        (Some(root_env), false) => {
            let ontology_root = env::var(root_env).map_err(|_| {
                ImageError::InvalidSpec(format!(
                    "ontology_schema segment `{segment_id}` requires env `{root_env}` to be set"
                ))
            })?;
            Ok((
                PathBuf::from(ontology_root).join("ontology/schema"),
                format!("${root_env}/ontology/schema/*.yaml"),
            ))
        }
        (None, true) => {
            let workspace_root = root.parent().and_then(Path::parent).ok_or_else(|| {
                ImageError::InvalidSpec(format!(
                    "ontology_schema segment `{segment_id}` cannot locate workspace root"
                ))
            })?;
            let workspace_root = fs::canonicalize(workspace_root)?;
            let schema_dir = fs::canonicalize(workspace_root.join(
                "services/krw-ontology-runtime/src/krw_capability_runtime/resources/ontology/schema",
            ))
            .map_err(|_| {
                ImageError::InvalidSpec(format!(
                    "ontology_schema segment `{segment_id}` cannot read bundled runtime schema"
                ))
            })?;
            if !schema_dir.starts_with(&workspace_root) {
                return Err(ImageError::InvalidSpec(format!(
                    "ontology_schema segment `{segment_id}` bundled schema escapes workspace"
                )));
            }
            Ok((schema_dir, "bundled runtime ontology schema".into()))
        }
        _ => Err(ImageError::InvalidSpec(format!(
            "ontology_schema segment `{segment_id}` must declare exactly one of root_env or bundled_runtime_schema"
        ))),
    }
}

/// Render a compact skill catalog from the explicit image-level skill
/// allowlist. The catalog and `skill.load` therefore share the same canonical
/// IDs: a model can never be shown a frontmatter name that the local loader
/// cannot resolve, nor request an internal policy segment by guessing its ID.
fn render_skill_catalog(
    source: &SkillCatalogSource,
    root: &Path,
    segment_id: &str,
    all_segments: &[PromptSegmentSource],
) -> Result<String, ImageError> {
    let catalog_dirs = resolve_skill_catalog_dirs(source, root, segment_id)?;
    let mut entries: Vec<(String, String, Option<String>)> = Vec::new();
    for candidate in all_segments.iter().filter(|candidate| candidate.loadable) {
        let path = candidate.path.as_deref().ok_or_else(|| {
            ImageError::InvalidSpec(format!(
                "loadable skill {} must use a static path source",
                candidate.id
            ))
        })?;
        let file_path = safe_source_path(root, path)?;
        if !catalog_dirs.iter().any(|dir| file_path.starts_with(dir)) {
            continue;
        }
        let content = fs::read_to_string(&file_path)?;
        let (description, when_to_use) = skill_frontmatter_metadata(&content, &candidate.id)?;
        entries.push((candidate.id.clone(), description, when_to_use));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));

    let mut out = String::new();
    out.push_str("<!-- AUTO-GENERATED skill catalog. Do not edit by hand.\n");
    out.push_str("     Source: explicitly registered loadable prompt segments. -->\n\n");
    out.push_str("# Skill catalog\n\n");
    out.push_str(
        "Each skill below is available on demand. To use one, call the \
         `skill.load` function with `{ \"skill_id\": \"<catalog-id>\" }`. The function \
         returns the full skill body; read it and follow it. Do not guess skill \
         contents — load the body first when a skill is relevant to the current \
         question. Compare the active question and role with each description and \
         `use when` hint. You may load more than one distinct relevant skill, one \
         `skill.load` call at a time, when each adds guidance not already supplied \
         by the active role. Do not reload a skill or load unrelated skills.\n\n",
    );
    if entries.is_empty() {
        out.push_str("(No registered skills are available in this catalog.)\n");
    } else {
        for (name, description, when_to_use) in &entries {
            out.push_str(&format!("- **{name}**: {description}"));
            if let Some(w) = when_to_use {
                out.push_str(&format!(" (use when: {w})"));
            }
            out.push('\n');
        }
    }
    Ok(out)
}

fn resolve_skill_catalog_dirs(
    source: &SkillCatalogSource,
    root: &Path,
    segment_id: &str,
) -> Result<Vec<PathBuf>, ImageError> {
    if source.skills_dirs.is_empty() {
        return Err(ImageError::InvalidSpec(format!(
            "skill_catalog segment `{segment_id}` declares an empty skills_dirs list"
        )));
    }
    source
        .skills_dirs
        .iter()
        .map(|dir| {
            let resolved = safe_source_path(root, dir)?;
            if !resolved.is_dir() {
                return Err(ImageError::InvalidSpec(format!(
                    "skill_catalog segment `{segment_id}`: {} is not a directory",
                    resolved.display()
                )));
            }
            Ok(resolved)
        })
        .collect()
}

fn skill_frontmatter_metadata(
    content: &str,
    segment_id: &str,
) -> Result<(String, Option<String>), ImageError> {
    let frontmatter = parse_frontmatter(content).ok_or_else(|| {
        ImageError::InvalidSpec(format!(
            "loadable skill {segment_id} must declare YAML frontmatter"
        ))
    })?;
    let name = frontmatter
        .get("name")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            ImageError::InvalidSpec(format!(
                "loadable skill {segment_id} frontmatter must include a name"
            ))
        })?;
    let description = frontmatter
        .get("description")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            ImageError::InvalidSpec(format!(
                "loadable skill {segment_id} frontmatter must include a description"
            ))
        })?;
    let when_to_use = frontmatter
        .get("when_to_use")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned);
    // Frontmatter names are human labels. The immutable prompt-segment ID is
    // the actual callable skill_id, so punctuation mismatches in Markdown
    // cannot break the provider tool contract.
    let _ = name;
    Ok((description.to_owned(), when_to_use))
}

fn validate_loadable_skill_sources(
    root: &Path,
    segments: &[PromptSegmentSource],
) -> Result<(), ImageError> {
    let mut catalog_dirs = Vec::new();
    for catalog in segments
        .iter()
        .filter_map(|segment| segment.skill_catalog.as_ref())
    {
        catalog_dirs.extend(resolve_skill_catalog_dirs(catalog, root, "registered")?);
    }
    for segment in segments.iter().filter(|segment| segment.loadable) {
        if !is_valid_skill_id(&segment.id) {
            return Err(ImageError::InvalidSpec(format!(
                "loadable skill id {} must be lowercase snake_case",
                segment.id
            )));
        }
        let path = segment.path.as_deref().ok_or_else(|| {
            ImageError::InvalidSpec(format!(
                "loadable skill {} must use a static path source",
                segment.id
            ))
        })?;
        let resolved = safe_source_path(root, path)?;
        if !catalog_dirs.iter().any(|dir| resolved.starts_with(dir)) {
            return Err(ImageError::InvalidSpec(format!(
                "loadable skill {} is absent from every skill catalog directory",
                segment.id
            )));
        }
        let content = fs::read_to_string(resolved)?;
        let _ = skill_frontmatter_metadata(&content, &segment.id)?;
    }
    Ok(())
}

fn is_valid_skill_id(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_lowercase())
        && value.len() <= 128
        && chars.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
}

/// Parse a YAML frontmatter block delimited by `---` lines at the start of
/// `content`. Returns the parsed mapping, or `None` if no frontmatter is
/// present or parsing fails.
fn parse_frontmatter(content: &str) -> Option<serde_yaml_ng::Value> {
    let trimmed = content.trim_start_matches('\u{feff}');
    if !trimmed.starts_with("---\n") && !trimmed.starts_with("---\r\n") {
        return None;
    }
    let after_open = &trimmed[3..];
    let body_start = after_open
        .strip_prefix('\n')
        .or_else(|| after_open.strip_prefix("\r\n"))
        .unwrap_or(after_open);
    // Find the closing `---` on its own line.
    let close_idx = body_start
        .find("\n---\n")
        .or_else(|| body_start.find("\n---\r\n"))
        .or_else(|| {
            body_start
                .rfind("\n---")
                .filter(|&i| body_start[i..].trim() == "---")
        })?;
    let frontmatter_text = &body_start[..close_idx];
    serde_yaml_ng::from_str(frontmatter_text).ok()
}

fn render_metric_section(value: &serde_yaml_ng::Value, out: &mut String) {
    let Some(metrics) = value.get("canonical_metrics").and_then(|v| v.as_mapping()) else {
        return;
    };
    out.push_str("## Metrics\n\n");
    out.push_str(
        "Use exactly one `metric` identifier in a metric goal. Each entry lists the \
         canonical identifier, display name, filing-language aliases, and a short \
         description.\n\n",
    );
    out.push_str("| Identifier | Display name | Aliases | Description |\n");
    out.push_str("|---|---|---|---|\n");
    for (key, entry) in metrics {
        let id = key.as_str().unwrap_or("?");
        let display = entry
            .get("display_name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let aliases = entry
            .get("aliases")
            .and_then(|v| v.as_sequence())
            .map(|seq| {
                seq.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let desc = entry
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        out.push_str(&format!("| `{id}` | {display} | {aliases} | {desc} |\n"));
    }
    out.push('\n');
}

fn render_quote_types_section(value: &serde_yaml_ng::Value, out: &mut String) {
    let Some(types) = value.get("quote_types").and_then(|v| v.as_mapping()) else {
        return;
    };
    out.push_str("## Quote type search hints\n\n");
    out.push_str(
        "When the evidence you need is qualitative, these are the canonical quote \
         types and the filing vocabulary that tends to co-occur with each.\n\n",
    );
    for (key, entry) in types {
        let id = key.as_str().unwrap_or("?");
        let guidance = entry
            .get("ai_guidance")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        out.push_str(&format!("- **{id}**: {guidance}\n"));
    }
    out.push('\n');
}

fn render_claim_types_section(value: &serde_yaml_ng::Value, out: &mut String) {
    let Some(types) = value.get("claim_types").and_then(|v| v.as_mapping()) else {
        return;
    };
    out.push_str("## Claim types\n\n");
    out.push_str("Canonical claim categories and example filing language.\n\n");
    for (key, entry) in types {
        let id = key.as_str().unwrap_or("?");
        let desc = entry
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let example = entry.get("example").and_then(|v| v.as_str()).unwrap_or("");
        out.push_str(&format!("- **{id}**: {desc}\n"));
        if !example.is_empty() {
            out.push_str(&format!("  - Example: \"{example}\"\n"));
        }
    }
    out.push('\n');
}

fn render_risk_categories_section(value: &serde_yaml_ng::Value, out: &mut String) {
    let Some(cats) = value.get("risk_categories").and_then(|v| v.as_mapping()) else {
        return;
    };
    out.push_str("## Risk categories\n\n");
    out.push_str("Use these category labels and their example factors when researching risk.\n\n");
    for (key, entry) in cats {
        let id = key.as_str().unwrap_or("?");
        let desc = entry
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let examples = entry
            .get("examples")
            .and_then(|v| v.as_sequence())
            .map(|seq| {
                seq.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        out.push_str(&format!("- **{id}**: {desc} (examples: {examples})\n"));
    }
    out.push('\n');
}

fn render_objects_section(value: &serde_yaml_ng::Value, out: &mut String) {
    let Some(types) = value.get("types").and_then(|v| v.as_mapping()) else {
        return;
    };
    out.push_str("## Searchable object types\n\n");
    out.push_str(
        "These are the object types the ontology can return as evidence. Use the \
         exact type name in `object_types` when you want a specific kind of \
         evidence.\n\n",
    );
    for (key, _) in types {
        let id = key.as_str().unwrap_or("?");
        out.push_str(&format!("- `{id}`\n"));
    }
    out.push('\n');
}

pub fn compile_agent_dir(root: impl AsRef<Path>) -> Result<CompiledImage, ImageError> {
    let root = fs::canonicalize(root.as_ref())?;
    let spec_path = root.join("agent.yaml");
    let source_bytes = fs::read(&spec_path)?;
    if source_bytes.len() > MAX_SPEC_BYTES {
        return Err(ImageError::Limit("AgentSpec bytes"));
    }
    let spec = parse_spec(&source_bytes)?;
    validate_spec(&spec)?;
    validate_loadable_skill_sources(&root, &spec.prompt_segments)?;

    let mut blobs = BTreeMap::new();
    let mut prompt_blobs = Vec::with_capacity(spec.prompt_segments.len());
    let mut total_prompt_bytes = 0_usize;
    for segment in &spec.prompt_segments {
        let bytes = load_prompt_segment_bytes(&root, segment, &spec.prompt_segments)?;
        if bytes.len() > MAX_PROMPT_BLOB_BYTES {
            return Err(ImageError::Limit("prompt blob bytes"));
        }
        total_prompt_bytes = total_prompt_bytes
            .checked_add(bytes.len())
            .ok_or(ImageError::Limit("total prompt bytes"))?;
        if total_prompt_bytes > MAX_TOTAL_PROMPT_BYTES {
            return Err(ImageError::Limit("total prompt bytes"));
        }
        let prompt = std::str::from_utf8(&bytes)
            .map_err(|_| ImageError::InvalidPromptUtf8(segment.id.clone()))?;
        if prompt.contains('\0') {
            return Err(ImageError::InvalidPromptText(segment.id.clone()));
        }
        let hash = ContentHash::sha256(&bytes);
        prompt_blobs.push(PromptBlob {
            id: segment.id.clone(),
            content_hash: hash.clone(),
            byte_len: u64::try_from(bytes.len())
                .map_err(|_| ImageError::Limit("prompt byte length"))?,
            stable_prefix: segment.stable_prefix,
            private: segment.private,
            loadable: segment.loadable,
        });
        blobs.entry(hash).or_insert(bytes);
    }
    prompt_blobs.sort_by(|left, right| left.id.cmp(&right.id));

    let source_value = serde_json::to_value(&spec)?;
    let source_hash = ContentHash::sha256(serde_jcs::to_vec(&source_value)?);
    let contract_pins = spec
        .contracts
        .iter()
        .map(|contract| (contract.id.clone(), contract_pin(contract)))
        .collect::<BTreeMap<_, _>>();
    let workflows = spec
        .workflows
        .iter()
        .map(|workflow| {
            compile_workflow(
                workflow,
                &spec.capabilities,
                &contract_pins,
                &spec.answer_policy.internal_format,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let body = AgentImageBody {
        format_version: AGENT_IMAGE_FORMAT_VERSION,
        rule_isa_version: RULE_ISA_VERSION,
        compiler_version: COMPILER_VERSION.to_owned(),
        source_hash,
        metadata: spec.metadata,
        entrypoints: spec.entrypoints,
        contracts: spec.contracts,
        roles: spec.roles,
        prompt_blobs,
        capabilities: spec.capabilities,
        workflows,
        planning_policy: spec.planning_policy,
        evidence_policy: spec.evidence_policy,
        period_policy: spec.period_policy,
        answer_policy: spec.answer_policy,
        security_policy: spec.security_policy,
        validators: spec.validators,
    };
    let content_hash = ContentHash::sha256(serde_jcs::to_vec(&body)?);
    Ok(CompiledImage {
        manifest: AgentImageManifest { content_hash, body },
        blobs,
    })
}

pub fn write_image(image: &CompiledImage, output: impl AsRef<Path>) -> Result<(), ImageError> {
    let output = output.as_ref();
    if output.exists() {
        return Err(ImageError::OutputExists(output.display().to_string()));
    }
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let file_name = output
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ImageError::InvalidSpec("image output must have a file name".into()))?;
    let staging = parent.join(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        IMAGE_WRITE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&staging)?;
    let write_result = (|| {
        let blob_dir = staging.join("blobs/sha256");
        fs::create_dir_all(&blob_dir)?;
        let manifest = serde_jcs::to_vec(&image.manifest)?;
        write_synced(&staging.join("manifest.json"), &manifest)?;
        for (hash, bytes) in &image.blobs {
            let digest = hash
                .as_str()
                .strip_prefix("sha256:")
                .ok_or_else(|| ImageError::InvalidSpec("invalid blob hash".into()))?;
            write_synced(&blob_dir.join(digest), bytes)?;
        }
        fs::File::open(&blob_dir)?.sync_all()?;
        fs::File::open(&staging)?.sync_all()?;
        fs::rename(&staging, output)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if write_result.is_err() && staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }
    write_result
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), ImageError> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

pub fn load_image(root: impl AsRef<Path>) -> Result<LoadedImage, ImageError> {
    load_image_with_interner(root, &mut PromptBlobInterner::default())
}

/// Load one immutable image into a caller-owned machine-wide prompt interner.
pub fn load_image_with_interner(
    root: impl AsRef<Path>,
    interner: &mut PromptBlobInterner,
) -> Result<LoadedImage, ImageError> {
    let root = fs::canonicalize(root.as_ref())?;
    let manifest_path = safe_image_path(&root, Path::new("manifest.json"))?;
    let metadata = fs::metadata(&manifest_path)?;
    if metadata.len() > MAX_MANIFEST_BYTES as u64 {
        return Err(ImageError::Limit("AgentImage manifest bytes"));
    }
    let bytes = fs::read(manifest_path)?;
    let manifest: AgentImageManifest = serde_json::from_slice(&bytes)?;
    validate_loaded_manifest(&manifest)?;
    let mut blobs = BTreeMap::new();
    let mut total_bytes = 0_usize;
    for descriptor in &manifest.body.prompt_blobs {
        let digest = descriptor
            .content_hash
            .as_str()
            .strip_prefix("sha256:")
            .ok_or_else(|| ImageError::InvalidSpec("invalid blob hash".into()))?;
        let relative = Path::new("blobs").join("sha256").join(digest);
        let path = safe_image_path(&root, &relative)?;
        let blob = fs::read(path)?;
        validate_prompt_blob(descriptor, &blob, &mut total_bytes)?;
        blobs
            .entry(descriptor.content_hash.clone())
            .or_insert(interner.intern(&descriptor.content_hash, &blob)?);
    }
    let image = LoadedImage { manifest, blobs };
    image.verify()?;
    Ok(image)
}

/// Load a complete startup image set with one content-addressed prompt store.
pub fn load_image_set<I, P>(roots: I) -> Result<Vec<LoadedImage>, ImageError>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    let mut interner = PromptBlobInterner::default();
    roots
        .into_iter()
        .map(|root| load_image_with_interner(root, &mut interner))
        .collect()
}

fn validate_loaded_manifest(manifest: &AgentImageManifest) -> Result<(), ImageError> {
    if manifest.body.format_version != AGENT_IMAGE_FORMAT_VERSION {
        return Err(ImageError::UnsupportedImageFormat(
            manifest.body.format_version,
        ));
    }
    if manifest.body.rule_isa_version != RULE_ISA_VERSION {
        return Err(ImageError::UnsupportedRuleIsa(
            manifest.body.rule_isa_version,
        ));
    }
    let expected = ContentHash::sha256(serde_jcs::to_vec(&manifest.body)?);
    if expected != manifest.content_hash {
        return Err(ImageError::ImageHashMismatch {
            expected,
            observed: manifest.content_hash.clone(),
        });
    }
    unique(
        manifest
            .body
            .prompt_blobs
            .iter()
            .map(|prompt| prompt.id.as_str()),
        "compiled prompt",
    )?;
    validate_contract_declarations(&manifest.body.contracts)?;
    for capability in &manifest.body.capabilities {
        unique(
            capability.output_contracts.iter().map(String::as_str),
            "capability output contract",
        )?;
        manifest.resolve_capability_contracts(capability)?;
    }
    for workflow in &manifest.body.workflows {
        manifest.state_program(&workflow.id)?;
    }
    Ok(())
}

fn validate_prompt_blob(
    descriptor: &PromptBlob,
    blob: &[u8],
    total_bytes: &mut usize,
) -> Result<(), ImageError> {
    if descriptor.byte_len > MAX_PROMPT_BLOB_BYTES as u64 {
        return Err(ImageError::Limit("prompt blob bytes"));
    }
    if blob.len() as u64 != descriptor.byte_len {
        return Err(ImageError::BlobLengthMismatch {
            hash: descriptor.content_hash.clone(),
            expected: descriptor.byte_len,
            observed: blob.len() as u64,
        });
    }
    let observed = ContentHash::sha256(blob);
    if observed != descriptor.content_hash {
        return Err(ImageError::BlobHashMismatch {
            expected: descriptor.content_hash.clone(),
            observed,
        });
    }
    let prompt = std::str::from_utf8(blob)
        .map_err(|_| ImageError::InvalidPromptUtf8(descriptor.id.clone()))?;
    if prompt.contains('\0') {
        return Err(ImageError::InvalidPromptText(descriptor.id.clone()));
    }
    *total_bytes = total_bytes
        .checked_add(blob.len())
        .ok_or(ImageError::Limit("total prompt bytes"))?;
    if *total_bytes > MAX_TOTAL_PROMPT_BYTES {
        return Err(ImageError::Limit("total prompt bytes"));
    }
    Ok(())
}

fn safe_image_path(root: &Path, relative: &Path) -> Result<PathBuf, ImageError> {
    let path = fs::canonicalize(root.join(relative))?;
    if !path.starts_with(root) {
        return Err(ImageError::PathEscape(relative.display().to_string()));
    }
    Ok(path)
}

pub fn validate_spec(spec: &AgentSpec) -> Result<(), ImageError> {
    if spec.api_version != AGENT_SPEC_API_VERSION || spec.kind != "AgentSpec" {
        return Err(ImageError::InvalidSpec(
            "unsupported api_version or kind".into(),
        ));
    }
    if spec.metadata.id.trim().is_empty() || spec.metadata.version.trim().is_empty() {
        return Err(ImageError::InvalidSpec(
            "agent id and version are required".into(),
        ));
    }
    unique(spec.roles.iter().map(|role| role.id.as_str()), "role")?;
    unique(
        spec.prompt_segments.iter().map(|prompt| prompt.id.as_str()),
        "prompt segment",
    )?;
    unique(
        spec.capabilities
            .iter()
            .map(|capability| capability.id.as_str()),
        "capability",
    )?;
    unique(
        spec.workflows.iter().map(|workflow| workflow.id.as_str()),
        "workflow",
    )?;
    validate_contract_declarations(&spec.contracts)?;
    unique(
        spec.validators.iter().map(|program| program.id.as_str()),
        "validator",
    )?;

    if spec.capabilities.len() > MAX_CAPABILITIES {
        return Err(ImageError::Limit("capabilities"));
    }
    if spec.prompt_segments.len() > MAX_PROMPT_SEGMENTS {
        return Err(ImageError::Limit("prompt segments"));
    }
    if spec.security_policy.filesystem_access
        || spec.security_policy.web_access
        || spec.security_policy.mutation_capabilities
        || spec.security_policy.reveal_hidden_prompt
    {
        return Err(ImageError::InvalidSpec(
            "the current read-only KRW kernel forbids filesystem, web, mutation, and hidden-prompt disclosure"
                .into(),
        ));
    }
    if spec
        .capabilities
        .iter()
        .any(|capability| capability.permission != Permission::Read)
    {
        return Err(ImageError::InvalidSpec(
            "the current capability ABI is read-only".into(),
        ));
    }
    if !spec.evidence_policy.grade_never_upgrades_directness {
        return Err(ImageError::InvalidSpec(
            "evidence grade and directness must remain independent axes".into(),
        ));
    }
    if spec.planning_policy.correction_field != "violations" {
        return Err(ImageError::InvalidSpec(
            "query_context correction contract uses violations[*]".into(),
        ));
    }

    let role_ids = spec
        .roles
        .iter()
        .map(|role| role.id.as_str())
        .collect::<BTreeSet<_>>();
    let prompt_ids = spec
        .prompt_segments
        .iter()
        .map(|prompt| prompt.id.as_str())
        .collect::<BTreeSet<_>>();
    for entrypoint in spec.entrypoints.values() {
        for pinned in &entrypoint.pinned_skills {
            if !prompt_ids.contains(pinned.as_str()) {
                return Err(ImageError::UnknownReference {
                    kind: "pinned prompt segment",
                    id: pinned.clone(),
                });
            }
        }
    }
    for role in &spec.roles {
        if role.deterministic && !role.execution.is_default() {
            return Err(ImageError::InvalidSpec(format!(
                "deterministic role {} cannot declare a model execution policy",
                role.id
            )));
        }
        if role
            .execution
            .max_output_tokens
            .is_some_and(|limit| !(128..=64_000).contains(&limit))
        {
            return Err(ImageError::InvalidSpec(format!(
                "role {} output token cap must be between 128 and 64000",
                role.id
            )));
        }
        for prompt in &role.prompt_segments {
            if !prompt_ids.contains(prompt.as_str()) {
                return Err(ImageError::UnknownReference {
                    kind: "prompt segment",
                    id: prompt.clone(),
                });
            }
        }
    }
    if role_ids.is_empty() {
        return Err(ImageError::InvalidSpec(
            "at least one role is required".into(),
        ));
    }

    let workflow_ids = spec
        .workflows
        .iter()
        .map(|item| item.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut entrypoint_selectors = BTreeSet::new();
    for entrypoint in spec.entrypoints.values() {
        if entrypoint.required_budget_profile.is_empty()
            || entrypoint.required_model_profile.is_empty()
        {
            return Err(ImageError::InvalidSpec(
                "entrypoint budget and model profiles are required".into(),
            ));
        }
        if !entrypoint_selectors.insert((entrypoint.run_kind.as_str(), entrypoint.locale.as_str()))
        {
            return Err(ImageError::InvalidSpec(
                "entrypoint run_kind/locale selectors must be unique".into(),
            ));
        }
        if !workflow_ids.contains(entrypoint.workflow.as_str()) {
            return Err(ImageError::UnknownReference {
                kind: "workflow",
                id: entrypoint.workflow.clone(),
            });
        }
        validate_entrypoint_scope(entrypoint)?;
        let expected_author = GuruAuthor::expected_for_run_kind(&entrypoint.run_kind);
        if entrypoint.constants.fixed_guru_author != expected_author {
            return Err(ImageError::InvalidSpec(format!(
                "entrypoint {} has an invalid fixed Guru author constant",
                entrypoint.run_kind
            )));
        }
    }
    let capability_ids = spec
        .capabilities
        .iter()
        .map(|item| item.id.as_str())
        .collect::<BTreeSet<_>>();
    for role in &spec.roles {
        let Some(child) = role.bounded_child.as_ref() else {
            continue;
        };
        if role.deterministic
            || child.max_children != 1
            || child.max_depth != 1
            || child.allowed_capabilities.is_empty()
            || child.allowed_capabilities.len() > 8
        {
            return Err(ImageError::InvalidSpec(format!(
                "role {} has an invalid bounded child policy",
                role.id
            )));
        }
        unique(
            child.allowed_capabilities.iter().map(String::as_str),
            "bounded child capability",
        )?;
        child
            .reservation
            .validate_for(&child.allowed_capabilities)
            .map_err(|error| {
                ImageError::InvalidSpec(format!(
                    "role {} has an invalid bounded child reservation: {error}",
                    role.id
                ))
            })?;
        for capability_id in &child.allowed_capabilities {
            let capability = spec
                .capabilities
                .iter()
                .find(|candidate| candidate.id == *capability_id)
                .ok_or_else(|| ImageError::UnknownReference {
                    kind: "bounded child capability",
                    id: capability_id.clone(),
                })?;
            if capability.permission != Permission::Read {
                return Err(ImageError::InvalidSpec(format!(
                    "bounded child capability {capability_id} must be read-only"
                )));
            }
        }
    }
    let contract_ids = spec
        .contracts
        .iter()
        .map(|item| item.id.as_str())
        .collect::<BTreeSet<_>>();
    for required in [
        STATE_OPERATION_OUTPUT_CONTRACT_ID,
        STATE_FACTS_CONTRACT_ID,
        spec.answer_policy.internal_format.as_str(),
    ] {
        if !contract_ids.contains(required) {
            return Err(ImageError::UnknownReference {
                kind: "typed state contract",
                id: required.to_owned(),
            });
        }
    }
    for capability in &spec.capabilities {
        if capability.binding_key.contains("://") {
            return Err(ImageError::InvalidSpec(format!(
                "capability {} embeds an endpoint instead of a symbolic binding key",
                capability.id
            )));
        }
        for prerequisite in &capability.prerequisites {
            if !capability_ids.contains(prerequisite.as_str()) {
                return Err(ImageError::UnknownReference {
                    kind: "capability prerequisite",
                    id: prerequisite.clone(),
                });
            }
        }
        if !contract_ids.contains(capability.input_contract.as_str()) {
            return Err(ImageError::UnknownReference {
                kind: "capability input contract",
                id: capability.input_contract.clone(),
            });
        }
        if let Some(model_input_contract) = &capability.model_input_contract
            && !contract_ids.contains(model_input_contract.as_str())
        {
            return Err(ImageError::UnknownReference {
                kind: "capability model input contract",
                id: model_input_contract.clone(),
            });
        }
        validate_capability_input_abi(capability, &spec.capabilities)?;
        validate_research_action_policy(capability)?;
        validate_scope_projection(capability)?;
        validate_capability_scope_binding(capability)?;
        if capability.output_contracts.is_empty() {
            return Err(ImageError::InvalidSpec(format!(
                "capability {} has no output contract",
                capability.id
            )));
        }
        unique(
            capability.output_contracts.iter().map(String::as_str),
            "capability output contract",
        )?;
        for output in &capability.output_contracts {
            if !contract_ids.contains(output.as_str()) {
                return Err(ImageError::UnknownReference {
                    kind: "capability output contract",
                    id: output.clone(),
                });
            }
        }
    }
    for workflow in &spec.workflows {
        validate_workflow(workflow, &spec.roles)?;
        for capability_id in workflow
            .states
            .iter()
            .filter_map(|state| state.capability_id.as_deref())
        {
            if !capability_ids.contains(capability_id) {
                return Err(ImageError::UnknownReference {
                    kind: "workflow capability",
                    id: capability_id.to_owned(),
                });
            }
        }
    }
    if spec.answer_policy.final_output_reserve_tokens.is_none()
        && (spec.answer_policy.minimum_research_turn_tokens.is_some()
            || spec.answer_policy.max_research_turn_tokens.is_some())
    {
        return Err(ImageError::InvalidSpec(
            "research turn policy requires a final output reserve".into(),
        ));
    }
    if let Some(reserve) = spec.answer_policy.final_output_reserve_tokens {
        let minimum_research_turn =
            spec.answer_policy
                .minimum_research_turn_tokens
                .ok_or_else(|| {
                    ImageError::InvalidSpec(
                        "final output reserve requires a minimum research turn policy".into(),
                    )
                })?;
        let max_research_turn = spec.answer_policy.max_research_turn_tokens.ok_or_else(|| {
            ImageError::InvalidSpec(
                "final output reserve requires a maximum research turn policy".into(),
            )
        })?;
        validate_final_output_reserve(spec, reserve, minimum_research_turn, max_research_turn)?;
    }
    for program in &spec.validators {
        validate_rule_program(program)?;
    }
    Ok(())
}

/// An image that reserves output for its terminal response must prove that the
/// reserve can actually be reached. Without this check a long research turn
/// could consume the last token and strand the run before composition. The
/// declared reserve is the common floor. Each workflow raises it to two times
/// its own composer cap, so a long-form answer does not force every shorter
/// workflow to carry the same reserve.
fn validate_final_output_reserve(
    spec: &AgentSpec,
    reserve: u32,
    minimum_research_turn: u32,
    max_research_turn: u32,
) -> Result<(), ImageError> {
    if !(256..=64_000).contains(&reserve) {
        return Err(ImageError::InvalidSpec(
            "final output reserve must be between 256 and 64000 tokens".into(),
        ));
    }
    if !(128..=16_384).contains(&minimum_research_turn)
        || reserve.checked_add(minimum_research_turn).is_none()
    {
        return Err(ImageError::InvalidSpec(
            "minimum research turn must be between 128 and 16384 tokens and fit with the final output reserve"
                .into(),
        ));
    }
    if !(128..=64_000).contains(&max_research_turn) || max_research_turn < minimum_research_turn {
        return Err(ImageError::InvalidSpec(
            "maximum research turn must be between 128 and 64000 tokens and no smaller than the minimum research turn"
                .into(),
        ));
    }

    let roles = spec
        .roles
        .iter()
        .map(|role| (role.id.as_str(), role))
        .collect::<BTreeMap<_, _>>();
    for workflow in &spec.workflows {
        for state in workflow
            .states
            .iter()
            .filter(|state| state.kind == StateKind::Compose)
        {
            let role_id = state
                .role_id
                .as_deref()
                .ok_or(ImageError::InvalidCompiledWorkflow)?;
            let role = roles
                .get(role_id)
                .ok_or_else(|| ImageError::UnknownReference {
                    kind: "compose role",
                    id: role_id.to_owned(),
                })?;
            let cap = role.execution.max_output_tokens.ok_or_else(|| {
                ImageError::InvalidSpec(format!(
                    "compose role {role_id} must declare an output token cap when final output is reserved"
                ))
            })?;
            let required_reserve = cap.checked_mul(2).ok_or_else(|| {
                ImageError::InvalidSpec(format!(
                    "compose role {role_id} output cap is too large to reserve a full retry"
                ))
            })?;
            if required_reserve > 64_000 {
                return Err(ImageError::InvalidSpec(format!(
                    "compose role {role_id} output cap exceeds the 64,000-token two-attempt reserve"
                )));
            }
        }

        for state in workflow
            .states
            .iter()
            .filter(|state| matches!(state.kind, StateKind::Ingest | StateKind::Assess))
        {
            let fallback = workflow
                .transitions
                .iter()
                .filter(|transition| {
                    transition.from == state.id && transition.on == "output_budget_reserved"
                })
                .collect::<Vec<_>>();
            let [fallback] = fallback.as_slice() else {
                return Err(ImageError::InvalidSpec(format!(
                    "{} state {} must have one output_budget_reserved transition when final output is reserved",
                    match state.kind {
                        StateKind::Ingest => "ingest",
                        StateKind::Assess => "assess",
                        _ => unreachable!("filter admits only ingest and assess states"),
                    },
                    state.id
                )));
            };
            let target = workflow
                .states
                .iter()
                .find(|state| state.id == fallback.to)
                .ok_or(ImageError::InvalidCompiledWorkflow)?;
            if target.kind != StateKind::Compose {
                return Err(ImageError::InvalidSpec(format!(
                    "output_budget_reserved from {} must enter a compose state",
                    state.id
                )));
            }
        }
    }
    Ok(())
}

fn validate_entrypoint_scope(entrypoint: &EntrypointSpec) -> Result<(), ImageError> {
    let cardinality = entrypoint.scope.cardinality;
    let value = cardinality.value();
    let valid = match entrypoint.scope.allowed_context {
        RunContextKind::CompanyTickerSet => {
            value > 0
                && usize::from(value) <= krw_agent_protocol::MAX_RUN_CONTEXT_TICKERS
                && entrypoint.scope.ticker_canonicalization
                    == TickerCanonicalizationPolicy::RequireUppercase
        }
        RunContextKind::CoveredUniverse => {
            matches!(cardinality, ScopeCardinality::Max { .. })
                && value > 0
                && usize::from(value) <= krw_agent_protocol::MAX_RUN_CONTEXT_TICKERS
                && entrypoint.scope.ticker_canonicalization
                    == TickerCanonicalizationPolicy::NotApplicable
        }
        RunContextKind::SelectedFeedItems => {
            value > 0
                && usize::from(value) <= krw_agent_protocol::MAX_SELECTED_FEED_ITEM_IDS
                && entrypoint.scope.ticker_canonicalization
                    == TickerCanonicalizationPolicy::NotApplicable
        }
        RunContextKind::SourceFiling => {
            cardinality == ScopeCardinality::Exact { value: 1 }
                && entrypoint.scope.ticker_canonicalization
                    == TickerCanonicalizationPolicy::NotApplicable
        }
        RunContextKind::ResearchNotebook => {
            cardinality == ScopeCardinality::Exact { value: 1 }
                && entrypoint.scope.ticker_canonicalization
                    == TickerCanonicalizationPolicy::RequireUppercase
        }
        RunContextKind::ExistingAnswer => {
            matches!(cardinality, ScopeCardinality::Max { .. })
                && value > 0
                && usize::from(value) <= krw_agent_protocol::MAX_EXISTING_ANSWER_SOURCE_UNITS
                && entrypoint.scope.ticker_canonicalization
                    == TickerCanonicalizationPolicy::NotApplicable
        }
        RunContextKind::RoutingRequest | RunContextKind::QuestionOnly => {
            cardinality == ScopeCardinality::Exact { value: 0 }
                && entrypoint.scope.ticker_canonicalization
                    == TickerCanonicalizationPolicy::NotApplicable
        }
    };
    if !valid {
        return Err(ImageError::InvalidSpec(format!(
            "entrypoint {} has an invalid closed scope policy",
            entrypoint.run_kind
        )));
    }
    Ok(())
}

fn validate_contract_declarations(contracts: &[ContractSpec]) -> Result<(), ImageError> {
    unique(
        contracts.iter().map(|contract| contract.id.as_str()),
        "contract",
    )?;
    unique(
        contracts
            .iter()
            .map(|contract| contract.schema_ref.as_str()),
        "contract schema_ref",
    )?;
    unique(
        contracts
            .iter()
            .map(|contract| contract.content_hash.as_str()),
        "contract content_hash",
    )?;
    if contracts.is_empty()
        || contracts.iter().any(|contract| {
            contract.id.trim().is_empty()
                || contract.schema_ref.trim().is_empty()
                || contract.semantic_authority.trim().is_empty()
                || contract.content_hash.as_str()
                    == "sha256:0000000000000000000000000000000000000000000000000000000000000000"
                || contract.schema_ref.contains("://")
        })
    {
        return Err(ImageError::InvalidSpec(
            "contracts require unique non-empty ids/refs/authorities and non-zero exact hashes"
                .into(),
        ));
    }
    // This runtime has one closed canonical contract registry. Validate each
    // source pin during AgentImage compilation so an unknown contract or a
    // wrong-but-well-formed SHA-256 can never enter an immutable image.
    // Runtime startup repeats the check after deployment resolution because
    // that protects a distinct, environment-bound trust boundary.
    for contract in contracts {
        verify_pin(&contract.id, &contract.content_hash)?;
    }
    Ok(())
}

fn safe_source_path(root: &Path, relative: &str) -> Result<PathBuf, ImageError> {
    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::RootDir))
    {
        return Err(ImageError::PathEscape(relative.to_owned()));
    }
    let path = fs::canonicalize(root.join(relative_path))?;
    if !path.starts_with(root) {
        return Err(ImageError::PathEscape(relative.to_owned()));
    }
    Ok(path)
}

fn unique<'a>(values: impl Iterator<Item = &'a str>, kind: &'static str) -> Result<(), ImageError> {
    let mut seen = BTreeSet::new();
    for value in values {
        if value.trim().is_empty() {
            return Err(ImageError::InvalidSpec(format!(
                "{kind} id must not be empty"
            )));
        }
        if !seen.insert(value) {
            return Err(ImageError::DuplicateId {
                kind,
                id: value.to_owned(),
            });
        }
    }
    Ok(())
}

fn validate_capability_input_abi(
    capability: &CapabilitySpec,
    all_capabilities: &[CapabilitySpec],
) -> Result<(), ImageError> {
    let valid_derivation = match &capability.input_derivation {
        InputDerivation::Identity => {
            capability.model_input_contract.is_none()
                && capability.research_proposal_anchor.is_none()
        }
        InputDerivation::CompanyContextRequestV1 => {
            capability.input_contract == "ontology-company-context/v1"
                && capability.model_input_contract.as_deref() == Some("company-context-request/v1")
                && capability.research_proposal_anchor.is_none()
                && capability.permission == Permission::Read
                && capability.idempotency == IdempotencyPolicy::CanonicalArgs
                && capability.result_ingest == CapabilityResultIngest::CompanyContextV1
                && capability.research_action.is_none()
                && matches!(
                    &capability.scope_binding,
                    CapabilityScopeBinding::TrustedTickerSet { .. }
                )
                // Company ontology orientation is a small, trusted-scope
                // preflight. It intentionally precedes the first evidence
                // proposal instead of depending on query_context.
                && capability.prerequisites.is_empty()
        }
        InputDerivation::SealedGuruQueryContextV1 => {
            capability.input_contract == "krw-guru-query-context-input/v1"
                && capability.model_input_contract.as_deref() == Some(GURU_QUERY_REQUEST_V1)
                && capability.research_proposal_anchor.is_none()
                && capability.permission == Permission::Read
                && capability.idempotency == IdempotencyPolicy::CanonicalArgs
                && capability.result_ingest == CapabilityResultIngest::GuruQueryContextV1
                && capability.retain_canonical_input
                && capability.research_action.is_none()
                && matches!(
                    &capability.scope_binding,
                    CapabilityScopeBinding::TrustedTickerSet { .. }
                )
                && capability.prerequisites.is_empty()
        }
        InputDerivation::ResearchProposalToSearchPlanV4 => {
            let valid_anchor = match &capability.research_proposal_anchor {
                Some(ResearchProposalAnchor::RunQuestion) => true,
                Some(ResearchProposalAnchor::SealedGuruInvestigationBriefV1 {
                    source_capability,
                }) => {
                    capability
                        .prerequisites
                        .iter()
                        .any(|id| id == source_capability)
                        && capability_has_result_ingest(
                            all_capabilities,
                            source_capability,
                            CapabilityResultIngest::GuruCompanyBriefV1,
                        )
                }
                None => false,
            };
            capability.input_contract == "search-plan/v2"
                && capability.model_input_contract.as_deref() == Some("research-proposal/v4")
                && capability.permission == Permission::Read
                && capability.idempotency == IdempotencyPolicy::CanonicalArgs
                && matches!(
                    capability.research_action.as_ref(),
                    Some(ResearchActionPolicy {
                        kind: ResearchActionKind::Context,
                        ..
                    })
                )
                && valid_anchor
                && capability
                    .output_contracts
                    .iter()
                    .any(|contract| contract == "research-state/v2")
        }
        InputDerivation::SealedGuruCompanyBriefV1 {
            query_context_capability,
        } => {
            capability.input_contract == "krw-guru-company-brief-input/v1"
                && capability.model_input_contract.as_deref()
                    == Some("krw-guru-investigation-question-draft/v1")
                && capability.result_ingest == CapabilityResultIngest::GuruCompanyBriefV1
                && matches!(
                    &capability.scope_binding,
                    CapabilityScopeBinding::TrustedTickerSet { .. }
                )
                && prerequisites_are_exactly(capability, &[query_context_capability])
                && capability_has_retained_result_ingest(
                    all_capabilities,
                    query_context_capability,
                    CapabilityResultIngest::GuruQueryContextV1,
                )
        }
        InputDerivation::SealedGuruEvidenceReviewV1 {
            query_context_capability,
            company_brief_capability,
            evidence_capabilities,
        } => {
            let source_ids = [
                query_context_capability.as_str(),
                company_brief_capability.as_str(),
            ]
            .into_iter()
            .chain(evidence_capabilities.iter().map(String::as_str))
            .collect::<BTreeSet<_>>();
            let valid_sources = source_ids.len() == evidence_capabilities.len().saturating_add(2)
                && !evidence_capabilities.is_empty()
                && evidence_capabilities
                    .iter()
                    .all(|source| capability_has_research_action(all_capabilities, source))
                && capability_has_retained_result_ingest(
                    all_capabilities,
                    query_context_capability,
                    CapabilityResultIngest::GuruQueryContextV1,
                )
                && capability_has_result_ingest(
                    all_capabilities,
                    company_brief_capability,
                    CapabilityResultIngest::GuruCompanyBriefV1,
                )
                && query_context_capability != company_brief_capability
                && !evidence_capabilities.iter().any(|source| {
                    source == query_context_capability || source == company_brief_capability
                });
            let required_prerequisites = [
                query_context_capability.as_str(),
                company_brief_capability.as_str(),
            ];
            let prerequisites_are_source_subset = capability
                .prerequisites
                .iter()
                .all(|source| source_ids.contains(source.as_str()));
            capability.input_contract == "krw-guru-evidence-review-input/v1"
                && capability.model_input_contract.as_deref()
                    == Some("krw-guru-agent-evidence-analysis/v1")
                && capability.result_ingest == CapabilityResultIngest::GuruEvidenceReviewV1
                && matches!(
                    &capability.scope_binding,
                    CapabilityScopeBinding::TrustedTickerSet { .. }
                )
                && valid_sources
                && required_prerequisites.iter().all(|required| {
                    capability
                        .prerequisites
                        .iter()
                        .any(|actual| actual == required)
                })
                && prerequisites_are_source_subset
        }
    };
    let valid_provider_input_codec = match &capability.input_derivation {
        InputDerivation::ResearchProposalToSearchPlanV4 => matches!(
            &capability.provider_input_codec,
            ProviderInputCodec::SingleFieldEnvelopeV1 { field } if field == "proposal"
        ),
        InputDerivation::Identity
        | InputDerivation::CompanyContextRequestV1
        | InputDerivation::SealedGuruQueryContextV1
        | InputDerivation::SealedGuruCompanyBriefV1 { .. }
        | InputDerivation::SealedGuruEvidenceReviewV1 { .. } => {
            capability.provider_input_codec.is_canonical_root()
        }
    };
    let valid_retention = !capability.retain_canonical_input
        || all_capabilities.iter().any(|candidate| {
            input_derivation_references(&candidate.input_derivation, &capability.id)
        });
    if valid_derivation
        && valid_provider_input_codec
        && valid_retention
        && capability.transport_codec == TransportCodec::CanonicalMcpV1
    {
        Ok(())
    } else {
        Err(ImageError::InvalidSpec(format!(
            "capability {} has an invalid input derivation or transport codec declaration",
            capability.id
        )))
    }
}

fn prerequisites_are_exactly(capability: &CapabilitySpec, expected: &[&String]) -> bool {
    capability.prerequisites.len() == expected.len()
        && capability
            .prerequisites
            .iter()
            .zip(expected)
            .all(|(actual, expected)| actual == *expected)
}

fn capability_has_result_ingest(
    all_capabilities: &[CapabilitySpec],
    capability_id: &str,
    expected: CapabilityResultIngest,
) -> bool {
    all_capabilities
        .iter()
        .find(|candidate| candidate.id == capability_id)
        .is_some_and(|candidate| candidate.result_ingest == expected)
}

fn capability_has_retained_result_ingest(
    all_capabilities: &[CapabilitySpec],
    capability_id: &str,
    expected: CapabilityResultIngest,
) -> bool {
    all_capabilities
        .iter()
        .find(|candidate| candidate.id == capability_id)
        .is_some_and(|candidate| {
            candidate.retain_canonical_input && candidate.result_ingest == expected
        })
}

fn capability_has_research_action(
    all_capabilities: &[CapabilitySpec],
    capability_id: &str,
) -> bool {
    all_capabilities
        .iter()
        .find(|candidate| candidate.id == capability_id)
        .is_some_and(|candidate| candidate.research_action.is_some())
}

fn input_derivation_references(derivation: &InputDerivation, capability_id: &str) -> bool {
    match derivation {
        InputDerivation::Identity
        | InputDerivation::CompanyContextRequestV1
        | InputDerivation::SealedGuruQueryContextV1
        | InputDerivation::ResearchProposalToSearchPlanV4 => false,
        InputDerivation::SealedGuruCompanyBriefV1 {
            query_context_capability,
        }
        | InputDerivation::SealedGuruEvidenceReviewV1 {
            query_context_capability,
            ..
        } => query_context_capability == capability_id,
    }
}

fn validate_research_action_policy(capability: &CapabilitySpec) -> Result<(), ImageError> {
    let Some(policy) = &capability.research_action else {
        return Ok(());
    };
    let estimate = policy.estimate;
    let valid_estimate = estimate.historical_success_lower_ppm > 0
        && estimate.historical_success_lower_ppm <= 1_000_000
        && estimate.expected_duplicate_ppm <= 1_000_000
        && estimate.failure_risk_upper_ppm <= 1_000_000
        && estimate.expected_latency_ms > 0
        && estimate.expected_tokens > 0
        && estimate.expected_tool_cost_micros > 0
        && estimate.expected_result_bytes > 0;
    let valid_domain = !policy.conflict_domain.is_empty()
        && policy.conflict_domain.len() <= 128
        && policy
            .conflict_domain
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'));
    let valid_contract_surface = match policy.kind {
        ResearchActionKind::Context => {
            capability.permission == Permission::Read
                && capability.idempotency == IdempotencyPolicy::CanonicalArgs
                && capability.input_contract == "search-plan/v2"
                && capability.result_ingest == CapabilityResultIngest::ResearchStateV2
                && capability
                    .output_contracts
                    .iter()
                    .any(|contract| contract == "research-state/v2")
        }
        ResearchActionKind::Targeted => {
            capability.permission == Permission::Read
                && capability.idempotency == IdempotencyPolicy::CanonicalArgs
                && capability.input_contract == "ontology-targeted-query/v1"
                && capability.result_ingest == CapabilityResultIngest::TargetedEvidenceV1
                && capability
                    .output_contracts
                    .iter()
                    .any(|contract| contract == "normalized-capability-result/v1")
        }
        ResearchActionKind::Trace => {
            capability.permission == Permission::Read
                && capability.idempotency == IdempotencyPolicy::CanonicalArgs
                && capability.input_contract == "ontology-trace-input/v1"
                && capability.result_ingest == CapabilityResultIngest::TraceLineageV1
                && capability
                    .output_contracts
                    .iter()
                    .any(|contract| contract == "normalized-capability-result/v1")
        }
    };
    if valid_estimate && valid_domain && valid_contract_surface {
        Ok(())
    } else {
        Err(ImageError::InvalidSpec(format!(
            "capability {} has an invalid declarative research action policy",
            capability.id
        )))
    }
}

fn validate_scope_projection(capability: &CapabilitySpec) -> Result<(), ImageError> {
    let Some(projection) = &capability.scope_projection else {
        return Ok(());
    };
    let valid_pointer = valid_json_pointer(&projection.source_pointer);
    let valid_surface = capability.permission == Permission::Read
        && capability
            .output_contracts
            .iter()
            .any(|contract| contract == &projection.output_contract)
        && projection.max_items > 0
        && usize::from(projection.max_items) <= krw_agent_protocol::MAX_RUN_CONTEXT_TICKERS;
    if valid_pointer && valid_surface {
        Ok(())
    } else {
        Err(ImageError::InvalidSpec(format!(
            "capability {} has an invalid result scope projection",
            capability.id
        )))
    }
}

fn validate_capability_scope_binding(capability: &CapabilitySpec) -> Result<(), ImageError> {
    let valid = match &capability.scope_binding {
        CapabilityScopeBinding::TrustedTickerSet {
            ticker_references,
            require_any_of,
            reject_non_null_pointers,
        } => {
            valid_ticker_reference_specs(ticker_references)
                && !require_any_of.is_empty()
                && unique_strings(require_any_of)
                && require_any_of.iter().all(|required| {
                    ticker_references
                        .iter()
                        .any(|reference| reference.id == *required)
                })
                && unique_strings(reject_non_null_pointers)
                && reject_non_null_pointers
                    .iter()
                    .all(|pointer| valid_json_pointer(pointer))
        }
        CapabilityScopeBinding::CoveredUniverse {
            ticker_references,
            required_string_values,
            bounded_integer_pointer,
        } => {
            valid_ticker_reference_specs(ticker_references)
                && unique_by(required_string_values, |requirement| {
                    requirement.pointer.as_str()
                })
                && required_string_values.iter().all(|requirement| {
                    valid_json_pointer(&requirement.pointer)
                        && !requirement.value.is_empty()
                        && requirement.value.len() <= 256
                })
                && bounded_integer_pointer
                    .as_deref()
                    .is_none_or(valid_json_pointer)
        }
        CapabilityScopeBinding::SelectedFeedItems {
            issue_ids_pointer,
            ticker_references,
            forbid_ticker_references,
        } => {
            valid_json_pointer(issue_ids_pointer)
                && (ticker_references.is_empty()
                    || valid_ticker_reference_specs(ticker_references))
                // A selected-feed run has no trusted ticker set until a
                // separately declared result projection commits. Therefore
                // non-forbidden ticker references would be an ambiguous,
                // unauthorized surface rather than a useful flexibility.
                && (*forbid_ticker_references || ticker_references.is_empty())
        }
        CapabilityScopeBinding::SourceFiling {
            filing_event_id_pointer,
        } => valid_json_pointer(filing_event_id_pointer),
        CapabilityScopeBinding::Unscoped => true,
    };
    if valid && capability.permission == Permission::Read {
        Ok(())
    } else {
        Err(ImageError::InvalidSpec(format!(
            "capability {} has an invalid declarative scope binding",
            capability.id
        )))
    }
}

fn valid_ticker_reference_specs(references: &[TickerReferenceSpec]) -> bool {
    !references.is_empty()
        && references.len() <= 32
        && unique_by(references, |reference| reference.id.as_str())
        && unique_by(references, |reference| reference.pointer_pattern.as_str())
        && references.iter().all(|reference| {
            !reference.id.is_empty()
                && reference.id.len() <= 64
                && reference
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                && valid_json_pointer_pattern(&reference.pointer_pattern)
        })
}

fn unique_strings(values: &[String]) -> bool {
    unique_by(values, String::as_str)
}

fn unique_by<T, F>(values: &[T], key: F) -> bool
where
    F: Fn(&T) -> &str,
{
    let mut seen = BTreeSet::new();
    values.iter().all(|value| {
        let key = key(value);
        !key.is_empty() && seen.insert(key)
    })
}

fn valid_json_pointer(pointer: &str) -> bool {
    valid_json_pointer_components(pointer, false)
}

fn valid_json_pointer_pattern(pointer: &str) -> bool {
    valid_json_pointer_components(pointer, true)
}

fn valid_json_pointer_components(pointer: &str, permit_wildcards: bool) -> bool {
    pointer.starts_with('/')
        && pointer.len() <= 256
        && !pointer.contains('\0')
        && !pointer.chars().any(char::is_control)
        && pointer.split('/').skip(1).count() <= MAX_PATH_DEPTH
        && pointer.split('/').skip(1).all(|segment| {
            if segment == "*" {
                permit_wildcards
            } else {
                !segment.contains('*')
                    && segment
                        .split('~')
                        .skip(1)
                        .all(|tail| tail.starts_with('0') || tail.starts_with('1'))
            }
        })
}

fn validate_workflow(workflow: &WorkflowSpec, roles: &[RoleSpec]) -> Result<(), ImageError> {
    if workflow.states.len() > MAX_STATES {
        return Err(ImageError::Limit("workflow states"));
    }
    if workflow.transitions.len() > MAX_TRANSITIONS {
        return Err(ImageError::Limit("workflow transitions"));
    }
    unique(
        workflow.states.iter().map(|state| state.id.as_str()),
        "state",
    )?;
    unique(
        workflow
            .states
            .iter()
            .filter_map(|state| state.capability_id.as_deref()),
        "workflow capability",
    )?;
    let roles_by_id = roles
        .iter()
        .map(|role| (role.id.as_str(), role))
        .collect::<BTreeMap<_, _>>();
    for state in &workflow.states {
        match (state.kind, state.capability_id.as_deref()) {
            (StateKind::Capability, None) => {
                return Err(ImageError::InvalidSpec(format!(
                    "capability state {} must declare capability_id",
                    state.id
                )));
            }
            (StateKind::Capability, Some(_)) | (_, None) => {}
            (_, Some(_)) => {
                return Err(ImageError::InvalidSpec(format!(
                    "non-capability state {} cannot declare capability_id",
                    state.id
                )));
            }
        }
        let model_driven = matches!(
            state.kind,
            StateKind::Plan | StateKind::Assess | StateKind::Compose
        );
        match (model_driven, state.role_id.as_deref()) {
            (true, None) => {
                return Err(ImageError::InvalidSpec(format!(
                    "model-driven state {} must declare role_id",
                    state.id
                )));
            }
            (true, Some(role_id)) => {
                let role =
                    roles_by_id
                        .get(role_id)
                        .ok_or_else(|| ImageError::UnknownReference {
                            kind: "workflow role",
                            id: role_id.to_owned(),
                        })?;
                if role.deterministic {
                    return Err(ImageError::InvalidSpec(format!(
                        "model-driven state {} cannot use deterministic role {}",
                        state.id, role_id
                    )));
                }
                if role.prompt_segments.is_empty() {
                    return Err(ImageError::InvalidSpec(format!(
                        "model-driven state {} uses role {} with no prompt segments",
                        state.id, role_id
                    )));
                }
            }
            (false, Some(_)) => {
                return Err(ImageError::InvalidSpec(format!(
                    "deterministic state {} cannot declare role_id",
                    state.id
                )));
            }
            (false, None) => {}
        }
        match (state.kind, state.terminal) {
            (StateKind::Terminal, None) => {
                return Err(ImageError::InvalidSpec(format!(
                    "terminal state {} must declare a terminal disposition",
                    state.id
                )));
            }
            (StateKind::Terminal, Some(_)) | (_, None) => {}
            (_, Some(_)) => {
                return Err(ImageError::InvalidSpec(format!(
                    "non-terminal state {} cannot declare a terminal disposition",
                    state.id
                )));
            }
        }
    }
    let states = workflow
        .states
        .iter()
        .map(|state| state.id.as_str())
        .collect::<BTreeSet<_>>();
    if !states.contains(workflow.initial.as_str()) {
        return Err(ImageError::UnknownReference {
            kind: "initial state",
            id: workflow.initial.clone(),
        });
    }
    if workflow
        .states
        .iter()
        .any(|state| state.max_visits == 0 || state.max_visits > MAX_STATE_VISITS)
    {
        return Err(ImageError::InvalidSpec(format!(
            "workflow {} contains an invalid state visit bound",
            workflow.id
        )));
    }
    if workflow
        .states
        .iter()
        .map(|state| u64::from(state.max_visits))
        .sum::<u64>()
        > MAX_TOTAL_STATE_VISITS
    {
        return Err(ImageError::Limit("total workflow state visits"));
    }
    let mut edges: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut reverse_edges: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut transition_keys = BTreeSet::new();
    let mut transition_groups: BTreeMap<(&str, &str), (usize, bool)> = BTreeMap::new();
    for transition in &workflow.transitions {
        if transition.on.trim().is_empty() {
            return Err(ImageError::InvalidSpec(
                "transition event must not be empty".into(),
            ));
        }
        if !states.contains(transition.from.as_str()) {
            return Err(ImageError::UnknownReference {
                kind: "transition source",
                id: transition.from.clone(),
            });
        }
        if !states.contains(transition.to.as_str()) {
            return Err(ImageError::UnknownReference {
                kind: "transition target",
                id: transition.to.clone(),
            });
        }
        validate_transition_guard(&transition.guard)?;
        let guard_key = serde_jcs::to_vec(&transition.guard)?;
        if !transition_keys.insert((transition.from.as_str(), transition.on.as_str(), guard_key)) {
            return Err(ImageError::InvalidSpec(format!(
                "workflow {} has a duplicate state/event/guard transition",
                workflow.id
            )));
        }
        let group = transition_groups
            .entry((transition.from.as_str(), transition.on.as_str()))
            .or_default();
        group.0 += 1;
        group.1 |= matches!(transition.guard, TransitionGuard::Always);
        edges
            .entry(&transition.from)
            .or_default()
            .push(&transition.to);
        reverse_edges
            .entry(&transition.to)
            .or_default()
            .push(&transition.from);
    }
    if transition_groups
        .values()
        .any(|(count, has_always)| *count > 1 && *has_always)
    {
        return Err(ImageError::InvalidSpec(format!(
            "workflow {} mixes an always guard with conditional transitions for one state/event",
            workflow.id
        )));
    }
    let mut reached = BTreeSet::new();
    let mut queue = VecDeque::from([workflow.initial.as_str()]);
    while let Some(state) = queue.pop_front() {
        if !reached.insert(state) {
            continue;
        }
        if let Some(next_states) = edges.get(state) {
            queue.extend(next_states.iter().copied());
        }
    }
    if reached.len() != states.len() {
        let missing = states
            .difference(&reached)
            .copied()
            .collect::<Vec<_>>()
            .join(", ");
        return Err(ImageError::InvalidSpec(format!(
            "workflow {} has unreachable states: {missing}",
            workflow.id
        )));
    }
    if !workflow
        .states
        .iter()
        .any(|state| state.kind == StateKind::Terminal && reached.contains(state.id.as_str()))
    {
        return Err(ImageError::InvalidSpec(format!(
            "workflow {} has no reachable terminal state",
            workflow.id
        )));
    }
    let terminal_states = workflow
        .states
        .iter()
        .filter(|state| state.kind == StateKind::Terminal)
        .map(|state| state.id.as_str())
        .collect::<BTreeSet<_>>();
    if terminal_states
        .iter()
        .any(|terminal| edges.contains_key(terminal))
    {
        return Err(ImageError::InvalidSpec(format!(
            "workflow {} has an outgoing terminal transition",
            workflow.id
        )));
    }
    if workflow
        .states
        .iter()
        .filter(|state| state.kind != StateKind::Terminal)
        .any(|state| !edges.contains_key(state.id.as_str()))
    {
        return Err(ImageError::InvalidSpec(format!(
            "workflow {} contains a non-terminal dead end",
            workflow.id
        )));
    }
    let mut can_reach_terminal = BTreeSet::new();
    let mut reverse_queue = terminal_states.iter().copied().collect::<VecDeque<_>>();
    while let Some(state) = reverse_queue.pop_front() {
        if !can_reach_terminal.insert(state) {
            continue;
        }
        if let Some(previous) = reverse_edges.get(state) {
            reverse_queue.extend(previous.iter().copied());
        }
    }
    if can_reach_terminal.len() != states.len() {
        return Err(ImageError::InvalidSpec(format!(
            "workflow {} contains a state with no terminal path",
            workflow.id
        )));
    }
    Ok(())
}

fn compile_workflow(
    workflow: &WorkflowSpec,
    capabilities: &[CapabilitySpec],
    contracts: &BTreeMap<String, ContractPin>,
    internal_format: &str,
) -> Result<CompiledWorkflow, ImageError> {
    let mut states = workflow.states.clone();
    states.sort_by(|left, right| left.id.cmp(&right.id));
    let ids = states
        .iter()
        .enumerate()
        .map(|(index, state)| {
            u16::try_from(index)
                .map(|numeric| (state.id.clone(), numeric))
                .map_err(|_| ImageError::Limit("workflow numeric state id"))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let output_contracts = workflow
        .states
        .iter()
        .map(|state| {
            Ok((
                state.id.clone(),
                state_output_contracts(state, workflow, capabilities, contracts, internal_format)?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>, ImageError>>()?;
    let compiled_states = states
        .into_iter()
        .map(|state| {
            let input_contracts = if state.id == workflow.initial {
                Vec::new()
            } else {
                workflow
                    .transitions
                    .iter()
                    .filter(|transition| transition.to == state.id)
                    .flat_map(|transition| output_contracts[&transition.from].iter().cloned())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect()
            };
            let operation = compile_state_operation(
                &state,
                input_contracts,
                output_contracts[&state.id].clone(),
                capabilities,
                internal_format,
            )?;
            Ok(CompiledState {
                numeric_id: ids[&state.id],
                stable_id: state.id,
                kind: state.kind,
                capability_id: state.capability_id,
                role_id: state.role_id,
                terminal: state.terminal,
                operation,
                max_visits: state.max_visits,
            })
        })
        .collect::<Result<Vec<_>, ImageError>>()?;
    let mut transitions = workflow
        .transitions
        .iter()
        .map(|transition| {
            let compiled = CompiledTransition {
                from: ids[&transition.from],
                event: transition.on.clone(),
                to: ids[&transition.to],
                guard: transition.guard.clone(),
            };
            Ok((compiled, serde_jcs::to_vec(&transition.guard)?))
        })
        .collect::<Result<Vec<_>, ImageError>>()?;
    transitions.sort_by(|(left, left_guard), (right, right_guard)| {
        (left.from, &left.event, left.to, left_guard).cmp(&(
            right.from,
            &right.event,
            right.to,
            right_guard,
        ))
    });
    let transitions = transitions
        .into_iter()
        .map(|(transition, _)| transition)
        .collect();
    Ok(CompiledWorkflow {
        id: workflow.id.clone(),
        initial: ids[&workflow.initial],
        states: compiled_states,
        transitions,
    })
}

fn contract_pin(contract: &ContractSpec) -> ContractPin {
    ContractPin {
        id: contract.id.clone(),
        content_hash: contract.content_hash.clone(),
    }
}

fn compile_state_operation(
    state: &StateSpec,
    input_contracts: Vec<ContractPin>,
    output_contracts: Vec<ContractPin>,
    capabilities: &[CapabilitySpec],
    internal_format: &str,
) -> Result<StateOperation, ImageError> {
    match state.kind {
        StateKind::Plan | StateKind::Assess | StateKind::Compose => {
            let output_mode =
                compile_model_output_mode(state, &output_contracts, capabilities, internal_format)?;
            Ok(StateOperation::ModelDecision {
                role_id: state
                    .role_id
                    .clone()
                    .ok_or(ImageError::InvalidCompiledWorkflow)?,
                output_mode,
                input_contracts,
                output_contracts,
            })
        }
        StateKind::Capability => Ok(StateOperation::CapabilityAction {
            capability_id: state
                .capability_id
                .clone()
                .ok_or(ImageError::InvalidCompiledWorkflow)?,
            input_contracts,
            output_contracts,
        }),
        StateKind::Start
        | StateKind::Validate
        | StateKind::Ingest
        | StateKind::Verify
        | StateKind::Render
        | StateKind::Commit => {
            let handler = match state.kind {
                StateKind::Start => BuiltinHandler::InitializeRun,
                StateKind::Validate => BuiltinHandler::ValidateArtifact,
                StateKind::Ingest => BuiltinHandler::IngestEvidence,
                StateKind::Verify => BuiltinHandler::VerifyOutput,
                StateKind::Render => BuiltinHandler::RenderOutput,
                StateKind::Commit => BuiltinHandler::CommitOutput,
                _ => return Err(ImageError::InvalidCompiledWorkflow),
            };
            Ok(StateOperation::Builtin {
                handler,
                input_contracts,
                output_contracts,
            })
        }
        StateKind::Terminal => Ok(StateOperation::Terminal {
            disposition: match state.terminal.ok_or(ImageError::InvalidCompiledWorkflow)? {
                TerminalDisposition::Succeeded => OperationTerminalDisposition::Succeeded,
                TerminalDisposition::Stopped => OperationTerminalDisposition::Stopped,
                TerminalDisposition::Failed => OperationTerminalDisposition::Failed,
            },
            accepted_contracts: input_contracts,
        }),
    }
}

/// Derive the provider output grammar from the declared statechart and
/// contract frontier, then persist that grammar in the immutable image. The
/// model is never asked to infer whether free text is a transition, a tool
/// proposal, or final output.
fn compile_model_output_mode(
    state: &StateSpec,
    output_contracts: &[ContractPin],
    capabilities: &[CapabilitySpec],
    internal_format: &str,
) -> Result<ModelOutputMode, ImageError> {
    let capability_input_contracts = capabilities
        .iter()
        .map(CapabilitySpec::model_input_contract_id)
        .collect::<BTreeSet<_>>();
    let exposes_capability = output_contracts
        .iter()
        .any(|contract| capability_input_contracts.contains(contract.id.as_str()));

    match state.kind {
        StateKind::Plan => Ok(if exposes_capability {
            ModelOutputMode::CapabilityCall
        } else {
            ModelOutputMode::WorkflowTransition
        }),
        StateKind::Assess => Ok(if exposes_capability {
            ModelOutputMode::CapabilityOrWorkflowTransition
        } else {
            ModelOutputMode::WorkflowTransition
        }),
        StateKind::Compose => {
            if output_contracts.len() != 1 || output_contracts[0].id != internal_format {
                return Err(ImageError::InvalidCompiledWorkflow);
            }
            Ok(if internal_format == FINAL_MARKDOWN_V1 {
                ModelOutputMode::Markdown
            } else {
                ModelOutputMode::TypedJson
            })
        }
        _ => Err(ImageError::InvalidCompiledWorkflow),
    }
}

fn state_output_contracts(
    state: &StateSpec,
    workflow: &WorkflowSpec,
    capabilities: &[CapabilitySpec],
    contracts: &BTreeMap<String, ContractPin>,
    internal_format: &str,
) -> Result<Vec<ContractPin>, ImageError> {
    let state_facts = || contract_by_id(contracts, STATE_FACTS_CONTRACT_ID);
    let internal = || contract_by_id(contracts, internal_format);
    let mut outputs = BTreeSet::new();
    match state.kind {
        StateKind::Plan | StateKind::Assess => {
            outputs.insert(state_facts()?);
            let direct_targets = workflow
                .transitions
                .iter()
                .filter(|transition| transition.from == state.id)
                .filter_map(|transition| {
                    workflow
                        .states
                        .iter()
                        .find(|candidate| candidate.id == transition.to)
                })
                .collect::<Vec<_>>();
            for target in direct_targets
                .iter()
                .copied()
                .filter(|target| target.kind == StateKind::Capability)
            {
                let capability_id = target
                    .capability_id
                    .as_deref()
                    .ok_or(ImageError::InvalidCompiledWorkflow)?;
                let capability = capabilities
                    .iter()
                    .find(|capability| capability.id == capability_id)
                    .ok_or(ImageError::InvalidCompiledWorkflow)?;
                outputs.insert(contract_by_id(
                    contracts,
                    capability.model_input_contract_id(),
                )?);
            }
            // A model-authored capability proposal commonly crosses one
            // deterministic ValidateArtifact state before dispatch. Carry the
            // exact capability input contract through that state rather than
            // reducing the proposal to untyped workflow facts.
            for validate in direct_targets
                .iter()
                .copied()
                .filter(|target| target.kind == StateKind::Validate)
            {
                for target in workflow
                    .transitions
                    .iter()
                    .filter(|transition| transition.from == validate.id)
                    .filter_map(|transition| {
                        workflow
                            .states
                            .iter()
                            .find(|candidate| candidate.id == transition.to)
                    })
                    .filter(|target| target.kind == StateKind::Capability)
                {
                    let capability_id = target
                        .capability_id
                        .as_deref()
                        .ok_or(ImageError::InvalidCompiledWorkflow)?;
                    let capability = capabilities
                        .iter()
                        .find(|capability| capability.id == capability_id)
                        .ok_or(ImageError::InvalidCompiledWorkflow)?;
                    outputs.insert(contract_by_id(
                        contracts,
                        capability.model_input_contract_id(),
                    )?);
                }
            }
        }
        StateKind::Compose | StateKind::Render | StateKind::Commit => {
            outputs.insert(internal()?);
        }
        StateKind::Capability => {
            let capability_id = state
                .capability_id
                .as_deref()
                .ok_or(ImageError::InvalidCompiledWorkflow)?;
            let capability = capabilities
                .iter()
                .find(|capability| capability.id == capability_id)
                .ok_or(ImageError::InvalidCompiledWorkflow)?;
            for contract_id in &capability.output_contracts {
                outputs.insert(contract_by_id(contracts, contract_id)?);
            }
        }
        StateKind::Start | StateKind::Ingest => {
            outputs.insert(state_facts()?);
        }
        StateKind::Validate => {
            outputs.insert(state_facts()?);
            if workflow
                .transitions
                .iter()
                .filter(|transition| transition.from == state.id)
                .filter_map(|transition| {
                    workflow
                        .states
                        .iter()
                        .find(|candidate| candidate.id == transition.to)
                })
                .any(|target| target.kind == StateKind::Render)
            {
                outputs.insert(internal()?);
            }
            // Validation preserves an already schema-valid proposal and may
            // only route it to the matching capability. This exact pin is
            // what makes the capability boundary typed after recovery.
            for target in workflow
                .transitions
                .iter()
                .filter(|transition| transition.from == state.id)
                .filter_map(|transition| {
                    workflow
                        .states
                        .iter()
                        .find(|candidate| candidate.id == transition.to)
                })
                .filter(|target| target.kind == StateKind::Capability)
            {
                let capability_id = target
                    .capability_id
                    .as_deref()
                    .ok_or(ImageError::InvalidCompiledWorkflow)?;
                let capability = capabilities
                    .iter()
                    .find(|capability| capability.id == capability_id)
                    .ok_or(ImageError::InvalidCompiledWorkflow)?;
                outputs.insert(contract_by_id(
                    contracts,
                    capability.model_input_contract_id(),
                )?);
            }
        }
        StateKind::Verify => {
            outputs.insert(state_facts()?);
            outputs.insert(internal()?);
        }
        StateKind::Terminal => {}
    }
    Ok(outputs.into_iter().collect())
}

fn contract_by_id(
    contracts: &BTreeMap<String, ContractPin>,
    contract_id: &str,
) -> Result<ContractPin, ImageError> {
    contracts
        .get(contract_id)
        .cloned()
        .ok_or_else(|| ImageError::UnknownReference {
            kind: "typed state contract",
            id: contract_id.to_owned(),
        })
}

fn validate_transition_guard(guard: &TransitionGuard) -> Result<(), ImageError> {
    if let Some(pointer) = guard.pointer()
        && (!pointer.starts_with('/') || pointer.split('/').skip(1).count() > MAX_PATH_DEPTH)
    {
        return Err(ImageError::InvalidSpec(format!(
            "invalid transition guard JSON pointer: {pointer}"
        )));
    }
    let constant = match guard {
        TransitionGuard::FieldEquals { value, .. }
        | TransitionGuard::FieldNotEquals { value, .. } => Some(value),
        TransitionGuard::Always
        | TransitionGuard::FieldPresent { .. }
        | TransitionGuard::FieldAbsent { .. } => None,
    };
    if let Some(value) = constant {
        if matches!(value, Value::Array(_) | Value::Object(_)) {
            return Err(ImageError::InvalidSpec(
                "transition guard constants must be JSON scalars".into(),
            ));
        }
        if serde_jcs::to_vec(value)?.len() > MAX_CONSTANT_BYTES {
            return Err(ImageError::Limit("transition guard constant"));
        }
    }
    Ok(())
}

fn validate_rule_program(program: &RuleProgram) -> Result<(), ImageError> {
    if !program.result_ingest_scope.is_empty() && program.phase != RulePhase::PostAction {
        return Err(ImageError::InvalidSpec(
            "result ingest scoped validators are valid only for post_action".into(),
        ));
    }
    if program.result_ingest_scope.len() > MAX_CAPABILITIES
        || program
            .result_ingest_scope
            .iter()
            .enumerate()
            .any(|(index, ingest)| program.result_ingest_scope[..index].contains(ingest))
    {
        return Err(ImageError::InvalidSpec(
            "validator result ingest scope must be bounded and unique".into(),
        ));
    }
    if program.instructions.len() > MAX_RULES_PER_PROGRAM {
        return Err(ImageError::Limit("validator instructions"));
    }
    if program.fuel == 0 || program.fuel > MAX_RULE_FUEL {
        return Err(ImageError::Limit("validator fuel"));
    }
    let constant_bytes = program
        .instructions
        .iter()
        .map(RuleInstruction::constant_bytes)
        .sum::<usize>();
    if constant_bytes > MAX_CONSTANT_BYTES {
        return Err(ImageError::Limit("validator constant pool"));
    }
    for instruction in &program.instructions {
        for path in instruction.paths() {
            if !path.starts_with('/') || path.split('/').skip(1).count() > MAX_PATH_DEPTH {
                return Err(ImageError::InvalidSpec(format!(
                    "invalid bounded JSON pointer: {path}"
                )));
            }
        }
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum ImageError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid AgentSpec YAML: {0}")]
    Yaml(#[from] serde_yaml_ng::Error),
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid AgentSpec: {0}")]
    InvalidSpec(String),
    #[error("compiled workflow contains inconsistent typed state metadata")]
    InvalidCompiledWorkflow,
    #[error("duplicate {kind} id: {id}")]
    DuplicateId { kind: &'static str, id: String },
    #[error("unknown {kind}: {id}")]
    UnknownReference { kind: &'static str, id: String },
    #[error("source path escapes AgentSpec root: {0}")]
    PathEscape(String),
    #[error("immutable AgentImage output already exists: {0}")]
    OutputExists(String),
    #[error("prompt is not valid UTF-8: {0}")]
    InvalidPromptUtf8(String),
    #[error("prompt contains forbidden NUL text: {0}")]
    InvalidPromptText(String),
    #[error("AgentImage is missing prompt blob {0}")]
    MissingBlob(ContentHash),
    #[error("content-addressed prompt store observed conflicting bytes for {0}")]
    InternedBlobConflict(ContentHash),
    #[error("prompt blob {hash} length mismatch: expected {expected}, observed {observed}")]
    BlobLengthMismatch {
        hash: ContentHash,
        expected: u64,
        observed: u64,
    },
    #[error("prompt blob hash mismatch: expected {expected}, observed {observed}")]
    BlobHashMismatch {
        expected: ContentHash,
        observed: ContentHash,
    },
    #[error("hard limit exceeded: {0}")]
    Limit(&'static str),
    #[error("unsupported AgentImage format version: {0}")]
    UnsupportedImageFormat(u16),
    #[error("unsupported rule ISA version: {0}")]
    UnsupportedRuleIsa(u16),
    #[error("AgentImage content hash mismatch: expected {expected}, observed {observed}")]
    ImageHashMismatch {
        expected: ContentHash,
        observed: ContentHash,
    },
    #[error("bounded rule program exhausted its fuel")]
    RuleFuelExhausted,
    #[error("invalid rule input: {0}")]
    RuleInput(String),
    #[error(transparent)]
    ContractRegistry(#[from] krw_agent_contracts::ContractArtifactError),
    #[error(transparent)]
    StateArtifact(#[from] krw_agent_state_artifact::ArtifactError),
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static TEST_DIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn agent_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../agents/krw-ontology")
    }

    #[test]
    fn hand_authored_agent_compiles_deterministically() {
        let first = compile_agent_dir(agent_root()).unwrap();
        let second = compile_agent_dir(agent_root()).unwrap();
        assert_eq!(first.manifest.content_hash, second.manifest.content_hash);
        assert_eq!(first.manifest, second.manifest);
        assert!(!first.blobs.is_empty());
    }

    #[test]
    fn skill_catalog_exposes_only_registered_loadable_ids() {
        let image = compile_agent_dir(agent_root())
            .unwrap()
            .into_loaded()
            .unwrap();
        let catalog = image.prompt("skill_catalog").unwrap();
        assert!(catalog.contains("**research_planner_skill**"));
        assert!(catalog.contains("**research_analysis**"));
        assert!(catalog.contains("**earnings_quality_policy**"));
        assert!(catalog.contains("**scenario_construction**"));
        assert!(catalog.contains("You may load more than one distinct relevant skill"));
        assert!(
            catalog.contains("when EPS, net income, margin, or free cash flow may be distorted")
        );
        assert!(!catalog.contains("**security_boundary**"));
        // A frontmatter label with hyphens must never become a callable ID.
        assert!(!catalog.contains("**research-structured-handoff-contract**"));

        let planner = image
            .body
            .prompt_blobs
            .iter()
            .find(|blob| blob.id == "research_planner_skill")
            .unwrap();
        let security = image
            .body
            .prompt_blobs
            .iter()
            .find(|blob| blob.id == "security_boundary")
            .unwrap();
        assert!(planner.loadable);
        assert!(!security.loadable);
    }

    #[test]
    fn compiler_closes_each_model_state_to_one_typed_output_lane() {
        let image = compile_agent_dir(agent_root()).unwrap().manifest;
        let workflow = image
            .body
            .workflows
            .iter()
            .find(|workflow| workflow.id == "company_research_v2")
            .unwrap();
        let mode = |state_id: &str| match &workflow
            .states
            .iter()
            .find(|state| state.stable_id == state_id)
            .unwrap()
            .operation
        {
            StateOperation::ModelDecision { output_mode, .. } => *output_mode,
            _ => panic!("{state_id} must be a model state"),
        };
        assert_eq!(
            mode("orient_company"),
            ModelOutputMode::CapabilityOrWorkflowTransition
        );
        assert_eq!(mode("author_plan"), ModelOutputMode::CapabilityCall);
        assert_eq!(
            mode("assess_obligations"),
            ModelOutputMode::CapabilityOrWorkflowTransition
        );
        assert_eq!(mode("compose_ir"), ModelOutputMode::Markdown);
        let composer = image
            .body
            .roles
            .iter()
            .find(|role| role.id == "composer")
            .unwrap();
        assert_eq!(composer.execution.reasoning, RoleReasoningMode::Inherit);
        assert_eq!(composer.execution.max_output_tokens, Some(16384));
        assert!(
            !composer
                .prompt_segments
                .iter()
                .any(|segment| { segment == "ontology_catalog" || segment == "skill_catalog" }),
            "the final writer cannot call capabilities or load skills, so backend catalogs must not be injected into investor prose"
        );
        let planner = image
            .body
            .roles
            .iter()
            .find(|role| role.id == "planner")
            .unwrap();
        assert_eq!(planner.execution.reasoning, RoleReasoningMode::Direct);
        assert_eq!(planner.execution.max_output_tokens, Some(3072));
        let analyst = image
            .body
            .roles
            .iter()
            .find(|role| role.id == "analyst")
            .unwrap();
        assert_eq!(analyst.execution.reasoning, RoleReasoningMode::Inherit);
        assert_eq!(analyst.execution.max_output_tokens, Some(16384));
        let orienter = image
            .body
            .roles
            .iter()
            .find(|role| role.id == "company_orienter")
            .unwrap();
        assert_eq!(orienter.execution.reasoning, RoleReasoningMode::Direct);
        assert_eq!(orienter.execution.max_output_tokens, Some(512));
        assert_eq!(
            image.body.answer_policy.max_research_turn_tokens,
            Some(16384)
        );

        let earnings = image
            .body
            .workflows
            .iter()
            .find(|workflow| workflow.id == "earnings_deep_dive_v1")
            .unwrap();
        let thesis = earnings
            .states
            .iter()
            .find(|state| state.stable_id == "define_quarter_thesis")
            .unwrap();
        assert!(matches!(
            &thesis.operation,
            StateOperation::ModelDecision {
                output_mode: ModelOutputMode::WorkflowTransition,
                ..
            }
        ));
    }

    #[test]
    fn final_output_reserve_scales_to_each_workflows_composer() {
        let image = compile_agent_dir(agent_root()).unwrap().manifest;
        assert_eq!(
            image
                .effective_final_output_reserve_tokens("company_research_v2")
                .unwrap(),
            Some(32_768)
        );
        assert_eq!(
            image
                .effective_final_output_reserve_tokens("idea_generation_v1")
                .unwrap(),
            Some(8_192)
        );
    }

    #[test]
    fn research_proposal_tool_guidance_never_describes_the_physical_search_plan() {
        let image = compile_agent_dir(agent_root()).unwrap();
        let capability = image
            .manifest
            .body
            .capabilities
            .iter()
            .find(|capability| capability.id == "ontology.query_context")
            .unwrap();
        let description = capability.provider_tool_description();
        assert!(description.contains("ResearchProposal v4"));
        assert!(description.contains("not a SearchPlan"));
        assert!(description.contains("Do not create goal IDs"));
        assert!(description.contains("retrieval_query"));
        assert!(!description.contains("Call with SearchPlan"));
    }

    #[test]
    fn company_context_is_a_narrow_preflight_orientation_capability() {
        let image = compile_agent_dir(agent_root()).unwrap();
        let capability = image
            .manifest
            .body
            .capabilities
            .iter()
            .find(|capability| capability.id == "ontology.company_context")
            .unwrap();

        assert_eq!(capability.input_contract, "ontology-company-context/v1");
        assert_eq!(
            capability.model_input_contract.as_deref(),
            Some("company-context-request/v1")
        );
        assert_eq!(
            capability.input_derivation,
            InputDerivation::CompanyContextRequestV1
        );
        assert_eq!(
            capability.result_ingest,
            CapabilityResultIngest::CompanyContextV1
        );
        assert!(capability.research_action.is_none());
        assert!(capability.prerequisites.is_empty());
        let guidance = capability.provider_tool_description();
        assert!(guidance.contains("orientation-only"));
        assert!(guidance.contains("never as factual support"));
    }

    #[test]
    fn every_agent_uses_the_closed_v2_entrypoint_scope() {
        let agents = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../agents");
        for package in [
            "krw-display",
            "krw-feed",
            "krw-guru-advisor",
            "krw-notebook",
            "krw-ontology-en",
            "krw-ontology",
            "krw-router",
            "krw-source-filing",
        ] {
            let image = compile_agent_dir(agents.join(package)).unwrap();
            assert_eq!(
                image.manifest.body.format_version,
                AGENT_IMAGE_FORMAT_VERSION
            );
        }
    }

    #[test]
    fn declarative_scope_bindings_reject_unknown_references_and_invalid_patterns() {
        let mut spec = parse_spec(&fs::read(agent_root().join("agent.yaml")).unwrap()).unwrap();
        let capability = spec
            .capabilities
            .iter_mut()
            .find(|capability| capability.id == "ontology.query_context")
            .unwrap();
        match &mut capability.scope_binding {
            CapabilityScopeBinding::TrustedTickerSet { require_any_of, .. } => {
                *require_any_of = vec!["not_declared".into()];
            }
            _ => panic!("company query_context must declare a ticker scope binding"),
        }
        assert!(matches!(
            validate_spec(&spec),
            Err(ImageError::InvalidSpec(message))
                if message.contains("declarative scope binding")
        ));

        let mut spec = parse_spec(&fs::read(agent_root().join("agent.yaml")).unwrap()).unwrap();
        let capability = spec
            .capabilities
            .iter_mut()
            .find(|capability| capability.id == "ontology.query_context")
            .unwrap();
        match &mut capability.scope_binding {
            CapabilityScopeBinding::TrustedTickerSet {
                ticker_references, ..
            } => ticker_references[1].pointer_pattern = "/clauses/*not-a-wildcard/tickers".into(),
            _ => panic!("company query_context must declare a ticker scope binding"),
        }
        assert!(matches!(
            validate_spec(&spec),
            Err(ImageError::InvalidSpec(message))
                if message.contains("declarative scope binding")
        ));
    }

    #[test]
    fn entrypoint_rejects_context_mismatch_lowercase_and_duplicates() {
        let image = compile_agent_dir(agent_root()).unwrap().manifest;
        let entrypoint = image.body.entrypoints.get("company_research").unwrap();
        entrypoint
            .validate_run_context(&RunContextV1::CompanyTickerSet {
                tickers: vec!["AAPL".into()],
            })
            .unwrap();

        assert_eq!(
            entrypoint.validate_run_context(&RunContextV1::QuestionOnly {}),
            Err(ContextPolicyError::ContextKindMismatch)
        );
        assert_eq!(
            entrypoint.validate_run_context(&RunContextV1::CompanyTickerSet {
                tickers: vec!["aapl".into()],
            }),
            Err(ContextPolicyError::InvalidContextShape)
        );
        assert_eq!(
            entrypoint.validate_run_context(&RunContextV1::CompanyTickerSet {
                tickers: vec!["AAPL".into(), "AAPL".into()],
            }),
            Err(ContextPolicyError::InvalidContextShape)
        );
    }

    #[test]
    fn guru_author_constant_is_entrypoint_local_and_not_spoofable() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../agents/krw-guru-advisor");
        let mut spec = parse_spec(&fs::read(root.join("agent.yaml")).unwrap()).unwrap();
        let ackman = spec.entrypoints.get("guru_ackman").unwrap();
        let buffett = spec.entrypoints.get("guru_buffett").unwrap();
        assert_eq!(ackman.constants.fixed_guru_author, Some(GuruAuthor::Ackman));
        assert_eq!(
            buffett.constants.fixed_guru_author,
            Some(GuruAuthor::Buffett)
        );

        spec.entrypoints
            .get_mut("guru_ackman")
            .unwrap()
            .constants
            .fixed_guru_author = Some(GuruAuthor::Buffett);
        assert!(matches!(
            validate_spec(&spec),
            Err(ImageError::InvalidSpec(_))
        ));
    }

    #[test]
    fn guru_company_researcher_is_an_ordinary_thinking_research_role() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../agents/krw-guru-advisor");
        let spec = parse_spec(&fs::read(root.join("agent.yaml")).unwrap()).unwrap();
        validate_spec(&spec).expect("Guru research role is a valid AgentSpec");
        let role = spec
            .roles
            .iter()
            .find(|role| role.id == "company_evidence_researcher")
            .expect("company-evidence research role");
        assert!(role.bounded_child.is_none());
        assert_eq!(role.execution.reasoning, RoleReasoningMode::Inherit);
        assert_eq!(role.execution.max_output_tokens, Some(8192));
    }

    #[test]
    fn guru_private_inputs_use_only_closed_kernel_derivations() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../agents/krw-guru-advisor");
        let image = compile_agent_dir(&root).unwrap().manifest;
        let brief = image
            .body
            .capabilities
            .iter()
            .find(|capability| capability.id == "guru.company_brief")
            .unwrap();
        assert_eq!(brief.input_contract, "krw-guru-company-brief-input/v1");
        assert_eq!(
            brief.model_input_contract.as_deref(),
            Some("krw-guru-investigation-question-draft/v1")
        );
        assert_eq!(
            brief.input_derivation,
            InputDerivation::SealedGuruCompanyBriefV1 {
                query_context_capability: "guru.query_context".into(),
            }
        );
        assert_eq!(
            image
                .resolve_capability_model_input_contract(brief)
                .unwrap()
                .id,
            "krw-guru-investigation-question-draft/v1"
        );

        let review = image
            .body
            .capabilities
            .iter()
            .find(|capability| capability.id == "guru.review_company_evidence")
            .unwrap();
        assert_eq!(review.input_contract, "krw-guru-evidence-review-input/v1");
        assert_eq!(
            review.model_input_contract.as_deref(),
            Some("krw-guru-agent-evidence-analysis/v1")
        );
        assert_eq!(
            review.input_derivation,
            InputDerivation::SealedGuruEvidenceReviewV1 {
                query_context_capability: "guru.query_context".into(),
                company_brief_capability: "guru.company_brief".into(),
                evidence_capabilities: vec![
                    "ontology.query_context".into(),
                    "ontology.query".into(),
                    "ontology.trace".into(),
                    "ontology.chain".into(),
                ],
            }
        );

        let mut spec = parse_spec(&fs::read(root.join("agent.yaml")).unwrap()).unwrap();
        spec.capabilities
            .iter_mut()
            .find(|capability| capability.id == "guru.company_brief")
            .unwrap()
            .model_input_contract = None;
        assert!(matches!(
            validate_spec(&spec),
            Err(ImageError::InvalidSpec(message))
                if message.contains("input derivation or transport codec declaration")
        ));
    }

    #[test]
    fn every_contract_is_exact_nonzero_and_uniquely_pinned() {
        let mut spec = parse_spec(&fs::read(agent_root().join("agent.yaml")).unwrap()).unwrap();
        validate_spec(&spec).unwrap();

        spec.contracts[1].content_hash = spec.contracts[0].content_hash.clone();
        assert!(matches!(
            validate_spec(&spec),
            Err(ImageError::DuplicateId {
                kind: "contract content_hash",
                ..
            })
        ));

        let mut spec = parse_spec(&fs::read(agent_root().join("agent.yaml")).unwrap()).unwrap();
        spec.contracts[0].content_hash = ContentHash::parse(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap();
        assert!(matches!(
            validate_spec(&spec),
            Err(ImageError::InvalidSpec(_))
        ));
    }

    #[test]
    fn compiler_rejects_a_well_formed_but_noncanonical_contract_pin() {
        let mut spec = parse_spec(&fs::read(agent_root().join("agent.yaml")).unwrap()).unwrap();
        spec.contracts[0].content_hash = ContentHash::sha256("wrong schema bytes");
        assert!(matches!(
            validate_spec(&spec),
            Err(ImageError::ContractRegistry(
                krw_agent_contracts::ContractArtifactError::PinnedHashMismatch { .. }
            ))
        ));
    }

    #[test]
    fn capability_output_hash_is_ordered_and_deterministic() {
        let image = compile_agent_dir(agent_root()).unwrap().manifest;
        let capability = &image.body.capabilities[0];
        let first = image.resolve_capability_contracts(capability).unwrap();
        let second = image.resolve_capability_contracts(capability).unwrap();
        assert_eq!(first, second);

        let mut reversed = first.outputs.clone();
        reversed.reverse();
        assert_ne!(
            first.output_contract_set_hash,
            composite_contract_hash(&reversed).unwrap()
        );
    }

    #[test]
    fn capability_states_require_explicit_known_unique_bindings() {
        let mut spec = parse_spec(&fs::read(agent_root().join("agent.yaml")).unwrap()).unwrap();
        let workflow = &mut spec.workflows[0];
        workflow
            .states
            .iter_mut()
            .find(|state| state.id == "query_context")
            .unwrap()
            .capability_id = None;
        assert!(matches!(
            validate_spec(&spec),
            Err(ImageError::InvalidSpec(message))
                if message.contains("must declare capability_id")
        ));

        let mut spec = parse_spec(&fs::read(agent_root().join("agent.yaml")).unwrap()).unwrap();
        spec.workflows[0]
            .states
            .iter_mut()
            .find(|state| state.id == "query_context")
            .unwrap()
            .capability_id = Some("ontology.unknown".into());
        assert!(matches!(
            validate_spec(&spec),
            Err(ImageError::UnknownReference {
                kind: "workflow capability",
                ..
            })
        ));
    }

    #[test]
    fn image_contains_no_endpoint_or_credential() {
        let image = compile_agent_dir(agent_root()).unwrap();
        let bytes = serde_jcs::to_vec(&image.manifest).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains("https://"));
        assert!(!text.to_lowercase().contains("api_key"));
        assert!(!text.to_lowercase().contains("credential_ref"));
    }

    #[test]
    fn loader_verifies_and_shares_every_prompt_blob() {
        let image = compile_agent_dir(agent_root()).unwrap();
        let output = std::env::temp_dir().join(format!(
            "krw-agent-image-test-{}-{}",
            std::process::id(),
            TEST_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        write_image(&image, &output).unwrap();
        let loaded = load_image(&output).unwrap();
        assert_eq!(loaded.content_hash, image.manifest.content_hash);
        assert!(
            loaded
                .prompt("security_boundary")
                .unwrap()
                .contains("untrusted")
        );
        let planner_prompt_count = loaded
            .body
            .roles
            .iter()
            .find(|role| role.id == "planner")
            .unwrap()
            .prompt_segments
            .len();
        assert_eq!(
            loaded.role_prompt_segments("planner").unwrap().len(),
            planner_prompt_count
        );

        let descriptor = &loaded.body.prompt_blobs[0];
        let digest = descriptor
            .content_hash
            .as_str()
            .strip_prefix("sha256:")
            .unwrap();
        fs::write(output.join("blobs/sha256").join(digest), b"tampered").unwrap();
        assert!(matches!(
            load_image(&output),
            Err(ImageError::BlobLengthMismatch { .. } | ImageError::BlobHashMismatch { .. })
        ));
        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn startup_interner_shares_equal_prompt_allocations_across_images() {
        let first = compile_agent_dir(agent_root()).unwrap();
        let mut second = first.clone();
        second.manifest.body.metadata.id = "krw-ontology-second-release".into();
        second.manifest.content_hash =
            ContentHash::sha256(serde_jcs::to_vec(&second.manifest.body).unwrap());
        let prompt_id = first.manifest.body.prompt_blobs[0].id.clone();
        let expected_unique_blobs = first.blobs.len();

        let mut interner = PromptBlobInterner::default();
        let first = first.into_loaded_with_interner(&mut interner).unwrap();
        let second = second.into_loaded_with_interner(&mut interner).unwrap();
        let first_blob = first.prompt_blob_arc(&prompt_id).unwrap();
        let second_blob = second.prompt_blob_arc(&prompt_id).unwrap();

        assert!(Arc::ptr_eq(&first_blob, &second_blob));
        assert_eq!(interner.unique_blob_count(), expected_unique_blobs);
    }

    #[test]
    fn bounded_rule_program_has_deterministic_violations() {
        let program = RuleProgram {
            id: "answer".into(),
            phase: RulePhase::Answer,
            result_ingest_scope: Vec::new(),
            fuel: 64,
            instructions: vec![
                RuleInstruction::ClaimHasEvidence {
                    except_kinds: vec!["uncertainty".into()],
                    code: "claim_evidence_required".into(),
                },
                RuleInstruction::NoInternalTerm {
                    terms: vec!["MCP".into()],
                    code: "internal_term_exposed".into(),
                },
            ],
        };
        let input = serde_json::json!({
            "claims": [
                {"kind": "fact", "text": "MCP result", "evidence_ids": []},
                {"kind": "uncertainty", "text": "unknown", "evidence_ids": []}
            ]
        });
        let first = evaluate_rule_program(&program, &input).unwrap();
        let second = evaluate_rule_program(&program, &input).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            first
                .violations
                .iter()
                .map(|violation| violation.code.as_str())
                .collect::<Vec<_>>(),
            vec!["claim_evidence_required", "internal_term_exposed"]
        );
    }

    #[test]
    fn rule_fuel_exhaustion_fails_closed() {
        let program = RuleProgram {
            id: "tiny".into(),
            phase: RulePhase::Answer,
            result_ingest_scope: Vec::new(),
            fuel: 1,
            instructions: vec![RuleInstruction::ClaimHasEvidence {
                except_kinds: vec![],
                code: "missing".into(),
            }],
        };
        let input = serde_json::json!({"claims": [{"kind": "fact", "evidence_ids": []}]});
        assert!(matches!(
            evaluate_rule_program(&program, &input),
            Err(ImageError::RuleFuelExhausted)
        ));
    }

    #[test]
    fn result_ingest_scoped_rules_are_limited_to_post_action() {
        let mut program = RuleProgram {
            id: "research-state-only".into(),
            phase: RulePhase::Answer,
            result_ingest_scope: vec![CapabilityResultIngest::ResearchStateV2],
            fuel: 64,
            instructions: vec![RuleInstruction::RequireField {
                path: "/contract_version".into(),
                code: "research_state_contract".into(),
            }],
        };
        assert!(matches!(
            validate_rule_program(&program),
            Err(ImageError::InvalidSpec(_))
        ));

        program.phase = RulePhase::PostAction;
        assert!(validate_rule_program(&program).is_ok());
    }
}
