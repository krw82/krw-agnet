//! Deterministic, evidence-preserving provider context compaction.
//!
//! This crate never summarizes with a model. It can run only at a settled
//! provider boundary and rebuilds the next context from validated state,
//! canonical research gaps, exact normalized facts, and committed
//! calculations. Provider reasoning, assistant prose, and raw tool output are
//! deliberately absent.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::fmt;

use krw_agent_evidence::{
    Answerability, Calculation, Directness, EvidenceGrade, EvidenceLedger, NormalizedFact,
    PublicCitation,
};
use krw_agent_protocol::ContentHash;
use krw_agent_provider_wire::ProviderMessage;
use krw_agent_state_artifact::{ContractPin, PhaseCompactionBoundaryV1, ValidatedArtifact};
use krw_ontology_adapter::ResearchPlanningProjection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use zeroize::Zeroize;

pub const COMPACTION_RECEIPT_SCHEMA_VERSION: u16 = 1;
pub const COMPACTED_CONTEXT_SCHEMA_VERSION: u16 = 1;
pub const DEFAULT_MAX_COMPACTED_CONTEXT_BYTES: usize = 512 * 1024;
pub const MIN_COMPACTED_CONTEXT_BYTES: usize = 32 * 1024;
pub const MAX_COMPACTED_CONTEXT_BYTES: usize = 2 * 1024 * 1024;

const MAX_SOURCE_MESSAGES: usize = 1_024;
const MAX_ACTIVE_EVIDENCE: usize = 512;
const MAX_FACT_CANDIDATES: usize = 8_192;
const MAX_OMITTED_FACT_REFS: usize = 8_192;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionStrategy {
    ExactTypedEvidencePriority,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactedStateArtifact {
    pub artifact_hash: ContentHash,
    pub contract: ContractPin,
    pub payload_hash: ContentHash,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactedEvidenceIndexEntry {
    pub evidence_id: String,
    pub content_hash: ContentHash,
    pub data_release_hash: ContentHash,
    pub entity: Option<String>,
    pub period: Option<String>,
    pub as_of: Option<String>,
    pub directness: Directness,
    pub grade: EvidenceGrade,
    pub strong_claim_allowed: bool,
    pub citation: PublicCitation,
    pub fact_count: u16,
    pub supports: Vec<String>,
    pub refutes: Vec<String>,
    pub qualifies: Vec<String>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactedFact {
    pub fact_ref: ContentHash,
    pub evidence_id: String,
    pub fact_index: u16,
    pub subject: String,
    pub predicate: String,
    pub value: Value,
    pub unit: Option<String>,
    pub period: Option<String>,
}

impl fmt::Debug for CompactedFact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompactedFact")
            .field("fact_ref", &self.fact_ref)
            .field("evidence_id", &self.evidence_id)
            .field("fact_index", &self.fact_index)
            .field("content", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactedProviderContext {
    pub schema_version: u16,
    pub authority: String,
    pub boundary_hash: ContentHash,
    pub state: CompactedStateArtifact,
    pub answerability: Answerability,
    pub research_projection: Option<ResearchPlanningProjection>,
    pub evidence_index: Vec<CompactedEvidenceIndexEntry>,
    pub retained_facts: Vec<CompactedFact>,
    pub omitted_fact_refs: Vec<ContentHash>,
    pub calculations: Vec<Calculation>,
}

impl fmt::Debug for CompactedProviderContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompactedProviderContext")
            .field("schema_version", &self.schema_version)
            .field("boundary_hash", &self.boundary_hash)
            .field("state_artifact_hash", &self.state.artifact_hash)
            .field("answerability", &self.answerability)
            .field("evidence_count", &self.evidence_index.len())
            .field("retained_fact_count", &self.retained_facts.len())
            .field("omitted_fact_count", &self.omitted_fact_refs.len())
            .field("calculation_count", &self.calculations.len())
            .finish_non_exhaustive()
    }
}

/// Role-filtered runtime projection of a [`CompactedProviderContext`].
///
/// The durable compaction receipt is computed over the FULL canonical context
/// (see [`CompactedProviderContext::canonical`] / [`CompactionOutput`]); a view
/// is only a prompt-time slice that reduces the bytes shown to a specific role.
/// The receipt's `compacted_context_hash` and the prompt-assembly segment's
/// `content_hash` therefore continue to pin the full canonical — never the view
/// — so receipt verification is unaffected by role filtering.
#[derive(Debug, Clone)]
pub struct CompactedContextView {
    /// Role-filtered canonical text (JCS-serialized projection). This is what
    /// goes into the `<verified-compacted-context>` prompt block.
    canonical: String,
    /// Length of `canonical` in bytes.
    byte_len: u64,
    /// The role this view was built for.
    role_id: String,
}

impl CompactedContextView {
    /// The role-filtered canonical text.
    pub fn canonical(&self) -> &str {
        &self.canonical
    }

    /// Length of the filtered canonical in bytes.
    pub fn byte_len(&self) -> u64 {
        self.byte_len
    }

    /// The role this view was built for.
    pub fn role_id(&self) -> &str {
        &self.role_id
    }

    /// Take ownership of the filtered canonical string, zeroizing the buffer
    /// held by this view.
    pub fn into_canonical(self) -> String {
        // `Drop` is implemented for `CompactedContextView` to scrub the
        // canonical buffer; wrap in `ManuallyDrop` so we can move the field
        // out without triggering the drop glue on `self`.
        let mut me = std::mem::ManuallyDrop::new(self);
        std::mem::take(&mut me.canonical)
    }
}

impl Drop for CompactedContextView {
    fn drop(&mut self) {
        self.canonical.zeroize();
    }
}

/// Canonical role identifiers used by `view_for_role`. These match the role
/// `id` values declared in agent images (`planner`, `analyst`, `composer`,
/// `repair`).
pub const ROLE_PLANNER: &str = "planner";
pub const ROLE_ANALYST: &str = "analyst";
pub const ROLE_COMPOSER: &str = "composer";
pub const ROLE_REPAIR: &str = "repair";

/// Maximum number of top-grade evidence entries retained for the `analyst` and
/// `repair` views. Keeps the analyst focused on research gaps rather than
/// re-reading the full evidence corpus.
const ANALYST_MAX_EVIDENCE: usize = 16;
/// Cap on retained facts in the minimal `repair` view. Repair reasoning
/// operates over the validated state artifact plus a small defect-relevant
/// fact slice; the full fact set remains durable in the receipt.
const REPAIR_MAX_FACTS: usize = 8;

impl CompactedProviderContext {
    /// Build a role-filtered runtime view of this compacted context.
    ///
    /// Filtering rules (per the runtime-perf-eval plan, task 6):
    ///
    /// - `planner` and any unrecognized role: the FULL canonical is returned
    ///   (current behavior). The planner orchestrates and needs every field.
    /// - `composer`: retained facts, calculations, citation handles from the
    ///   evidence index, and the validated state. The research projection
    ///   (unresolved goals, conflicts, missing parts) is omitted — composition
    ///   works from established facts, not open research gaps.
    /// - `analyst`: the full research projection, top-graded evidence, and
    ///   retained facts (needed to interpret goals). Detailed calculations are
    ///   omitted — the analyst reasons about coverage gaps, not computation.
    /// - `repair`: a minimal defect-relevant slice — the validated state
    ///   artifact plus a small fact subset. Repair context is small anyway;
    ///   the projection, calculations, and the bulk of the evidence index are
    ///   omitted.
    ///
    /// # Receipt safety
    ///
    /// The returned view is a NEW canonical string produced by re-serializing a
    /// filtered CLONE of this context. The durable `CompactionReceipt`
    /// (computed in [`compact`]) and the prompt-assembly receipt both pin the
    /// FULL canonical — callers MUST use the full canonical (e.g. via
    /// [`CompactedProviderContext::canonical`]) when constructing the receipt
    /// segment, and only use this view for the prompt body text. See the
    /// run-engine `build_trusted_messages` wiring for the canonical usage.
    pub fn view_for_role(&self, role_id: &str) -> Result<CompactedContextView, CompactionError> {
        // Planner (and any unrecognized role) gets the full, unfiltered view.
        // This preserves the prior behavior for every code path that has not
        // been explicitly migrated to role-filtered views.
        if role_id != ROLE_ANALYST && role_id != ROLE_COMPOSER && role_id != ROLE_REPAIR {
            let canonical = String::from_utf8(serde_jcs::to_vec(self)?)
                .map_err(|_| CompactionError::Invariant("canonical context is not UTF-8"))?;
            let byte_len = u64::try_from(canonical.len())
                .map_err(|_| CompactionError::Limit("compacted view bytes"))?;
            return Ok(CompactedContextView {
                canonical,
                byte_len,
                role_id: role_id.to_owned(),
            });
        }

        // Build a filtered clone. We never mutate `self`; the durable context
        // is preserved so the receipt continues to verify.
        let mut filtered = self.clone();

        match role_id {
            ROLE_COMPOSER => {
                // Composer assembles the answer from established facts and
                // calculations; it does not need the open research projection.
                filtered.research_projection = None;
                // Keep evidence_index (citation handles), retained_facts, and
                // calculations in full — these are exactly what composition
                // grounds its claims in.
            }
            ROLE_ANALYST => {
                // The analyst reasons about research coverage; drop the
                // detailed calculation transcript and trim the evidence index
                // to the top-graded entries so the analyst can scan gaps.
                filtered.calculations.clear();
                trim_evidence_by_grade(&mut filtered.evidence_index, ANALYST_MAX_EVIDENCE);
                // Retain the full research_projection (goals, missing parts,
                // recommended actions) and retained_facts (needed to interpret
                // goal status).
            }
            ROLE_REPAIR => {
                // Minimal defect-relevant slice: state artifact + small fact
                // subset. Drop projection, calculations, and almost all
                // evidence; cap facts.
                filtered.research_projection = None;
                filtered.calculations.clear();
                filtered.omitted_fact_refs.clear();
                trim_evidence_by_grade(&mut filtered.evidence_index, REPAIR_MAX_FACTS);
                if filtered.retained_facts.len() > REPAIR_MAX_FACTS {
                    let split = filtered.retained_facts.len() - REPAIR_MAX_FACTS;
                    let drained: Vec<_> = filtered.retained_facts.drain(split..).collect();
                    // The dropped facts are not erased from the durable record
                    // (the receipt still accounts for them); we only narrow the
                    // runtime view. Stash their refs under omitted_fact_refs so
                    // the view remains internally honest about what was hidden.
                    filtered
                        .omitted_fact_refs
                        .extend(drained.into_iter().map(|fact| fact.fact_ref.clone()));
                    filtered.omitted_fact_refs.sort();
                }
            }
            _ => unreachable!("role gate above excludes every other branch"),
        }

        let canonical = String::from_utf8(serde_jcs::to_vec(&filtered)?)
            .map_err(|_| CompactionError::Invariant("canonical context is not UTF-8"))?;
        let byte_len = u64::try_from(canonical.len())
            .map_err(|_| CompactionError::Limit("compacted view bytes"))?;
        // Zeroize the filtered clone's sensitive buffers eagerly; Drop on
        // CompactedFact / CompactedStateArtifact already scrubs on drop, but
        // clearing the projection here keeps the filtered text from lingering
        // in heap fragments longer than necessary.
        drop(filtered);
        Ok(CompactedContextView {
            canonical,
            byte_len,
            role_id: role_id.to_owned(),
        })
    }

    /// Produce the FULL canonical JCS serialization of this context. This is
    /// the bytes over which the durable receipt is computed and the bytes that
    /// must be pinned in the prompt-assembly segment. Role views are derived
    /// from — but never replace — this canonical form.
    pub fn canonical(&self) -> Result<String, CompactionError> {
        String::from_utf8(serde_jcs::to_vec(self)?)
            .map_err(|_| CompactionError::Invariant("canonical context is not UTF-8"))
    }
}

/// Keep only the top `max` evidence entries by grade (Strong > Medium > Weak >
/// Unverified), breaking ties on `evidence_id` for determinism. The durable
/// receipt is unaffected: the FULL `evidence_index` remains in `self.context`.
fn trim_evidence_by_grade(entries: &mut Vec<CompactedEvidenceIndexEntry>, max: usize) {
    if entries.len() <= max {
        return;
    }
    entries.sort_by(|left, right| {
        // Higher grade first; tie-break on evidence_id for determinism.
        right
            .grade
            .cmp(&left.grade)
            .then_with(|| left.evidence_id.cmp(&right.evidence_id))
    });
    entries.truncate(max);
    // Restore the durable canonical ordering (sorted by evidence_id) so the
    // serialized view is stable regardless of input order.
    entries.sort_by(|left, right| left.evidence_id.cmp(&right.evidence_id));
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactionReceipt {
    pub schema_version: u16,
    pub strategy: CompactionStrategy,
    pub boundary_hash: ContentHash,
    pub source_conversation_hash: ContentHash,
    pub source_message_count: u32,
    pub source_bytes: u64,
    pub compacted_context_hash: ContentHash,
    pub compacted_bytes: u64,
    pub state_artifact_hash: ContentHash,
    pub research_projection_hash: Option<ContentHash>,
    pub evidence_index_hash: ContentHash,
    pub retained_fact_set_hash: ContentHash,
    pub omitted_fact_set_hash: ContentHash,
    pub calculation_set_hash: ContentHash,
    pub active_evidence_count: u16,
    pub retained_fact_count: u16,
    pub omitted_fact_count: u16,
    pub calculation_count: u16,
}

impl CompactionReceipt {
    pub fn receipt_hash(&self) -> Result<ContentHash, CompactionError> {
        self.verify()?;
        Ok(ContentHash::sha256(serde_jcs::to_vec(self)?))
    }

    pub fn verify(&self) -> Result<(), CompactionError> {
        if self.schema_version != COMPACTION_RECEIPT_SCHEMA_VERSION
            || self.source_message_count as usize > MAX_SOURCE_MESSAGES
            || self.active_evidence_count as usize > MAX_ACTIVE_EVIDENCE
            || self.retained_fact_count as usize > MAX_FACT_CANDIDATES
            || self.omitted_fact_count as usize > MAX_OMITTED_FACT_REFS
            || self.compacted_bytes == 0
            || self.compacted_bytes
                > u64::try_from(MAX_COMPACTED_CONTEXT_BYTES)
                    .map_err(|_| CompactionError::InvalidReceipt)?
        {
            return Err(CompactionError::InvalidReceipt);
        }
        Ok(())
    }
}

pub struct CompactionOutput {
    canonical_context: String,
    pub context: CompactedProviderContext,
    pub receipt: CompactionReceipt,
}

impl fmt::Debug for CompactionOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompactionOutput")
            .field("canonical_context", &"[REDACTED]")
            .field("context", &self.context)
            .field("receipt", &self.receipt)
            .finish()
    }
}

impl CompactionOutput {
    pub fn canonical_context(&self) -> &str {
        &self.canonical_context
    }

    pub fn into_canonical_context(mut self) -> String {
        std::mem::take(&mut self.canonical_context)
    }

    pub fn verify(&self) -> Result<(), CompactionError> {
        self.receipt.verify()?;
        let canonical = serde_jcs::to_vec(&self.context)?;
        if canonical.as_slice() != self.canonical_context.as_bytes()
            || ContentHash::sha256(&canonical) != self.receipt.compacted_context_hash
            || u64::try_from(canonical.len()).ok() != Some(self.receipt.compacted_bytes)
            || self.context.boundary_hash != self.receipt.boundary_hash
            || self.context.state.artifact_hash != self.receipt.state_artifact_hash
            || hash_optional(self.context.research_projection.as_ref())?
                != self.receipt.research_projection_hash
            || hash_slice(&self.context.evidence_index)? != self.receipt.evidence_index_hash
            || hash_slice(&self.context.retained_facts)? != self.receipt.retained_fact_set_hash
            || hash_slice(&self.context.omitted_fact_refs)? != self.receipt.omitted_fact_set_hash
            || hash_slice(&self.context.calculations)? != self.receipt.calculation_set_hash
            || usize::from(self.receipt.active_evidence_count) != self.context.evidence_index.len()
            || usize::from(self.receipt.retained_fact_count) != self.context.retained_facts.len()
            || usize::from(self.receipt.omitted_fact_count) != self.context.omitted_fact_refs.len()
            || usize::from(self.receipt.calculation_count) != self.context.calculations.len()
        {
            return Err(CompactionError::ReceiptMismatch);
        }
        Ok(())
    }
}

impl Drop for CompactionOutput {
    fn drop(&mut self) {
        self.canonical_context.zeroize();
    }
}

pub struct CompactionInput<'a> {
    pub boundary: &'a PhaseCompactionBoundaryV1,
    pub state_artifact: &'a ValidatedArtifact,
    pub source_messages: &'a [ProviderMessage],
    pub ledger: &'a EvidenceLedger,
    pub calculations: &'a BTreeMap<String, Calculation>,
    pub research_projection: Option<&'a ResearchPlanningProjection>,
    pub max_context_bytes: usize,
}

impl fmt::Debug for CompactionInput<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompactionInput")
            .field("boundary", self.boundary)
            .field("state_artifact", self.state_artifact)
            .field("source_message_count", &self.source_messages.len())
            .field("evidence_count", &self.ledger.len())
            .field("calculation_count", &self.calculations.len())
            .field(
                "has_research_projection",
                &self.research_projection.is_some(),
            )
            .field("max_context_bytes", &self.max_context_bytes)
            .finish()
    }
}

#[derive(Clone)]
struct FactCandidate {
    priority: FactPriority,
    fact: CompactedFact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct FactPriority {
    relation: u8,
    numeric_or_boolean: u8,
    directness: u8,
    grade: u8,
    strong: u8,
}

pub fn compact(input: &CompactionInput<'_>) -> Result<CompactionOutput, CompactionError> {
    validate_input(input)?;
    let source_bytes = serde_jcs::to_vec(input.source_messages)?;
    let source_conversation_hash = ContentHash::sha256(&source_bytes);
    let boundary_hash = input.boundary.boundary_hash()?;
    let artifact_hash = input.state_artifact.artifact_hash()?;
    if artifact_hash != input.boundary.artifact_hash
        || input.state_artifact.contract() != &input.boundary.artifact_contract
    {
        return Err(CompactionError::BoundaryArtifactMismatch);
    }
    let artifact_payload = input.state_artifact.payload().clone();
    let state = CompactedStateArtifact {
        artifact_hash: artifact_hash.clone(),
        contract: input.state_artifact.contract().clone(),
        payload_hash: ContentHash::sha256(serde_jcs::to_vec(&artifact_payload)?),
        payload: artifact_payload,
    };

    let mut evidence_index = Vec::new();
    let mut candidates = Vec::new();
    for (evidence_id, record) in input.ledger.iter() {
        if input
            .ledger
            .active(evidence_id)
            .is_none_or(|active| active.evidence_id != evidence_id)
        {
            continue;
        }
        let fact_count = u16::try_from(record.facts.len())
            .map_err(|_| CompactionError::Limit("facts per evidence"))?;
        evidence_index.push(CompactedEvidenceIndexEntry {
            evidence_id: evidence_id.to_owned(),
            content_hash: record.content_hash.clone(),
            data_release_hash: record.source.data_release_hash.clone(),
            entity: record.entity.clone(),
            period: record.period.clone(),
            as_of: record.as_of.clone(),
            directness: record.directness,
            grade: record.grade,
            strong_claim_allowed: record.strong_claim_allowed,
            citation: record.citation.clone(),
            fact_count,
            supports: record.supports.clone(),
            refutes: record.refutes.clone(),
            qualifies: record.qualifies.clone(),
        });
        for (fact_index, fact) in record.facts.iter().enumerate() {
            let fact_index =
                u16::try_from(fact_index).map_err(|_| CompactionError::Limit("fact index"))?;
            let fact_ref = fact_ref(evidence_id, fact_index, fact)?;
            candidates.push(FactCandidate {
                priority: fact_priority(record, fact),
                fact: CompactedFact {
                    fact_ref,
                    evidence_id: evidence_id.to_owned(),
                    fact_index,
                    subject: fact.subject.clone(),
                    predicate: fact.predicate.clone(),
                    value: fact.value.clone(),
                    unit: fact.unit.clone(),
                    period: fact.period.clone(),
                },
            });
        }
    }
    if evidence_index.len() > MAX_ACTIVE_EVIDENCE || candidates.len() > MAX_FACT_CANDIDATES {
        return Err(CompactionError::Limit("evidence or facts"));
    }
    evidence_index.sort_by(|left, right| left.evidence_id.cmp(&right.evidence_id));
    candidates.sort_by(|left, right| {
        Reverse(left.priority)
            .cmp(&Reverse(right.priority))
            .then_with(|| left.fact.evidence_id.cmp(&right.fact.evidence_id))
            .then_with(|| left.fact.fact_index.cmp(&right.fact.fact_index))
    });
    let calculations = input.calculations.values().cloned().collect::<Vec<_>>();
    let research_projection = input.research_projection.cloned();

    let mut context = CompactedProviderContext {
        schema_version: COMPACTED_CONTEXT_SCHEMA_VERSION,
        authority: "validated_state_and_committed_evidence_only".into(),
        boundary_hash: boundary_hash.clone(),
        state,
        answerability: input.ledger.answerability(),
        research_projection,
        evidence_index,
        retained_facts: candidates
            .iter()
            .map(|candidate| candidate.fact.clone())
            .collect(),
        omitted_fact_refs: Vec::new(),
        calculations,
    };
    shrink_to_fit(&mut context, input.max_context_bytes)?;
    let canonical = serde_jcs::to_vec(&context)?;
    let compacted_context_hash = ContentHash::sha256(&canonical);
    let receipt = CompactionReceipt {
        schema_version: COMPACTION_RECEIPT_SCHEMA_VERSION,
        strategy: CompactionStrategy::ExactTypedEvidencePriority,
        boundary_hash,
        source_conversation_hash,
        source_message_count: u32::try_from(input.source_messages.len())
            .map_err(|_| CompactionError::Limit("source messages"))?,
        source_bytes: u64::try_from(source_bytes.len())
            .map_err(|_| CompactionError::Limit("source bytes"))?,
        compacted_context_hash,
        compacted_bytes: u64::try_from(canonical.len())
            .map_err(|_| CompactionError::Limit("compacted bytes"))?,
        state_artifact_hash: artifact_hash,
        research_projection_hash: hash_optional(context.research_projection.as_ref())?,
        evidence_index_hash: hash_slice(&context.evidence_index)?,
        retained_fact_set_hash: hash_slice(&context.retained_facts)?,
        omitted_fact_set_hash: hash_slice(&context.omitted_fact_refs)?,
        calculation_set_hash: hash_slice(&context.calculations)?,
        active_evidence_count: u16::try_from(context.evidence_index.len())
            .map_err(|_| CompactionError::Limit("active evidence"))?,
        retained_fact_count: u16::try_from(context.retained_facts.len())
            .map_err(|_| CompactionError::Limit("retained facts"))?,
        omitted_fact_count: u16::try_from(context.omitted_fact_refs.len())
            .map_err(|_| CompactionError::Limit("omitted facts"))?,
        calculation_count: u16::try_from(context.calculations.len())
            .map_err(|_| CompactionError::Limit("calculations"))?,
    };
    let canonical_context = String::from_utf8(canonical)
        .map_err(|_| CompactionError::Invariant("canonical context is not UTF-8"))?;
    let output = CompactionOutput {
        canonical_context,
        context,
        receipt,
    };
    output.verify()?;
    Ok(output)
}

fn validate_input(input: &CompactionInput<'_>) -> Result<(), CompactionError> {
    if !(MIN_COMPACTED_CONTEXT_BYTES..=MAX_COMPACTED_CONTEXT_BYTES)
        .contains(&input.max_context_bytes)
    {
        return Err(CompactionError::Limit("max compacted context bytes"));
    }
    if input.source_messages.is_empty() || input.source_messages.len() > MAX_SOURCE_MESSAGES {
        return Err(CompactionError::Limit("source messages"));
    }
    Ok(())
}

fn shrink_to_fit(
    context: &mut CompactedProviderContext,
    max_bytes: usize,
) -> Result<(), CompactionError> {
    while serde_jcs::to_vec(&*context)?.len() > max_bytes {
        let Some(removed) = context.retained_facts.pop() else {
            return Err(CompactionError::EssentialContextExceedsLimit);
        };
        context.omitted_fact_refs.push(removed.fact_ref.clone());
        if context.omitted_fact_refs.len() > MAX_OMITTED_FACT_REFS {
            return Err(CompactionError::Limit("omitted fact refs"));
        }
    }
    context.omitted_fact_refs.sort();
    Ok(())
}

fn fact_priority(
    record: &krw_agent_evidence::EvidenceRecord,
    fact: &NormalizedFact,
) -> FactPriority {
    let relation_priority = u8::from(!record.refutes.is_empty() || !record.qualifies.is_empty());
    // Causal text evidence used to rank below numeric/boolean facts, so it was
    // the first to be dropped at a compaction boundary.  Text now carries the
    // same base weight as typed values; numeric/boolean values and facts with
    // unit/period context still rank higher so quantitative lineage is kept
    // before prose when the budget is tight.
    let has_typed_context = fact.value.is_number()
        || fact.value.is_boolean()
        || fact.unit.is_some()
        || fact.period.is_some();
    let numeric_or_boolean_priority = if has_typed_context { 2 } else { 1 };
    let directness_priority = match record.directness {
        Directness::Direct => 3,
        Directness::MetricLineage => 2,
        Directness::Related => 1,
        Directness::Unverified => 0,
    };
    let grade_priority = match record.grade {
        EvidenceGrade::Strong => 3,
        EvidenceGrade::Medium => 2,
        EvidenceGrade::Weak => 1,
        EvidenceGrade::Unverified => 0,
    };
    FactPriority {
        relation: relation_priority,
        numeric_or_boolean: numeric_or_boolean_priority,
        directness: directness_priority,
        grade: grade_priority,
        strong: u8::from(record.strong_claim_allowed),
    }
}

fn fact_ref(
    evidence_id: &str,
    fact_index: u16,
    fact: &NormalizedFact,
) -> Result<ContentHash, CompactionError> {
    #[derive(Serialize)]
    struct FactRef<'a> {
        evidence_id: &'a str,
        fact_index: u16,
        fact: &'a NormalizedFact,
    }
    Ok(ContentHash::sha256(serde_jcs::to_vec(&FactRef {
        evidence_id,
        fact_index,
        fact,
    })?))
}

fn hash_optional<T: Serialize>(value: Option<&T>) -> Result<Option<ContentHash>, CompactionError> {
    value
        .map(|value| serde_jcs::to_vec(value).map(ContentHash::sha256))
        .transpose()
        .map_err(CompactionError::from)
}

fn hash_slice<T: Serialize>(value: &[T]) -> Result<ContentHash, CompactionError> {
    Ok(ContentHash::sha256(serde_jcs::to_vec(value)?))
}

fn scrub_value(value: &mut Value) {
    match value {
        Value::String(text) => text.zeroize(),
        Value::Array(values) => values.iter_mut().for_each(scrub_value),
        Value::Object(values) => values.values_mut().for_each(scrub_value),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

impl Drop for CompactedStateArtifact {
    fn drop(&mut self) {
        scrub_value(&mut self.payload);
    }
}

impl Drop for CompactedFact {
    fn drop(&mut self) {
        self.subject.zeroize();
        self.predicate.zeroize();
        scrub_value(&mut self.value);
        if let Some(unit) = &mut self.unit {
            unit.zeroize();
        }
        if let Some(period) = &mut self.period {
            period.zeroize();
        }
    }
}

#[derive(Debug, Error)]
pub enum CompactionError {
    #[error("context compaction limit exceeded: {0}")]
    Limit(&'static str),
    #[error("essential typed context does not fit the configured compaction limit")]
    EssentialContextExceedsLimit,
    #[error("settled boundary does not match the validated state artifact")]
    BoundaryArtifactMismatch,
    #[error("compaction receipt is invalid")]
    InvalidReceipt,
    #[error("compaction receipt does not bind the compacted context")]
    ReceiptMismatch,
    #[error("context compaction invariant failed: {0}")]
    Invariant(&'static str),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Artifact(#[from] krw_agent_state_artifact::ArtifactError),
}

#[cfg(test)]
mod tests {
    use krw_agent_evidence::{
        Calculation, Directness, EvidenceGrade, EvidenceLedger, EvidenceRecord, EvidenceScope,
        EvidenceSource, NormalizedFact, PublicCitation,
    };
    use krw_agent_protocol::{AuthScope, ContentHash};
    use krw_agent_state_artifact::{
        ArtifactProducer, ArtifactValidator, ContractPin, ModelOutputMode,
        PhaseCompactionBoundaryV1, ProviderReplayState, StateArtifactDraft, StateIdentity,
        StateOperation,
    };
    use serde_json::json;
    use std::collections::BTreeMap;

    use super::*;

    fn hash(value: &str) -> ContentHash {
        ContentHash::sha256(value)
    }

    fn evidence(id: &str, facts: Vec<NormalizedFact>) -> EvidenceRecord {
        EvidenceRecord {
            evidence_id: id.into(),
            content_hash: hash(&format!("content-{id}")),
            source: EvidenceSource {
                capability_id: "ontology.query_context".into(),
                action_key: hash("action").to_string(),
                server_build: "build-1".into(),
                normalized_contract_hash: hash("normalized"),
                server_schema_bundle_hash: hash("schema"),
                data_release_hash: hash("release"),
            },
            scope: EvidenceScope {
                auth_scope: AuthScope::Public,
                scope_hash: hash("scope"),
            },
            entity: Some("TEST".into()),
            period: Some("FY2025".into()),
            as_of: Some("2025-12-31".into()),
            directness: Directness::Direct,
            grade: EvidenceGrade::Strong,
            strong_claim_allowed: true,
            payload_ref: hash("payload"),
            citation: PublicCitation {
                title: "Annual filing".into(),
                document_type: Some("10-K".into()),
                period: Some("FY2025".into()),
            },
            facts,
            supports: vec!["claim-growth".into()],
            refutes: vec!["claim-no-risk".into()],
            qualifies: Vec::new(),
            source_object_ids: Vec::new(),
        }
    }

    fn artifact_and_boundary() -> (
        krw_agent_state_artifact::ValidatedArtifact,
        PhaseCompactionBoundaryV1,
    ) {
        let validator = ArtifactValidator::default();
        let contract = ContractPin::canonical("state-facts/v1").unwrap();
        let operation = StateOperation::ModelDecision {
            role_id: "analyst".into(),
            output_mode: ModelOutputMode::WorkflowTransition,
            input_contracts: Vec::new(),
            output_contracts: vec![contract.clone()],
        };
        let artifact = validator
            .seal_for_operation(
                &StateIdentity {
                    image_hash: hash("image"),
                    workflow_id: "company_research".into(),
                    state_id: "assess".into(),
                },
                &operation,
                StateArtifactDraft {
                    producer: ArtifactProducer::Model {
                        role_id: "analyst".into(),
                        provider_episode_hash: hash("episode"),
                    },
                    event: "evidence_sufficient".into(),
                    declared_contract: contract,
                    payload: json!({"period":"FY2025","not_deteriorating":false}),
                    lineage_refs: Vec::new(),
                },
            )
            .unwrap();
        let boundary = PhaseCompactionBoundaryV1::seal(
            &artifact,
            &ProviderReplayState::Settled {
                provider_episode_hash: hash("episode"),
            },
            Vec::new(),
        )
        .unwrap();
        (artifact, boundary)
    }

    #[test]
    fn compaction_preserves_exact_number_period_negation_and_calculation() {
        let (artifact, boundary) = artifact_and_boundary();
        let mut ledger = EvidenceLedger::default();
        ledger
            .append(evidence(
                "evidence-1",
                vec![
                    NormalizedFact {
                        subject: "TEST".into(),
                        predicate: "revenue".into(),
                        value: json!(12_345_678.91),
                        unit: Some("USD".into()),
                        period: Some("FY2025".into()),
                    },
                    NormalizedFact {
                        subject: "TEST".into(),
                        predicate: "guidance_withdrawn".into(),
                        value: json!(false),
                        unit: None,
                        period: Some("Q4 2025".into()),
                    },
                ],
            ))
            .unwrap();
        ledger.set_answerability(Answerability::StrongAllowed);
        let calculation = Calculation {
            calculation_id: "calc-1".into(),
            expression: "12345678.91 / 2".into(),
            input_evidence_ids: vec!["evidence-1".into()],
            output: json!(6_172_839.455),
            unit: Some("USD".into()),
            rounding: None,
            subject: Some("TEST".into()),
            metric: Some("half_revenue".into()),
            period: Some("FY2025".into()),
            currency: Some("USD".into()),
        };
        ledger.append_calculation(calculation.clone()).unwrap();
        let calculations = BTreeMap::from([("calc-1".into(), calculation)]);
        let messages = vec![ProviderMessage::assistant("PRIVATE_REASONING_CANARY")];
        let output = compact(&CompactionInput {
            boundary: &boundary,
            state_artifact: &artifact,
            source_messages: &messages,
            ledger: &ledger,
            calculations: &calculations,
            research_projection: None,
            max_context_bytes: DEFAULT_MAX_COMPACTED_CONTEXT_BYTES,
        })
        .unwrap();
        output.verify().unwrap();
        let canonical = output.canonical_context();
        assert!(canonical.contains("12345678.91"));
        assert!(canonical.contains("FY2025"));
        assert!(canonical.contains("guidance_withdrawn"));
        assert!(canonical.contains("false"));
        assert!(canonical.contains("6172839.455"));
        assert!(!canonical.contains("PRIVATE_REASONING_CANARY"));
        assert_eq!(output.receipt.source_message_count, 1);
    }

    #[test]
    fn active_tool_chain_cannot_create_a_compaction_boundary() {
        let (artifact, _) = artifact_and_boundary();
        let result = PhaseCompactionBoundaryV1::seal(
            &artifact,
            &ProviderReplayState::ActiveToolChain {
                provider_episode_hash: hash("episode"),
                tool_schema_hash: hash("tools"),
            },
            Vec::new(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn bounded_compaction_drops_low_priority_facts_before_exact_numeric_facts() {
        let (artifact, boundary) = artifact_and_boundary();
        let mut ledger = EvidenceLedger::default();
        let mut facts = (0..500)
            .map(|index| NormalizedFact {
                subject: format!("subject-{index}"),
                predicate: "commentary".into(),
                value: json!("x".repeat(256)),
                unit: None,
                period: None,
            })
            .collect::<Vec<_>>();
        facts.push(NormalizedFact {
            subject: "TEST".into(),
            predicate: "revenue".into(),
            value: json!(999_999_999),
            unit: Some("KRW".into()),
            period: Some("FY2025".into()),
        });
        // Split across records to stay within the evidence-record fact bound.
        for (chunk, values) in facts.chunks(100).enumerate() {
            ledger
                .append(evidence(&format!("evidence-{chunk}"), values.to_vec()))
                .unwrap();
        }
        let output = compact(&CompactionInput {
            boundary: &boundary,
            state_artifact: &artifact,
            source_messages: &[ProviderMessage::assistant("settled")],
            ledger: &ledger,
            calculations: &BTreeMap::new(),
            research_projection: None,
            max_context_bytes: 128 * 1024,
        })
        .unwrap();
        assert!(!output.context.omitted_fact_refs.is_empty());
        assert!(output.canonical_context().contains("999999999"));
        assert!(output.canonical_context().contains("FY2025"));
    }

    #[test]
    fn receipt_detects_context_tampering() {
        let (artifact, boundary) = artifact_and_boundary();
        let ledger = EvidenceLedger::default();
        let mut output = compact(&CompactionInput {
            boundary: &boundary,
            state_artifact: &artifact,
            source_messages: &[ProviderMessage::assistant("settled")],
            ledger: &ledger,
            calculations: &BTreeMap::new(),
            research_projection: None,
            max_context_bytes: DEFAULT_MAX_COMPACTED_CONTEXT_BYTES,
        })
        .unwrap();
        output.context.state.payload = json!({"tampered":true});
        assert!(matches!(
            output.verify(),
            Err(CompactionError::ReceiptMismatch)
        ));
    }

    #[test]
    fn fifty_settled_boundaries_remain_bounded_exact_and_reasoning_free() {
        let (artifact, boundary) = artifact_and_boundary();
        let mut ledger = EvidenceLedger::default();
        ledger
            .append(evidence(
                "evidence-stress",
                vec![NormalizedFact {
                    subject: "TEST".into(),
                    predicate: "net_debt_not_increasing".into(),
                    value: json!(false),
                    unit: Some("KRW".into()),
                    period: Some("FY2026".into()),
                }],
            ))
            .unwrap();
        let mut messages = vec![ProviderMessage::assistant(
            "PRIVATE_REASONING_CANARY boundary zero",
        )];
        let mut stable_hash = None;
        for _ in 0..50 {
            let output = compact(&CompactionInput {
                boundary: &boundary,
                state_artifact: &artifact,
                source_messages: &messages,
                ledger: &ledger,
                calculations: &BTreeMap::new(),
                research_projection: None,
                max_context_bytes: MIN_COMPACTED_CONTEXT_BYTES,
            })
            .unwrap();
            output.verify().unwrap();
            assert!(output.receipt.compacted_bytes <= MIN_COMPACTED_CONTEXT_BYTES as u64);
            assert!(
                output
                    .canonical_context()
                    .contains("net_debt_not_increasing")
            );
            assert!(output.canonical_context().contains("FY2026"));
            assert!(output.canonical_context().contains("false"));
            assert!(
                !output
                    .canonical_context()
                    .contains("PRIVATE_REASONING_CANARY")
            );
            if let Some(expected) = &stable_hash {
                assert_eq!(&output.receipt.compacted_context_hash, expected);
            } else {
                stable_hash = Some(output.receipt.compacted_context_hash.clone());
            }
            messages = vec![ProviderMessage::user(output.canonical_context())];
        }
    }

    /// Build a `CompactedProviderContext` fixture that has BOTH facts (revenue,
    /// a calculation) and a research projection (an unresolved goal + a missing
    /// part). This is the shape required to differentiate role views.
    fn fixture_context_with_projection() -> CompactedProviderContext {
        let (artifact, boundary) = artifact_and_boundary();
        let mut ledger = EvidenceLedger::default();
        ledger
            .append(evidence(
                "evidence-1",
                vec![NormalizedFact {
                    subject: "TEST".into(),
                    predicate: "revenue".into(),
                    value: json!(12_345_678.91),
                    unit: Some("USD".into()),
                    period: Some("FY2025".into()),
                }],
            ))
            .unwrap();
        ledger.set_answerability(Answerability::StrongAllowed);
        let calculation = Calculation {
            calculation_id: "calc-1".into(),
            expression: "12345678.91 / 2".into(),
            input_evidence_ids: vec!["evidence-1".into()],
            output: json!(6_172_839.455),
            unit: Some("USD".into()),
            rounding: None,
            subject: Some("TEST".into()),
            metric: Some("half_revenue".into()),
            period: Some("FY2025".into()),
            currency: Some("USD".into()),
        };
        ledger.append_calculation(calculation.clone()).unwrap();
        let calculations = BTreeMap::from([("calc-1".into(), calculation)]);
        // Build the research projection via JSON deserialization so the test
        // does not need a direct dependency on the planning crate. The graph
        // contains one unresolved goal and one missing part.
        let projection_json = json!({
            "graph": {
                "version": 1,
                "goals": {
                    "goal-coverage": {
                        "goal_id": "goal-coverage",
                        "required": true,
                        "weight": 100,
                        "dependencies": [],
                        "directness": "direct",
                        "calculation_required": true,
                        "status": "unresolved",
                        "coverage_ppm": 0,
                        "evidence_ids": [],
                        "calculation_ids": []
                    }
                },
                "original_order": ["goal-coverage"]
            },
            "clauses": [],
            "missing_parts": [
                {"code":"missing_counter_evidence","detail":"No refuting evidence collected for claim-risk","clause_id":null,"ticker":null}
            ],
            "recommended_actions": []
        });
        let projection: ResearchPlanningProjection =
            serde_json::from_value(projection_json).unwrap();
        let output = compact(&CompactionInput {
            boundary: &boundary,
            state_artifact: &artifact,
            source_messages: &[ProviderMessage::assistant("settled")],
            ledger: &ledger,
            calculations: &calculations,
            research_projection: Some(&projection),
            max_context_bytes: DEFAULT_MAX_COMPACTED_CONTEXT_BYTES,
        })
        .unwrap();
        output.verify().unwrap();
        // `CompactionOutput` implements `Drop` (it zeroizes the canonical
        // string), so we cannot move `context` out by field access. Wrap in
        // `ManuallyDrop` and read the field by reference-clone instead.
        let me = std::mem::ManuallyDrop::new(output);
        me.context.clone()
    }

    #[test]
    fn planner_and_unknown_roles_get_full_canonical_view() {
        let context = fixture_context_with_projection();
        let full = context.canonical().unwrap();
        let planner_view = context.view_for_role(ROLE_PLANNER).unwrap();
        assert_eq!(planner_view.role_id(), ROLE_PLANNER);
        assert_eq!(planner_view.canonical(), full);
        assert_eq!(planner_view.byte_len(), u64::try_from(full.len()).unwrap());
        // An unrecognized role also gets the full canonical — preserves prior
        // behavior for any code path not yet migrated to role views.
        let unknown = context.view_for_role("supervisor").unwrap();
        assert_eq!(unknown.canonical(), full);
    }

    #[test]
    fn role_views_differ_in_byte_length_and_receipt_pins_full_canonical() {
        let context = fixture_context_with_projection();
        let full = context.canonical().unwrap();
        let full_len = full.len();

        let composer = context.view_for_role(ROLE_COMPOSER).unwrap();
        let analyst = context.view_for_role(ROLE_ANALYST).unwrap();
        let repair = context.view_for_role(ROLE_REPAIR).unwrap();

        // Every view MUST be strictly smaller than the full canonical.
        assert!(
            composer.byte_len() < u64::try_from(full_len).unwrap(),
            "composer view must be smaller than full canonical"
        );
        assert!(
            analyst.byte_len() < u64::try_from(full_len).unwrap(),
            "analyst view must be smaller than full canonical"
        );
        assert!(
            repair.byte_len() < u64::try_from(full_len).unwrap(),
            "repair view must be smaller than full canonical"
        );
        // The composer and analyst views are filtered differently (composer
        // keeps calculations but drops the projection; analyst drops
        // calculations but keeps the projection). For this fixture the two
        // views therefore land at different byte lengths.
        assert_ne!(
            composer.byte_len(),
            analyst.byte_len(),
            "composer and analyst views must differ in byte length for a fixture with both facts and conflicts"
        );
        // Repair is the minimal slice — smaller than both composer and analyst.
        assert!(repair.byte_len() < composer.byte_len());
        assert!(repair.byte_len() < analyst.byte_len());

        // Composer: projection is gone (no unresolved goal text), but the
        // calculation transcript is still present.
        assert!(!composer.canonical().contains("goal-coverage"));
        assert!(composer.canonical().contains("6172839.455"));
        // Analyst: projection (goals + missing parts) is present; the detailed
        // calculation transcript is gone.
        assert!(analyst.canonical().contains("goal-coverage"));
        assert!(analyst.canonical().contains("missing_counter_evidence"));
        assert!(!analyst.canonical().contains("6172839.455"));
        // Repair: minimal slice — projection and calculations gone.
        assert!(!repair.canonical().contains("goal-coverage"));
        assert!(!repair.canonical().contains("6172839.455"));

        // Receipt-safety invariant: the FULL canonical is still recoverable and
        // unchanged after producing every view. The view methods never mutate
        // `self`; the durable canonical form that the receipt pins is intact.
        let full_again = context.canonical().unwrap();
        assert_eq!(full, full_again);
    }

    #[test]
    fn view_for_role_is_deterministic() {
        let context = fixture_context_with_projection();
        let first = context.view_for_role(ROLE_COMPOSER).unwrap();
        let second = context.view_for_role(ROLE_COMPOSER).unwrap();
        assert_eq!(first.canonical(), second.canonical());
        assert_eq!(first.byte_len(), second.byte_len());
    }
}
