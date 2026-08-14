//! Startup-compiled, state-scoped provider context plans.
//!
//! The planner keeps plugin packaging out of the runtime. It resolves one
//! directly-authored `AgentImage` into immutable per-state prompt references
//! and the smallest capability frontier that the current model state can
//! reach without crossing another model decision. Runtime receipts contain
//! only hashes, sizes, and reason codes; they never persist prompt text,
//! untrusted request text, or provider reasoning.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use krw_agent_contracts::{contract as canonical_contract, verify_pin};
use krw_agent_image::{
    AgentImageManifest, CompiledState, CompiledWorkflow, ImageError, LoadedImage, ModelOutputMode,
    StateKind, StateOperation, provider_input_parameters,
};
use krw_agent_protocol::{ContentHash, RunRequest, provider_tool_name};
use krw_agent_provider_wire::{
    JsonSchemaDocument, ProviderToolDefinition, WireError, project_anthropic_json_schema,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const PROMPT_ASSEMBLY_RECEIPT_SCHEMA_VERSION: u16 = 2;

const MAX_DYNAMIC_SEGMENTS: usize = 32;
const MAX_STATIC_SEGMENTS: usize = 64;
const MAX_CAPABILITY_SCHEMAS: usize = 64;
const MAX_SEGMENT_ID_BYTES: usize = 128;
const MAX_FRONTIER_VISITS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextSegmentKind {
    AgentInstruction,
    KernelStateContract,
    TrustedRunScope,
    /// A bounded, kernel-fetched current-market snapshot. It is distinct from
    /// evidence because volatile advisory data can orient a provider turn but
    /// must never inherit filing-evidence authority.
    TrustedMarketSnapshot,
    UntrustedUserTask,
    SessionMemory,
    EvidenceDigest,
    ProviderLineage,
    RepairFeedback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoadReason {
    RoleMatch,
    CurrentState,
    ImmutableRunScope,
    /// Best-effort, fixed market context fetched before the first provider
    /// turn. It is not model selected and cannot widen the run scope.
    PreEntryMarketSnapshot,
    CurrentUserTurn,
    RelevantMemory,
    CurrentEvidence,
    ActiveToolLineage,
    ValidationRepair,
    /// Tier 2 pinned skill: force-loaded by the entrypoint's `pinned_skills`
    /// declaration, not by role membership.
    PinnedSkill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OmissionReason {
    RoleMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticPromptSegmentRef {
    pub segment_id: String,
    pub content_hash: ContentHash,
    pub byte_len: u64,
    pub stable_prefix: bool,
    pub private: bool,
    pub kind: ContextSegmentKind,
    pub load_reason: LoadReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DynamicContextSegmentRef {
    pub segment_id: String,
    pub content_hash: ContentHash,
    pub byte_len: u64,
    pub private: bool,
    pub kind: ContextSegmentKind,
    pub load_reason: LoadReason,
}

impl DynamicContextSegmentRef {
    pub fn validate(&self) -> Result<(), ContextPlanError> {
        validate_segment_id(&self.segment_id)?;
        if self.byte_len == 0 {
            return Err(ContextPlanError::InvalidDynamicSegment("zero bytes"));
        }
        match (self.kind, self.load_reason) {
            (ContextSegmentKind::KernelStateContract, LoadReason::CurrentState)
            | (ContextSegmentKind::TrustedRunScope, LoadReason::ImmutableRunScope)
            | (ContextSegmentKind::TrustedMarketSnapshot, LoadReason::PreEntryMarketSnapshot)
            | (ContextSegmentKind::UntrustedUserTask, LoadReason::CurrentUserTurn)
            | (ContextSegmentKind::SessionMemory, LoadReason::RelevantMemory)
            | (ContextSegmentKind::EvidenceDigest, LoadReason::CurrentEvidence)
            | (ContextSegmentKind::ProviderLineage, LoadReason::ActiveToolLineage)
            | (ContextSegmentKind::RepairFeedback, LoadReason::ValidationRepair) => Ok(()),
            _ => Err(ContextPlanError::InvalidDynamicSegment(
                "kind and load reason disagree",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OmittedPromptSegmentRef {
    pub segment_id: String,
    pub content_hash: ContentHash,
    pub reason: OmissionReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilitySchemaRef {
    pub capability_id: String,
    pub input_contract_id: String,
    pub input_schema_hash: ContentHash,
    pub definition_hash: ContentHash,
    pub load_reason: LoadReason,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderOutputSchemaRef {
    pub output_contract_id: String,
    pub canonical_schema_hash: ContentHash,
    pub projected_schema_hash: ContentHash,
    pub projected_schema: JsonSchemaDocument,
}

#[derive(Debug, Clone)]
pub struct CompiledStateContext {
    pub run_kind: String,
    pub locale: String,
    pub workflow_id: String,
    pub state_id: String,
    pub role_id: String,
    pub static_segments: Arc<[StaticPromptSegmentRef]>,
    pub omitted_segments: Arc<[OmittedPromptSegmentRef]>,
    pub capability_schemas: Arc<[CapabilitySchemaRef]>,
    pub tool_definitions: Arc<[ProviderToolDefinition]>,
    pub provider_output_schema: Option<ProviderOutputSchemaRef>,
    pub tool_schema_hash: ContentHash,
    pub stable_prefix_hash: ContentHash,
    pub plan_hash: ContentHash,
    pub static_bytes: u64,
}

impl CompiledStateContext {
    pub fn receipt(
        &self,
        image_hash: &ContentHash,
        request: &RunRequest,
        episode_seq: u64,
        dynamic_segments: Vec<DynamicContextSegmentRef>,
    ) -> Result<PromptAssemblyReceipt, ContextPlanError> {
        if episode_seq == 0 {
            return Err(ContextPlanError::InvalidEpisodeSequence);
        }
        if dynamic_segments.len() > MAX_DYNAMIC_SEGMENTS {
            return Err(ContextPlanError::Limit("dynamic segments"));
        }
        let mut ids = BTreeSet::new();
        let mut dynamic_bytes = 0_u64;
        for segment in &dynamic_segments {
            segment.validate()?;
            if !ids.insert(segment.segment_id.as_str()) {
                return Err(ContextPlanError::DuplicateDynamicSegment(
                    segment.segment_id.clone(),
                ));
            }
            dynamic_bytes = dynamic_bytes
                .checked_add(segment.byte_len)
                .ok_or(ContextPlanError::Limit("context bytes"))?;
        }
        let total_bytes = self
            .static_bytes
            .checked_add(dynamic_bytes)
            .ok_or(ContextPlanError::Limit("context bytes"))?;
        let assembly_input = AssemblyHashInput {
            plan_hash: &self.plan_hash,
            episode_seq,
            dynamic_segments: &dynamic_segments,
        };
        let assembly_hash = ContentHash::sha256(serde_jcs::to_vec(&assembly_input)?);
        let receipt = PromptAssemblyReceipt {
            schema_version: PROMPT_ASSEMBLY_RECEIPT_SCHEMA_VERSION,
            run_id_hash: ContentHash::sha256(&request.run_id),
            episode_seq,
            image_hash: image_hash.clone(),
            run_kind: self.run_kind.clone(),
            locale: self.locale.clone(),
            workflow_id: self.workflow_id.clone(),
            state_id: self.state_id.clone(),
            role_id: self.role_id.clone(),
            static_segments: self.static_segments.iter().cloned().collect(),
            dynamic_segments,
            omitted_segments: self.omitted_segments.iter().cloned().collect(),
            capability_schemas: self.capability_schemas.iter().cloned().collect(),
            provider_output_schema_hash: self
                .provider_output_schema
                .as_ref()
                .map(|schema| schema.projected_schema_hash.clone()),
            total_bytes,
            estimated_tokens_upper_bound: total_bytes,
            stable_prefix_hash: self.stable_prefix_hash.clone(),
            tool_schema_hash: self.tool_schema_hash.clone(),
            plan_hash: self.plan_hash.clone(),
            assembly_hash,
        };
        receipt.verify()?;
        Ok(receipt)
    }

    pub fn verify_receipt(
        &self,
        receipt: &PromptAssemblyReceipt,
        image_hash: &ContentHash,
        request: &RunRequest,
    ) -> Result<(), ContextPlanError> {
        receipt.verify()?;
        if receipt.run_id_hash != ContentHash::sha256(&request.run_id)
            || receipt.image_hash != *image_hash
            || receipt.run_kind != self.run_kind
            || receipt.locale != self.locale
            || receipt.workflow_id != self.workflow_id
            || receipt.state_id != self.state_id
            || receipt.role_id != self.role_id
            || receipt.static_segments.as_slice() != self.static_segments.as_ref()
            || receipt.omitted_segments.as_slice() != self.omitted_segments.as_ref()
            || receipt.capability_schemas.as_slice() != self.capability_schemas.as_ref()
            || receipt.provider_output_schema_hash
                != self
                    .provider_output_schema
                    .as_ref()
                    .map(|schema| schema.projected_schema_hash.clone())
            || receipt.stable_prefix_hash != self.stable_prefix_hash
            || receipt.tool_schema_hash != self.tool_schema_hash
            || receipt.plan_hash != self.plan_hash
        {
            return Err(ContextPlanError::ReceiptPlanMismatch);
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct AssemblyHashInput<'a> {
    plan_hash: &'a ContentHash,
    episode_seq: u64,
    dynamic_segments: &'a [DynamicContextSegmentRef],
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct PlanHashInput<'a> {
    image_hash: &'a ContentHash,
    run_kind: &'a str,
    locale: &'a str,
    workflow_id: &'a str,
    state_id: &'a str,
    role_id: &'a str,
    static_segments: &'a [StaticPromptSegmentRef],
    omitted_segments: &'a [OmittedPromptSegmentRef],
    capability_schemas: &'a [CapabilitySchemaRef],
    provider_output_schema_hash: Option<&'a ContentHash>,
    tool_schema_hash: &'a ContentHash,
    stable_prefix_hash: &'a ContentHash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptAssemblyReceipt {
    pub schema_version: u16,
    pub run_id_hash: ContentHash,
    pub episode_seq: u64,
    pub image_hash: ContentHash,
    pub run_kind: String,
    pub locale: String,
    pub workflow_id: String,
    pub state_id: String,
    pub role_id: String,
    pub static_segments: Vec<StaticPromptSegmentRef>,
    pub dynamic_segments: Vec<DynamicContextSegmentRef>,
    pub omitted_segments: Vec<OmittedPromptSegmentRef>,
    pub capability_schemas: Vec<CapabilitySchemaRef>,
    pub provider_output_schema_hash: Option<ContentHash>,
    pub total_bytes: u64,
    pub estimated_tokens_upper_bound: u64,
    pub stable_prefix_hash: ContentHash,
    pub tool_schema_hash: ContentHash,
    pub plan_hash: ContentHash,
    pub assembly_hash: ContentHash,
}

impl PromptAssemblyReceipt {
    pub fn verify(&self) -> Result<(), ContextPlanError> {
        if self.schema_version != PROMPT_ASSEMBLY_RECEIPT_SCHEMA_VERSION || self.episode_seq == 0 {
            return Err(ContextPlanError::InvalidReceipt);
        }
        if self.static_segments.len() > MAX_STATIC_SEGMENTS
            || self.omitted_segments.len() > MAX_STATIC_SEGMENTS
            || self.capability_schemas.len() > MAX_CAPABILITY_SCHEMAS
            || self.dynamic_segments.len() > MAX_DYNAMIC_SEGMENTS
        {
            return Err(ContextPlanError::Limit("receipt collections"));
        }
        let mut segment_ids = BTreeSet::new();
        for segment in &self.static_segments {
            validate_segment_id(&segment.segment_id)?;
            // Static prompt segments are either role-matched prompt segments
            // or entrypoint-pinned skills (progressive disclosure). Both are
            // trusted agent instructions assembled at compile time.
            if segment.byte_len == 0
                || segment.kind != ContextSegmentKind::AgentInstruction
                || !matches!(
                    segment.load_reason,
                    LoadReason::RoleMatch | LoadReason::PinnedSkill
                )
                || !segment_ids.insert(segment.segment_id.as_str())
            {
                return Err(ContextPlanError::InvalidReceipt);
            }
        }
        for segment in &self.omitted_segments {
            validate_segment_id(&segment.segment_id)?;
            if !segment_ids.insert(segment.segment_id.as_str()) {
                return Err(ContextPlanError::InvalidReceipt);
            }
        }
        let mut dynamic_ids = BTreeSet::new();
        let mut dynamic_kinds = BTreeSet::new();
        let dynamic_bytes = self
            .dynamic_segments
            .iter()
            .try_fold(0_u64, |total, segment| {
                segment.validate()?;
                if !dynamic_ids.insert(segment.segment_id.as_str())
                    || !dynamic_kinds.insert(segment.kind)
                {
                    return Err(ContextPlanError::InvalidReceipt);
                }
                total
                    .checked_add(segment.byte_len)
                    .ok_or(ContextPlanError::Limit("context bytes"))
            })?;
        let required_dynamic = [
            ContextSegmentKind::KernelStateContract,
            ContextSegmentKind::TrustedRunScope,
            ContextSegmentKind::UntrustedUserTask,
        ];
        if required_dynamic
            .iter()
            .any(|kind| !dynamic_kinds.contains(kind))
        {
            return Err(ContextPlanError::MissingRequiredContext);
        }
        let static_bytes = self
            .static_segments
            .iter()
            .try_fold(0_u64, |total, segment| {
                total
                    .checked_add(segment.byte_len)
                    .ok_or(ContextPlanError::Limit("context bytes"))
            })?;
        let expected_total = static_bytes
            .checked_add(dynamic_bytes)
            .ok_or(ContextPlanError::Limit("context bytes"))?;
        if self.total_bytes != expected_total
            || self.estimated_tokens_upper_bound != self.total_bytes
        {
            return Err(ContextPlanError::InvalidReceipt);
        }
        let mut capability_ids = BTreeSet::new();
        for schema in &self.capability_schemas {
            validate_segment_id(&schema.capability_id)?;
            validate_segment_id(&schema.input_contract_id)?;
            if schema.load_reason != LoadReason::CurrentState
                || !capability_ids.insert(schema.capability_id.as_str())
            {
                return Err(ContextPlanError::InvalidReceipt);
            }
        }
        let stable_refs = self
            .static_segments
            .iter()
            .filter(|segment| segment.stable_prefix)
            .map(|segment| (&segment.segment_id, &segment.content_hash))
            .collect::<Vec<_>>();
        if ContentHash::sha256(serde_jcs::to_vec(&stable_refs)?) != self.stable_prefix_hash {
            return Err(ContextPlanError::InvalidReceipt);
        }
        let expected_plan = ContentHash::sha256(serde_jcs::to_vec(&PlanHashInput {
            image_hash: &self.image_hash,
            run_kind: &self.run_kind,
            locale: &self.locale,
            workflow_id: &self.workflow_id,
            state_id: &self.state_id,
            role_id: &self.role_id,
            static_segments: &self.static_segments,
            omitted_segments: &self.omitted_segments,
            capability_schemas: &self.capability_schemas,
            provider_output_schema_hash: self.provider_output_schema_hash.as_ref(),
            tool_schema_hash: &self.tool_schema_hash,
            stable_prefix_hash: &self.stable_prefix_hash,
        })?);
        if expected_plan != self.plan_hash {
            return Err(ContextPlanError::InvalidReceipt);
        }
        let expected = ContentHash::sha256(serde_jcs::to_vec(&AssemblyHashInput {
            plan_hash: &self.plan_hash,
            episode_seq: self.episode_seq,
            dynamic_segments: &self.dynamic_segments,
        })?);
        if expected != self.assembly_hash {
            return Err(ContextPlanError::InvalidReceipt);
        }
        Ok(())
    }

    pub fn content_hash(&self) -> Result<ContentHash, ContextPlanError> {
        self.verify()?;
        Ok(ContentHash::sha256(serde_jcs::to_vec(self)?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ContextKey {
    run_kind: String,
    locale: String,
    state_id: String,
}

#[derive(Debug, Clone)]
pub struct ContextPlanner {
    image_hash: ContentHash,
    states: BTreeMap<ContextKey, Arc<CompiledStateContext>>,
}

impl ContextPlanner {
    pub fn compile(image: &LoadedImage) -> Result<Self, ContextPlanError> {
        image.verify()?;
        let manifest = &image.manifest;
        let mut states = BTreeMap::new();
        let mut tool_interner: BTreeMap<ContentHash, Arc<[ProviderToolDefinition]>> =
            BTreeMap::new();
        let mut schema_interner: BTreeMap<ContentHash, Arc<[CapabilitySchemaRef]>> =
            BTreeMap::new();

        for entrypoint in manifest.body.entrypoints.values() {
            let workflow = manifest
                .body
                .workflows
                .iter()
                .find(|workflow| workflow.id == entrypoint.workflow)
                .ok_or_else(|| ContextPlanError::MissingWorkflow(entrypoint.workflow.clone()))?;
            for state in workflow
                .states
                .iter()
                .filter(|state| state.role_id.is_some())
            {
                let compiled = compile_state_context(
                    manifest,
                    entrypoint.run_kind.as_str(),
                    entrypoint.locale.as_str(),
                    workflow,
                    state,
                    &mut tool_interner,
                    &mut schema_interner,
                )?;
                let key = ContextKey {
                    run_kind: entrypoint.run_kind.clone(),
                    locale: entrypoint.locale.clone(),
                    state_id: state.stable_id.clone(),
                };
                if states.insert(key, Arc::new(compiled)).is_some() {
                    return Err(ContextPlanError::DuplicateStateContext);
                }
            }
        }
        Ok(Self {
            image_hash: manifest.content_hash.clone(),
            states,
        })
    }

    pub fn image_hash(&self) -> &ContentHash {
        &self.image_hash
    }

    pub fn for_request(
        &self,
        request: &RunRequest,
        state_id: &str,
    ) -> Result<Arc<CompiledStateContext>, ContextPlanError> {
        self.states
            .get(&ContextKey {
                run_kind: request.run_kind.clone(),
                locale: request.locale.clone(),
                state_id: state_id.to_owned(),
            })
            .cloned()
            .ok_or_else(|| ContextPlanError::UnknownStateContext(state_id.to_owned()))
    }

    pub fn state_count(&self) -> usize {
        self.states.len()
    }
}

fn compile_state_context(
    image: &AgentImageManifest,
    run_kind: &str,
    locale: &str,
    workflow: &CompiledWorkflow,
    state: &CompiledState,
    tool_interner: &mut BTreeMap<ContentHash, Arc<[ProviderToolDefinition]>>,
    schema_interner: &mut BTreeMap<ContentHash, Arc<[CapabilitySchemaRef]>>,
) -> Result<CompiledStateContext, ContextPlanError> {
    let role_id = state
        .role_id
        .as_ref()
        .ok_or(ContextPlanError::ModelStateHasNoRole)?;
    let role = image
        .body
        .roles
        .iter()
        .find(|role| role.id == *role_id)
        .ok_or_else(|| ContextPlanError::MissingRole(role_id.clone()))?;
    // Tier 2 (pinned) skills: the entrypoint for this run_kind declares skill
    // segment IDs that are force-loaded into every model state, in addition
    // to the role's own segments. This guarantees critical analysis
    // frameworks (e.g., earnings 5-stage chain) are always present without
    // relying on the model to call skill.load.
    let entrypoint = image
        .body
        .entrypoints
        .values()
        .find(|entrypoint| entrypoint.run_kind == run_kind && entrypoint.locale == locale)
        .ok_or_else(|| ContextPlanError::MissingEntrypoint(run_kind.to_string()))?;
    // Merge role segments and pinned skills (dedup, role-first order).
    let mut segment_ids_to_load: Vec<String> = role.prompt_segments.clone();
    for pinned in &entrypoint.pinned_skills {
        if !segment_ids_to_load.iter().any(|id| id == pinned) {
            segment_ids_to_load.push(pinned.clone());
        }
    }
    let selected_ids = segment_ids_to_load
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let descriptors = image
        .body
        .prompt_blobs
        .iter()
        .map(|descriptor| (descriptor.id.as_str(), descriptor))
        .collect::<BTreeMap<_, _>>();

    let mut static_bytes = 0_u64;
    let mut static_segments = Vec::with_capacity(segment_ids_to_load.len());
    for segment_id in &segment_ids_to_load {
        let descriptor = descriptors
            .get(segment_id.as_str())
            .ok_or_else(|| ContextPlanError::MissingPrompt(segment_id.clone()))?;
        static_bytes = static_bytes
            .checked_add(descriptor.byte_len)
            .ok_or(ContextPlanError::Limit("prompt bytes"))?;
        // Tag pinned skills so the load reason is distinguishable from
        // role-level segments.
        let is_role_segment = role.prompt_segments.iter().any(|id| id == segment_id);
        static_segments.push(StaticPromptSegmentRef {
            segment_id: segment_id.clone(),
            content_hash: descriptor.content_hash.clone(),
            byte_len: descriptor.byte_len,
            stable_prefix: descriptor.stable_prefix,
            private: descriptor.private,
            kind: ContextSegmentKind::AgentInstruction,
            load_reason: if is_role_segment {
                LoadReason::RoleMatch
            } else {
                LoadReason::PinnedSkill
            },
        });
    }
    let omitted_segments = image
        .body
        .prompt_blobs
        .iter()
        .filter(|descriptor| !selected_ids.contains(descriptor.id.as_str()))
        .map(|descriptor| OmittedPromptSegmentRef {
            segment_id: descriptor.id.clone(),
            content_hash: descriptor.content_hash.clone(),
            reason: OmissionReason::RoleMismatch,
        })
        .collect::<Vec<_>>();

    let frontier = capability_frontier(workflow, state)?;
    // Progressive disclosure is opt-in per role: advertising skill.load
    // without also supplying the compact catalog gives the model an opaque
    // extra tool. Roles retain their primary skills as static context, while
    // the catalog lets them add one or more distinct relevant skills during
    // the same run.
    // Catalog IDs are image-authored role boundaries. Keep the legacy
    // `skill_catalog` name working while allowing each planner/analyst role
    // to receive only its own reference catalog.
    let include_skill_load = role.prompt_segments.iter().any(|segment_id| {
        segment_id == "skill_catalog" || segment_id.ends_with("_skill_catalog")
    });
    let (tool_definitions, capability_schemas) =
        build_frontier_tools(image, &frontier, include_skill_load)?;
    let provider_output_schema = match &state.operation {
        StateOperation::ModelDecision {
            output_mode: ModelOutputMode::TypedJson,
            ..
        } => Some(build_provider_output_schema(&state.operation)?),
        _ => None,
    };
    let tool_schema_hash = ContentHash::sha256(serde_jcs::to_vec(&tool_definitions)?);
    let shared_tools = tool_interner
        .entry(tool_schema_hash.clone())
        .or_insert_with(|| Arc::from(tool_definitions))
        .clone();
    let schema_hash = ContentHash::sha256(serde_jcs::to_vec(&capability_schemas)?);
    let shared_schemas = schema_interner
        .entry(schema_hash)
        .or_insert_with(|| Arc::from(capability_schemas))
        .clone();

    let stable_refs = static_segments
        .iter()
        .filter(|segment| segment.stable_prefix)
        .map(|segment| (&segment.segment_id, &segment.content_hash))
        .collect::<Vec<_>>();
    let stable_prefix_hash = ContentHash::sha256(serde_jcs::to_vec(&stable_refs)?);
    let plan_hash = ContentHash::sha256(serde_jcs::to_vec(&PlanHashInput {
        image_hash: &image.content_hash,
        run_kind,
        locale,
        workflow_id: &workflow.id,
        state_id: &state.stable_id,
        role_id,
        static_segments: &static_segments,
        omitted_segments: &omitted_segments,
        capability_schemas: shared_schemas.as_ref(),
        provider_output_schema_hash: provider_output_schema
            .as_ref()
            .map(|schema| &schema.projected_schema_hash),
        tool_schema_hash: &tool_schema_hash,
        stable_prefix_hash: &stable_prefix_hash,
    })?);

    Ok(CompiledStateContext {
        run_kind: run_kind.to_owned(),
        locale: locale.to_owned(),
        workflow_id: workflow.id.clone(),
        state_id: state.stable_id.clone(),
        role_id: role_id.clone(),
        static_segments: Arc::from(static_segments),
        omitted_segments: Arc::from(omitted_segments),
        capability_schemas: shared_schemas,
        tool_definitions: shared_tools,
        provider_output_schema,
        tool_schema_hash,
        stable_prefix_hash,
        plan_hash,
        static_bytes,
    })
}

fn build_provider_output_schema(
    operation: &StateOperation,
) -> Result<ProviderOutputSchemaRef, ContextPlanError> {
    let StateOperation::ModelDecision {
        output_mode: ModelOutputMode::TypedJson,
        output_contracts,
        ..
    } = operation
    else {
        return Err(ContextPlanError::CanonicalContract(
            "provider output schema requested for a non-TypedJson state".into(),
        ));
    };
    let [output_contract] = output_contracts.as_slice() else {
        return Err(ContextPlanError::CanonicalContract(
            "typed JSON state must declare exactly one output contract".into(),
        ));
    };
    verify_pin(&output_contract.id, &output_contract.content_hash)
        .map_err(|error| ContextPlanError::CanonicalContract(format!("{error:?}")))?;
    let descriptor = canonical_contract(&output_contract.id).ok_or_else(|| {
        ContextPlanError::CanonicalContract(format!(
            "unknown typed JSON output contract {}",
            output_contract.id
        ))
    })?;
    let canonical_schema = descriptor
        .canonical_schema()
        .map_err(|error| ContextPlanError::CanonicalContract(format!("{error:?}")))?;
    if ContentHash::sha256(&canonical_schema) != output_contract.content_hash {
        return Err(ContextPlanError::CanonicalContract(format!(
            "schema pin mismatch for {}",
            output_contract.id
        )));
    }
    let canonical_value: serde_json::Value = serde_json::from_slice(&canonical_schema)?;
    let projected_schema = project_anthropic_json_schema(canonical_value)?;
    let projected_schema_hash = ContentHash::sha256(serde_jcs::to_vec(&projected_schema)?);
    Ok(ProviderOutputSchemaRef {
        output_contract_id: output_contract.id.clone(),
        canonical_schema_hash: output_contract.content_hash.clone(),
        projected_schema_hash,
        projected_schema,
    })
}

fn capability_frontier(
    workflow: &CompiledWorkflow,
    current: &CompiledState,
) -> Result<BTreeSet<String>, ContextPlanError> {
    let states = workflow
        .states
        .iter()
        .map(|state| (state.numeric_id, state))
        .collect::<BTreeMap<_, _>>();
    let mut queue = workflow
        .transitions
        .iter()
        .filter(|transition| transition.from == current.numeric_id)
        .map(|transition| transition.to)
        .collect::<VecDeque<_>>();
    let mut visited = BTreeSet::new();
    let mut capabilities = BTreeSet::new();

    while let Some(state_id) = queue.pop_front() {
        if !visited.insert(state_id) {
            continue;
        }
        if visited.len() > MAX_FRONTIER_VISITS {
            return Err(ContextPlanError::Limit("frontier states"));
        }
        let state = states
            .get(&state_id)
            .ok_or(ContextPlanError::BrokenWorkflow)?;
        if state.kind == StateKind::Capability {
            capabilities.insert(
                state
                    .capability_id
                    .clone()
                    .ok_or(ContextPlanError::BrokenWorkflow)?,
            );
            continue;
        }
        if state.role_id.is_some() || state.kind == StateKind::Terminal {
            continue;
        }
        queue.extend(
            workflow
                .transitions
                .iter()
                .filter(|transition| transition.from == state_id)
                .map(|transition| transition.to),
        );
    }
    Ok(capabilities)
}

fn build_frontier_tools(
    image: &AgentImageManifest,
    frontier: &BTreeSet<String>,
    include_skill_load: bool,
) -> Result<(Vec<ProviderToolDefinition>, Vec<CapabilitySchemaRef>), ContextPlanError> {
    // `skill.load` is an ambient, image-local capability only for roles that
    // include a skill catalog. It never requires a workflow-state detour or
    // an MCP round trip.
    let mut frontier = frontier.clone();
    if include_skill_load
        && image
            .body
            .capabilities
            .iter()
            .any(|capability| capability.id == "skill.load")
    {
        frontier.insert("skill.load".to_string());
    }
    let mut definitions = Vec::with_capacity(frontier.len());
    let mut schemas = Vec::with_capacity(frontier.len());
    let mut provider_names = BTreeSet::new();
    for capability in &image.body.capabilities {
        if !frontier.contains(&capability.id) {
            continue;
        }
        let model_input = image.resolve_capability_model_input_contract(capability)?;
        verify_pin(&model_input.id, &model_input.content_hash)
            .map_err(|error| ContextPlanError::CanonicalContract(format!("{error:?}")))?;
        let descriptor = canonical_contract(&model_input.id).ok_or_else(|| {
            ContextPlanError::CanonicalContract(format!(
                "unknown input contract {}",
                model_input.id
            ))
        })?;
        let canonical_schema = descriptor
            .canonical_schema()
            .map_err(|error| ContextPlanError::CanonicalContract(format!("{error:?}")))?;
        if ContentHash::sha256(&canonical_schema) != model_input.content_hash {
            return Err(ContextPlanError::CanonicalContract(format!(
                "schema pin mismatch for {}",
                model_input.id
            )));
        }
        let parameters = provider_input_parameters(
            &capability.provider_input_codec,
            serde_json::from_slice(&canonical_schema)?,
        )?;
        let provider_name = provider_tool_name(&capability.id);
        if !provider_names.insert(provider_name.clone()) {
            return Err(ContextPlanError::ToolNameCollision);
        }
        let definition = ProviderToolDefinition::new(
            provider_name,
            capability.provider_tool_description(),
            parameters,
        )?;
        let definition_hash = ContentHash::sha256(serde_jcs::to_vec(&definition)?);
        definitions.push(definition);
        schemas.push(CapabilitySchemaRef {
            capability_id: capability.id.clone(),
            input_contract_id: model_input.id,
            input_schema_hash: model_input.content_hash,
            definition_hash,
            load_reason: LoadReason::CurrentState,
        });
    }
    if definitions.len() != frontier.len() {
        return Err(ContextPlanError::BrokenWorkflow);
    }
    Ok((definitions, schemas))
}

fn validate_segment_id(value: &str) -> Result<(), ContextPlanError> {
    if value.is_empty()
        || value.len() > MAX_SEGMENT_ID_BYTES
        || value.contains('\0')
        || value.chars().any(char::is_control)
    {
        Err(ContextPlanError::InvalidDynamicSegment("segment id"))
    } else {
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ContextPlanError {
    #[error("provider wire contract failed: {0}")]
    Wire(#[from] WireError),
    #[error("AgentImage validation failed: {0}")]
    Image(#[from] ImageError),
    #[error("JSON serialization failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("context plan exceeds the closed bound: {0}")]
    Limit(&'static str),
    #[error("missing workflow: {0}")]
    MissingWorkflow(String),
    #[error("missing role: {0}")]
    MissingRole(String),
    #[error("missing entrypoint for run kind: {0}")]
    MissingEntrypoint(String),
    #[error("missing prompt segment: {0}")]
    MissingPrompt(String),
    #[error("model state has no role")]
    ModelStateHasNoRole,
    #[error("compiled workflow is internally inconsistent")]
    BrokenWorkflow,
    #[error("provider tool-name codec collision")]
    ToolNameCollision,
    #[error("duplicate state context")]
    DuplicateStateContext,
    #[error("unknown state context: {0}")]
    UnknownStateContext(String),
    #[error("canonical contract verification failed: {0}")]
    CanonicalContract(String),
    #[error("invalid dynamic context segment: {0}")]
    InvalidDynamicSegment(&'static str),
    #[error("duplicate dynamic context segment: {0}")]
    DuplicateDynamicSegment(String),
    #[error("episode sequence must be positive")]
    InvalidEpisodeSequence,
    #[error("prompt assembly receipt is invalid")]
    InvalidReceipt,
    #[error("prompt assembly receipt does not match the compiled state plan")]
    ReceiptPlanMismatch,
    #[error("prompt assembly receipt is missing required state, scope, or user context")]
    MissingRequiredContext,
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use krw_agent_image::compile_agent_dir;
    use krw_agent_protocol::{BudgetLimits, RunContextV1};

    use super::*;

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap()
    }

    fn fixture() -> (LoadedImage, RunRequest) {
        let image = compile_agent_dir(workspace_root().join("agents/krw-ontology"))
            .unwrap()
            .into_loaded()
            .unwrap();
        let request = RunRequest {
            run_id: "run-sensitive-raw-id".into(),
            session_id: "session-a".into(),
            tenant_id: "tenant-a".into(),
            principal_id: "principal-a".into(),
            run_kind: "company_research".into(),
            locale: "ko-KR".into(),
            question: "회사를 분석해줘".into(),
            requested_model: "glm-5.3".into(),
            model_profile: "glm_high".into(),
            budget: BudgetLimits {
                max_provider_turns: 8,
                max_capability_calls: 5,
                max_replans: 3,
                max_repairs: 1,
                max_input_tokens: 50_000,
                max_output_tokens: 8_000,
                max_evidence_bytes: 4 * 1024 * 1024,
                deadline_ms: 60_000,
                capability_call_limits: BTreeMap::new(),
            },
            context: RunContextV1::CompanyTickerSet {
                tickers: vec!["AAPL".into()],
            },
            session_memory: None,
        };
        (image, request)
    }

    fn required_dynamic_segments() -> Vec<DynamicContextSegmentRef> {
        vec![
            DynamicContextSegmentRef {
                segment_id: "kernel-state".into(),
                content_hash: ContentHash::sha256("state"),
                byte_len: 32,
                private: true,
                kind: ContextSegmentKind::KernelStateContract,
                load_reason: LoadReason::CurrentState,
            },
            DynamicContextSegmentRef {
                segment_id: "trusted-scope".into(),
                content_hash: ContentHash::sha256("AAPL"),
                byte_len: 32,
                private: true,
                kind: ContextSegmentKind::TrustedRunScope,
                load_reason: LoadReason::ImmutableRunScope,
            },
            DynamicContextSegmentRef {
                segment_id: "user-task".into(),
                content_hash: ContentHash::sha256("question"),
                byte_len: 64,
                private: true,
                kind: ContextSegmentKind::UntrustedUserTask,
                load_reason: LoadReason::CurrentUserTurn,
            },
        ]
    }

    #[test]
    fn state_frontier_loads_only_reachable_capabilities() {
        let (image, request) = fixture();
        let planner = ContextPlanner::compile(&image).unwrap();
        let plan = planner.for_request(&request, "author_plan").unwrap();
        assert_eq!(plan.capability_schemas.len(), 2);
        let plan_ids = plan
            .capability_schemas
            .iter()
            .map(|schema| schema.capability_id.as_str())
            .collect::<BTreeSet<_>>();
        assert!(plan_ids.contains("ontology.query_context"));
        assert!(plan_ids.contains("skill.load"));

        let assess = planner.for_request(&request, "assess_obligations").unwrap();
        assert_eq!(
            assess
                .capability_schemas
                .iter()
                .map(|schema| schema.capability_id.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "ontology.query_context",
                "market.snapshot",
                "ontology.query",
                "ontology.trace",
                "ontology.chain",
                "skill.load",
            ])
        );

        // Composer has no capability frontier and therefore receives its
        // writing references statically; it never needs a skill-load detour.
        let compose = planner.for_request(&request, "compose_ir").unwrap();
        assert!(compose
            .capability_schemas
            .iter()
            .all(|schema| schema.capability_id != "skill.load"));
    }

    #[test]
    fn guru_model_tools_expose_only_narrow_drafts_not_private_envelopes() {
        let image = compile_agent_dir(workspace_root().join("agents/krw-guru-advisor"))
            .unwrap()
            .into_loaded()
            .unwrap();
        let (_, mut request) = fixture();
        request.run_kind = "guru_buffett".into();
        request.model_profile = "glm_max".into();
        let planner = ContextPlanner::compile(&image).unwrap();

        let brief = planner
            .for_request(&request, "draft_one_key_question")
            .unwrap();
        assert_eq!(brief.capability_schemas.len(), 1);
        assert_eq!(
            brief.capability_schemas[0].input_contract_id,
            "krw-guru-investigation-question-draft/v1"
        );
        let review = planner
            .for_request(&request, "author_agent_analysis")
            .unwrap();
        assert_eq!(review.capability_schemas.len(), 2);
        let review_contracts = review
            .capability_schemas
            .iter()
            .map(|schema| schema.input_contract_id.as_str())
            .collect::<BTreeSet<_>>();
        assert!(review_contracts.contains("krw-guru-agent-evidence-analysis/v1"));
        assert!(review_contracts.contains("skill-load/v1"));
    }

    #[test]
    fn role_plan_omits_unrelated_prompt_bodies() {
        let (image, request) = fixture();
        let planner = ContextPlanner::compile(&image).unwrap();
        let plan = planner.for_request(&request, "author_plan").unwrap();
        let loaded = plan
            .static_segments
            .iter()
            .map(|segment| segment.segment_id.as_str())
            .collect::<BTreeSet<_>>();
        assert!(loaded.contains("security_boundary"));
        assert!(loaded.contains("planner_skill_catalog"));
        assert!(loaded.contains("research_planner_skill"));
        // The planner skill owns the proposal contract, examples, and repair
        // procedure. Do not duplicate those bodies in every planning turn.
        assert!(!loaded.contains("provider_proposal_contract"));
        assert!(!loaded.contains("research_proposal_examples"));
        assert!(!loaded.contains("research_recovery_loop"));
        assert!(!loaded.contains("research_analysis"));
        assert!(!loaded.contains("retrieval_planner"));
        assert!(!loaded.contains("earnings_analysis"));
        assert!(!loaded.contains("guru_answer"));
        assert!(
            plan.omitted_segments
                .iter()
                .any(|segment| segment.segment_id == "scenario_analysis")
        );

        let assess = planner.for_request(&request, "assess_obligations").unwrap();
        let analyst = assess
            .static_segments
            .iter()
            .map(|segment| segment.segment_id.as_str())
            .collect::<BTreeSet<_>>();
        assert!(analyst.contains("company_skill_catalog"));
        assert!(analyst.contains("evidence_analyst"));
        assert!(analyst.contains("research_analysis"));
    }

    #[test]
    fn specialized_roles_keep_core_skills_and_offer_related_skill_loading() {
        let (image, mut request) = fixture();
        request.run_kind = "earnings_deep_dive".into();
        let planner = ContextPlanner::compile(&image).unwrap();
        let plan = planner.for_request(&request, "author_plan").unwrap();
        let planner_segments = plan
            .static_segments
            .iter()
            .map(|segment| segment.segment_id.as_str())
            .collect::<BTreeSet<_>>();
        assert!(planner_segments.contains("planner_skill_catalog"));
        assert!(planner_segments.contains("research_planner_skill"));
        assert!(!planner_segments.contains("earnings_analysis"));

        let assess = planner
            .for_request(&request, "reconcile_periods_and_commentary")
            .unwrap();
        let analyst_segments = assess
            .static_segments
            .iter()
            .map(|segment| segment.segment_id.as_str())
            .collect::<BTreeSet<_>>();
        assert!(analyst_segments.contains("earnings_skill_catalog"));
        assert!(analyst_segments.contains("evidence_analyst"));
        assert!(analyst_segments.contains("earnings_analysis"));
        assert!(!analyst_segments.contains("research_analysis"));
        assert!(!analyst_segments.contains("thesis_change_policy"));

        let compose = planner.for_request(&request, "compose_ir").unwrap();
        let composer_segments = compose
            .static_segments
            .iter()
            .map(|segment| segment.segment_id.as_str())
            .collect::<BTreeSet<_>>();
        assert!(composer_segments.contains("earnings_output_contract"));
        assert!(!composer_segments.contains("skill_catalog"));
        assert!(!composer_segments.contains("scenario_output_contract"));
    }

    #[test]
    fn identical_frontiers_share_immutable_tool_allocations() {
        let (image, request) = fixture();
        let planner = ContextPlanner::compile(&image).unwrap();
        let author = planner.for_request(&request, "author_plan").unwrap();
        let repair = planner.for_request(&request, "repair_plan").unwrap();
        assert_eq!(author.tool_schema_hash, repair.tool_schema_hash);
        assert!(Arc::ptr_eq(
            &author.tool_definitions,
            &repair.tool_definitions
        ));
        assert!(Arc::ptr_eq(
            &author.capability_schemas,
            &repair.capability_schemas
        ));
    }

    #[test]
    fn receipt_is_canonical_redacted_and_tamper_evident() {
        let (image, request) = fixture();
        let planner = ContextPlanner::compile(&image).unwrap();
        let plan = planner.for_request(&request, "author_plan").unwrap();
        let dynamic = required_dynamic_segments();
        let receipt = plan
            .receipt(planner.image_hash(), &request, 1, dynamic)
            .unwrap();
        receipt.verify().unwrap();
        plan.verify_receipt(&receipt, planner.image_hash(), &request)
            .unwrap();
        assert_eq!(receipt.run_id_hash, ContentHash::sha256(&request.run_id));
        let serialized = String::from_utf8(serde_jcs::to_vec(&receipt).unwrap()).unwrap();
        assert!(!serialized.contains(&request.run_id));
        assert!(!serialized.contains(&request.question));
        assert!(!serialized.contains("KRW_AGENT_TRUSTED_PROGRAM"));
        assert_eq!(
            receipt.content_hash().unwrap(),
            receipt.content_hash().unwrap()
        );

        let mut tampered = receipt;
        tampered.dynamic_segments[0].byte_len += 1;
        assert!(matches!(
            tampered.verify(),
            Err(ContextPlanError::InvalidReceipt)
        ));
    }

    #[test]
    fn dynamic_segment_kind_reason_mismatch_fails_closed() {
        let segment = DynamicContextSegmentRef {
            segment_id: "bad".into(),
            content_hash: ContentHash::sha256("bad"),
            byte_len: 3,
            private: true,
            kind: ContextSegmentKind::ProviderLineage,
            load_reason: LoadReason::CurrentUserTurn,
        };
        assert!(matches!(
            segment.validate(),
            Err(ContextPlanError::InvalidDynamicSegment(_))
        ));
    }

    #[test]
    fn every_direct_authored_agent_image_precompiles_context_plans() {
        let root = workspace_root();
        let agent_dirs = [
            "krw-ontology",
            "krw-ontology-en",
            "krw-guru-advisor",
            "krw-feed",
            "krw-source-filing",
            "krw-notebook",
            "krw-display",
            "krw-router",
        ];
        for agent_dir in agent_dirs {
            let image = compile_agent_dir(root.join("agents").join(agent_dir))
                .unwrap()
                .into_loaded()
                .unwrap();
            let planner = ContextPlanner::compile(&image).unwrap();
            assert!(planner.state_count() > 0, "{agent_dir}");
            assert_eq!(planner.image_hash(), &image.content_hash, "{agent_dir}");
        }
    }
}
