//! Canonical typed state artifacts and a bounded workflow interpreter.
//!
//! The public durable type in this crate is intentionally not an authority by
//! itself. A deserialized [`StateArtifactEnvelope`] becomes transition-capable
//! only after [`ArtifactValidator`] has rechecked its canonical bytes, exact
//! contract pin, payload hash, producer, image, workflow, and state.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use krw_agent_contracts::{STATE_FACTS_V1, validate_value, verify_pin};
use krw_agent_protocol::ContentHash;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Durable envelope format version implemented by this crate.
pub const STATE_ARTIFACT_SCHEMA_VERSION: u16 = 1;
/// Durable interpreter checkpoint format version implemented by this crate.
pub const INTERPRETER_CHECKPOINT_SCHEMA_VERSION: u16 = 1;
/// Durable provider phase-compaction boundary format version.
pub const PHASE_COMPACTION_SCHEMA_VERSION: u16 = 1;

const MAX_STATES: usize = 128;
const MAX_TRANSITIONS: usize = 512;
const MAX_CONTRACTS_PER_OPERATION: usize = 16;
const MAX_ID_BYTES: usize = 160;
const MAX_LINEAGE_REFS: usize = 64;
const MAX_EVIDENCE_REFS: usize = 256;
const MAX_ARTIFACT_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
const MAX_ARTIFACT_ENVELOPE_BYTES: usize = 9 * 1024 * 1024;
const MAX_WORKFLOW_FUEL: u32 = 4_096;

/// Exact identity of a canonical typed contract.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractPin {
    pub id: String,
    pub content_hash: ContentHash,
}

impl ContractPin {
    pub fn canonical(contract_id: &str) -> Result<Self, ArtifactError> {
        let descriptor = krw_agent_contracts::contract(contract_id)
            .ok_or_else(|| ArtifactError::UnknownContract(contract_id.to_owned()))?;
        Ok(Self {
            id: descriptor.id.to_owned(),
            content_hash: descriptor.content_hash()?,
        })
    }

    fn validate(&self) -> Result<(), ArtifactError> {
        validate_id(&self.id, "contract id")?;
        validate_hash(&self.content_hash, "contract hash")?;
        verify_pin(&self.id, &self.content_hash)?;
        Ok(())
    }
}

/// Immutable image/workflow/state coordinates attached to every artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateIdentity {
    pub image_hash: ContentHash,
    pub workflow_id: String,
    pub state_id: String,
}

impl StateIdentity {
    fn validate(&self) -> Result<(), ArtifactError> {
        validate_hash(&self.image_hash, "image hash")?;
        validate_id(&self.workflow_id, "workflow id")?;
        validate_id(&self.state_id, "state id")
    }
}

/// Closed Rust-owned handlers. Agent images may select one of these values but
/// cannot inject a script, function name, dynamic library, or arbitrary hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinHandler {
    InitializeRun,
    ValidateArtifact,
    IngestEvidence,
    VerifyOutput,
    RenderOutput,
    CommitOutput,
    CompactProviderPhase,
}

/// Closed terminal meanings shared by the interpreter and image compiler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalDisposition {
    Succeeded,
    Stopped,
    Failed,
}

/// Trusted host entrypoints are intentionally closed and cannot contain an
/// arbitrary callback or source-controlled handler name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedHostSource {
    RunRequest,
    Recovery,
    VerifiedPriorAnswer,
}

/// Closed reasons for a kernel-authored transition out of a model state.
///
/// A provider episode may be perfectly well-formed while its proposed typed
/// capability input is not.  The rejection itself is a kernel fact, not a
/// model fact, but it is causally bound to the exact provider episode that
/// produced the proposal.  Keeping this as a closed enum avoids inventing a
/// generic "run arbitrary kernel hook" producer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelArtifactReason {
    RejectedModelCapabilityProposal,
    /// A model-decision turn exhausted the image-declared research allowance
    /// after evidence had already been admitted. The kernel may only use this
    /// bounded fact to take an image-declared composition edge.
    OutputBudgetReserved,
}

/// The trusted producer of a state artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ArtifactProducer {
    Model {
        role_id: String,
        provider_episode_hash: ContentHash,
    },
    Capability {
        capability_id: String,
        action_key: ContentHash,
        action_receipt_hash: ContentHash,
    },
    Builtin {
        handler: BuiltinHandler,
    },
    Kernel {
        reason: KernelArtifactReason,
        provider_episode_hash: ContentHash,
    },
    TrustedHost {
        source: TrustedHostSource,
    },
}

impl ArtifactProducer {
    fn validate(&self) -> Result<(), ArtifactError> {
        match self {
            Self::Model {
                role_id,
                provider_episode_hash,
            } => {
                validate_id(role_id, "model role")?;
                validate_hash(provider_episode_hash, "provider episode hash")
            }
            Self::Capability {
                capability_id,
                action_key,
                action_receipt_hash,
            } => {
                validate_id(capability_id, "capability id")?;
                validate_hash(action_key, "action key")?;
                validate_hash(action_receipt_hash, "action receipt hash")
            }
            Self::Kernel {
                provider_episode_hash,
                ..
            } => validate_hash(provider_episode_hash, "provider episode hash"),
            Self::Builtin { .. } | Self::TrustedHost { .. } => Ok(()),
        }
    }
}

/// Closed lineage relations retained in durable artifacts and compacted
/// provider context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineageRelation {
    Input,
    PriorState,
    Evidence,
    Calculation,
    ProviderEpisode,
    CapabilityReceipt,
    HostContext,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactLineageRef {
    pub artifact_hash: ContentHash,
    pub relation: LineageRelation,
}

impl ArtifactLineageRef {
    fn validate(&self) -> Result<(), ArtifactError> {
        validate_hash(&self.artifact_hash, "lineage hash")
    }
}

/// Canonical durable artifact. Deserializing this type does not validate it;
/// callers must use [`ArtifactValidator::recover_for_operation`] or
/// [`ArtifactValidator::recover_host_input`] before it can drive a transition.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateArtifactEnvelope {
    pub schema_version: u16,
    pub image_hash: ContentHash,
    pub workflow_id: String,
    pub state_id: String,
    pub producer: ArtifactProducer,
    pub event: String,
    pub declared_contract: ContractPin,
    pub payload_hash: ContentHash,
    pub payload_bytes: u32,
    pub lineage_refs: Vec<ArtifactLineageRef>,
    pub payload: Value,
}

impl fmt::Debug for StateArtifactEnvelope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StateArtifactEnvelope")
            .field("schema_version", &self.schema_version)
            .field("image_hash", &self.image_hash)
            .field("workflow_id", &self.workflow_id)
            .field("state_id", &self.state_id)
            .field("producer", &self.producer)
            .field("event", &self.event)
            .field("declared_contract", &self.declared_contract)
            .field("payload_hash", &self.payload_hash)
            .field("payload_bytes", &self.payload_bytes)
            .field("lineage_refs", &self.lineage_refs)
            .field("payload", &"<redacted>")
            .finish()
    }
}

impl StateArtifactEnvelope {
    pub fn identity(&self) -> StateIdentity {
        StateIdentity {
            image_hash: self.image_hash.clone(),
            workflow_id: self.workflow_id.clone(),
            state_id: self.state_id.clone(),
        }
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ArtifactError> {
        let bytes = serde_jcs::to_vec(self)?;
        ensure_limit(
            bytes.len(),
            MAX_ARTIFACT_ENVELOPE_BYTES,
            "artifact envelope bytes",
        )?;
        Ok(bytes)
    }

    pub fn envelope_hash(&self) -> Result<ContentHash, ArtifactError> {
        Ok(ContentHash::sha256(self.canonical_bytes()?))
    }
}

/// Opaque transition authority created only after full validation.
#[derive(Clone)]
pub struct ValidatedArtifact(Arc<StateArtifactEnvelope>);

impl fmt::Debug for ValidatedArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ValidatedArtifact")
            .field("image_hash", &self.0.image_hash)
            .field("workflow_id", &self.0.workflow_id)
            .field("state_id", &self.0.state_id)
            .field("producer", &self.0.producer)
            .field("event", &self.0.event)
            .field("declared_contract", &self.0.declared_contract)
            .field("payload_hash", &self.0.payload_hash)
            .finish()
    }
}

impl ValidatedArtifact {
    pub fn envelope(&self) -> &StateArtifactEnvelope {
        &self.0
    }

    pub fn payload(&self) -> &Value {
        &self.0.payload
    }

    pub fn contract(&self) -> &ContractPin {
        &self.0.declared_contract
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ArtifactError> {
        self.0.canonical_bytes()
    }

    pub fn artifact_hash(&self) -> Result<ContentHash, ArtifactError> {
        self.0.envelope_hash()
    }
}

/// The only provider-visible output shape that a model decision state may
/// emit. This is compiled into the immutable state program rather than
/// inferred from natural-language instructions at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelOutputMode {
    /// The provider must invoke exactly one advertised capability function.
    CapabilityCall,
    /// The provider must invoke the kernel-owned typed transition function.
    WorkflowTransition,
    /// The provider may invoke either one advertised capability or the typed
    /// transition function. This preserves model autonomy at assessment
    /// points while keeping the choice structurally unambiguous.
    CapabilityOrWorkflowTransition,
    /// The provider must return one JSON object for the declared final output
    /// contract. No capability or transition function is exposed.
    TypedJson,
    /// The provider must return one user-facing Markdown document. The
    /// kernel-owned EvidenceLedger remains attached to the same final commit,
    /// but the model is not forced to serialize its prose into a claim IR.
    /// No capability or transition function is exposed.
    Markdown,
}

/// Explicit executable meaning of a compiled workflow state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StateOperation {
    ModelDecision {
        role_id: String,
        output_mode: ModelOutputMode,
        input_contracts: Vec<ContractPin>,
        output_contracts: Vec<ContractPin>,
    },
    CapabilityAction {
        capability_id: String,
        input_contracts: Vec<ContractPin>,
        output_contracts: Vec<ContractPin>,
    },
    Builtin {
        handler: BuiltinHandler,
        input_contracts: Vec<ContractPin>,
        output_contracts: Vec<ContractPin>,
    },
    Terminal {
        disposition: TerminalDisposition,
        accepted_contracts: Vec<ContractPin>,
    },
}

impl StateOperation {
    pub fn input_contracts(&self) -> &[ContractPin] {
        match self {
            Self::ModelDecision {
                input_contracts, ..
            }
            | Self::CapabilityAction {
                input_contracts, ..
            }
            | Self::Builtin {
                input_contracts, ..
            } => input_contracts,
            Self::Terminal {
                accepted_contracts, ..
            } => accepted_contracts,
        }
    }

    pub fn output_contracts(&self) -> &[ContractPin] {
        match self {
            Self::ModelDecision {
                output_contracts, ..
            }
            | Self::CapabilityAction {
                output_contracts, ..
            }
            | Self::Builtin {
                output_contracts, ..
            } => output_contracts,
            Self::Terminal { .. } => &[],
        }
    }

    fn validate(&self) -> Result<(), ArtifactError> {
        let (subject, input_contracts, output_contracts) = match self {
            Self::ModelDecision {
                role_id,
                output_mode: _,
                input_contracts,
                output_contracts,
            } => {
                validate_id(role_id, "model role")?;
                (
                    "model decision",
                    input_contracts.as_slice(),
                    output_contracts.as_slice(),
                )
            }
            Self::CapabilityAction {
                capability_id,
                input_contracts,
                output_contracts,
            } => {
                validate_id(capability_id, "capability id")?;
                (
                    "capability action",
                    input_contracts.as_slice(),
                    output_contracts.as_slice(),
                )
            }
            Self::Builtin {
                input_contracts,
                output_contracts,
                ..
            } => (
                "builtin",
                input_contracts.as_slice(),
                output_contracts.as_slice(),
            ),
            Self::Terminal {
                accepted_contracts, ..
            } => (
                "terminal",
                accepted_contracts.as_slice(),
                &[] as &[ContractPin],
            ),
        };
        validate_contract_set(input_contracts, true, subject)?;
        if matches!(self, Self::Terminal { .. }) {
            if input_contracts.is_empty() {
                return Err(ArtifactError::InvalidOperation(
                    "terminal must accept at least one typed contract",
                ));
            }
        } else {
            validate_contract_set(output_contracts, false, subject)?;
        }
        Ok(())
    }

    fn producer_matches(&self, producer: &ArtifactProducer) -> bool {
        match (self, producer) {
            (
                Self::ModelDecision { role_id, .. },
                ArtifactProducer::Model {
                    role_id: observed, ..
                },
            ) => role_id == observed,
            (
                Self::CapabilityAction { capability_id, .. },
                ArtifactProducer::Capability {
                    capability_id: observed,
                    ..
                },
            ) => capability_id == observed,
            (Self::Builtin { handler, .. }, ArtifactProducer::Builtin { handler: observed }) => {
                handler == observed
            }
            (
                Self::ModelDecision { .. },
                ArtifactProducer::Kernel {
                    reason: KernelArtifactReason::RejectedModelCapabilityProposal,
                    ..
                },
            ) => true,
            (
                Self::ModelDecision { .. },
                ArtifactProducer::Kernel {
                    reason: KernelArtifactReason::OutputBudgetReserved,
                    ..
                },
            ) => true,
            _ => false,
        }
    }
}

/// Untrusted artifact material submitted to the canonical envelope factory.
/// Authority is granted only after `seal_for_operation` validates every field
/// against the exact current operation.
#[derive(Debug, Clone, PartialEq)]
pub struct StateArtifactDraft {
    pub producer: ArtifactProducer,
    pub event: String,
    pub declared_contract: ContractPin,
    pub payload: Value,
    pub lineage_refs: Vec<ArtifactLineageRef>,
}

/// Bounded validator and canonical envelope factory.
#[derive(Debug, Clone)]
pub struct ArtifactValidator {
    max_payload_bytes: usize,
}

impl Default for ArtifactValidator {
    fn default() -> Self {
        Self {
            max_payload_bytes: MAX_ARTIFACT_PAYLOAD_BYTES,
        }
    }
}

impl ArtifactValidator {
    pub fn seal_for_operation(
        &self,
        identity: &StateIdentity,
        operation: &StateOperation,
        draft: StateArtifactDraft,
    ) -> Result<ValidatedArtifact, ArtifactError> {
        operation.validate()?;
        let artifact = self.seal_unbound(
            identity,
            draft.producer,
            draft.event,
            draft.declared_contract,
            draft.payload,
            draft.lineage_refs,
        )?;
        Self::validate_operation_authority(artifact.envelope(), identity, operation)?;
        Ok(artifact)
    }

    pub fn seal_host_input(
        &self,
        identity: &StateIdentity,
        source: TrustedHostSource,
        declared_contract: ContractPin,
        payload: Value,
        lineage_refs: Vec<ArtifactLineageRef>,
    ) -> Result<ValidatedArtifact, ArtifactError> {
        self.seal_unbound(
            identity,
            ArtifactProducer::TrustedHost { source },
            "host_input".into(),
            declared_contract,
            payload,
            lineage_refs,
        )
    }

    pub fn recover_for_operation(
        &self,
        canonical_bytes: &[u8],
        identity: &StateIdentity,
        operation: &StateOperation,
    ) -> Result<ValidatedArtifact, ArtifactError> {
        operation.validate()?;
        let artifact = self.recover_unbound(canonical_bytes)?;
        Self::validate_operation_authority(artifact.envelope(), identity, operation)?;
        Ok(artifact)
    }

    pub fn recover_host_input(
        &self,
        canonical_bytes: &[u8],
        identity: &StateIdentity,
        source: TrustedHostSource,
        accepted_contracts: &[ContractPin],
    ) -> Result<ValidatedArtifact, ArtifactError> {
        validate_contract_set(accepted_contracts, false, "host input")?;
        let artifact = self.recover_unbound(canonical_bytes)?;
        if artifact.envelope().identity() != *identity {
            return Err(ArtifactError::StateIdentityMismatch);
        }
        if artifact.envelope().producer != (ArtifactProducer::TrustedHost { source }) {
            return Err(ArtifactError::ProducerMismatch);
        }
        if !accepted_contracts.contains(artifact.contract()) {
            return Err(ArtifactError::ContractNotAllowed);
        }
        Ok(artifact)
    }

    fn seal_unbound(
        &self,
        identity: &StateIdentity,
        producer: ArtifactProducer,
        event: String,
        declared_contract: ContractPin,
        payload: Value,
        lineage_refs: Vec<ArtifactLineageRef>,
    ) -> Result<ValidatedArtifact, ArtifactError> {
        identity.validate()?;
        producer.validate()?;
        validate_id(&event, "artifact event")?;
        declared_contract.validate()?;
        validate_lineage(&lineage_refs)?;
        validate_value(&declared_contract.id, &payload)?;
        let payload_bytes = serde_jcs::to_vec(&payload)?;
        ensure_limit(
            payload_bytes.len(),
            self.max_payload_bytes,
            "artifact payload bytes",
        )?;
        let payload_size = u32::try_from(payload_bytes.len())
            .map_err(|_| ArtifactError::LimitExceeded("artifact payload bytes"))?;
        let envelope = StateArtifactEnvelope {
            schema_version: STATE_ARTIFACT_SCHEMA_VERSION,
            image_hash: identity.image_hash.clone(),
            workflow_id: identity.workflow_id.clone(),
            state_id: identity.state_id.clone(),
            producer,
            event,
            declared_contract,
            payload_hash: ContentHash::sha256(&payload_bytes),
            payload_bytes: payload_size,
            lineage_refs,
            payload,
        };
        self.validate_integrity(&envelope)?;
        Ok(ValidatedArtifact(Arc::new(envelope)))
    }

    fn recover_unbound(&self, canonical_bytes: &[u8]) -> Result<ValidatedArtifact, ArtifactError> {
        ensure_limit(
            canonical_bytes.len(),
            MAX_ARTIFACT_ENVELOPE_BYTES,
            "artifact envelope bytes",
        )?;
        let envelope: StateArtifactEnvelope = serde_json::from_slice(canonical_bytes)?;
        if serde_jcs::to_vec(&envelope)? != canonical_bytes {
            return Err(ArtifactError::NonCanonicalEnvelope);
        }
        self.validate_integrity(&envelope)?;
        Ok(ValidatedArtifact(Arc::new(envelope)))
    }

    fn validate_integrity(&self, envelope: &StateArtifactEnvelope) -> Result<(), ArtifactError> {
        if envelope.schema_version != STATE_ARTIFACT_SCHEMA_VERSION {
            return Err(ArtifactError::SchemaVersionMismatch);
        }
        envelope.identity().validate()?;
        envelope.producer.validate()?;
        validate_id(&envelope.event, "artifact event")?;
        envelope.declared_contract.validate()?;
        validate_hash(&envelope.payload_hash, "payload hash")?;
        validate_lineage(&envelope.lineage_refs)?;
        validate_value(&envelope.declared_contract.id, &envelope.payload)?;
        let payload_bytes = serde_jcs::to_vec(&envelope.payload)?;
        ensure_limit(
            payload_bytes.len(),
            self.max_payload_bytes,
            "artifact payload bytes",
        )?;
        let observed_size = u32::try_from(payload_bytes.len())
            .map_err(|_| ArtifactError::LimitExceeded("artifact payload bytes"))?;
        if observed_size != envelope.payload_bytes {
            return Err(ArtifactError::PayloadSizeMismatch);
        }
        if ContentHash::sha256(payload_bytes) != envelope.payload_hash {
            return Err(ArtifactError::PayloadHashMismatch);
        }
        Ok(())
    }

    fn validate_operation_authority(
        envelope: &StateArtifactEnvelope,
        identity: &StateIdentity,
        operation: &StateOperation,
    ) -> Result<(), ArtifactError> {
        if envelope.identity() != *identity {
            return Err(ArtifactError::StateIdentityMismatch);
        }
        if !operation.producer_matches(&envelope.producer) {
            return Err(ArtifactError::ProducerMismatch);
        }
        if !operation
            .output_contracts()
            .contains(&envelope.declared_contract)
        {
            return Err(ArtifactError::ContractNotAllowed);
        }
        // Kernel-originated artifacts may only carry the bounded state-facts
        // contract.  In particular, the kernel cannot fabricate a model
        // capability proposal or a final answer merely because it is allowed
        // to route a rejected proposal to a repair state.
        if matches!(envelope.producer, ArtifactProducer::Kernel { .. })
            && envelope.declared_contract.id != STATE_FACTS_V1
        {
            return Err(ArtifactError::ContractNotAllowed);
        }
        Ok(())
    }
}

/// Bounded condition evaluated only against a validated artifact payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ArtifactGuard {
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

impl ArtifactGuard {
    fn matches(&self, payload: &Value) -> bool {
        match self {
            Self::Always => true,
            Self::FieldPresent { pointer } => payload.pointer(pointer).is_some(),
            Self::FieldAbsent { pointer } => payload.pointer(pointer).is_none(),
            Self::FieldEquals { pointer, value } => payload.pointer(pointer) == Some(value),
            Self::FieldNotEquals { pointer, value } => payload.pointer(pointer) != Some(value),
        }
    }

    fn validate(&self) -> Result<(), ArtifactError> {
        match self {
            Self::Always => Ok(()),
            Self::FieldPresent { pointer } | Self::FieldAbsent { pointer } => {
                validate_pointer(pointer)
            }
            Self::FieldEquals { pointer, value } | Self::FieldNotEquals { pointer, value } => {
                validate_pointer(pointer)?;
                ensure_limit(
                    serde_jcs::to_vec(value)?.len(),
                    64 * 1024,
                    "transition guard constant",
                )
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateNode {
    pub id: String,
    pub operation: StateOperation,
    pub max_visits: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactTransition {
    pub from: String,
    pub event: Option<String>,
    pub guard: ArtifactGuard,
    pub to: String,
}

impl ArtifactTransition {
    fn matches(&self, artifact: &ValidatedArtifact) -> bool {
        let event_matches = match &self.event {
            Some(expected) => artifact.envelope().event == *expected,
            None => true,
        };
        event_matches && self.guard.matches(artifact.payload())
    }
}

/// Immutable, hash-bound state program interpreted without scripts or hooks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateProgram {
    pub image_hash: ContentHash,
    pub workflow_id: String,
    pub initial_state: String,
    pub states: Vec<StateNode>,
    pub transitions: Vec<ArtifactTransition>,
    pub max_fuel: u32,
}

impl StateProgram {
    pub fn validate(&self) -> Result<(), ArtifactError> {
        validate_hash(&self.image_hash, "program image hash")?;
        validate_id(&self.workflow_id, "program workflow id")?;
        validate_id(&self.initial_state, "initial state id")?;
        if self.states.is_empty() || self.states.len() > MAX_STATES {
            return Err(ArtifactError::LimitExceeded("program states"));
        }
        if self.transitions.len() > MAX_TRANSITIONS {
            return Err(ArtifactError::LimitExceeded("program transitions"));
        }
        if self.max_fuel == 0 || self.max_fuel > MAX_WORKFLOW_FUEL {
            return Err(ArtifactError::LimitExceeded("program fuel"));
        }
        let mut state_ids = BTreeSet::new();
        for state in &self.states {
            validate_id(&state.id, "state id")?;
            if !state_ids.insert(state.id.as_str()) {
                return Err(ArtifactError::DuplicateState(state.id.clone()));
            }
            if state.max_visits == 0 {
                return Err(ArtifactError::InvalidState("state max_visits is zero"));
            }
            state.operation.validate()?;
        }
        if !state_ids.contains(self.initial_state.as_str()) {
            return Err(ArtifactError::UnknownState(self.initial_state.clone()));
        }
        let mut transition_keys = BTreeSet::new();
        for transition in &self.transitions {
            if !state_ids.contains(transition.from.as_str()) {
                return Err(ArtifactError::UnknownState(transition.from.clone()));
            }
            if !state_ids.contains(transition.to.as_str()) {
                return Err(ArtifactError::UnknownState(transition.to.clone()));
            }
            if let Some(event) = &transition.event {
                validate_id(event, "transition event")?;
            }
            transition.guard.validate()?;
            let key = serde_jcs::to_vec(transition)?;
            if !transition_keys.insert(key) {
                return Err(ArtifactError::DuplicateTransition);
            }
        }
        for state in &self.states {
            let outgoing = self
                .transitions
                .iter()
                .any(|transition| transition.from == state.id);
            match state.operation {
                StateOperation::Terminal { .. } if outgoing => {
                    return Err(ArtifactError::InvalidState(
                        "terminal state has an outgoing transition",
                    ));
                }
                StateOperation::Terminal { .. } => {}
                _ if !outgoing => {
                    return Err(ArtifactError::InvalidState(
                        "nonterminal state has no outgoing transition",
                    ));
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub fn program_hash(&self) -> Result<ContentHash, ArtifactError> {
        self.validate()?;
        Ok(ContentHash::sha256(serde_jcs::to_vec(self)?))
    }

    fn state(&self, state_id: &str) -> Result<&StateNode, ArtifactError> {
        self.states
            .iter()
            .find(|state| state.id == state_id)
            .ok_or_else(|| ArtifactError::UnknownState(state_id.to_owned()))
    }

    fn identity(&self, state_id: &str) -> StateIdentity {
        StateIdentity {
            image_hash: self.image_hash.clone(),
            workflow_id: self.workflow_id.clone(),
            state_id: state_id.to_owned(),
        }
    }
}

/// Boundary at which the deterministic interpreter yields to a trusted
/// provider, capability executor, or terminal owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionBoundary {
    ModelDecision {
        identity: StateIdentity,
        role_id: String,
        output_mode: ModelOutputMode,
        input_contracts: Vec<ContractPin>,
        output_contracts: Vec<ContractPin>,
    },
    CapabilityAction {
        identity: StateIdentity,
        capability_id: String,
        input_contracts: Vec<ContractPin>,
        output_contracts: Vec<ContractPin>,
    },
    Terminal {
        identity: StateIdentity,
        disposition: TerminalDisposition,
        accepted_contracts: Vec<ContractPin>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterpreterCheckpointV1 {
    pub schema_version: u16,
    pub program_hash: ContentHash,
    pub current_state: String,
    pub fuel_used: u32,
    pub visits: BTreeMap<String, u16>,
    pub ingress_hash: Option<ContentHash>,
    pub artifact_hashes: Vec<ContentHash>,
    pub history_hash: ContentHash,
}

/// Deterministic typed-state interpreter. It retains only the last validated
/// payload plus bounded hashes; provider transcript compaction is separate.
#[derive(Clone)]
pub struct StateInterpreter {
    program: Arc<StateProgram>,
    current_state: String,
    fuel_used: u32,
    visits: BTreeMap<String, u16>,
    ingress_hash: Option<ContentHash>,
    artifact_hashes: Vec<ContentHash>,
    last_artifact: Option<ValidatedArtifact>,
}

impl fmt::Debug for StateInterpreter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StateInterpreter")
            .field("program_hash", &self.program.program_hash().ok())
            .field("current_state", &self.current_state)
            .field("fuel_used", &self.fuel_used)
            .field("visits", &self.visits)
            .field("ingress_hash", &self.ingress_hash)
            .field("artifact_hashes", &self.artifact_hashes)
            .finish_non_exhaustive()
    }
}

impl StateInterpreter {
    pub fn new(program: StateProgram) -> Result<Self, ArtifactError> {
        program.validate()?;
        let initial_state = program.initial_state.clone();
        Ok(Self {
            program: Arc::new(program),
            current_state: initial_state.clone(),
            fuel_used: 0,
            visits: BTreeMap::from([(initial_state, 1)]),
            ingress_hash: None,
            artifact_hashes: Vec::new(),
            last_artifact: None,
        })
    }

    pub fn current_state(&self) -> &str {
        &self.current_state
    }

    /// Return the remaining number of legal entries for a compiled state.
    /// This is a read-only admission primitive: callers can avoid offering an
    /// action that a later transition would deterministically reject, while
    /// the interpreter remains the sole authority that actually mutates the
    /// visit counter.
    pub fn remaining_visits(&self, state_id: &str) -> Result<u16, ArtifactError> {
        let state = self.program.state(state_id)?;
        let used = self.visits.get(state_id).copied().unwrap_or_default();
        Ok(state.max_visits.saturating_sub(used))
    }

    pub fn current_operation(&self) -> Result<&StateOperation, ArtifactError> {
        Ok(&self.program.state(&self.current_state)?.operation)
    }

    pub fn last_artifact(&self) -> Option<&ValidatedArtifact> {
        self.last_artifact.as_ref()
    }

    pub fn bind_ingress(
        &mut self,
        validator: &ArtifactValidator,
        artifact: &ValidatedArtifact,
        source: TrustedHostSource,
    ) -> Result<(), ArtifactError> {
        if self.ingress_hash.is_some() || !self.artifact_hashes.is_empty() {
            return Err(ArtifactError::IngressAlreadyBound);
        }
        let state = self.program.state(&self.current_state)?;
        let identity = self.program.identity(&self.current_state);
        let canonical = artifact.canonical_bytes()?;
        let recovered = validator.recover_host_input(
            &canonical,
            &identity,
            source,
            state.operation.input_contracts(),
        )?;
        let hash = recovered.artifact_hash()?;
        self.ingress_hash = Some(hash);
        self.last_artifact = Some(recovered);
        Ok(())
    }

    pub fn boundary(&self) -> Result<ExecutionBoundary, ArtifactError> {
        let state = self.program.state(&self.current_state)?;
        self.validate_current_input(&state.operation)?;
        let identity = self.program.identity(&self.current_state);
        match &state.operation {
            StateOperation::ModelDecision {
                role_id,
                output_mode,
                input_contracts,
                output_contracts,
            } => Ok(ExecutionBoundary::ModelDecision {
                identity,
                role_id: role_id.clone(),
                output_mode: *output_mode,
                input_contracts: input_contracts.clone(),
                output_contracts: output_contracts.clone(),
            }),
            StateOperation::CapabilityAction {
                capability_id,
                input_contracts,
                output_contracts,
            } => Ok(ExecutionBoundary::CapabilityAction {
                identity,
                capability_id: capability_id.clone(),
                input_contracts: input_contracts.clone(),
                output_contracts: output_contracts.clone(),
            }),
            StateOperation::Terminal {
                disposition,
                accepted_contracts,
            } => Ok(ExecutionBoundary::Terminal {
                identity,
                disposition: *disposition,
                accepted_contracts: accepted_contracts.clone(),
            }),
            StateOperation::Builtin { .. } => Err(ArtifactError::BuiltinRequiresAutoDrive),
        }
    }

    pub fn apply_artifact(
        &mut self,
        validator: &ArtifactValidator,
        artifact: &ValidatedArtifact,
    ) -> Result<(), ArtifactError> {
        let state = self.program.state(&self.current_state)?;
        self.validate_current_input(&state.operation)?;
        let identity = self.program.identity(&self.current_state);
        let canonical = artifact.canonical_bytes()?;
        let recovered = validator.recover_for_operation(&canonical, &identity, &state.operation)?;
        if let Some(previous) = &self.last_artifact {
            let previous_hash = previous.artifact_hash()?;
            if !recovered.envelope().lineage_refs.iter().any(|lineage| {
                lineage.artifact_hash == previous_hash
                    && matches!(
                        lineage.relation,
                        LineageRelation::Input | LineageRelation::PriorState
                    )
            }) {
                return Err(ArtifactError::PreviousArtifactNotLinked);
            }
        }
        let matching = self
            .program
            .transitions
            .iter()
            .filter(|transition| transition.from == self.current_state)
            .filter(|transition| transition.matches(&recovered))
            .collect::<Vec<_>>();
        let [transition] = matching.as_slice() else {
            return Err(if matching.is_empty() {
                ArtifactError::NoTransition
            } else {
                ArtifactError::AmbiguousTransition
            });
        };
        let next_state = transition.to.clone();
        let artifact_hash = recovered.artifact_hash()?;
        let next_fuel = self
            .fuel_used
            .checked_add(1)
            .ok_or(ArtifactError::FuelExhausted)?;
        if next_fuel > self.program.max_fuel {
            return Err(ArtifactError::FuelExhausted);
        }
        let next_visits = self
            .visits
            .get(&next_state)
            .copied()
            .unwrap_or_default()
            .checked_add(1)
            .ok_or_else(|| ArtifactError::StateVisitLimit(next_state.clone()))?;
        if next_visits > self.program.state(&next_state)?.max_visits {
            return Err(ArtifactError::StateVisitLimit(next_state));
        }
        self.fuel_used = next_fuel;
        self.visits.insert(next_state.clone(), next_visits);
        self.current_state = next_state;
        self.artifact_hashes.push(artifact_hash);
        self.last_artifact = Some(recovered);
        Ok(())
    }

    pub fn auto_drive<E: BuiltinExecutor>(
        &mut self,
        validator: &ArtifactValidator,
        executor: &mut E,
    ) -> Result<AutoDriveOutcome, ArtifactError> {
        let mut produced = Vec::new();
        loop {
            let state = self.program.state(&self.current_state)?.clone();
            let operation = state.operation.clone();
            let StateOperation::Builtin {
                handler,
                input_contracts: _,
                output_contracts,
            } = &operation
            else {
                return Ok(AutoDriveOutcome {
                    produced,
                    boundary: self.boundary()?,
                });
            };
            self.validate_current_input(&operation)?;
            let identity = self.program.identity(&state.id);
            let invocation = BuiltinInvocation {
                identity: &identity,
                handler: *handler,
                input: self.last_artifact.as_ref(),
                output_contracts,
            };
            let mut draft = executor
                .execute(invocation)
                .map_err(|failure| ArtifactError::BuiltinFailed(failure.code))?;
            if let Some(input) = &self.last_artifact {
                let input_hash = input.artifact_hash()?;
                if !draft.lineage_refs.iter().any(|lineage| {
                    lineage.artifact_hash == input_hash
                        && matches!(
                            lineage.relation,
                            LineageRelation::Input | LineageRelation::PriorState
                        )
                }) {
                    draft.lineage_refs.push(ArtifactLineageRef {
                        artifact_hash: input_hash,
                        relation: LineageRelation::Input,
                    });
                }
            }
            let artifact = validator.seal_for_operation(
                &identity,
                &operation,
                StateArtifactDraft {
                    producer: ArtifactProducer::Builtin { handler: *handler },
                    event: draft.event,
                    declared_contract: draft.declared_contract,
                    payload: draft.payload,
                    lineage_refs: draft.lineage_refs,
                },
            )?;
            self.apply_artifact(validator, &artifact)?;
            produced.push(artifact);
        }
    }

    pub fn checkpoint(&self) -> Result<InterpreterCheckpointV1, ArtifactError> {
        let history_hash =
            history_hash(self.ingress_hash.as_ref(), self.artifact_hashes.as_slice())?;
        Ok(InterpreterCheckpointV1 {
            schema_version: INTERPRETER_CHECKPOINT_SCHEMA_VERSION,
            program_hash: self.program.program_hash()?,
            current_state: self.current_state.clone(),
            fuel_used: self.fuel_used,
            visits: self.visits.clone(),
            ingress_hash: self.ingress_hash.clone(),
            artifact_hashes: self.artifact_hashes.clone(),
            history_hash,
        })
    }

    pub fn reconstruct(
        program: StateProgram,
        validator: &ArtifactValidator,
        ingress: Option<(&[u8], TrustedHostSource)>,
        artifact_bytes: &[Vec<u8>],
        expected: &InterpreterCheckpointV1,
    ) -> Result<Self, ArtifactError> {
        let mut interpreter = Self::new(program)?;
        if let Some((bytes, source)) = ingress {
            let state = interpreter.program.state(&interpreter.current_state)?;
            let identity = interpreter.program.identity(&interpreter.current_state);
            let artifact = validator.recover_host_input(
                bytes,
                &identity,
                source,
                state.operation.input_contracts(),
            )?;
            interpreter.bind_ingress(validator, &artifact, source)?;
        }
        for bytes in artifact_bytes {
            let state = interpreter.program.state(&interpreter.current_state)?;
            let identity = interpreter.program.identity(&interpreter.current_state);
            let artifact = validator.recover_for_operation(bytes, &identity, &state.operation)?;
            interpreter.apply_artifact(validator, &artifact)?;
        }
        if interpreter.checkpoint()? != *expected {
            return Err(ArtifactError::CheckpointMismatch);
        }
        Ok(interpreter)
    }

    fn validate_current_input(&self, operation: &StateOperation) -> Result<(), ArtifactError> {
        let accepted = operation.input_contracts();
        match (accepted.is_empty(), self.last_artifact.as_ref()) {
            (true, None) => Ok(()),
            (true, Some(_)) => Err(ArtifactError::UnexpectedInputArtifact),
            (false, None) => Err(ArtifactError::MissingInputArtifact),
            (false, Some(artifact)) if accepted.contains(artifact.contract()) => Ok(()),
            (false, Some(_)) => Err(ArtifactError::InputContractMismatch),
        }
    }
}

pub struct BuiltinInvocation<'a> {
    pub identity: &'a StateIdentity,
    pub handler: BuiltinHandler,
    pub input: Option<&'a ValidatedArtifact>,
    pub output_contracts: &'a [ContractPin],
}

impl fmt::Debug for BuiltinInvocation<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BuiltinInvocation")
            .field("identity", self.identity)
            .field("handler", &self.handler)
            .field("input", &self.input)
            .field("output_contracts", &self.output_contracts)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BuiltinDraft {
    pub event: String,
    pub declared_contract: ContractPin,
    pub payload: Value,
    pub lineage_refs: Vec<ArtifactLineageRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinFailure {
    pub code: &'static str,
}

/// Implemented only by trusted Rust registries. The image selects a closed
/// [`BuiltinHandler`] value and cannot supply executable code.
pub trait BuiltinExecutor {
    fn execute(
        &mut self,
        invocation: BuiltinInvocation<'_>,
    ) -> Result<BuiltinDraft, BuiltinFailure>;
}

#[derive(Debug, Clone)]
pub struct AutoDriveOutcome {
    pub produced: Vec<ValidatedArtifact>,
    pub boundary: ExecutionBoundary,
}

/// Provider replay state used to prevent compaction across an active thinking
/// plus tool-call chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderReplayState {
    Settled {
        provider_episode_hash: ContentHash,
    },
    ActiveToolChain {
        provider_episode_hash: ContentHash,
        tool_schema_hash: ContentHash,
    },
}

/// Safe conversation-compaction boundary. It contains only validated artifact
/// and evidence references, never hidden reasoning or raw tool output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhaseCompactionBoundaryV1 {
    pub schema_version: u16,
    pub image_hash: ContentHash,
    pub workflow_id: String,
    pub completed_state_id: String,
    pub artifact_hash: ContentHash,
    pub artifact_contract: ContractPin,
    pub provider_episode_hash: ContentHash,
    pub evidence_refs: Vec<ContentHash>,
}

impl PhaseCompactionBoundaryV1 {
    pub fn seal(
        artifact: &ValidatedArtifact,
        replay_state: &ProviderReplayState,
        evidence_refs: Vec<ContentHash>,
    ) -> Result<Self, ArtifactError> {
        let ProviderReplayState::Settled {
            provider_episode_hash,
        } = replay_state
        else {
            return Err(ArtifactError::ActiveProviderChainCannotCompact);
        };
        validate_hash(provider_episode_hash, "provider episode hash")?;
        if evidence_refs.len() > MAX_EVIDENCE_REFS {
            return Err(ArtifactError::LimitExceeded("compaction evidence refs"));
        }
        let mut unique = BTreeSet::new();
        for reference in &evidence_refs {
            validate_hash(reference, "evidence ref")?;
            if !unique.insert(reference) {
                return Err(ArtifactError::DuplicateEvidenceRef);
            }
        }
        let envelope = artifact.envelope();
        Ok(Self {
            schema_version: PHASE_COMPACTION_SCHEMA_VERSION,
            image_hash: envelope.image_hash.clone(),
            workflow_id: envelope.workflow_id.clone(),
            completed_state_id: envelope.state_id.clone(),
            artifact_hash: artifact.artifact_hash()?,
            artifact_contract: envelope.declared_contract.clone(),
            provider_episode_hash: provider_episode_hash.clone(),
            evidence_refs,
        })
    }

    pub fn boundary_hash(&self) -> Result<ContentHash, ArtifactError> {
        if self.schema_version != PHASE_COMPACTION_SCHEMA_VERSION {
            return Err(ArtifactError::SchemaVersionMismatch);
        }
        Ok(ContentHash::sha256(serde_jcs::to_vec(self)?))
    }

    pub fn provider_context(&self) -> Value {
        serde_json::json!({
            "kind": "validated_phase_context",
            "image_hash": self.image_hash,
            "workflow_id": self.workflow_id,
            "completed_state_id": self.completed_state_id,
            "artifact_hash": self.artifact_hash,
            "artifact_contract": self.artifact_contract,
            "evidence_refs": self.evidence_refs,
        })
    }
}

fn validate_contract_set(
    contracts: &[ContractPin],
    allow_empty: bool,
    _subject: &'static str,
) -> Result<(), ArtifactError> {
    if (!allow_empty && contracts.is_empty()) || contracts.len() > MAX_CONTRACTS_PER_OPERATION {
        return Err(ArtifactError::LimitExceeded("operation contracts"));
    }
    let mut unique = BTreeSet::new();
    for contract in contracts {
        contract.validate()?;
        if !unique.insert(contract) {
            return Err(ArtifactError::DuplicateContract(contract.id.clone()));
        }
    }
    Ok(())
}

fn validate_lineage(lineage: &[ArtifactLineageRef]) -> Result<(), ArtifactError> {
    if lineage.len() > MAX_LINEAGE_REFS {
        return Err(ArtifactError::LimitExceeded("artifact lineage"));
    }
    let mut unique = BTreeSet::new();
    for reference in lineage {
        reference.validate()?;
        if !unique.insert(reference) {
            return Err(ArtifactError::DuplicateLineageRef);
        }
    }
    Ok(())
}

fn validate_hash(hash: &ContentHash, kind: &'static str) -> Result<(), ArtifactError> {
    ContentHash::parse(hash.as_str())
        .map(|_| ())
        .map_err(|_| ArtifactError::InvalidHash(kind))
}

fn validate_id(value: &str, kind: &'static str) -> Result<(), ArtifactError> {
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/' | b':')
        })
    {
        return Err(ArtifactError::InvalidIdentifier(kind));
    }
    Ok(())
}

fn validate_pointer(pointer: &str) -> Result<(), ArtifactError> {
    if pointer.is_empty()
        || pointer.len() > MAX_ID_BYTES
        || !pointer.starts_with('/')
        || pointer.contains("//")
        || pointer.contains('\0')
    {
        return Err(ArtifactError::InvalidJsonPointer);
    }
    Ok(())
}

fn ensure_limit(
    observed: usize,
    limit: usize,
    resource: &'static str,
) -> Result<(), ArtifactError> {
    if observed > limit {
        return Err(ArtifactError::LimitExceeded(resource));
    }
    Ok(())
}

#[derive(Serialize)]
struct HistoryHash<'a> {
    format: &'static str,
    ingress_hash: Option<&'a ContentHash>,
    artifact_hashes: &'a [ContentHash],
}

fn history_hash(
    ingress_hash: Option<&ContentHash>,
    artifact_hashes: &[ContentHash],
) -> Result<ContentHash, ArtifactError> {
    Ok(ContentHash::sha256(serde_jcs::to_vec(&HistoryHash {
        format: "krw.agent/state-artifact-history-v1",
        ingress_hash,
        artifact_hashes,
    })?))
}

#[derive(Debug, Error)]
pub enum ArtifactError {
    #[error("state artifact JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("canonical contract registry rejected the pin: {0}")]
    ContractArtifact(#[from] krw_agent_contracts::ContractArtifactError),
    #[error("typed contract rejected the artifact payload: {0}")]
    ContractValue(#[from] krw_agent_contracts::ContractValueError),
    #[error("unknown canonical contract: {0}")]
    UnknownContract(String),
    #[error("invalid identifier: {0}")]
    InvalidIdentifier(&'static str),
    #[error("invalid content hash: {0}")]
    InvalidHash(&'static str),
    #[error("invalid JSON pointer")]
    InvalidJsonPointer,
    #[error("resource limit exceeded: {0}")]
    LimitExceeded(&'static str),
    #[error("state artifact schema version mismatch")]
    SchemaVersionMismatch,
    #[error("durable state artifact bytes are not RFC 8785 canonical")]
    NonCanonicalEnvelope,
    #[error("state artifact payload byte length differs from its receipt")]
    PayloadSizeMismatch,
    #[error("state artifact payload hash differs from its receipt")]
    PayloadHashMismatch,
    #[error("state artifact image/workflow/state differs from the current state")]
    StateIdentityMismatch,
    #[error("state artifact producer is not authorized for the current operation")]
    ProducerMismatch,
    #[error("state artifact contract is not allowed by the current operation")]
    ContractNotAllowed,
    #[error("state operation is invalid: {0}")]
    InvalidOperation(&'static str),
    #[error("state is invalid: {0}")]
    InvalidState(&'static str),
    #[error("duplicate contract pin: {0}")]
    DuplicateContract(String),
    #[error("duplicate state: {0}")]
    DuplicateState(String),
    #[error("unknown state: {0}")]
    UnknownState(String),
    #[error("duplicate transition")]
    DuplicateTransition,
    #[error("duplicate lineage reference")]
    DuplicateLineageRef,
    #[error("duplicate evidence reference")]
    DuplicateEvidenceRef,
    #[error("state input artifact is missing")]
    MissingInputArtifact,
    #[error("state unexpectedly received an input artifact")]
    UnexpectedInputArtifact,
    #[error("state input artifact contract is not accepted")]
    InputContractMismatch,
    #[error("trusted host ingress is already bound")]
    IngressAlreadyBound,
    #[error("state artifact does not link the immediately preceding artifact")]
    PreviousArtifactNotLinked,
    #[error("validated artifact selects no workflow transition")]
    NoTransition,
    #[error("validated artifact ambiguously selects multiple workflow transitions")]
    AmbiguousTransition,
    #[error("workflow interpreter fuel exhausted")]
    FuelExhausted,
    #[error("state visit limit exceeded: {0}")]
    StateVisitLimit(String),
    #[error("builtin state must be driven through the closed registry")]
    BuiltinRequiresAutoDrive,
    #[error("closed builtin handler failed: {0}")]
    BuiltinFailed(&'static str),
    #[error("interpreter checkpoint differs from deterministic reconstruction")]
    CheckpointMismatch,
    #[error("active provider thinking/tool chain cannot be compacted")]
    ActiveProviderChainCannotCompact,
}

#[cfg(test)]
mod tests {
    use super::*;
    use krw_agent_contracts::{ANSWER_IR_V1, ONTOLOGY_TARGETED_QUERY_V1};

    fn hash(label: &str) -> ContentHash {
        ContentHash::sha256(label)
    }

    fn targeted_pin() -> ContractPin {
        ContractPin::canonical(ONTOLOGY_TARGETED_QUERY_V1).unwrap()
    }

    fn answer_pin() -> ContractPin {
        ContractPin::canonical(ANSWER_IR_V1).unwrap()
    }

    fn identity(state_id: &str) -> StateIdentity {
        StateIdentity {
            image_hash: hash("image"),
            workflow_id: "company_research_v2".into(),
            state_id: state_id.into(),
        }
    }

    fn model_operation() -> StateOperation {
        StateOperation::ModelDecision {
            role_id: "planner".into(),
            output_mode: ModelOutputMode::CapabilityCall,
            input_contracts: vec![targeted_pin()],
            output_contracts: vec![targeted_pin()],
        }
    }

    fn model_producer() -> ArtifactProducer {
        ArtifactProducer::Model {
            role_id: "planner".into(),
            provider_episode_hash: hash("episode"),
        }
    }

    fn lineage(input: &ValidatedArtifact) -> Vec<ArtifactLineageRef> {
        vec![ArtifactLineageRef {
            artifact_hash: input.artifact_hash().unwrap(),
            relation: LineageRelation::Input,
        }]
    }

    fn program(max_visits: u16) -> StateProgram {
        StateProgram {
            image_hash: hash("image"),
            workflow_id: "company_research_v2".into(),
            initial_state: "accepted".into(),
            states: vec![
                StateNode {
                    id: "accepted".into(),
                    operation: StateOperation::Builtin {
                        handler: BuiltinHandler::InitializeRun,
                        input_contracts: vec![],
                        output_contracts: vec![targeted_pin()],
                    },
                    max_visits,
                },
                StateNode {
                    id: "plan".into(),
                    operation: model_operation(),
                    max_visits,
                },
                StateNode {
                    id: "succeeded".into(),
                    operation: StateOperation::Terminal {
                        disposition: TerminalDisposition::Succeeded,
                        accepted_contracts: vec![targeted_pin()],
                    },
                    max_visits: 1,
                },
            ],
            transitions: vec![
                ArtifactTransition {
                    from: "accepted".into(),
                    event: Some("begin".into()),
                    guard: ArtifactGuard::Always,
                    to: "plan".into(),
                },
                ArtifactTransition {
                    from: "plan".into(),
                    event: Some("planned".into()),
                    guard: ArtifactGuard::Always,
                    to: "succeeded".into(),
                },
            ],
            max_fuel: 8,
        }
    }

    #[derive(Debug, Default)]
    struct FixtureBuiltins;

    impl BuiltinExecutor for FixtureBuiltins {
        fn execute(
            &mut self,
            invocation: BuiltinInvocation<'_>,
        ) -> Result<BuiltinDraft, BuiltinFailure> {
            assert_eq!(invocation.handler, BuiltinHandler::InitializeRun);
            assert!(invocation.input.is_none());
            Ok(BuiltinDraft {
                event: "begin".into(),
                declared_contract: targeted_pin(),
                payload: serde_json::json!({}),
                lineage_refs: vec![],
            })
        }
    }

    #[test]
    fn canonical_artifact_requires_exact_typed_contract_and_hashes() {
        let validator = ArtifactValidator::default();
        let operation = StateOperation::Builtin {
            handler: BuiltinHandler::InitializeRun,
            input_contracts: vec![],
            output_contracts: vec![targeted_pin()],
        };
        let artifact = validator
            .seal_for_operation(
                &identity("accepted"),
                &operation,
                StateArtifactDraft {
                    producer: ArtifactProducer::Builtin {
                        handler: BuiltinHandler::InitializeRun,
                    },
                    event: "begin".into(),
                    declared_contract: targeted_pin(),
                    payload: serde_json::json!({"ticker": "VG"}),
                    lineage_refs: vec![],
                },
            )
            .unwrap();
        let bytes = artifact.canonical_bytes().unwrap();
        assert_eq!(
            validator
                .recover_for_operation(&bytes, &identity("accepted"), &operation)
                .unwrap()
                .artifact_hash()
                .unwrap(),
            artifact.artifact_hash().unwrap()
        );

        let invalid = validator.seal_for_operation(
            &identity("accepted"),
            &operation,
            StateArtifactDraft {
                producer: ArtifactProducer::Builtin {
                    handler: BuiltinHandler::InitializeRun,
                },
                event: "begin".into(),
                declared_contract: targeted_pin(),
                payload: serde_json::json!({"event": "raw_fact_bypass", "facts": {"approved": true}}),
                lineage_refs: vec![],
            },
        );
        assert!(matches!(invalid, Err(ArtifactError::ContractValue(_))));
    }

    #[test]
    fn remaining_visits_is_a_read_only_admission_view() {
        let interpreter = StateInterpreter::new(program(2)).unwrap();

        // The initial state has already been entered once during interpreter
        // construction; inspecting remaining capacity must not mutate it.
        assert_eq!(interpreter.remaining_visits("accepted").unwrap(), 1);
        assert_eq!(interpreter.remaining_visits("plan").unwrap(), 2);
        assert_eq!(interpreter.remaining_visits("accepted").unwrap(), 1);
        assert!(interpreter.remaining_visits("does-not-exist").is_err());
    }

    #[test]
    fn kernel_rejection_can_only_emit_state_facts_from_a_model_state() {
        let validator = ArtifactValidator::default();
        let state_facts = ContractPin::canonical(STATE_FACTS_V1).unwrap();
        let operation = StateOperation::ModelDecision {
            role_id: "planner".into(),
            output_mode: ModelOutputMode::CapabilityOrWorkflowTransition,
            input_contracts: vec![],
            output_contracts: vec![state_facts.clone(), targeted_pin()],
        };
        let producer = ArtifactProducer::Kernel {
            reason: KernelArtifactReason::RejectedModelCapabilityProposal,
            provider_episode_hash: hash("episode"),
        };
        assert!(
            validator
                .seal_for_operation(
                    &identity("plan"),
                    &operation,
                    StateArtifactDraft {
                        producer: producer.clone(),
                        event: "proposal_contract_invalid".into(),
                        declared_contract: state_facts,
                        payload: serde_json::json!({
                            "proposal_valid": false,
                            "reason_code": "model_input_shape_invalid",
                        }),
                        lineage_refs: vec![],
                    },
                )
                .is_ok()
        );
        assert!(matches!(
            validator.seal_for_operation(
                &identity("plan"),
                &operation,
                StateArtifactDraft {
                    producer,
                    event: "proposal_contract_invalid".into(),
                    declared_contract: targeted_pin(),
                    payload: serde_json::json!({"topic": "cash generation"}),
                    lineage_refs: vec![],
                },
            ),
            Err(ArtifactError::ContractNotAllowed)
        ));
    }

    #[test]
    fn recovery_rejects_payload_producer_state_and_contract_tampering() {
        let validator = ArtifactValidator::default();
        let operation = model_operation();
        let base = validator
            .seal_for_operation(
                &identity("plan"),
                &operation,
                StateArtifactDraft {
                    producer: model_producer(),
                    event: "planned".into(),
                    declared_contract: targeted_pin(),
                    payload: serde_json::json!({"topic": "cash generation"}),
                    lineage_refs: vec![],
                },
            )
            .unwrap();

        let mut payload_tamper = base.envelope().clone();
        payload_tamper.payload = serde_json::json!({"topic": "untrusted rewrite"});
        assert!(matches!(
            validator.recover_for_operation(
                &serde_jcs::to_vec(&payload_tamper).unwrap(),
                &identity("plan"),
                &operation
            ),
            Err(ArtifactError::PayloadSizeMismatch | ArtifactError::PayloadHashMismatch)
        ));

        let mut producer_tamper = base.envelope().clone();
        producer_tamper.producer = ArtifactProducer::Builtin {
            handler: BuiltinHandler::ValidateArtifact,
        };
        assert!(matches!(
            validator.recover_for_operation(
                &serde_jcs::to_vec(&producer_tamper).unwrap(),
                &identity("plan"),
                &operation
            ),
            Err(ArtifactError::ProducerMismatch)
        ));

        let mut state_tamper = base.envelope().clone();
        state_tamper.state_id = "other".into();
        assert!(matches!(
            validator.recover_for_operation(
                &serde_jcs::to_vec(&state_tamper).unwrap(),
                &identity("plan"),
                &operation
            ),
            Err(ArtifactError::StateIdentityMismatch)
        ));

        let mut contract_tamper = base.envelope().clone();
        contract_tamper.declared_contract = answer_pin();
        assert!(matches!(
            validator.recover_for_operation(
                &serde_jcs::to_vec(&contract_tamper).unwrap(),
                &identity("plan"),
                &operation
            ),
            Err(ArtifactError::ContractNotAllowed)
        ));
    }

    #[test]
    fn builtin_auto_drive_stops_at_model_and_terminal_boundaries() {
        let validator = ArtifactValidator::default();
        let mut interpreter = StateInterpreter::new(program(2)).unwrap();
        let mut builtins = FixtureBuiltins;
        let first = interpreter.auto_drive(&validator, &mut builtins).unwrap();
        assert_eq!(first.produced.len(), 1);
        assert!(matches!(
            first.boundary,
            ExecutionBoundary::ModelDecision { ref role_id, .. } if role_id == "planner"
        ));

        let model = validator
            .seal_for_operation(
                &identity("plan"),
                &model_operation(),
                StateArtifactDraft {
                    producer: model_producer(),
                    event: "planned".into(),
                    declared_contract: targeted_pin(),
                    payload: serde_json::json!({"topic": "cash generation"}),
                    lineage_refs: lineage(&first.produced[0]),
                },
            )
            .unwrap();
        interpreter.apply_artifact(&validator, &model).unwrap();
        assert!(matches!(
            interpreter.boundary().unwrap(),
            ExecutionBoundary::Terminal {
                disposition: TerminalDisposition::Succeeded,
                ..
            }
        ));
        assert_eq!(interpreter.checkpoint().unwrap().artifact_hashes.len(), 2);
    }

    #[test]
    fn deterministic_recovery_revalidates_every_artifact_and_checkpoint() {
        let validator = ArtifactValidator::default();
        let mut interpreter = StateInterpreter::new(program(2)).unwrap();
        let mut builtins = FixtureBuiltins;
        let first = interpreter.auto_drive(&validator, &mut builtins).unwrap();
        let model = validator
            .seal_for_operation(
                &identity("plan"),
                &model_operation(),
                StateArtifactDraft {
                    producer: model_producer(),
                    event: "planned".into(),
                    declared_contract: targeted_pin(),
                    payload: serde_json::json!({"topic": "cash generation"}),
                    lineage_refs: lineage(&first.produced[0]),
                },
            )
            .unwrap();
        interpreter.apply_artifact(&validator, &model).unwrap();
        let checkpoint = interpreter.checkpoint().unwrap();
        let recovered = StateInterpreter::reconstruct(
            program(2),
            &validator,
            None,
            &[
                first.produced[0].canonical_bytes().unwrap(),
                model.canonical_bytes().unwrap(),
            ],
            &checkpoint,
        )
        .unwrap();
        assert_eq!(recovered.checkpoint().unwrap(), checkpoint);

        let mut tampered_checkpoint = checkpoint;
        tampered_checkpoint.fuel_used = 1;
        assert!(matches!(
            StateInterpreter::reconstruct(
                program(2),
                &validator,
                None,
                &[
                    first.produced[0].canonical_bytes().unwrap(),
                    model.canonical_bytes().unwrap(),
                ],
                &tampered_checkpoint,
            ),
            Err(ArtifactError::CheckpointMismatch)
        ));
    }

    #[derive(Debug, Default)]
    struct LoopBuiltin;

    impl BuiltinExecutor for LoopBuiltin {
        fn execute(
            &mut self,
            _invocation: BuiltinInvocation<'_>,
        ) -> Result<BuiltinDraft, BuiltinFailure> {
            Ok(BuiltinDraft {
                event: "again".into(),
                declared_contract: targeted_pin(),
                payload: serde_json::json!({}),
                lineage_refs: vec![],
            })
        }
    }

    #[test]
    fn deterministic_auto_drive_enforces_fuel_and_visit_bounds() {
        let loop_program = StateProgram {
            image_hash: hash("image"),
            workflow_id: "loop".into(),
            initial_state: "builtin".into(),
            states: vec![StateNode {
                id: "builtin".into(),
                operation: StateOperation::Builtin {
                    handler: BuiltinHandler::ValidateArtifact,
                    input_contracts: vec![targeted_pin()],
                    output_contracts: vec![targeted_pin()],
                },
                max_visits: 2,
            }],
            transitions: vec![ArtifactTransition {
                from: "builtin".into(),
                event: Some("again".into()),
                guard: ArtifactGuard::Always,
                to: "builtin".into(),
            }],
            max_fuel: 2,
        };
        let validator = ArtifactValidator::default();
        let ingress = validator
            .seal_host_input(
                &StateIdentity {
                    image_hash: hash("image"),
                    workflow_id: "loop".into(),
                    state_id: "builtin".into(),
                },
                TrustedHostSource::RunRequest,
                targeted_pin(),
                serde_json::json!({}),
                vec![],
            )
            .unwrap();
        let mut interpreter = StateInterpreter::new(loop_program).unwrap();
        interpreter
            .bind_ingress(&validator, &ingress, TrustedHostSource::RunRequest)
            .unwrap();
        assert!(matches!(
            interpreter.auto_drive(&validator, &mut LoopBuiltin),
            Err(ArtifactError::StateVisitLimit(ref state)) if state == "builtin"
        ));
    }

    #[test]
    fn phase_compaction_requires_settled_provider_chain_and_validated_refs() {
        let validator = ArtifactValidator::default();
        let artifact = validator
            .seal_for_operation(
                &identity("plan"),
                &model_operation(),
                StateArtifactDraft {
                    producer: model_producer(),
                    event: "planned".into(),
                    declared_contract: targeted_pin(),
                    payload: serde_json::json!({}),
                    lineage_refs: vec![],
                },
            )
            .unwrap();
        let active = ProviderReplayState::ActiveToolChain {
            provider_episode_hash: hash("episode"),
            tool_schema_hash: hash("tools"),
        };
        assert!(matches!(
            PhaseCompactionBoundaryV1::seal(&artifact, &active, vec![]),
            Err(ArtifactError::ActiveProviderChainCannotCompact)
        ));
        let settled = ProviderReplayState::Settled {
            provider_episode_hash: hash("episode"),
        };
        let boundary =
            PhaseCompactionBoundaryV1::seal(&artifact, &settled, vec![hash("evidence-1")]).unwrap();
        assert_eq!(
            boundary.provider_context()["artifact_hash"],
            Value::String(artifact.artifact_hash().unwrap().to_string())
        );
        assert_ne!(boundary.boundary_hash().unwrap(), hash("unrelated"));
    }
}
