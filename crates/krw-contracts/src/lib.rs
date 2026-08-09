//! Closed build-time contract registry for the KRW agent kernel.
//!
//! Python/Pydantic remains authoritative for ontology contracts, while the
//! read-only feed/filing contracts are source-pinned to the actual
//! `krw-ontology-front` TypeScript/Zod implementation. Production code imports
//! neither source runtime: it embeds canonical schemas, exact hashes, and
//! bounded typed validators for startup/readiness and per-action checks.

use std::collections::BTreeMap;

use krw_agent_protocol::ContentHash;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

mod front;
pub use front::*;
mod guru;
pub use guru::*;
mod product;
pub use product::*;

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/krw-ontology/v2/bindings.rs"
));

const MANIFEST_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/krw-ontology/v2/manifest.json"
));
const BUNDLE_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/krw-ontology/v2/schema-bundle.json"
));
const VECTOR_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/krw-ontology/v2/conformance-vectors.json"
));
const SEARCH_PLAN_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/krw-ontology/v2/schemas/search-plan-v2.json"
));
const RESEARCH_PROPOSAL_V4_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/kernel/v4/schemas/research-proposal-v4.json"
));
const RESEARCH_STATE_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/krw-ontology/v2/schemas/research-state-v2.json"
));
const CORRECTION_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/krw-ontology/v2/schemas/query-context-input-correction-v1.json"
));
const TARGETED_QUERY_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/krw-ontology/v2/schemas/ontology-targeted-query-v1.json"
));
const TRACE_INPUT_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/krw-ontology/v2/schemas/ontology-trace-input-v1.json"
));
const COMPANY_CONTEXT_INPUT_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/krw-ontology/v2/schemas/ontology-company-context-v1.json"
));
const COMPANY_CONTEXT_REQUEST_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/kernel/v1/schemas/company-context-request-v1.json"
));
const NORMALIZED_CAPABILITY_RESULT_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/kernel/v1/schemas/normalized-capability-result-v1.json"
));
const ANSWER_IR_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/kernel/v1/schemas/answer-ir-v1.json"
));
const FINAL_MARKDOWN_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/kernel/v1/schemas/final-markdown-v1.json"
));
const STATE_OPERATION_OUTPUT_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/kernel/v1/schemas/state-operation-output-v1.json"
));
const STATE_FACTS_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/kernel/v1/schemas/state-facts-v1.json"
));
const SKILL_LOAD_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/krw-ontology/v2/schemas/skill-load-v1.json"
));
const SKILL_CONTENT_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/krw-ontology/v2/schemas/skill-content-v1.json"
));
/// The largest canonical payload admitted by the runtime capability envelope.
/// A single filing/document field may legitimately occupy most of this budget;
/// per-field caps therefore belong to the typed contract that knows the field,
/// not to this generic preflight bound.
const MAX_CANONICAL_VALUE_BYTES: usize = 8 * 1024 * 1024;
pub const SEARCH_PLAN_V2: &str = "search-plan/v2";
/// The only model-authored research-planning ABI.  The kernel lowers this
/// small proposal into its private goal graph and the physical `SearchPlan`.
pub const RESEARCH_PROPOSAL_V4: &str = "research-proposal/v4";
pub const RESEARCH_STATE_V2: &str = "research-state/v2";
pub const QUERY_CONTEXT_INPUT_CORRECTION_V1: &str = "query-context-input-correction/v1";
/// Physical MCP input for the ontology's company-topic context endpoint.
pub const ONTOLOGY_COMPANY_CONTEXT_V1: &str = "ontology-company-context/v1";
/// Narrow model-authored request that the kernel expands into
/// [`ONTOLOGY_COMPANY_CONTEXT_V1`]. It deliberately excludes transport and
/// internal-ID controls so this optional orientation read works with both
/// current and older compatible MCP deployments.
pub const COMPANY_CONTEXT_REQUEST_V1: &str = "company-context-request/v1";
pub const ONTOLOGY_TARGETED_QUERY_V1: &str = "ontology-targeted-query/v1";
pub const ONTOLOGY_TRACE_INPUT_V1: &str = "ontology-trace-input/v1";
pub const NORMALIZED_CAPABILITY_RESULT_V1: &str = "normalized-capability-result/v1";
pub const ANSWER_IR_V1: &str = "answer-ir/v1";
/// Direct user-facing Markdown emitted after evidence collection.  The
/// `EvidenceLedger` remains kernel-owned; this contract deliberately carries
/// no model-authored claim graph or presentation wrapper.
pub const FINAL_MARKDOWN_V1: &str = "final-markdown/v1";
pub const STATE_OPERATION_OUTPUT_V1: &str = "state-operation-output/v1";
pub const STATE_FACTS_V1: &str = "state-facts/v1";
/// Model-authored skill load request: `{ "skill_id": "<catalog-id>" }`. The body is
/// resolved locally from the immutable image blob store — no MCP round trip.
pub const SKILL_LOAD_V1: &str = "skill-load/v1";
/// Skill body returned by the local `skill.load` capability handler.
pub const SKILL_CONTENT_V1: &str = "skill-content/v1";
pub const NORMALIZED_CAPABILITY_RESULT_V1_SCHEMA_SHA256: &str =
    "sha256:8c44e23d6a2e0b565b7eed9e31cfd702dc5cbd5f139a99f9b55aa903f28cc151";
pub const ANSWER_IR_V1_SCHEMA_SHA256: &str =
    "sha256:618de032c0f85bf332dc761779a2a040891aba27033d0761464d6bd212a4b634";
pub const FINAL_MARKDOWN_V1_SCHEMA_SHA256: &str =
    "sha256:8e747a3d70dc03decffa17a7f90ab1f8026bd0464322ccfb975c39f8f3a3ec8e";
pub const STATE_OPERATION_OUTPUT_V1_SCHEMA_SHA256: &str =
    "sha256:13817320290f727d29c80b8f485a75fcb717e6495236f6b650c3dd1213fa593c";
pub const STATE_FACTS_V1_SCHEMA_SHA256: &str =
    "sha256:a38950f994e8f783d759d530e6c35026491da30e0cdd03a82d42fe10e76d8ce8";
pub const RESEARCH_PROPOSAL_V4_SCHEMA_SHA256: &str =
    "sha256:b0e3e1f3a01636ac6e92d42e8cf9aab5c4bd3dd5fab35263a5bb67a5dde6ea49";
pub const COMPANY_CONTEXT_REQUEST_V1_SCHEMA_SHA256: &str =
    "sha256:d557cc2a3f534d625dccc66714d007ad7685b91aaf38dcd4ae7606ff6298e680";
pub const SKILL_LOAD_V1_SCHEMA_SHA256: &str =
    "sha256:4b19a78d66ff14ef967f10bd30789569eba0c7ceef34f6c280274042baf7de00";
pub const SKILL_CONTENT_V1_SCHEMA_SHA256: &str =
    "sha256:29e76200a214632bb215b6427f1308438f11adc6437cbad1ed466818f23ab55e";

// Canonical metric identifiers accepted by ResearchProposal v4. Generated
// from the ontology metric dictionary at build time by build.rs.
include!(concat!(env!("OUT_DIR"), "/ontology_metrics.rs"));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContractDescriptor {
    pub id: &'static str,
    pub schema_sha256: &'static str,
    pub schema: &'static [u8],
}

impl ContractDescriptor {
    /// Return the RFC 8785 representation whose hash is the contract identity.
    /// Generated exports are already canonical; kernel-owned source artifacts
    /// are canonicalized here so formatting can never alter their identity.
    pub fn canonical_schema(self) -> Result<Vec<u8>, ContractArtifactError> {
        let value: Value = serde_json::from_slice(self.schema)?;
        Ok(serde_jcs::to_vec(&value)?)
    }

    pub fn content_hash(self) -> Result<ContentHash, ContractArtifactError> {
        ContentHash::parse(self.schema_sha256).map_err(|_| {
            ContractArtifactError::InvalidManifest("descriptor contains an invalid hash")
        })
    }
}

pub fn contract(contract_id: &str) -> Option<ContractDescriptor> {
    match contract_id {
        SEARCH_PLAN_V2 => Some(ContractDescriptor {
            id: SEARCH_PLAN_V2,
            schema_sha256: SEARCH_PLAN_V2_SCHEMA_SHA256,
            schema: SEARCH_PLAN_BYTES,
        }),
        RESEARCH_PROPOSAL_V4 => Some(ContractDescriptor {
            id: RESEARCH_PROPOSAL_V4,
            schema_sha256: RESEARCH_PROPOSAL_V4_SCHEMA_SHA256,
            schema: RESEARCH_PROPOSAL_V4_BYTES,
        }),
        RESEARCH_STATE_V2 => Some(ContractDescriptor {
            id: RESEARCH_STATE_V2,
            schema_sha256: RESEARCH_STATE_V2_SCHEMA_SHA256,
            schema: RESEARCH_STATE_BYTES,
        }),
        QUERY_CONTEXT_INPUT_CORRECTION_V1 => Some(ContractDescriptor {
            id: QUERY_CONTEXT_INPUT_CORRECTION_V1,
            schema_sha256: QUERY_CONTEXT_INPUT_CORRECTION_V1_SCHEMA_SHA256,
            schema: CORRECTION_BYTES,
        }),
        ONTOLOGY_COMPANY_CONTEXT_V1 => Some(ContractDescriptor {
            id: ONTOLOGY_COMPANY_CONTEXT_V1,
            schema_sha256: ONTOLOGY_COMPANY_CONTEXT_V1_SCHEMA_SHA256,
            schema: COMPANY_CONTEXT_INPUT_BYTES,
        }),
        COMPANY_CONTEXT_REQUEST_V1 => Some(ContractDescriptor {
            id: COMPANY_CONTEXT_REQUEST_V1,
            schema_sha256: COMPANY_CONTEXT_REQUEST_V1_SCHEMA_SHA256,
            schema: COMPANY_CONTEXT_REQUEST_BYTES,
        }),
        ONTOLOGY_TARGETED_QUERY_V1 => Some(ContractDescriptor {
            id: ONTOLOGY_TARGETED_QUERY_V1,
            schema_sha256: ONTOLOGY_TARGETED_QUERY_V1_SCHEMA_SHA256,
            schema: TARGETED_QUERY_BYTES,
        }),
        ONTOLOGY_TRACE_INPUT_V1 => Some(ContractDescriptor {
            id: ONTOLOGY_TRACE_INPUT_V1,
            schema_sha256: ONTOLOGY_TRACE_INPUT_V1_SCHEMA_SHA256,
            schema: TRACE_INPUT_BYTES,
        }),
        NORMALIZED_CAPABILITY_RESULT_V1 => Some(ContractDescriptor {
            id: NORMALIZED_CAPABILITY_RESULT_V1,
            schema_sha256: NORMALIZED_CAPABILITY_RESULT_V1_SCHEMA_SHA256,
            schema: NORMALIZED_CAPABILITY_RESULT_BYTES,
        }),
        ANSWER_IR_V1 => Some(ContractDescriptor {
            id: ANSWER_IR_V1,
            schema_sha256: ANSWER_IR_V1_SCHEMA_SHA256,
            schema: ANSWER_IR_BYTES,
        }),
        FINAL_MARKDOWN_V1 => Some(ContractDescriptor {
            id: FINAL_MARKDOWN_V1,
            schema_sha256: FINAL_MARKDOWN_V1_SCHEMA_SHA256,
            schema: FINAL_MARKDOWN_BYTES,
        }),
        STATE_OPERATION_OUTPUT_V1 => Some(ContractDescriptor {
            id: STATE_OPERATION_OUTPUT_V1,
            schema_sha256: STATE_OPERATION_OUTPUT_V1_SCHEMA_SHA256,
            schema: STATE_OPERATION_OUTPUT_BYTES,
        }),
        STATE_FACTS_V1 => Some(ContractDescriptor {
            id: STATE_FACTS_V1,
            schema_sha256: STATE_FACTS_V1_SCHEMA_SHA256,
            schema: STATE_FACTS_BYTES,
        }),
        SKILL_LOAD_V1 => Some(ContractDescriptor {
            id: SKILL_LOAD_V1,
            schema_sha256: SKILL_LOAD_V1_SCHEMA_SHA256,
            schema: SKILL_LOAD_BYTES,
        }),
        SKILL_CONTENT_V1 => Some(ContractDescriptor {
            id: SKILL_CONTENT_V1,
            schema_sha256: SKILL_CONTENT_V1_SCHEMA_SHA256,
            schema: SKILL_CONTENT_BYTES,
        }),
        _ => front::contract(contract_id)
            .or_else(|| guru::contract(contract_id))
            .or_else(|| product::contract(contract_id)),
    }
}

pub fn descriptors() -> Vec<ContractDescriptor> {
    let mut descriptors = vec![
        contract(SEARCH_PLAN_V2).expect("static contract"),
        contract(RESEARCH_PROPOSAL_V4).expect("static contract"),
        contract(RESEARCH_STATE_V2).expect("static contract"),
        contract(QUERY_CONTEXT_INPUT_CORRECTION_V1).expect("static contract"),
        contract(ONTOLOGY_COMPANY_CONTEXT_V1).expect("static contract"),
        contract(COMPANY_CONTEXT_REQUEST_V1).expect("static contract"),
        contract(ONTOLOGY_TARGETED_QUERY_V1).expect("static contract"),
        contract(ONTOLOGY_TRACE_INPUT_V1).expect("static contract"),
        contract(NORMALIZED_CAPABILITY_RESULT_V1).expect("static contract"),
        contract(ANSWER_IR_V1).expect("static contract"),
        contract(FINAL_MARKDOWN_V1).expect("static contract"),
        contract(STATE_OPERATION_OUTPUT_V1).expect("static contract"),
        contract(STATE_FACTS_V1).expect("static contract"),
        contract(SKILL_LOAD_V1).expect("static contract"),
        contract(SKILL_CONTENT_V1).expect("static contract"),
    ];
    descriptors.extend(front::descriptors());
    descriptors.extend(guru::descriptors());
    descriptors.extend(product::descriptors());
    descriptors
}

pub fn verify_registry() -> Result<(), ContractArtifactError> {
    verify_embedded()?;
    front::verify_front_embedded()?;
    guru::verify_embedded()?;
    for descriptor in descriptors() {
        let canonical = descriptor.canonical_schema()?;
        // Hand-authored JSON files conventionally end in one POSIX newline.
        // That transport-only byte is not part of the RFC 8785 value; accept
        // exactly that one suffix while continuing to reject every other raw
        // byte drift before verifying the canonical content hash below.
        let source_matches_canonical = canonical == descriptor.schema
            || (descriptor.schema.last() == Some(&b'\n')
                && descriptor.schema[..descriptor.schema.len() - 1] == canonical);
        if !source_matches_canonical {
            return Err(ContractArtifactError::NonCanonical(
                descriptor.id.to_owned(),
            ));
        }
        verify_hash(descriptor.id, &canonical, descriptor.schema_sha256)?;
    }
    Ok(())
}

pub fn verify_pin(
    contract_id: &str,
    content_hash: &ContentHash,
) -> Result<(), ContractArtifactError> {
    let descriptor = contract(contract_id)
        .ok_or_else(|| ContractArtifactError::UnknownContract(contract_id.to_owned()))?;
    let expected = descriptor.content_hash()?;
    if &expected != content_hash {
        return Err(ContractArtifactError::PinnedHashMismatch {
            contract_id: contract_id.to_owned(),
            expected: expected.to_string(),
            observed: content_hash.to_string(),
        });
    }
    Ok(())
}

/// Bounded, hand-written validation for the canonical contracts on the hot
/// path. This deliberately implements only a closed opcode-free set of typed
/// checks; it is not a dynamic JSON Schema interpreter.
pub fn validate_value(contract_id: &str, value: &Value) -> Result<(), ContractValueError> {
    if front::contract(contract_id).is_some() {
        return front::validate_value(contract_id, value);
    }
    if guru::contract(contract_id).is_some() {
        return guru::validate_value(contract_id, value);
    }
    if product::contract(contract_id).is_some() {
        return product::validate_value(contract_id, value);
    }
    validate_value_bounds(value)?;
    match contract_id {
        SEARCH_PLAN_V2 => validate_search_plan(value),
        RESEARCH_PROPOSAL_V4 => validate_research_proposal_v4(value),
        RESEARCH_STATE_V2 => validate_research_state(value),
        QUERY_CONTEXT_INPUT_CORRECTION_V1 => validate_correction(value),
        ONTOLOGY_COMPANY_CONTEXT_V1 => validate_company_context_input(value),
        COMPANY_CONTEXT_REQUEST_V1 => validate_company_context_request(value),
        ONTOLOGY_TARGETED_QUERY_V1 => validate_targeted_query(value),
        ONTOLOGY_TRACE_INPUT_V1 => validate_trace_input(value),
        STATE_OPERATION_OUTPUT_V1 => validate_state_operation_output(value),
        STATE_FACTS_V1 => validate_state_facts(value),
        SKILL_LOAD_V1 => validate_skill_load(value),
        SKILL_CONTENT_V1 => validate_skill_content(value),
        NORMALIZED_CAPABILITY_RESULT_V1 | ANSWER_IR_V1 => Ok(()),
        FINAL_MARKDOWN_V1 if bounded_string(Some(value), 1, 64_000) => Ok(()),
        FINAL_MARKDOWN_V1 => Err(ContractValueError::Shape(FINAL_MARKDOWN_V1)),
        _ => Err(ContractValueError::UnknownContract(contract_id.to_owned())),
    }
}

/// Closed model-input violation families. The variant payload carries a
/// bounded diagnostic — a JSON pointer to the offending site and, for metric
/// identity failures, the bad value plus the canonical list. These fields are
/// safe to cross the provider boundary because metric identifiers are already
/// advertised in the system prompt ontology catalog; they are not user text,
/// evidence, or internal state identifiers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ResearchProposalViolation {
    ShapeInvalid {
        pointer: String,
        offending: Option<String>,
        allowed: Vec<String>,
    },
    LimitExceeded,
    SerializationInvalid,
    RequiredObjectiveMissing,
    MetricIdentityInvalid {
        offending: String,
        valid: Vec<String>,
        pointer: String,
    },
    QualitativeConceptsMissing {
        pointer: String,
    },
    QualitativePredicateMissing {
        pointer: String,
    },
}

/// The only corrective operations the kernel may suggest. It never provides
/// factual content or rewrites a research judgment on the model's behalf.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResearchProposalRepairMode {
    Replace,
    Narrow,
    Split,
}

/// A safe repair directive may cross the provider boundary and be checkpointed
/// as a kernel artifact. It contains no model terms, user text, or retrieved
/// evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchProposalRepairDirective {
    pub violation: ResearchProposalViolation,
    pub repair_mode: ResearchProposalRepairMode,
}

impl ResearchProposalViolation {
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::ShapeInvalid { .. } => "proposal_shape_invalid",
            Self::LimitExceeded => "proposal_limit_exceeded",
            Self::SerializationInvalid => "proposal_serialization_invalid",
            Self::RequiredObjectiveMissing => "proposal_required_objective_missing",
            Self::MetricIdentityInvalid { .. } => "proposal_metric_identity_invalid",
            Self::QualitativeConceptsMissing { .. } => "proposal_qualitative_concepts_missing",
            Self::QualitativePredicateMissing { .. } => "proposal_qualitative_predicate_missing",
        }
    }

    #[must_use]
    pub fn repair_mode(&self) -> ResearchProposalRepairMode {
        match self {
            Self::LimitExceeded => ResearchProposalRepairMode::Narrow,
            Self::QualitativeConceptsMissing { .. } | Self::QualitativePredicateMissing { .. } => {
                ResearchProposalRepairMode::Split
            }
            Self::ShapeInvalid { .. }
            | Self::SerializationInvalid
            | Self::RequiredObjectiveMissing
            | Self::MetricIdentityInvalid { .. } => ResearchProposalRepairMode::Replace,
        }
    }
}

impl ResearchProposalRepairDirective {
    #[must_use]
    pub fn code(&self) -> &'static str {
        self.violation.code()
    }
}

/// Return the structured, content-free repair instruction for an invalid V4
/// proposal. `None` means the proposal is accepted. This is the canonical
/// source of a model repair reason; operators and ordinary logs never receive
/// its raw terms, concepts, or user question.
#[must_use]
pub fn research_proposal_v4_repair_directive(
    value: &Value,
) -> Option<ResearchProposalRepairDirective> {
    #[allow(clippy::match_same_arms)]
    let violation = match validate_value(RESEARCH_PROPOSAL_V4, value) {
        Ok(()) => return None,
        Err(ContractValueError::UnknownContract(_)) => research_proposal_v4_shape_detail(value)
            .unwrap_or(ResearchProposalViolation::ShapeInvalid {
                pointer: "/".to_string(),
                offending: None,
                allowed: vec![],
            }),
        Err(ContractValueError::Shape(_)) => research_proposal_v4_shape_detail(value).unwrap_or(
            ResearchProposalViolation::ShapeInvalid {
                pointer: "/".to_string(),
                offending: None,
                allowed: vec![],
            },
        ),
        Err(ContractValueError::Limit(_)) => ResearchProposalViolation::LimitExceeded,
        Err(ContractValueError::Json(_)) => ResearchProposalViolation::SerializationInvalid,
        Err(ContractValueError::Semantic(_)) => research_proposal_v4_semantic_violation(value)
            .unwrap_or(ResearchProposalViolation::ShapeInvalid {
                pointer: "/".to_string(),
                offending: None,
                allowed: vec![],
            }),
    };
    Some(ResearchProposalRepairDirective {
        repair_mode: violation.repair_mode(),
        violation,
    })
}

/// Compatibility for safe, scalar diagnostic sinks. New execution code uses
/// `research_proposal_v4_repair_directive` so repair mode is not lost.
#[must_use]
pub fn research_proposal_v4_validation_code(value: &Value) -> &'static str {
    research_proposal_v4_repair_directive(value).map_or("valid", |directive| directive.code())
}

fn validate_state_operation_output(value: &Value) -> Result<(), ContractValueError> {
    let output = object(value, STATE_OPERATION_OUTPUT_V1)?;
    exact_keys(
        output,
        &["data", "data_contract", "event", "schema_version"],
        STATE_OPERATION_OUTPUT_V1,
    )?;
    if output.get("schema_version").and_then(Value::as_u64) != Some(1)
        || !bounded_string(output.get("event"), 1, 160)
    {
        return Err(ContractValueError::Shape(STATE_OPERATION_OUTPUT_V1));
    }
    let contract = output
        .get("data_contract")
        .and_then(Value::as_object)
        .ok_or(ContractValueError::Shape(STATE_OPERATION_OUTPUT_V1))?;
    exact_keys(contract, &["content_hash", "id"], STATE_OPERATION_OUTPUT_V1)?;
    let contract_id = contract
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 160)
        .ok_or(ContractValueError::Shape(STATE_OPERATION_OUTPUT_V1))?;
    if contract_id == STATE_OPERATION_OUTPUT_V1 {
        return Err(ContractValueError::Semantic(STATE_OPERATION_OUTPUT_V1));
    }
    let contract_hash = contract
        .get("content_hash")
        .and_then(Value::as_str)
        .and_then(|hash| ContentHash::parse(hash).ok())
        .ok_or(ContractValueError::Shape(STATE_OPERATION_OUTPUT_V1))?;
    if verify_pin(contract_id, &contract_hash).is_err() {
        return Err(ContractValueError::Semantic(STATE_OPERATION_OUTPUT_V1));
    }
    let data = output
        .get("data")
        .ok_or(ContractValueError::Shape(STATE_OPERATION_OUTPUT_V1))?;
    validate_value(contract_id, data)
}

fn validate_state_facts(value: &Value) -> Result<(), ContractValueError> {
    let facts = object(value, STATE_FACTS_V1)?;
    if facts.len() > 64 {
        return Err(ContractValueError::Limit(STATE_FACTS_V1));
    }
    Ok(())
}

/// Validate a `skill-load/v1` request: `{ "skill_id": "<catalog-id>" }`.
fn validate_skill_load(value: &Value) -> Result<(), ContractValueError> {
    let body = object(value, SKILL_LOAD_V1)?;
    exact_keys(body, &["skill_id"], SKILL_LOAD_V1)?;
    if !bounded_string(body.get("skill_id"), 1, 128) {
        return Err(ContractValueError::Shape(SKILL_LOAD_V1));
    }
    Ok(())
}

/// Validate a `skill-content/v1` result: `{ "skill_id": "<catalog-id>", "content": "<md>" }`.
fn validate_skill_content(value: &Value) -> Result<(), ContractValueError> {
    let body = object(value, SKILL_CONTENT_V1)?;
    exact_keys(body, &["skill_id", "content"], SKILL_CONTENT_V1)?;
    if !bounded_string(body.get("skill_id"), 1, 128)
        || !bounded_string(body.get("content"), 1, 256_000)
    {
        return Err(ContractValueError::Shape(SKILL_CONTENT_V1));
    }
    Ok(())
}

fn validate_value_bounds(value: &Value) -> Result<(), ContractValueError> {
    let bytes = serde_jcs::to_vec(value).map_err(ContractValueError::Json)?;
    if bytes.len() > MAX_CANONICAL_VALUE_BYTES {
        return Err(ContractValueError::Limit("canonical bytes"));
    }
    let mut stack = vec![(value, 0_u8)];
    let mut visited = 0_usize;
    while let Some((current, depth)) = stack.pop() {
        visited = visited
            .checked_add(1)
            .ok_or(ContractValueError::Limit("value items"))?;
        if visited > 16_384 || depth > 32 {
            return Err(ContractValueError::Limit("value depth/items"));
        }
        match current {
            // The canonical-byte cap above already bounds an opaque capability
            // payload even when it is one large text field. Typed contracts
            // (SearchPlan, front/Guru records, product artifacts) add their
            // narrower field-specific limits below this common guard.
            Value::String(text) if text.contains('\0') => {
                return Err(ContractValueError::Limit("string bytes"));
            }
            Value::Array(values) => {
                stack.extend(values.iter().map(|value| (value, depth.saturating_add(1))));
            }
            Value::Object(values) => {
                stack.extend(
                    values
                        .values()
                        .map(|value| (value, depth.saturating_add(1))),
                );
            }
            _ => {}
        }
    }
    Ok(())
}

fn object<'a>(
    value: &'a Value,
    contract: &'static str,
) -> Result<&'a serde_json::Map<String, Value>, ContractValueError> {
    value.as_object().ok_or(ContractValueError::Shape(contract))
}

fn exact_keys(
    object: &serde_json::Map<String, Value>,
    allowed: &[&str],
    contract: &'static str,
) -> Result<(), ContractValueError> {
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(ContractValueError::Shape(contract));
    }
    Ok(())
}

fn bounded_string(value: Option<&Value>, min: usize, max: usize) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|value| value.len() >= min && value.len() <= max && !value.contains('\0'))
}

fn optional_string(value: Option<&Value>, max: usize) -> bool {
    value.is_none_or(|value| value.is_null() || bounded_string(Some(value), 1, max))
}

fn string_array(value: Option<&Value>, max_items: usize, max_len: usize) -> bool {
    value.is_none_or(|value| {
        value.as_array().is_some_and(|values| {
            values.len() <= max_items
                && values
                    .iter()
                    .all(|value| bounded_string(Some(value), 1, max_len))
        })
    })
}

fn integer_range(value: Option<&Value>, min: u64, max: u64) -> bool {
    value.is_none_or(|value| {
        value
            .as_u64()
            .is_some_and(|value| (min..=max).contains(&value))
    })
}

fn valid_slug(value: &str, lowercase_first: bool) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    let first_ok = if lowercase_first {
        first.is_ascii_lowercase()
    } else {
        first.is_ascii_alphanumeric()
    };
    first_ok && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

fn validate_search_plan(value: &Value) -> Result<(), ContractValueError> {
    let plan = object(value, SEARCH_PLAN_V2)?;
    exact_keys(
        plan,
        &[
            "answer_scope",
            "clauses",
            "comparison_axes",
            "document_types",
            "intent",
            "limit_results",
            "limit_tickers",
            "periods",
            "question",
            "tickers",
            "uncertainty",
            "universe",
        ],
        SEARCH_PLAN_V2,
    )?;
    if !bounded_string(plan.get("question"), 2, 4_000)
        || !bounded_string(plan.get("intent"), 1, 64)
        || !plan
            .get("intent")
            .and_then(Value::as_str)
            .is_some_and(|value| valid_slug(value, true))
        || !integer_range(plan.get("limit_results"), 1, 50)
        || !integer_range(plan.get("limit_tickers"), 1, 50)
        || !string_array(plan.get("tickers"), 50, 32)
        || !string_array(plan.get("periods"), 40, 128)
        || !string_array(plan.get("document_types"), 16, 128)
    {
        return Err(ContractValueError::Shape(SEARCH_PLAN_V2));
    }
    if plan
        .get("answer_scope")
        .is_some_and(|value| !matches!(value.as_str(), Some("direct" | "supporting_context_only")))
        || plan
            .get("uncertainty")
            .is_some_and(|value| !matches!(value.as_str(), Some("low" | "medium" | "high")))
        || plan
            .get("universe")
            .is_some_and(|value| !value.is_null() && value.as_str() != Some("covered"))
    {
        return Err(ContractValueError::Shape(SEARCH_PLAN_V2));
    }
    let clauses = plan
        .get("clauses")
        .and_then(Value::as_array)
        .filter(|values| (1..=12).contains(&values.len()))
        .ok_or(ContractValueError::Shape(SEARCH_PLAN_V2))?;
    let plan_tickers = plan
        .get("tickers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    let mut clause_ids = std::collections::BTreeSet::new();
    for clause in clauses {
        let clause = object(clause, SEARCH_PLAN_V2)?;
        exact_keys(
            clause,
            &[
                "calculation_window",
                "clause_id",
                "directness",
                "metric_dimensions",
                "metric_scope",
                "metrics",
                "object_types",
                "required",
                "required_concepts",
                "required_predicates",
                "retrieval_query",
                "tickers",
            ],
            SEARCH_PLAN_V2,
        )?;
        let clause_id = clause
            .get("clause_id")
            .and_then(Value::as_str)
            .filter(|value| value.len() <= 64 && valid_slug(value, false));
        if clause_id.is_none()
            || !clause_ids.insert(clause_id.expect("checked"))
            || !bounded_string(clause.get("retrieval_query"), 2, 1_000)
            || !string_array(clause.get("tickers"), 50, 32)
            || !string_array(clause.get("metrics"), 32, 128)
            || !string_array(clause.get("object_types"), 32, 128)
            || !string_array(clause.get("required_concepts"), 32, 160)
            || !string_array(clause.get("required_predicates"), 16, 160)
            || !string_array(clause.get("metric_dimensions"), 16, 160)
            || clause
                .get("required")
                .is_some_and(|value| !value.is_boolean())
            || clause.get("directness").is_some_and(|value| {
                !matches!(
                    value.as_str(),
                    Some("any" | "direct_preferred" | "direct_required")
                )
            })
            || clause.get("metric_scope").is_some_and(|value| {
                !matches!(
                    value.as_str(),
                    Some("company_total" | "dimensioned" | "any")
                )
            })
            || clause.get("calculation_window").is_some_and(|value| {
                !value.is_null()
                    && !matches!(
                        value.as_str(),
                        Some("period_over_period" | "year_over_year")
                    )
            })
        {
            return Err(ContractValueError::Shape(SEARCH_PLAN_V2));
        }
        if !plan_tickers.is_empty()
            && clause
                .get("tickers")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .any(|ticker| !plan_tickers.contains(ticker))
        {
            return Err(ContractValueError::Semantic(SEARCH_PLAN_V2));
        }
    }
    Ok(())
}

/// Validate the V4 model-authored semantic proposal. The model chooses only
/// the evidence meaning; the kernel owns scopes, IDs, plan limits and physical
/// MCP encoding. A tagged goal prevents unrelated temporal/numeric/qualitative
/// fields from having to agree by convention.
fn validate_research_proposal_v4(value: &Value) -> Result<(), ContractValueError> {
    let proposal = object(value, RESEARCH_PROPOSAL_V4)?;
    exact_keys(
        proposal,
        &[
            "answer_scope",
            "document_types",
            "intent",
            "objectives",
            "periods",
            "uncertainty",
        ],
        RESEARCH_PROPOSAL_V4,
    )?;
    let objectives = proposal
        .get("objectives")
        .and_then(Value::as_array)
        .filter(|values| (1..=12).contains(&values.len()))
        .ok_or(ContractValueError::Shape(RESEARCH_PROPOSAL_V4))?;
    if !bounded_string(proposal.get("intent"), 1, 64)
        || !proposal
            .get("intent")
            .and_then(Value::as_str)
            .is_some_and(|value| valid_slug(value, true))
        || !matches!(proposal.get("document_types"), Some(Value::Array(_)))
        || !string_array(proposal.get("document_types"), 16, 128)
        || !matches!(proposal.get("periods"), Some(Value::Array(_)))
        || !string_array(proposal.get("periods"), 40, 128)
        || proposal
            .get("answer_scope")
            .and_then(Value::as_str)
            .is_none_or(|value| !matches!(value, "direct" | "supporting_context_only"))
        || proposal
            .get("uncertainty")
            .and_then(Value::as_str)
            .is_none_or(|value| !matches!(value, "low" | "medium" | "high"))
    {
        return Err(ContractValueError::Shape(RESEARCH_PROPOSAL_V4));
    }

    let mut has_required_objective = false;
    for objective in objectives {
        let objective = object(objective, RESEARCH_PROPOSAL_V4)?;
        exact_keys(
            objective,
            &[
                "alternatives",
                "directness",
                "goal",
                "object_types",
                "priority",
            ],
            RESEARCH_PROPOSAL_V4,
        )?;
        let required_now = match objective.get("priority").and_then(Value::as_str) {
            Some("required") => true,
            Some("deferred") => false,
            _ => return Err(ContractValueError::Shape(RESEARCH_PROPOSAL_V4)),
        };
        has_required_objective |= required_now;
        if !matches!(
            objective.get("directness").and_then(Value::as_str),
            Some("any" | "direct_preferred" | "direct_required")
        ) || !matches!(objective.get("object_types"), Some(Value::Array(_)))
            || !string_array(objective.get("object_types"), 32, 128)
        {
            return Err(ContractValueError::Shape(RESEARCH_PROPOSAL_V4));
        }
        validate_proposal_alternatives(objective.get("alternatives"))?;
        validate_research_proposal_goal(objective.get("goal"))?;
    }
    if !has_required_objective {
        return Err(ContractValueError::Semantic(RESEARCH_PROPOSAL_V4));
    }
    Ok(())
}

fn validate_proposal_alternatives(value: Option<&Value>) -> Result<(), ContractValueError> {
    let alternatives = value
        .and_then(Value::as_array)
        .filter(|values| (1..=4).contains(&values.len()))
        .ok_or(ContractValueError::Shape(RESEARCH_PROPOSAL_V4))?;
    for alternative in alternatives {
        let alternative = object(alternative, RESEARCH_PROPOSAL_V4)?;
        exact_keys(alternative, &["terms"], RESEARCH_PROPOSAL_V4)?;
        let terms = alternative
            .get("terms")
            .and_then(Value::as_array)
            .filter(|values| (1..=16).contains(&values.len()))
            .ok_or(ContractValueError::Shape(RESEARCH_PROPOSAL_V4))?;
        let joined_terms_bytes = terms
            .iter()
            .filter_map(Value::as_str)
            .map(str::len)
            .sum::<usize>()
            .saturating_add(terms.len().saturating_sub(1));
        if !string_array(alternative.get("terms"), 16, 160)
            || !(2..=1_000).contains(&joined_terms_bytes)
        {
            return Err(ContractValueError::Shape(RESEARCH_PROPOSAL_V4));
        }
    }
    Ok(())
}

fn validate_research_proposal_goal(value: Option<&Value>) -> Result<(), ContractValueError> {
    let goal = object(
        value.ok_or(ContractValueError::Shape(RESEARCH_PROPOSAL_V4))?,
        RESEARCH_PROPOSAL_V4,
    )?;
    let kind = goal
        .get("kind")
        .and_then(Value::as_str)
        .ok_or(ContractValueError::Shape(RESEARCH_PROPOSAL_V4))?;
    match kind {
        "metric_observation" | "metric_time_series" | "metric_difference" => {
            exact_keys(
                goal,
                &["kind", "metric", "metric_dimensions"],
                RESEARCH_PROPOSAL_V4,
            )?;
            validate_metric_goal(goal)
        }
        "metric_change" => {
            exact_keys(
                goal,
                &["change", "kind", "metric", "metric_dimensions", "window"],
                RESEARCH_PROPOSAL_V4,
            )?;
            validate_metric_goal(goal)?;
            if !matches!(
                goal.get("change").and_then(Value::as_str),
                Some("absolute_change" | "growth_rate")
            ) || !matches!(
                goal.get("window").and_then(Value::as_str),
                Some("period_over_period" | "year_over_year")
            ) {
                return Err(ContractValueError::Shape(RESEARCH_PROPOSAL_V4));
            }
            Ok(())
        }
        "qualitative_evidence" => {
            exact_keys(
                goal,
                &["concepts", "kind", "predicates"],
                RESEARCH_PROPOSAL_V4,
            )?;
            let concepts = goal
                .get("concepts")
                .and_then(Value::as_array)
                .ok_or(ContractValueError::Shape(RESEARCH_PROPOSAL_V4))?;
            let predicates = goal
                .get("predicates")
                .and_then(Value::as_array)
                .ok_or(ContractValueError::Shape(RESEARCH_PROPOSAL_V4))?;
            if !string_array(goal.get("concepts"), 16, 160)
                || !string_array(goal.get("predicates"), 16, 160)
            {
                return Err(ContractValueError::Shape(RESEARCH_PROPOSAL_V4));
            }
            if concepts.is_empty() || (concepts.len() > 1 && predicates.is_empty()) {
                return Err(ContractValueError::Semantic(RESEARCH_PROPOSAL_V4));
            }
            Ok(())
        }
        _ => Err(ContractValueError::Shape(RESEARCH_PROPOSAL_V4)),
    }
}

fn validate_metric_goal(goal: &serde_json::Map<String, Value>) -> Result<(), ContractValueError> {
    if !goal
        .get("metric")
        .and_then(Value::as_str)
        .is_some_and(|metric| RESEARCH_PROPOSAL_V4_METRICS.contains(&metric))
        || !matches!(goal.get("metric_dimensions"), Some(Value::Array(_)))
        || !string_array(goal.get("metric_dimensions"), 16, 160)
    {
        return Err(ContractValueError::Semantic(RESEARCH_PROPOSAL_V4));
    }
    Ok(())
}

fn research_proposal_v4_semantic_violation(value: &Value) -> Option<ResearchProposalViolation> {
    let proposal = value.as_object()?;
    let objectives = proposal.get("objectives")?.as_array()?;
    let mut has_required_objective = false;
    for (idx, objective) in objectives.iter().enumerate() {
        let objective = objective.as_object()?;
        has_required_objective |= objective.get("priority")?.as_str()? == "required";
        let goal = objective.get("goal")?.as_object()?;
        let pointer = format!("/objectives/{idx}/goal");
        match goal.get("kind")?.as_str()? {
            "metric_observation" | "metric_time_series" | "metric_change" | "metric_difference" => {
                let metric_val = goal.get("metric").and_then(Value::as_str).unwrap_or("");
                if !RESEARCH_PROPOSAL_V4_METRICS.contains(&metric_val) {
                    return Some(ResearchProposalViolation::MetricIdentityInvalid {
                        offending: metric_val.to_string(),
                        valid: RESEARCH_PROPOSAL_V4_METRICS
                            .iter()
                            .map(|s| (*s).to_string())
                            .collect(),
                        pointer: format!("{pointer}/metric"),
                    });
                }
            }
            "qualitative_evidence" => {
                let concepts = goal.get("concepts")?.as_array()?;
                let predicates = goal.get("predicates")?.as_array()?;
                if concepts.is_empty() {
                    return Some(ResearchProposalViolation::QualitativeConceptsMissing {
                        pointer: format!("{pointer}/concepts"),
                    });
                }
                if concepts.len() > 1 && predicates.is_empty() {
                    return Some(ResearchProposalViolation::QualitativePredicateMissing {
                        pointer: format!("{pointer}/predicates"),
                    });
                }
            }
            _ => {
                return Some(ResearchProposalViolation::ShapeInvalid {
                    pointer: format!("{pointer}/kind"),
                    offending: goal.get("kind").and_then(Value::as_str).map(String::from),
                    allowed: vec![
                        "metric_observation".into(),
                        "metric_time_series".into(),
                        "metric_change".into(),
                        "metric_difference".into(),
                        "qualitative_evidence".into(),
                    ],
                });
            }
        }
    }
    (!has_required_objective).then_some(ResearchProposalViolation::RequiredObjectiveMissing)
}

/// Re-derive a precise `ShapeInvalid` with offending value and allowed set by
/// re-walking the proposal JSON. This runs only when the main validator
/// returned `Shape` (structural failure), recovering information the
/// `ContractValueError::Shape(&'static str)` type discards. All values
/// surfaced here (keys, enum tokens) are schema-public.
fn research_proposal_v4_shape_detail(value: &Value) -> Option<ResearchProposalViolation> {
    let proposal = value.as_object()?;
    let allowed_top: &[&str] = &[
        "answer_scope",
        "document_types",
        "intent",
        "objectives",
        "periods",
        "uncertainty",
    ];
    if let Some(violation) = exact_shape_keys(proposal, allowed_top, "/") {
        return Some(violation);
    }
    // Check enum fields at the top level.
    for (field, allowed_vals) in [
        ("answer_scope", vec!["direct", "supporting_context_only"]),
        ("uncertainty", vec!["low", "medium", "high"]),
    ] {
        if let Some(actual) = proposal.get(field).and_then(Value::as_str)
            && !allowed_vals.contains(&actual)
        {
            return Some(ResearchProposalViolation::ShapeInvalid {
                pointer: format!("/{field}"),
                offending: Some(actual.to_string()),
                allowed: allowed_vals.iter().map(|s| (*s).to_string()).collect(),
            });
        }
    }
    // Check every nested object boundary as well. The generic canonical
    // validator intentionally erases this location, but a one-shot provider
    // repair needs to know whether the problem is its objective, alternative,
    // or tagged goal shape. These paths contain only schema positions.
    let objectives = proposal.get("objectives")?.as_array()?;
    for (idx, objective) in objectives.iter().enumerate() {
        let obj = objective.as_object()?;
        let base = format!("/objectives/{idx}");
        if let Some(violation) = exact_shape_keys(
            obj,
            &[
                "alternatives",
                "directness",
                "goal",
                "object_types",
                "priority",
            ],
            &base,
        ) {
            return Some(violation);
        }
        for (field, allowed_vals) in [
            ("priority", vec!["required", "deferred"]),
            (
                "directness",
                vec!["any", "direct_preferred", "direct_required"],
            ),
        ] {
            if let Some(actual) = obj.get(field).and_then(Value::as_str)
                && !allowed_vals.contains(&actual)
            {
                return Some(ResearchProposalViolation::ShapeInvalid {
                    pointer: format!("{base}/{field}"),
                    offending: Some(actual.to_string()),
                    allowed: allowed_vals.iter().map(|s| (*s).to_string()).collect(),
                });
            }
        }
        let alternatives = obj.get("alternatives")?.as_array()?;
        for (alternative_index, alternative) in alternatives.iter().enumerate() {
            let alternative = alternative.as_object()?;
            if let Some(violation) = exact_shape_keys(
                alternative,
                &["terms"],
                &format!("{base}/alternatives/{alternative_index}"),
            ) {
                return Some(violation);
            }
        }
        // Check goal.kind and goal enums.
        if let Some(goal) = obj.get("goal").and_then(Value::as_object)
            && let Some(kind) = goal.get("kind").and_then(Value::as_str)
        {
            let known_kinds = [
                "metric_observation",
                "metric_time_series",
                "metric_change",
                "metric_difference",
                "qualitative_evidence",
            ];
            if !known_kinds.contains(&kind) {
                return Some(ResearchProposalViolation::ShapeInvalid {
                    pointer: format!("{base}/goal/kind"),
                    offending: Some(kind.to_string()),
                    allowed: known_kinds.iter().map(|s| (*s).to_string()).collect(),
                });
            }
            let allowed_goal_keys: &[&str] = match kind {
                "metric_observation" | "metric_time_series" | "metric_difference" => {
                    &["kind", "metric", "metric_dimensions"]
                }
                "metric_change" => &["change", "kind", "metric", "metric_dimensions", "window"],
                "qualitative_evidence" => &["concepts", "kind", "predicates"],
                _ => unreachable!("unknown goal kind returned above"),
            };
            if let Some(violation) =
                exact_shape_keys(goal, allowed_goal_keys, &format!("{base}/goal"))
            {
                return Some(violation);
            }
            // Check metric_change enums.
            if kind == "metric_change" {
                for (field, allowed_vals) in [
                    ("change", vec!["absolute_change", "growth_rate"]),
                    (
                        "window",
                        vec![
                            "year_over_year",
                            "quarter_over_quarter",
                            "sequential",
                            "period_over_period",
                        ],
                    ),
                ] {
                    if let Some(actual) = goal.get(field).and_then(Value::as_str)
                        && !allowed_vals.contains(&actual)
                    {
                        return Some(ResearchProposalViolation::ShapeInvalid {
                            pointer: format!("{base}/goal/{field}"),
                            offending: Some(actual.to_string()),
                            allowed: allowed_vals.iter().map(|s| (*s).to_string()).collect(),
                        });
                    }
                }
            }
        }
    }
    // Fallback: shape error we couldn't localise.
    Some(ResearchProposalViolation::ShapeInvalid {
        pointer: "/".to_string(),
        offending: None,
        allowed: allowed_top.iter().map(|s| (*s).to_string()).collect(),
    })
}

fn exact_shape_keys(
    object: &serde_json::Map<String, Value>,
    allowed: &[&str],
    pointer: &str,
) -> Option<ResearchProposalViolation> {
    (object.len() != allowed.len()
        || allowed.iter().any(|key| !object.contains_key(*key))
        || object.keys().any(|key| !allowed.contains(&key.as_str())))
    .then(|| ResearchProposalViolation::ShapeInvalid {
        pointer: pointer.to_string(),
        offending: None,
        allowed: allowed.iter().map(|value| (*value).to_string()).collect(),
    })
}

fn validate_targeted_query(value: &Value) -> Result<(), ContractValueError> {
    let query = object(value, ONTOLOGY_TARGETED_QUERY_V1)?;
    exact_keys(
        query,
        &[
            "answer_candidate_only",
            "document_type",
            "document_types",
            "group_by",
            "include_rejected",
            "limit",
            "limit_groups",
            "limit_per_group",
            "object_type",
            "object_types",
            "offset",
            "period",
            "periods",
            "response_detail",
            "response_format",
            "ticker",
            "tickers",
            "topic",
        ],
        ONTOLOGY_TARGETED_QUERY_V1,
    )?;
    if !optional_string(query.get("ticker"), 32)
        || !optional_string(query.get("topic"), 512)
        || !optional_string(query.get("period"), 128)
        || !optional_string(query.get("document_type"), 128)
        || !optional_string(query.get("object_type"), 128)
        || !optional_string(query.get("group_by"), 128)
        || !string_array(
            query.get("tickers").filter(|value| !value.is_null()),
            50,
            32,
        )
        || !string_array(
            query.get("periods").filter(|value| !value.is_null()),
            40,
            128,
        )
        || !string_array(
            query.get("document_types").filter(|value| !value.is_null()),
            16,
            128,
        )
        || !string_array(
            query.get("object_types").filter(|value| !value.is_null()),
            32,
            128,
        )
        || !integer_range(query.get("limit"), 1, 100)
        || !integer_range(query.get("limit_groups"), 1, 100)
        || !integer_range(query.get("limit_per_group"), 1, 100)
        || !integer_range(query.get("offset"), 0, 10_000)
        || query
            .get("answer_candidate_only")
            .is_some_and(|value| !value.is_boolean())
        || query
            .get("include_rejected")
            .is_some_and(|value| !value.is_boolean())
        || query
            .get("response_format")
            .is_some_and(|value| value.as_str() != Some("json"))
        || query.get("response_detail").is_some_and(|value| {
            !matches!(
                value.as_str(),
                Some("ids_only" | "compact" | "ticker_summary" | "full")
            )
        })
    {
        return Err(ContractValueError::Shape(ONTOLOGY_TARGETED_QUERY_V1));
    }
    Ok(())
}

/// Validate the physical company-context MCP request. The model never sees
/// this broader surface directly: the kernel-owned request derivation pins
/// `include_internal_ids=false` and omits `response_format` for compatibility.
fn validate_company_context_input(value: &Value) -> Result<(), ContractValueError> {
    let request = object(value, ONTOLOGY_COMPANY_CONTEXT_V1)?;
    exact_keys(
        request,
        &[
            "document_types",
            "include_internal_ids",
            "limit_topics",
            "periods",
            "response_format",
            "ticker",
        ],
        ONTOLOGY_COMPANY_CONTEXT_V1,
    )?;
    if !bounded_string(request.get("ticker"), 1, 32)
        || !string_array(
            request.get("document_types").filter(|value| !value.is_null()),
            8,
            128,
        )
        || !string_array(
            request.get("periods").filter(|value| !value.is_null()),
            8,
            128,
        )
        || !integer_range(request.get("limit_topics"), 1, 8)
        || request
            .get("include_internal_ids")
            .is_some_and(|value| !value.is_boolean())
        // The normalized runtime can only accept JSON object results. The
        // model request surface omits this field entirely, but reject a
        // direct physical invocation that would ask the server for Markdown.
        || request
            .get("response_format")
            .is_some_and(|value| value.as_str() != Some("json"))
    {
        return Err(ContractValueError::Shape(ONTOLOGY_COMPANY_CONTEXT_V1));
    }
    Ok(())
}

/// Validate the small model-authored company-context request. Keeping this
/// separate from the physical MCP contract prevents optional server controls
/// from becoming new LLM failure points.
fn validate_company_context_request(value: &Value) -> Result<(), ContractValueError> {
    let request = object(value, COMPANY_CONTEXT_REQUEST_V1)?;
    exact_keys(
        request,
        &["document_types", "limit_topics", "periods", "ticker"],
        COMPANY_CONTEXT_REQUEST_V1,
    )?;
    if !bounded_string(request.get("ticker"), 1, 32)
        || !string_array(
            request
                .get("document_types")
                .filter(|value| !value.is_null()),
            8,
            128,
        )
        || !string_array(
            request.get("periods").filter(|value| !value.is_null()),
            8,
            128,
        )
        || !integer_range(request.get("limit_topics"), 1, 8)
    {
        return Err(ContractValueError::Shape(COMPANY_CONTEXT_REQUEST_V1));
    }
    Ok(())
}

fn validate_trace_input(value: &Value) -> Result<(), ContractValueError> {
    let trace = object(value, ONTOLOGY_TRACE_INPUT_V1)?;
    exact_keys(
        trace,
        &["object_id", "response_format", "ticker"],
        ONTOLOGY_TRACE_INPUT_V1,
    )?;
    if !bounded_string(trace.get("object_id"), 1, 256)
        || !optional_string(trace.get("ticker"), 32)
        || trace
            .get("response_format")
            .is_some_and(|value| value.as_str() != Some("json"))
    {
        return Err(ContractValueError::Shape(ONTOLOGY_TRACE_INPUT_V1));
    }
    Ok(())
}

fn validate_correction(value: &Value) -> Result<(), ContractValueError> {
    let correction = object(value, QUERY_CONTEXT_INPUT_CORRECTION_V1)?;
    exact_keys(
        correction,
        &[
            "allowed_next_tools",
            "code",
            "message",
            "status",
            "violations",
        ],
        QUERY_CONTEXT_INPUT_CORRECTION_V1,
    )?;
    let violations = correction
        .get("violations")
        .and_then(Value::as_array)
        .filter(|values| (1..=32).contains(&values.len()));
    if !bounded_string(correction.get("message"), 1, 2_000)
        || violations.is_none()
        || correction
            .get("code")
            .is_some_and(|value| value.as_str() != Some("search_plan_validation_failed"))
        || correction
            .get("status")
            .is_some_and(|value| value.as_str() != Some("input_correction_required"))
        || correction.get("allowed_next_tools").is_some_and(|value| {
            value.as_array().is_none_or(|values| {
                values.as_slice() != [Value::String("krw_ontology_query_context".into())]
            })
        })
    {
        return Err(ContractValueError::Shape(QUERY_CONTEXT_INPUT_CORRECTION_V1));
    }
    for violation in violations.expect("checked") {
        let violation = object(violation, QUERY_CONTEXT_INPUT_CORRECTION_V1)?;
        exact_keys(
            violation,
            &[
                "field",
                "message",
                "required_change",
                "required_literals",
                "rule",
            ],
            QUERY_CONTEXT_INPUT_CORRECTION_V1,
        )?;
        if !bounded_string(violation.get("field"), 1, 512)
            || !bounded_string(violation.get("message"), 1, 2_000)
            || !bounded_string(violation.get("required_change"), 1, 2_000)
            || !string_array(violation.get("required_literals"), 32, 2_000)
            || !matches!(
                violation.get("rule").and_then(Value::as_str),
                Some(
                    "missing_literal_term"
                        | "mixed_metric_and_qualitative_clause"
                        | "missing_calculation_window"
                        | "invalid_search_plan"
                )
            )
        {
            return Err(ContractValueError::Shape(QUERY_CONTEXT_INPUT_CORRECTION_V1));
        }
    }
    Ok(())
}

fn validate_research_state(value: &Value) -> Result<(), ContractValueError> {
    let state = object(value, RESEARCH_STATE_V2)?;
    exact_keys(
        state,
        &[
            "answerability",
            "calculation_coverage",
            "clause_coverage",
            "computed_values",
            "continuation",
            "contract_version",
            "evidence_units",
            "metric_series_pack",
            "missing_parts",
            "plan",
            "recommended_actions",
            "release_id",
            "resolved_scope",
            "source_anchors",
            "warnings",
        ],
        RESEARCH_STATE_V2,
    )?;
    if state.get("contract_version").is_some_and(|value| value.as_str() != Some("research-state/v2"))
        || !matches!(state.get("plan"), Some(Value::Object(_)))
        || !matches!(state.get("resolved_scope"), Some(Value::Object(_)))
        || !matches!(state.get("answerability"), Some(Value::Object(_)))
        || !matches!(state.get("clause_coverage"), Some(Value::Array(values)) if values.len() <= 256)
        || !matches!(state.get("evidence_units"), Some(Value::Array(values)) if values.len() <= 512)
        || state.get("computed_values").is_some_and(|value| !matches!(value, Value::Array(values) if values.len() <= 256))
        || state.get("warnings").is_some_and(|value| !matches!(value, Value::Array(values) if values.len() <= 256 && values.iter().all(|item| bounded_string(Some(item), 1, 2_000))))
    {
        return Err(ContractValueError::Shape(RESEARCH_STATE_V2));
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum ContractValueError {
    #[error("unsupported canonical contract: {0}")]
    UnknownContract(String),
    #[error("canonical contract has invalid shape: {0}")]
    Shape(&'static str),
    #[error("canonical contract failed semantic validation: {0}")]
    Semantic(&'static str),
    #[error("canonical contract exceeds bound: {0}")]
    Limit(&'static str),
    #[error("canonical contract JSON could not be serialized: {0}")]
    Json(serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedBundle {
    pub manifest_sha256: String,
    pub authority_sha256: String,
    pub schema_bundle_sha256: String,
    pub conformance_vectors_sha256: String,
    pub contract_count: usize,
}

/// Verify every embedded artifact, cross-file reference, and JCS byte form.
///
/// This is intended for tests and one-time startup/readiness checks, not the
/// per-action hot path.
pub fn verify_embedded() -> Result<VerifiedBundle, ContractArtifactError> {
    verify_json("manifest.json", MANIFEST_BYTES)?;
    verify_hash("manifest.json", MANIFEST_BYTES, GENERATED_MANIFEST_SHA256)?;
    let manifest: Manifest = serde_json::from_slice(MANIFEST_BYTES)?;
    if manifest.format != "krw-agent-contract-export/v1" {
        return Err(ContractArtifactError::InvalidManifest(
            "unsupported manifest format",
        ));
    }

    verify_json(&manifest.schema_bundle.path, BUNDLE_BYTES)?;
    verify_hash(
        &manifest.schema_bundle.path,
        BUNDLE_BYTES,
        &manifest.schema_bundle.sha256,
    )?;
    verify_json(&manifest.conformance_vectors.path, VECTOR_BYTES)?;
    verify_hash(
        &manifest.conformance_vectors.path,
        VECTOR_BYTES,
        &manifest.conformance_vectors.sha256,
    )?;
    verify_hash(
        "manifest.authority",
        &serde_jcs::to_vec(&manifest.authority)?,
        &manifest.authority_sha256,
    )?;

    let bundle: SchemaBundle = serde_json::from_slice(BUNDLE_BYTES)?;
    if bundle.format != "krw-agent-contract-export/v1"
        || bundle.json_schema_dialect != "https://json-schema.org/draft/2020-12/schema"
        || bundle.notice.is_empty()
    {
        return Err(ContractArtifactError::InvalidManifest(
            "invalid schema bundle metadata",
        ));
    }
    if bundle.authority_sha256 != manifest.authority_sha256 {
        return Err(ContractArtifactError::InvalidManifest(
            "schema bundle authority hash differs from manifest",
        ));
    }
    let vectors: ConformanceVectors = serde_json::from_slice(VECTOR_BYTES)?;
    if vectors.format != "krw-agent-contract-conformance/v1" || vectors.vectors.is_empty() {
        return Err(ContractArtifactError::InvalidManifest(
            "invalid conformance vector metadata",
        ));
    }
    if vectors.authority_sha256 != manifest.authority_sha256 {
        return Err(ContractArtifactError::InvalidManifest(
            "conformance vector authority hash differs from manifest",
        ));
    }

    for descriptor in [
        contract(SEARCH_PLAN_V2).expect("static contract"),
        contract(RESEARCH_STATE_V2).expect("static contract"),
        contract(QUERY_CONTEXT_INPUT_CORRECTION_V1).expect("static contract"),
        contract(ONTOLOGY_COMPANY_CONTEXT_V1).expect("static contract"),
        contract(ONTOLOGY_TARGETED_QUERY_V1).expect("static contract"),
        contract(ONTOLOGY_TRACE_INPUT_V1).expect("static contract"),
        contract(SKILL_LOAD_V1).expect("static contract"),
        contract(SKILL_CONTENT_V1).expect("static contract"),
    ] {
        verify_json(descriptor.id, descriptor.schema)?;
        let manifest_contract = manifest
            .contracts
            .get(descriptor.id)
            .ok_or(ContractArtifactError::MissingContract(descriptor.id))?;
        if manifest_contract.schema_sha256 != descriptor.schema_sha256 {
            return Err(ContractArtifactError::InvalidManifest(
                "generated binding differs from manifest schema hash",
            ));
        }
        if manifest_contract.authority_ref.is_empty()
            || !matches!(
                manifest_contract.authority_kind.as_str(),
                "pydantic_model" | "mcp_tool_input_schema" | "json_schema"
            )
            || !matches!(
                manifest_contract.semantic_validation.as_str(),
                "canonical_pydantic_model"
                    | "canonical_fastmcp_argument_model"
                    | "canonical_json_schema"
            )
            || manifest_contract.schema_path.is_empty()
        {
            return Err(ContractArtifactError::InvalidManifest(
                "invalid contract authority descriptor",
            ));
        }
        verify_hash(descriptor.id, descriptor.schema, descriptor.schema_sha256)?;
        let bundled_schema = bundle
            .contracts
            .get(descriptor.id)
            .ok_or(ContractArtifactError::MissingContract(descriptor.id))?;
        let bundled_bytes = serde_jcs::to_vec(bundled_schema)?;
        if bundled_bytes != descriptor.schema {
            return Err(ContractArtifactError::BundleSchemaMismatch(descriptor.id));
        }
    }
    if manifest.contracts.len() != bundle.contracts.len() {
        return Err(ContractArtifactError::InvalidManifest(
            "manifest and bundle contract counts differ",
        ));
    }

    Ok(VerifiedBundle {
        manifest_sha256: GENERATED_MANIFEST_SHA256.to_owned(),
        authority_sha256: manifest.authority_sha256,
        schema_bundle_sha256: manifest.schema_bundle.sha256,
        conformance_vectors_sha256: manifest.conformance_vectors.sha256,
        contract_count: manifest.contracts.len(),
    })
}

fn verify_json(name: &str, bytes: &[u8]) -> Result<(), ContractArtifactError> {
    let value: Value = serde_json::from_slice(bytes)?;
    let canonical = serde_jcs::to_vec(&value)?;
    if canonical != bytes {
        return Err(ContractArtifactError::NonCanonical(name.to_owned()));
    }
    Ok(())
}

fn verify_hash(artifact: &str, bytes: &[u8], expected: &str) -> Result<(), ContractArtifactError> {
    let actual = format!("sha256:{}", hex_digest(bytes));
    if actual != expected {
        return Err(ContractArtifactError::HashMismatch {
            artifact: artifact.to_owned(),
            expected: expected.to_owned(),
            actual,
        });
    }
    Ok(())
}

fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(output, "{byte:02x}").expect("writing into String cannot fail");
    }
    output
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: String,
    authority: Value,
    authority_sha256: String,
    schema_bundle: ArtifactReference,
    conformance_vectors: ArtifactReference,
    contracts: BTreeMap<String, ManifestContract>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactReference {
    path: String,
    sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestContract {
    authority_kind: String,
    authority_ref: String,
    schema_path: String,
    schema_sha256: String,
    semantic_validation: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SchemaBundle {
    authority_sha256: String,
    contracts: BTreeMap<String, Value>,
    format: String,
    json_schema_dialect: String,
    notice: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConformanceVectors {
    authority_sha256: String,
    format: String,
    vectors: Vec<Value>,
}

#[derive(Debug, Error)]
pub enum ContractArtifactError {
    #[error("invalid generated JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("generated artifact is not RFC 8785 canonical JSON: {0}")]
    NonCanonical(String),
    #[error("generated artifact hash mismatch for {artifact}: expected {expected}, got {actual}")]
    HashMismatch {
        artifact: String,
        expected: String,
        actual: String,
    },
    #[error("contract missing from generated bundle: {0}")]
    MissingContract(&'static str),
    #[error("individual schema differs from bundled schema: {0}")]
    BundleSchemaMismatch(&'static str),
    #[error("invalid generated contract manifest: {0}")]
    InvalidManifest(&'static str),
    #[error("unknown contract in canonical registry: {0}")]
    UnknownContract(String),
    #[error("pinned contract hash mismatch for {contract_id}: expected {expected}, got {observed}")]
    PinnedHashMismatch {
        contract_id: String,
        expected: String,
        observed: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_export_is_complete_canonical_and_hash_bound() {
        let verified = verify_embedded().expect("generated contracts must verify");
        assert_eq!(verified.contract_count, 8);
        assert_eq!(verified.manifest_sha256, GENERATED_MANIFEST_SHA256);
    }

    #[test]
    fn complete_registry_includes_hash_bound_kernel_contracts() {
        verify_registry().expect("all registry contracts must be canonical and hash-bound");
        assert_eq!(descriptors().len(), 57);
        assert_eq!(
            contract(ANSWER_IR_V1)
                .unwrap()
                .content_hash()
                .unwrap()
                .as_str(),
            ANSWER_IR_V1_SCHEMA_SHA256
        );
        assert_eq!(
            contract(RESEARCH_PROPOSAL_V4)
                .unwrap()
                .content_hash()
                .unwrap()
                .as_str(),
            RESEARCH_PROPOSAL_V4_SCHEMA_SHA256
        );
        assert_eq!(
            contract(FINAL_MARKDOWN_V1)
                .unwrap()
                .content_hash()
                .unwrap()
                .as_str(),
            FINAL_MARKDOWN_V1_SCHEMA_SHA256
        );
    }

    #[test]
    fn lookup_rejects_unknown_contracts() {
        assert!(contract("search-plan/v2").is_some());
        assert!(contract("research-state/v1").is_none());
    }

    #[test]
    fn ontology_authority_hashes_are_exact() {
        let expected = [
            (SEARCH_PLAN_V2, SEARCH_PLAN_V2_SCHEMA_SHA256),
            (RESEARCH_STATE_V2, RESEARCH_STATE_V2_SCHEMA_SHA256),
            (
                QUERY_CONTEXT_INPUT_CORRECTION_V1,
                QUERY_CONTEXT_INPUT_CORRECTION_V1_SCHEMA_SHA256,
            ),
            (
                ONTOLOGY_COMPANY_CONTEXT_V1,
                ONTOLOGY_COMPANY_CONTEXT_V1_SCHEMA_SHA256,
            ),
            (
                ONTOLOGY_TARGETED_QUERY_V1,
                ONTOLOGY_TARGETED_QUERY_V1_SCHEMA_SHA256,
            ),
            (
                ONTOLOGY_TRACE_INPUT_V1,
                ONTOLOGY_TRACE_INPUT_V1_SCHEMA_SHA256,
            ),
        ];
        for (id, expected_hash) in expected {
            let descriptor = contract(id).unwrap();
            assert_eq!(descriptor.schema_sha256, expected_hash);
            assert_eq!(
                ContentHash::sha256(descriptor.canonical_schema().unwrap()).as_str(),
                expected_hash
            );
        }
    }

    #[test]
    fn bounded_semantic_validators_fail_closed() {
        let plan = serde_json::json!({
            "question": "How durable is ACME cash generation?",
            "intent": "company_research",
            "tickers": ["ACME"],
            "clauses": [{
                "clause_id": "cash_generation",
                "retrieval_query": "ACME cash generation",
                "tickers": ["ACME"]
            }]
        });
        validate_value(SEARCH_PLAN_V2, &plan).unwrap();
        let mut invalid_plan = plan;
        invalid_plan["clauses"][0]["tickers"] = serde_json::json!(["OTHER"]);
        assert!(matches!(
            validate_value(SEARCH_PLAN_V2, &invalid_plan),
            Err(ContractValueError::Semantic(SEARCH_PLAN_V2))
        ));

        validate_value(ONTOLOGY_TARGETED_QUERY_V1, &serde_json::json!({})).unwrap();
        validate_value(
            COMPANY_CONTEXT_REQUEST_V1,
            &serde_json::json!({"ticker": "ACME", "limit_topics": 6}),
        )
        .unwrap();
        assert!(
            validate_value(
                COMPANY_CONTEXT_REQUEST_V1,
                &serde_json::json!({"ticker": "ACME", "include_internal_ids": false})
            )
            .is_err()
        );
        validate_value(
            ONTOLOGY_COMPANY_CONTEXT_V1,
            &serde_json::json!({"ticker": "ACME", "include_internal_ids": false}),
        )
        .unwrap();
        assert!(
            validate_value(
                ONTOLOGY_TARGETED_QUERY_V1,
                &serde_json::json!({"response_format": "markdown"})
            )
            .is_err()
        );
        assert!(
            validate_value(
                ONTOLOGY_TRACE_INPUT_V1,
                &serde_json::json!({"ticker": "ACME"})
            )
            .is_err()
        );
        validate_value(
            ONTOLOGY_TRACE_INPUT_V1,
            &serde_json::json!({"object_id": "obj-1", "response_format": "json"}),
        )
        .unwrap();
    }

    #[test]
    fn research_proposal_v4_uses_tagged_goals_and_returns_closed_repair_directives() {
        let mut proposal = serde_json::json!({
            "intent": "company_research",
            "answer_scope": "direct",
            "uncertainty": "low",
            "document_types": ["10-K"],
            "periods": [],
            "objectives": [{
                "priority": "required",
                "alternatives": [{"terms": ["AAPL", "revenue"]}],
                "directness": "direct_required",
                "object_types": [],
                "goal": {
                    "kind": "metric_time_series",
                    "metric": "revenue",
                    "metric_dimensions": []
                }
            }]
        });
        validate_value(RESEARCH_PROPOSAL_V4, &proposal).unwrap();

        proposal["comparison_axes"] = serde_json::json!(["growth_rate"]);
        assert!(matches!(
            validate_value(RESEARCH_PROPOSAL_V4, &proposal),
            Err(ContractValueError::Shape(RESEARCH_PROPOSAL_V4))
        ));

        proposal.as_object_mut().unwrap().remove("comparison_axes");
        proposal["objectives"][0]["alternatives"][0]["terms"] = serde_json::json!(["a"]);
        assert!(matches!(
            validate_value(RESEARCH_PROPOSAL_V4, &proposal),
            Err(ContractValueError::Shape(RESEARCH_PROPOSAL_V4))
        ));

        let mut qualitative = serde_json::json!({
            "intent": "company_research",
            "answer_scope": "direct",
            "uncertainty": "low",
            "document_types": ["10-K"],
            "periods": [],
            "objectives": [{
                "priority": "required",
                "alternatives": [{"terms": ["AAPL", "revenue and competitive moat"]}],
                "directness": "direct_required",
                "object_types": [],
                "goal": {
                    "kind": "qualitative_evidence",
                    "concepts": ["competitive moat", "revenue growth"],
                    "predicates": []
                }
            }]
        });
        assert!(matches!(
            validate_value(RESEARCH_PROPOSAL_V4, &qualitative),
            Err(ContractValueError::Semantic(RESEARCH_PROPOSAL_V4))
        ));
        assert_eq!(
            research_proposal_v4_validation_code(&qualitative),
            "proposal_qualitative_predicate_missing"
        );
        assert_eq!(
            research_proposal_v4_repair_directive(&qualitative)
                .expect("invalid proposal has repair directive")
                .repair_mode,
            ResearchProposalRepairMode::Split
        );

        qualitative["objectives"][0]["goal"]["predicates"] = serde_json::json!(["supports"]);
        validate_value(RESEARCH_PROPOSAL_V4, &qualitative).unwrap();

        qualitative["objectives"][0]["priority"] = serde_json::json!("deferred");
        assert!(matches!(
            validate_value(RESEARCH_PROPOSAL_V4, &qualitative),
            Err(ContractValueError::Semantic(RESEARCH_PROPOSAL_V4))
        ));
        assert_eq!(
            research_proposal_v4_validation_code(&qualitative),
            "proposal_required_objective_missing"
        );
    }

    #[test]
    fn normalized_capability_payload_accepts_the_bounded_post_tool_fixture_size() {
        // The performance harness intentionally keeps a 256 KiB post-tool
        // result live while a second provider turn waits. A generic 64 KiB
        // scalar cap would make the runtime's documented 8 MiB envelope
        // unreachable for ordinary filing/document payloads.
        let result = serde_json::json!({
            "provider_content": {"document_text": "x".repeat(256 * 1024)},
            "evidence": [],
            "answerability": null,
            "calculations": [],
        });
        validate_value(NORMALIZED_CAPABILITY_RESULT_V1, &result).unwrap();
    }
}
