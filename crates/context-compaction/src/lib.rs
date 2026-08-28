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
    AnalystJudgmentNote, Answerability, Calculation, Directness, EvidenceGrade, EvidenceLedger,
    NormalizedFact, PublicCitation,
};
use krw_agent_planning::{
    DirectnessRequirement, EvidenceGoal, EvidenceGoalGraph, GoalStatus,
};
use krw_agent_protocol::ContentHash;
use krw_agent_provider_wire::ProviderMessage;
use krw_agent_state_artifact::{ContractPin, PhaseCompactionBoundaryV1, ValidatedArtifact};
use krw_ontology_adapter::{
    Continuation, MAX_SUPPLEMENTAL_READ_STATUSES, ResearchPlanningProjection,
    ResearchRetrievalStatus, SupplementalReadStatus,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use zeroize::Zeroize;

pub const COMPACTION_RECEIPT_SCHEMA_VERSION: u16 = 2;
pub const COMPACTED_CONTEXT_SCHEMA_VERSION: u16 = 2;
pub const DEFAULT_MAX_COMPACTED_CONTEXT_BYTES: usize = 512 * 1024;
pub const MIN_COMPACTED_CONTEXT_BYTES: usize = 32 * 1024;
pub const MAX_COMPACTED_CONTEXT_BYTES: usize = 2 * 1024 * 1024;

const MAX_SOURCE_MESSAGES: usize = 1_024;
const MAX_ACTIVE_EVIDENCE: usize = 512;
const MAX_FACT_CANDIDATES: usize = 8_192;

const OMISSION_LINEAGE_DOMAIN: &[u8] = b"krw-context-compaction-omissions/v2";

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
    #[serde(default)]
    pub source_object_ids: Vec<String>,
}

/// Fixed-size disclosure of information removed only to satisfy a hard
/// provider-context bound.  The previous representation retained every
/// omitted fact hash in the prompt; with a large ledger that omission list
/// could itself exceed the bound and abort an otherwise answerable run.
///
/// Counts remain human/model-readable while `lineage_hash` cryptographically
/// binds the exact deterministic removal sequence without replaying it into
/// the model context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactionOmissions {
    pub state_payload: bool,
    pub research_projection: bool,
    pub retrieval_status: bool,
    pub evidence_entries: u32,
    pub facts: u32,
    pub calculations: u32,
    pub lineage_hash: ContentHash,
}

impl Default for CompactionOmissions {
    fn default() -> Self {
        Self {
            state_payload: false,
            research_projection: false,
            retrieval_status: false,
            evidence_entries: 0,
            facts: 0,
            calculations: 0,
            lineage_hash: ContentHash::sha256(OMISSION_LINEAGE_DOMAIN),
        }
    }
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
    /// Bounded retrieval provenance remains visible even to the composer,
    /// which intentionally does not see open goals. This prevents a
    /// pagination or dependency warning from turning into a false statement
    /// that the company did not disclose the fact.
    #[serde(default)]
    pub retrieval_status: Option<krw_ontology_adapter::ResearchRetrievalStatus>,
    pub evidence_index: Vec<CompactedEvidenceIndexEntry>,
    pub retained_facts: Vec<CompactedFact>,
    pub omissions: CompactionOmissions,
    pub calculations: Vec<Calculation>,
    /// Release B: the analyst's bounded judgment notes from the
    /// evidence-sufficient handoff. Advisory framing for the writer; the
    /// analyst's own view drops them so research turns cannot loop on them.
    #[serde(default)]
    pub analyst_judgment: Vec<AnalystJudgmentNote>,
}

impl fmt::Debug for CompactedProviderContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompactedProviderContext")
            .field("schema_version", &self.schema_version)
            .field("boundary_hash", &self.boundary_hash)
            .field("state_artifact_hash", &self.state.artifact_hash)
            .field("answerability", &self.answerability)
            .field("has_retrieval_status", &self.retrieval_status.is_some())
            .field("evidence_count", &self.evidence_index.len())
            .field("retained_fact_count", &self.retained_facts.len())
            .field("omissions", &self.omissions)
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
const COMPANY_ANALYST_VIEW_MAX_BYTES: usize = 64 * 1024;
const COMPANY_COMPOSER_VIEW_MAX_BYTES: usize = 64 * 1024;
const SPECIALIZED_COMPOSER_VIEW_MAX_BYTES: usize = 96 * 1024;
const MAX_FACTS_PER_VIEW_EVIDENCE: usize = 6;
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
        let composer_role = is_composer_role(role_id);
        if role_id != ROLE_ANALYST && !composer_role && role_id != ROLE_REPAIR {
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
        let protected_goal_evidence_ids = protected_goal_evidence_ids(self);

        match role_id {
            _ if composer_role => {
                // Composer assembles the answer from established facts and
                // calculations; the OPEN research projection (tool payloads,
                // action scoring, orientation vocabulary) stays hidden. What
                // survives is a sanitized coverage map (release E): each
                // question clause's retrieval text, its goal status, and the
                // prose of what stayed missing — enough for a partial-material
                // answer to frame coverage conditionally without inheriting
                // any execution vocabulary.
                filtered.research_projection =
                    composer_research_projection(filtered.research_projection.take());
                // The settled state artifact is an analyst/control carrier: it
                // includes raw server vocabulary such as clause status codes,
                // ontology calendar buckets, object identifiers, and bounded
                // diagnostics. The composer already receives the exact
                // investor-facing sources of truth below (retained facts,
                // citations, calculations, and the answerability boundary),
                // so showing the raw artifact can only leak implementation
                // labels into Korean prose. Keep the immutable hash/contract
                // as a receipt handle but omit its payload from the
                // composition-only projection.
                filtered.state.payload = Value::Null;
                // Pagination still matters: the writer must not turn a
                // truncated retrieval into a company non-disclosure. Retain
                // only that semantic signal, not raw warning codes, source
                // labels, or routing-period aliases.
                filtered.retrieval_status = filtered
                    .retrieval_status
                    .take()
                    .map(composer_retrieval_status);
                sanitize_composer_period_aliases(&mut filtered);
                // Object identifiers are execution handles for analyst
                // trace/chain calls, not investor-facing evidence. They can
                // embed source routing buckets such as `CY2026Q1`; retaining
                // them in the composer view makes it too easy to echo an
                // internal label even when every factual period is already
                // normalized. Keep them in the durable context and analyst
                // view, but strip them from this answer-writing projection.
                for evidence in &mut filtered.evidence_index {
                    evidence.source_object_ids.clear();
                }
                // Keep evidence_index (citation handles), retained_facts, and
                // calculations in full — these are exactly what composition
                // grounds its claims in.
            }
            ROLE_ANALYST => {
                // The analyst reasons about research coverage; drop the
                // detailed calculation transcript and trim the evidence index
                // to the top-graded entries so the analyst can scan gaps.
                filtered.calculations.clear();
                // Judgment notes are the handoff artifact for the writer.
                // The analyst authored them; re-reading them each turn only
                // invites anchoring on its own prior framing.
                filtered.analyst_judgment.clear();
                trim_evidence_by_grade(&mut filtered.evidence_index, ANALYST_MAX_EVIDENCE);
                retain_facts_for_evidence(&mut filtered);
                // Retain the full research_projection (goals, missing parts,
                // recommended actions) and retained_facts (needed to interpret
                // goal status).
            }
            ROLE_REPAIR => {
                // Minimal defect-relevant slice: state artifact + small fact
                // subset. Drop projection, calculations, judgment notes, and
                // almost all evidence; cap facts.
                filtered.research_projection = None;
                filtered.calculations.clear();
                filtered.analyst_judgment.clear();
                trim_evidence_by_grade(&mut filtered.evidence_index, REPAIR_MAX_FACTS);
                if filtered.retained_facts.len() > REPAIR_MAX_FACTS {
                    let split = filtered.retained_facts.len() - REPAIR_MAX_FACTS;
                    // This is only a prompt-time role projection. The durable
                    // canonical context and its omission receipt remain
                    // unchanged and continue to bind the full retained set.
                    filtered.retained_facts.drain(split..);
                }
            }
            _ => unreachable!("role gate above excludes every other branch"),
        }

        retain_facts_per_evidence(&mut filtered);
        let max_bytes = if composer_role {
            if role_id == ROLE_COMPOSER {
                COMPANY_COMPOSER_VIEW_MAX_BYTES
            } else {
                SPECIALIZED_COMPOSER_VIEW_MAX_BYTES
            }
        } else {
            COMPANY_ANALYST_VIEW_MAX_BYTES
        };
        shrink_role_view_best_effort(&mut filtered, max_bytes, &protected_goal_evidence_ids)?;

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

fn protected_goal_evidence_ids(
    context: &CompactedProviderContext,
) -> std::collections::BTreeSet<String> {
    context
        .research_projection
        .as_ref()
        .into_iter()
        .flat_map(|projection| projection.graph.goals())
        .flat_map(|goal| goal.evidence_ids.iter().cloned())
        .collect()
}

fn retain_facts_for_evidence(context: &mut CompactedProviderContext) {
    let evidence_ids = context
        .evidence_index
        .iter()
        .map(|entry| entry.evidence_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    context
        .retained_facts
        .retain(|fact| evidence_ids.contains(fact.evidence_id.as_str()));
}

fn retain_facts_per_evidence(context: &mut CompactedProviderContext) {
    let mut counts = BTreeMap::<String, usize>::new();
    context.retained_facts.retain(|fact| {
        let count = counts.entry(fact.evidence_id.clone()).or_default();
        if *count >= MAX_FACTS_PER_VIEW_EVIDENCE {
            false
        } else {
            *count += 1;
            true
        }
    });
}

/// Best-effort prompt-view bound. The durable compaction receipt is computed
/// from the unfiltered context, so a large protected witness set must remain a
/// valid over-target view rather than becoming a new runtime failure.
fn shrink_role_view_best_effort(
    context: &mut CompactedProviderContext,
    max_bytes: usize,
    protected: &std::collections::BTreeSet<String>,
) -> Result<(), CompactionError> {
    while serde_jcs::to_vec(&*context)?.len() > max_bytes {
        if let Some(index) = context
            .evidence_index
            .iter()
            .rposition(|entry| !protected.contains(&entry.evidence_id))
        {
            let removed = context.evidence_index.remove(index).evidence_id;
            context
                .retained_facts
                .retain(|fact| fact.evidence_id != removed);
            continue;
        }
        if let Some(index) = context
            .retained_facts
            .iter()
            .rposition(|fact| !protected.contains(&fact.evidence_id))
        {
            context.retained_facts.remove(index);
            continue;
        }
        // Essential state, protected goal witnesses, and the remaining
        // investor-facing facts alone exceed the soft role target. Keep them
        // and let the caller send a valid, slightly larger prompt view.
        break;
    }
    Ok(())
}

/// Every visible final-writer role uses either `composer` or a specialized
/// `<workflow>_composer` identifier. They share the same safety requirement:
/// no backend routing/control fields belong in the investor-facing prompt.
/// Keep this classification local and deterministic rather than duplicating a
/// finite list that silently misses a newly added specialized workflow.
fn is_composer_role(role_id: &str) -> bool {
    role_id == ROLE_COMPOSER || role_id.ends_with("_composer")
}

/// Composer-visible retrieval status contains only the fact that the source
/// set may be incomplete. Raw warning codes and source anchors are planning
/// diagnostics and routinely contain internal vocabulary (`CY...` buckets,
/// state names, or provider-specific reason strings) that must not shape
/// investor prose. The evidence ledger retains public citations separately.
fn composer_retrieval_status(status: ResearchRetrievalStatus) -> ResearchRetrievalStatus {
    ResearchRetrievalStatus {
        source_anchors: Vec::new(),
        continuation: status.continuation.map(|continuation| Continuation {
            has_more: continuation.has_more,
            omitted_evidence_count: continuation.omitted_evidence_count,
            reason: None,
        }),
        warnings: Vec::new(),
        supplemental_reads: status
            .supplemental_reads
            .into_iter()
            .rev()
            .take(MAX_SUPPLEMENTAL_READ_STATUSES)
            .map(|read| SupplementalReadStatus {
                kind: read.kind,
                result_count: read.result_count,
                has_more: read.has_more,
                next_offset: None,
                warning_codes: Vec::new(),
            })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect(),
    }
}

/// The composer's slice of the research projection: a coverage map, nothing
/// else. Clause-level goals keep their status and their calculation links
/// (the calculation channel the writer already sees); execution-shaped
/// content — recommended tools, exact query payloads, orientation terms,
/// evidence ids, directness ceilings, coverage parts-per-million, missing-part
/// reason codes — is stripped so the writer learns WHAT was established and
/// misses, never HOW the runtime would chase it.
fn composer_research_projection(
    projection: Option<ResearchPlanningProjection>,
) -> Option<ResearchPlanningProjection> {
    let mut projection = projection?;
    let clause_goals: Vec<EvidenceGoal> = projection
        .graph
        .goals()
        .filter(|goal| goal.dependencies.is_empty())
        .map(|goal| EvidenceGoal {
            goal_id: goal.goal_id.clone(),
            required: goal.required,
            weight: goal.weight,
            dependencies: Vec::new(),
            directness: DirectnessRequirement::Related,
            calculation_required: false,
            status: goal.status,
            // Kept status-consistent: the graph validator rejects a
            // satisfied/partial goal whose coverage was zeroed, and the
            // number itself carries no execution vocabulary.
            coverage_ppm: goal.coverage_ppm,
            evidence_ids: Vec::new(),
            calculation_ids: goal.calculation_ids.clone(),
            event_premise: goal.event_premise,
        })
        .collect();
    if clause_goals.is_empty() && projection.clauses.is_empty() {
        return None;
    }
    projection.graph = EvidenceGoalGraph::new(clause_goals).ok()?;
    projection.recommended_actions = Vec::new();
    projection.exact_precise_query_candidates = Vec::new();
    projection.orientation_vocabulary = Vec::new();
    projection.retrieval_status = ResearchRetrievalStatus {
        source_anchors: Vec::new(),
        continuation: None,
        warnings: Vec::new(),
        supplemental_reads: Vec::new(),
    };
    for part in &mut projection.missing_parts {
        part.code = String::new();
    }
    Some(projection)
}

/// `CY2026Q1` is an ontology routing bucket, not an investor-facing fiscal
/// label. The analyst may need it to choose another retrieval action, but the
/// composer must rely on an observed end date, a documented fiscal label, or
/// the filing type instead. Removing these aliases from the composition view
/// prevents a model from accidentally presenting routing vocabulary as a
/// reported accounting period while preserving every real number and date.
fn sanitize_composer_period_aliases(context: &mut CompactedProviderContext) {
    for evidence in &mut context.evidence_index {
        evidence.period = public_period_label(evidence.period.take());
        evidence.citation.period = public_period_label(evidence.citation.period.take());
        evidence.citation.title = redact_calendar_bucket_tokens(&evidence.citation.title);
    }
    for fact in &mut context.retained_facts {
        fact.period = public_period_label(fact.period.take());
        redact_calendar_bucket_values(&mut fact.value);
    }
    for calculation in &mut context.calculations {
        calculation.period = public_period_label(calculation.period.take());
        redact_calendar_bucket_values(&mut calculation.output);
    }
}

fn public_period_label(value: Option<String>) -> Option<String> {
    value.filter(|value| !is_ontology_calendar_bucket(value))
}

fn redact_calendar_bucket_values(value: &mut Value) {
    match value {
        Value::String(text) if is_ontology_calendar_bucket(text) => *value = Value::Null,
        Value::Array(values) => values.iter_mut().for_each(redact_calendar_bucket_values),
        Value::Object(values) => values.values_mut().for_each(redact_calendar_bucket_values),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn is_ontology_calendar_bucket(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 6 && bytes.len() != 8 {
        return false;
    }
    if bytes.get(0..2) != Some(b"CY") || !bytes[2..6].iter().all(u8::is_ascii_digit) {
        return false;
    }
    bytes.len() == 6 || (bytes[6] == b'Q' && matches!(bytes[7], b'1'..=b'4'))
}

/// Remove embedded calendar-bucket tokens from a citation title without
/// changing its document name or issuer text. Titles are advisory context;
/// `document_type` remains the authoritative public citation field.
fn redact_calendar_bucket_tokens(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut result = String::with_capacity(value.len());
    let mut cursor = 0;
    let mut index = 0;
    while index < bytes.len() {
        let candidate_end = if bytes
            .get(index..index + 6)
            .is_some_and(|part| part[0..2] == *b"CY" && part[2..6].iter().all(u8::is_ascii_digit))
        {
            if bytes
                .get(index + 6..index + 8)
                .is_some_and(|part| part[0] == b'Q' && matches!(part[1], b'1'..=b'4'))
            {
                Some(index + 8)
            } else {
                Some(index + 6)
            }
        } else {
            None
        };
        if let Some(end) = candidate_end {
            result.push_str(&value[cursor..index]);
            cursor = end;
            index = end;
        } else {
            index += 1;
        }
    }
    result.push_str(&value[cursor..]);
    result
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
    pub omission_summary_hash: ContentHash,
    pub calculation_set_hash: ContentHash,
    /// `None` covers both pre-B receipts and runs whose analyst handed over
    /// no judgment notes; a non-empty note set is always hashed.
    #[serde(default)]
    pub analyst_judgment_hash: Option<ContentHash>,
    pub active_evidence_count: u16,
    pub retained_fact_count: u16,
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
            || hash_value(&self.context.omissions)? != self.receipt.omission_summary_hash
            || hash_slice(&self.context.calculations)? != self.receipt.calculation_set_hash
            || hash_optional_slice(&self.context.analyst_judgment)?
                != self.receipt.analyst_judgment_hash
            || usize::from(self.receipt.active_evidence_count) != self.context.evidence_index.len()
            || usize::from(self.receipt.retained_fact_count) != self.context.retained_facts.len()
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
    pub analyst_judgment: &'a [AnalystJudgmentNote],
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
            source_object_ids: record.source_object_ids.clone(),
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
    evidence_index.sort_by(|left, right| left.evidence_id.cmp(&right.evidence_id));
    candidates.sort_by(|left, right| {
        Reverse(left.priority)
            .cmp(&Reverse(right.priority))
            .then_with(|| left.fact.evidence_id.cmp(&right.fact.evidence_id))
            .then_with(|| left.fact.fact_index.cmp(&right.fact.fact_index))
    });
    // Wave 4 answer-always policy: compaction is a total function. A ledger
    // beyond the durable count bounds previously aborted the run with
    // `Limit("evidence or facts")`; it now degrades deterministically to a
    // bounded ranked view with a reason-coded omission receipt.
    let mut omissions = CompactionOmissions::default();
    let protected = unresolved_required_goal_evidence_ids(input.research_projection);
    enforce_count_bounds(
        &mut evidence_index,
        &mut candidates,
        &protected,
        &mut omissions,
    )?;
    let calculations = input.calculations.values().cloned().collect::<Vec<_>>();
    let research_projection = input.research_projection.cloned();
    let retrieval_status = research_projection
        .as_ref()
        .map(|projection| projection.retrieval_status.clone());

    let mut context = CompactedProviderContext {
        schema_version: COMPACTED_CONTEXT_SCHEMA_VERSION,
        authority: "validated_state_and_committed_evidence_only".into(),
        boundary_hash: boundary_hash.clone(),
        state,
        answerability: input.ledger.answerability(),
        research_projection,
        retrieval_status,
        evidence_index,
        retained_facts: candidates
            .iter()
            .map(|candidate| candidate.fact.clone())
            .collect(),
        omissions,
        calculations,
        analyst_judgment: input.analyst_judgment.to_vec(),
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
        omission_summary_hash: hash_value(&context.omissions)?,
        calculation_set_hash: hash_slice(&context.calculations)?,
        analyst_judgment_hash: hash_optional_slice(&context.analyst_judgment)?,
        active_evidence_count: u16::try_from(context.evidence_index.len())
            .map_err(|_| CompactionError::Limit("active evidence"))?,
        retained_fact_count: u16::try_from(context.retained_facts.len())
            .map_err(|_| CompactionError::Limit("retained facts"))?,
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

/// Evidence IDs pinned by still-open required goals in the research
/// projection. These witnesses are the "unresolved required goal" material
/// that must survive count-bound degradation before any ranked cut.
fn unresolved_required_goal_evidence_ids(
    projection: Option<&ResearchPlanningProjection>,
) -> std::collections::BTreeSet<String> {
    projection
        .into_iter()
        .flat_map(|projection| projection.graph.goals())
        .filter(|goal| goal.required && goal.status != GoalStatus::Satisfied)
        .flat_map(|goal| goal.evidence_ids.iter().cloned())
        .collect()
}

/// Deterministic count-bound degradation for pathological input volume.
///
/// Replaces the previous hard error for ledgers beyond the durable count
/// bounds (`MAX_ACTIVE_EVIDENCE` / `MAX_FACT_CANDIDATES`). Per the runtime
/// failure policy (doc/refetoring/03-runtime-failure-policy.md, compaction):
///
/// 1. unresolved-required-goal witnesses are reserved first,
/// 2. one representative per (entity, period) group keeps minimal
///    ticker/period coverage before any group takes a second slot,
/// 3. everything else competes on (grade, directness, strong-claim) rank,
/// 4. every dropped item increments an omitted count and extends the
///    `lineage_hash` chain with a reason-coded link, so the receipt binds
///    exactly what was removed without replaying it into the prompt.
///
/// Inputs within the bounds keep every entry: this path is a no-op and the
/// happy-path canonical bytes and receipt hashes are unchanged.
fn enforce_count_bounds(
    evidence_index: &mut Vec<CompactedEvidenceIndexEntry>,
    candidates: &mut Vec<FactCandidate>,
    protected: &std::collections::BTreeSet<String>,
    omissions: &mut CompactionOmissions,
) -> Result<(), CompactionError> {
    if evidence_index.len() > MAX_ACTIVE_EVIDENCE {
        let kept = select_evidence_under_count_bound(evidence_index, protected);
        let mut dropped_evidence = Vec::new();
        evidence_index.retain(|entry| {
            if kept.contains(&entry.evidence_id) {
                true
            } else {
                dropped_evidence.push(entry.clone());
                false
            }
        });
        // Record the removal in canonical id order so the lineage chain is
        // independent of the ranking pass.
        dropped_evidence.sort_by(|left, right| left.evidence_id.cmp(&right.evidence_id));
        for entry in &dropped_evidence {
            omissions.evidence_entries = omissions.evidence_entries.saturating_add(1);
            record_omission(omissions, "evidence_overcount", &hash_value(entry)?)?;
        }
        // A fact without its evidence entry has no citation handle and cannot
        // ground a claim; it is omitted alongside its evidence.
        let mut dropped_facts = Vec::new();
        candidates.retain(|candidate| {
            if kept.contains(&candidate.fact.evidence_id) {
                true
            } else {
                dropped_facts.push(candidate.fact.fact_ref.clone());
                false
            }
        });
        for fact_ref in &dropped_facts {
            omissions.facts = omissions.facts.saturating_add(1);
            record_omission(omissions, "fact_evidence_dropped", fact_ref)?;
        }
    }
    if candidates.len() > MAX_FACT_CANDIDATES {
        // Stable priority partition: unresolved-goal witnesses first, then the
        // ranked remainder. Both runs keep the deterministic candidate order
        // (priority desc, evidence_id, fact_index), so the lowest-priority
        // facts fall off the tail.
        let mut ordered = Vec::with_capacity(candidates.len());
        ordered.extend(
            candidates
                .iter()
                .filter(|candidate| protected.contains(&candidate.fact.evidence_id))
                .cloned(),
        );
        ordered.extend(
            candidates
                .iter()
                .filter(|candidate| !protected.contains(&candidate.fact.evidence_id))
                .cloned(),
        );
        let dropped = ordered.split_off(MAX_FACT_CANDIDATES);
        for candidate in &dropped {
            omissions.facts = omissions.facts.saturating_add(1);
            record_omission(omissions, "fact_overcount", &candidate.fact.fact_ref)?;
        }
        *candidates = ordered;
    }
    Ok(())
}

/// Rank-then-reserve selection of at most `MAX_ACTIVE_EVIDENCE` entries.
fn select_evidence_under_count_bound(
    evidence_index: &[CompactedEvidenceIndexEntry],
    protected: &std::collections::BTreeSet<String>,
) -> std::collections::BTreeSet<String> {
    let mut ranked: Vec<&CompactedEvidenceIndexEntry> = evidence_index.iter().collect();
    ranked.sort_by(|left, right| {
        right
            .grade
            .cmp(&left.grade)
            .then_with(|| right.directness.cmp(&left.directness))
            .then_with(|| right.strong_claim_allowed.cmp(&left.strong_claim_allowed))
            .then_with(|| left.evidence_id.cmp(&right.evidence_id))
    });
    let mut kept = std::collections::BTreeSet::new();
    // Pass 1: witnesses of still-open required goals.
    for entry in &ranked {
        if kept.len() >= MAX_ACTIVE_EVIDENCE {
            break;
        }
        if protected.contains(&entry.evidence_id) {
            kept.insert(entry.evidence_id.clone());
        }
    }
    // Pass 2: the highest-ranked entry of every (entity, period) group, so a
    // single ticker/period flood cannot evict the minimal coverage of every
    // other group.
    let mut represented = std::collections::BTreeSet::new();
    for entry in &ranked {
        if kept.len() >= MAX_ACTIVE_EVIDENCE {
            break;
        }
        if represented.insert((entry.entity.clone(), entry.period.clone())) {
            kept.insert(entry.evidence_id.clone());
        }
    }
    // Pass 3: everything else competes on rank.
    for entry in &ranked {
        if kept.len() >= MAX_ACTIVE_EVIDENCE {
            break;
        }
        kept.insert(entry.evidence_id.clone());
    }
    kept
}

fn shrink_to_fit(
    context: &mut CompactedProviderContext,
    max_bytes: usize,
) -> Result<(), CompactionError> {
    if serialized_len(context)? <= max_bytes {
        return Ok(());
    }

    // A large workflow payload is control state, not answer evidence. Keep its
    // immutable artifact/payload hashes and remove the bytes before sacrificing
    // useful evidence facts when it is itself a material part of the budget.
    if serialized_len(&context.state.payload)? > max_bytes / 4 {
        omit_state_payload(context)?;
    }

    // Facts are the only segment whose count can reach MAX_FACT_CANDIDATES
    // (8192), so a one-pop-per-full-re-serialization loop is quadratic and
    // would stall a pathological — but now admissible — compaction for
    // minutes. JCS arrays are additive: `[a,b]` serializes to brackets plus
    // each item plus one comma per item, so the serialized length after
    // popping a suffix of facts can be predicted in O(1) per pop from the
    // item's standalone length. Pop by prediction first, then let the exact
    // loop below absorb the residual drift from the omission counter's digit
    // growth (bounded to a couple of pops).
    if serialized_len(context)? > max_bytes {
        let mut predicted = serialized_len(context)?;
        while predicted > max_bytes {
            let item = match context.retained_facts.last() {
                Some(candidate) => {
                    serde_jcs::to_vec(candidate)?.len()
                        + usize::from(context.retained_facts.len() > 1)
                }
                None => break,
            };
            predicted = predicted.saturating_sub(item);
            let Some(removed) = context.retained_facts.pop() else {
                break;
            };
            context.omissions.facts = context.omissions.facts.saturating_add(1);
            record_omission(&mut context.omissions, "fact", &removed.fact_ref)?;
        }
    }

    while serialized_len(context)? > max_bytes {
        let Some(removed) = context.retained_facts.pop() else {
            break;
        };
        context.omissions.facts = context.omissions.facts.saturating_add(1);
        record_omission(&mut context.omissions, "fact", &removed.fact_ref)?;
    }

    if serialized_len(context)? > max_bytes {
        omit_state_payload(context)?;
    }

    if serialized_len(context)? > max_bytes {
        if let Some(projection) = context.research_projection.take() {
            let hash = hash_value(&projection)?;
            context.omissions.research_projection = true;
            record_omission(&mut context.omissions, "research_projection", &hash)?;
        }
    }

    if serialized_len(context)? > max_bytes {
        if let Some(status) = context.retrieval_status.take() {
            let hash = hash_value(&status)?;
            context.omissions.retrieval_status = true;
            record_omission(&mut context.omissions, "retrieval_status", &hash)?;
        }
    }

    while serialized_len(context)? > max_bytes {
        let Some(calculation) = context.calculations.pop() else {
            break;
        };
        let hash = hash_value(&calculation)?;
        context.omissions.calculations = context.omissions.calculations.saturating_add(1);
        record_omission(&mut context.omissions, "calculation", &hash)?;
    }

    while serialized_len(context)? > max_bytes {
        let Some(evidence) = context.evidence_index.pop() else {
            break;
        };
        let hash = hash_value(&evidence)?;
        context.omissions.evidence_entries = context.omissions.evidence_entries.saturating_add(1);
        record_omission(&mut context.omissions, "evidence", &hash)?;
    }

    // Everything above this line is auxiliary prompt material. What remains
    // after these loops is the bounded essential view: the identity core
    // (boundary hash, state artifact/contract pins, answerability boundary)
    // plus the omission receipt. Every remaining segment is fixed-size, so
    // with the configured 32 KiB minimum the essential view always fits and
    // compaction stays a total function — size pressure alone can never fail
    // a run. If a future field ever pushes the essential view past a
    // configured bound, the over-target view is returned unchanged, the same
    // policy `shrink_role_view_best_effort` applies to role views, instead of
    // aborting an otherwise answerable run.
    Ok(())
}

fn serialized_len<T: Serialize>(value: &T) -> Result<usize, CompactionError> {
    Ok(serde_jcs::to_vec(value)?.len())
}

fn omit_state_payload(context: &mut CompactedProviderContext) -> Result<(), CompactionError> {
    if context.omissions.state_payload || context.state.payload.is_null() {
        return Ok(());
    }
    let payload_hash = context.state.payload_hash.clone();
    context.state.payload = Value::Null;
    context.omissions.state_payload = true;
    record_omission(&mut context.omissions, "state_payload", &payload_hash)
}

fn record_omission(
    summary: &mut CompactionOmissions,
    kind: &'static str,
    item_hash: &ContentHash,
) -> Result<(), CompactionError> {
    #[derive(Serialize)]
    struct OmissionLink<'a> {
        domain: &'static str,
        previous: &'a ContentHash,
        kind: &'static str,
        item_hash: &'a ContentHash,
    }
    summary.lineage_hash = ContentHash::sha256(serde_jcs::to_vec(&OmissionLink {
        domain: "krw-context-compaction-omission-link/v2",
        previous: &summary.lineage_hash,
        kind,
        item_hash,
    })?);
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

fn hash_value<T: Serialize>(value: &T) -> Result<ContentHash, CompactionError> {
    Ok(ContentHash::sha256(serde_jcs::to_vec(value)?))
}

fn hash_slice<T: Serialize>(value: &[T]) -> Result<ContentHash, CompactionError> {
    Ok(ContentHash::sha256(serde_jcs::to_vec(value)?))
}

/// `None` for an empty slice so pre-B receipts and note-less runs share one
/// canonical shape; any actual note set is hashed like every other channel.
fn hash_optional_slice<T: Serialize>(
    value: &[T],
) -> Result<Option<ContentHash>, CompactionError> {
    if value.is_empty() {
        return Ok(None);
    }
    hash_slice(value).map(Some)
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
    use krw_ontology_adapter::SupplementalReadKind;
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

    fn artifact_and_boundary_with_payload(
        payload: Value,
    ) -> (
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
                    payload,
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

    fn artifact_and_boundary() -> (
        krw_agent_state_artifact::ValidatedArtifact,
        PhaseCompactionBoundaryV1,
    ) {
        artifact_and_boundary_with_payload(json!({
            "period":"FY2025",
            "not_deteriorating":false
        }))
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
            label: None,
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
            analyst_judgment: &[],
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
    fn compaction_keeps_traceable_object_ids_and_safe_retrieval_status() {
        let (artifact, boundary) = artifact_and_boundary();
        let mut record = evidence(
            "evidence-1",
            vec![NormalizedFact {
                subject: "TEST".into(),
                predicate: "revenue".into(),
                value: json!(100),
                unit: Some("USD".into()),
                period: Some("CY2026Q1".into()),
            }],
        );
        record.source_object_ids = vec!["metric:TEST:revenue:CY2026Q1".into()];
        let ledger = EvidenceLedger::from_records(vec![record]).unwrap();
        let projection: ResearchPlanningProjection = serde_json::from_value(json!({
            "graph": {"version":1,"goals":{},"original_order":[]},
            "clauses": [],
            "missing_parts": [],
            "recommended_actions": [],
            "exact_precise_query_candidates": [{
                "clause_id":"segment_revenue",
                "ticker":"TEST",
                "topic":"TEST product revenue by segment",
                "object_types":["MetricObservation"],
                "response_detail":"full",
                "limit":20
            }],
            "retrieval_status": {
                "source_anchors": [{
                    "ticker":"TEST",
                    "period":"CY2026Q1",
                    "document_type":"10-Q",
                    "role":"current_driver",
                    "source_label":"TEST Form 10-Q"
                }],
                "continuation": {"has_more":true,"omitted_evidence_count":4,"reason":"limit"},
                "warnings": ["planned_evidence_truncated"],
                "supplemental_reads": [
                    {
                        "kind":"retrieved",
                        "result_count":20,
                        "has_more":true,
                        "next_offset":20,
                        "warning_codes":["supplemental_truncated"]
                    },
                    {
                        "kind":"ambiguous",
                        "result_count":0,
                        "has_more":false,
                        "next_offset":null,
                        "warning_codes":["supplemental_ambiguous"]
                    }
                ]
            }
        }))
        .unwrap();
        let output = compact(&CompactionInput {
            boundary: &boundary,
            state_artifact: &artifact,
            source_messages: &[ProviderMessage::assistant("settled")],
            ledger: &ledger,
            calculations: &BTreeMap::new(),
            research_projection: Some(&projection),
            analyst_judgment: &[],
            max_context_bytes: DEFAULT_MAX_COMPACTED_CONTEXT_BYTES,
        })
        .unwrap();

        assert_eq!(
            output.context.evidence_index[0].source_object_ids,
            ["metric:TEST:revenue:CY2026Q1"]
        );
        let status = output.context.retrieval_status.as_ref().unwrap();
        assert!(status.continuation.as_ref().unwrap().has_more);
        assert_eq!(status.source_anchors[0].role, "current_driver");
        assert_eq!(status.supplemental_reads.len(), 2);
        assert_eq!(status.supplemental_reads[0].result_count, 20);
        assert_eq!(
            status.supplemental_reads[1].kind,
            SupplementalReadKind::Ambiguous
        );
        let composer = output.context.view_for_role(ROLE_COMPOSER).unwrap();
        assert!(composer.canonical().contains("\"kind\":\"retrieved\""));
        assert!(composer.canonical().contains("\"kind\":\"ambiguous\""));
        assert!(!composer.canonical().contains("\"next_offset\":20"));
        assert!(!composer.canonical().contains("supplemental_ambiguous"));
        assert!(composer.canonical().contains("\"has_more\":true"));
        assert!(
            composer
                .canonical()
                .contains("\"omitted_evidence_count\":4")
        );
        assert!(!composer.canonical().contains("planned_evidence_truncated"));
        assert!(!composer.canonical().contains("current_driver"));
        assert!(!composer.canonical().contains("CY2026Q1"));
        assert!(
            !composer
                .canonical()
                .contains("metric:TEST:revenue:CY2026Q1")
        );
        let analyst = output.context.view_for_role(ROLE_ANALYST).unwrap();
        assert!(
            analyst
                .canonical()
                .contains("TEST product revenue by segment")
        );
        assert!(analyst.canonical().contains("\"response_detail\":\"full\""));
        assert!(analyst.canonical().contains("metric:TEST:revenue:CY2026Q1"));
    }

    #[test]
    fn composer_view_removes_raw_control_state_but_keeps_public_facts() {
        let mut context = fixture_context_with_projection();
        context.state.payload = json!({
            "planner_control": "conditional",
            "routing_period": "CY2026Q1",
            "private_clause_id": "revenue_driver"
        });
        context.retained_facts[0].period = Some("CY2026Q1".into());
        context.retained_facts[0].value = json!({
            "routing_period": "CY2026Q1",
            "reported_end_date": "2026-03-28",
            "amount": 123
        });
        context.evidence_index[0].citation.title = "TEST CY2026Q1 Form 10-Q".into();
        context.evidence_index[0].citation.period = Some("CY2026Q1".into());

        let composer = context.view_for_role(ROLE_COMPOSER).unwrap();
        assert!(composer.canonical().contains("\"payload\":null"));
        assert!(!composer.canonical().contains("planner_control"));
        assert!(!composer.canonical().contains("private_clause_id"));
        assert!(!composer.canonical().contains("conditional"));
        assert!(!composer.canonical().contains("CY2026Q1"));
        assert!(composer.canonical().contains("2026-03-28"));
        assert!(composer.canonical().contains("\"amount\":123"));
        assert!(composer.canonical().contains("TEST  Form 10-Q"));

        let analyst = context.view_for_role(ROLE_ANALYST).unwrap();
        assert!(analyst.canonical().contains("planner_control"));
        assert!(analyst.canonical().contains("CY2026Q1"));

        let specialized_composer = context.view_for_role("earnings_composer").unwrap();
        assert_eq!(specialized_composer.canonical(), composer.canonical());
        assert!(!specialized_composer.canonical().contains("planner_control"));
        assert!(!specialized_composer.canonical().contains("CY2026Q1"));
    }

    #[test]
    fn calendar_bucket_detection_does_not_remove_public_dates_or_fiscal_labels() {
        for bucket in ["CY2026", "CY2026Q1", "CY1999Q4"] {
            assert!(is_ontology_calendar_bucket(bucket), "{bucket}");
        }
        for public_label in [
            "FY2026",
            "FY2026Q1",
            "2026-03-28",
            "Q1 2026",
            "CY2026Q5",
            "CY20261",
        ] {
            assert!(
                !is_ontology_calendar_bucket(public_label),
                "{public_label} must remain available to the composer"
            );
        }
        assert_eq!(
            redact_calendar_bucket_tokens("TEST CY2026Q1 Form 10-Q"),
            "TEST  Form 10-Q"
        );
        assert_eq!(
            redact_calendar_bucket_tokens("FY2026 Form 10-K"),
            "FY2026 Form 10-K"
        );
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
            analyst_judgment: &[],
            max_context_bytes: 128 * 1024,
        })
        .unwrap();
        assert!(output.context.omissions.facts > 0);
        assert!(output.canonical_context().contains("999999999"));
        assert!(output.canonical_context().contains("FY2025"));
    }

    #[test]
    fn oversized_state_payload_degrades_to_a_hash_bound_context_instead_of_failing() {
        let (artifact, boundary) = artifact_and_boundary_with_payload(json!({
            "opaque_state": "x".repeat(128 * 1024)
        }));

        let output = compact(&CompactionInput {
            boundary: &boundary,
            state_artifact: &artifact,
            source_messages: &[ProviderMessage::assistant("settled")],
            ledger: &EvidenceLedger::default(),
            calculations: &BTreeMap::new(),
            research_projection: None,
            analyst_judgment: &[],
            max_context_bytes: MIN_COMPACTED_CONTEXT_BYTES,
        })
        .expect("oversized auxiliary state payload must not abort the research run");

        output.verify().unwrap();
        assert!(output.receipt.compacted_bytes <= MIN_COMPACTED_CONTEXT_BYTES as u64);
        assert_eq!(output.context.state.payload, Value::Null);
        assert!(output.context.omissions.state_payload);
    }

    #[test]
    fn orientation_vocabulary_survives_into_the_planner_and_analyst_views() {
        let (artifact, boundary) = artifact_and_boundary();
        let projection: ResearchPlanningProjection = serde_json::from_value(json!({
            "graph": {
                "version": 1,
                "goals": {
                    "goal-coverage": {
                        "goal_id": "goal-coverage",
                        "required": true,
                        "weight": 100,
                        "dependencies": [],
                        "directness": "direct",
                        "calculation_required": false,
                        "status": "unresolved",
                        "coverage_ppm": 0,
                        "evidence_ids": [],
                        "calculation_ids": []
                    }
                },
                "original_order": ["goal-coverage"]
            },
            "clauses": [],
            "missing_parts": [],
            "recommended_actions": [],
            "orientation_vocabulary": [
                {
                    "term": "Component Procurement",
                    "document_type": "10-K",
                    "period": "2026년"
                },
                {"term": "Services Growth"}
            ]
        }))
        .unwrap();

        let output = compact(&CompactionInput {
            boundary: &boundary,
            state_artifact: &artifact,
            source_messages: &[ProviderMessage::assistant("settled")],
            ledger: &EvidenceLedger::default(),
            calculations: &BTreeMap::new(),
            research_projection: Some(&projection),
            analyst_judgment: &[],
            max_context_bytes: DEFAULT_MAX_COMPACTED_CONTEXT_BYTES,
        })
        .unwrap();

        // The analyst keeps the full research projection, so the advisory
        // orientation wording reaches the model exactly when it is planning
        // the next evidence query; the planner (an unrecognized role here)
        // gets the full canonical view.
        let analyst = output.context.view_for_role(ROLE_ANALYST).unwrap();
        assert!(analyst.canonical.contains("Component Procurement"));
        assert!(analyst.canonical.contains("orientation_vocabulary"));
        let planner = output.context.view_for_role("planner").unwrap();
        assert!(planner.canonical.contains("Component Procurement"));
        // The composer and repair views intentionally drop the whole research
        // projection, vocabulary included.
        let composer = output.context.view_for_role(ROLE_COMPOSER).unwrap();
        assert!(!composer.canonical.contains("Component Procurement"));
        let repair = output.context.view_for_role(ROLE_REPAIR).unwrap();
        assert!(!repair.canonical.contains("Component Procurement"));
    }

    #[test]
    fn more_than_512_active_evidence_records_degrade_to_a_bounded_view_with_receipt() {
        let (artifact, boundary) = artifact_and_boundary();
        let mut ledger = EvidenceLedger::default();
        // 530 low-grade filler records spread across 35 (entity, period)
        // groups plus one weak goal witness that only the unresolved required
        // goal protects. 531 active records exceed MAX_ACTIVE_EVIDENCE.
        for index in 0..530 {
            let mut record = evidence(
                &format!("evidence-{index:03}"),
                vec![NormalizedFact {
                    subject: format!("subject-{index}"),
                    predicate: "revenue".into(),
                    value: json!(index),
                    unit: None,
                    period: None,
                }],
            );
            record.entity = Some(format!("TICKER{}", index % 7));
            record.period = Some(format!("FY{}", 2015 + index % 5));
            record.grade = EvidenceGrade::Weak;
            record.refutes.clear();
            ledger.append(record).unwrap();
        }
        let mut goal_witness = evidence(
            "evidence-goal",
            vec![NormalizedFact {
                subject: "TEST".into(),
                predicate: "goal_witness".into(),
                value: json!(7),
                unit: Some("USD".into()),
                period: Some("FY2025".into()),
            }],
        );
        goal_witness.grade = EvidenceGrade::Weak;
        goal_witness.refutes.clear();
        ledger.append(goal_witness).unwrap();
        let projection: ResearchPlanningProjection = serde_json::from_value(json!({
            "graph": {
                "version": 1,
                "goals": {
                    "goal-coverage": {
                        "goal_id": "goal-coverage",
                        "required": true,
                        "weight": 100,
                        "dependencies": [],
                        "directness": "direct",
                        "calculation_required": false,
                        "status": "unresolved",
                        "coverage_ppm": 0,
                        "evidence_ids": ["evidence-goal"],
                        "calculation_ids": []
                    }
                },
                "original_order": ["goal-coverage"]
            },
            "clauses": [],
            "missing_parts": [],
            "recommended_actions": []
        }))
        .unwrap();

        let output = compact(&CompactionInput {
            boundary: &boundary,
            state_artifact: &artifact,
            source_messages: &[ProviderMessage::assistant("settled")],
            ledger: &ledger,
            calculations: &BTreeMap::new(),
            research_projection: Some(&projection),
            analyst_judgment: &[],
            max_context_bytes: DEFAULT_MAX_COMPACTED_CONTEXT_BYTES,
        })
        .expect("an over-count evidence ledger must degrade to a bounded view, not fail the run");

        output.verify().unwrap();
        assert!(output.receipt.compacted_bytes <= DEFAULT_MAX_COMPACTED_CONTEXT_BYTES as u64);
        assert!(output.context.evidence_index.len() <= MAX_ACTIVE_EVIDENCE);
        assert!(output.context.omissions.evidence_entries >= (531 - MAX_ACTIVE_EVIDENCE) as u32);
        assert_ne!(
            output.context.omissions.lineage_hash,
            CompactionOmissions::default().lineage_hash,
            "dropped evidence must be hash-recorded in the omission receipt"
        );
        // Identity material survives the degradation untouched.
        assert_eq!(
            output.context.boundary_hash,
            boundary.boundary_hash().unwrap()
        );
        assert_eq!(output.context.state.contract.id, "state-facts/v1");
        // The unresolved required goal's witness is reserved.
        assert!(
            output
                .context
                .evidence_index
                .iter()
                .any(|entry| entry.evidence_id == "evidence-goal")
        );
        // Minimal per-(ticker, period) evidence is reserved: every one of the
        // 35 filler groups plus the goal witness group is still represented.
        let mut groups = std::collections::BTreeSet::new();
        for entry in &output.context.evidence_index {
            groups.insert((entry.entity.clone(), entry.period.clone()));
        }
        assert_eq!(groups.len(), 36);
    }

    #[test]
    fn more_than_8192_fact_candidates_degrade_to_a_bounded_view_with_receipt() {
        let (artifact, boundary) = artifact_and_boundary();
        let mut ledger = EvidenceLedger::default();
        // 70 records x 120 low-priority text facts = 8400 candidates, plus
        // one top-priority numeric fact -> 8401 > MAX_FACT_CANDIDATES while
        // staying well under the 512-evidence bound.
        for chunk in 0..70 {
            let mut record = evidence(
                &format!("fact-flood-{chunk:02}"),
                (0..120)
                    .map(|index| NormalizedFact {
                        subject: format!("s{chunk}"),
                        predicate: format!("p{index}"),
                        value: json!("x"),
                        unit: None,
                        period: None,
                    })
                    .collect(),
            );
            record.grade = EvidenceGrade::Weak;
            record.refutes.clear();
            ledger.append(record).unwrap();
        }
        ledger
            .append(evidence(
                "evidence-keeper",
                vec![NormalizedFact {
                    subject: "TEST".into(),
                    predicate: "revenue".into(),
                    value: json!(987_654_321),
                    unit: Some("KRW".into()),
                    period: Some("FY2025".into()),
                }],
            ))
            .unwrap();

        let output = compact(&CompactionInput {
            boundary: &boundary,
            state_artifact: &artifact,
            source_messages: &[ProviderMessage::assistant("settled")],
            ledger: &ledger,
            calculations: &BTreeMap::new(),
            research_projection: None,
            analyst_judgment: &[],
            max_context_bytes: DEFAULT_MAX_COMPACTED_CONTEXT_BYTES,
        })
        .expect("an over-count fact ledger must degrade to a bounded view, not fail the run");

        output.verify().unwrap();
        assert!(output.receipt.compacted_bytes <= DEFAULT_MAX_COMPACTED_CONTEXT_BYTES as u64);
        assert!(output.context.omissions.facts >= (8401 - MAX_FACT_CANDIDATES) as u32);
        assert_ne!(
            output.context.omissions.lineage_hash,
            CompactionOmissions::default().lineage_hash
        );
        // The evidence set itself fits, so no evidence entry is omitted.
        assert_eq!(output.context.omissions.evidence_entries, 0);
        // Highest-priority quantitative material survives the cut.
        assert!(output.canonical_context().contains("987654321"));
    }

    #[test]
    fn pathological_volume_at_minimum_bound_produces_a_bounded_essential_view() {
        let (artifact, boundary) = artifact_and_boundary_with_payload(json!({
            "opaque_state": "x".repeat(128 * 1024)
        }));
        let mut ledger = EvidenceLedger::default();
        for index in 0..600 {
            let mut record = evidence(
                &format!("bulk-{index:03}"),
                vec![NormalizedFact {
                    subject: "TEST".into(),
                    predicate: "revenue".into(),
                    value: json!(index),
                    unit: None,
                    period: None,
                }],
            );
            record.grade = EvidenceGrade::Weak;
            record.refutes.clear();
            ledger.append(record).unwrap();
        }

        let output = compact(&CompactionInput {
            boundary: &boundary,
            state_artifact: &artifact,
            source_messages: &[ProviderMessage::assistant("settled")],
            ledger: &ledger,
            calculations: &BTreeMap::new(),
            research_projection: None,
            analyst_judgment: &[],
            max_context_bytes: MIN_COMPACTED_CONTEXT_BYTES,
        })
        .expect("pathological volume must produce a bounded essential view, not fail the run");

        output.verify().unwrap();
        assert!(output.receipt.compacted_bytes <= MIN_COMPACTED_CONTEXT_BYTES as u64);
        assert!(output.context.evidence_index.len() <= MAX_ACTIVE_EVIDENCE);
        // The oversized auxiliary payload degrades to its hash handle.
        assert!(output.context.omissions.state_payload);
        assert_eq!(output.context.state.payload, Value::Null);
        // Count- and byte-bound omissions are both receipted.
        assert!(output.context.omissions.evidence_entries >= (600 - MAX_ACTIVE_EVIDENCE) as u32);
        assert!(output.context.omissions.facts >= (600 - MAX_ACTIVE_EVIDENCE) as u32);
        // The irreducible identity core survives.
        assert_eq!(
            output.context.boundary_hash,
            boundary.boundary_hash().unwrap()
        );
        assert_eq!(
            output.context.state.artifact_hash,
            artifact.artifact_hash().unwrap()
        );
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
            analyst_judgment: &[],
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
                analyst_judgment: &[],
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
            label: None,
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
            analyst_judgment: &[],
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

        // Composer: the projection survives only as the sanitized coverage
        // map — the clause goal's status is visible, its reason codes are
        // not, and the calculation transcript is still present beside it.
        assert!(composer.canonical().contains("goal-coverage"));
        assert!(composer.canonical().contains("unresolved"));
        assert!(!composer.canonical().contains("missing_counter_evidence"));
        assert!(composer.canonical().contains("6172839.455"));
        // Analyst: projection (goals + missing parts) is present in full; the
        // detailed calculation transcript is gone.
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

    #[test]
    fn composer_coverage_map_keeps_clause_queries_and_drops_execution_vocabulary() {
        // A projection with everything the composer must never see: a
        // calculation goal hanging off a clause goal, a recommended tool
        // action, an exact query payload, and orientation vocabulary.
        let (artifact, boundary) = artifact_and_boundary();
        let projection: ResearchPlanningProjection = serde_json::from_value(json!({
            "graph": {
                "version": 1,
                "goals": {
                    "goal-clause-rev": {
                        "goal_id": "goal-clause-rev",
                        "required": true,
                        "weight": 100,
                        "dependencies": [],
                        "directness": "metric_lineage",
                        "calculation_required": true,
                        "status": "partial",
                        "coverage_ppm": 400000,
                        "evidence_ids": ["ev-1"],
                        "calculation_ids": ["calc-share"]
                    },
                    "goal-clause-rev-growth": {
                        "goal_id": "goal-clause-rev-growth",
                        "required": true,
                        "weight": 100,
                        "dependencies": ["goal-clause-rev"],
                        "directness": "metric_lineage",
                        "calculation_required": true,
                        "status": "satisfied",
                        "coverage_ppm": 1000000,
                        "evidence_ids": ["ev-2"],
                        "calculation_ids": ["calc-growth"]
                    }
                },
                "original_order": ["goal-clause-rev", "goal-clause-rev-growth"]
            },
            "clauses": [
                {"clause_id": "clause-rev", "retrieval_query": "TEST consumer subscription revenue CY2025 vs CY2024"}
            ],
            "missing_parts": [
                {"code": "dimension:product", "detail": "신규 상품 개별 금액은 공시에서 분리되지 않음", "clause_id": "clause-rev", "ticker": "TEST"}
            ],
            "recommended_actions": [
                {"tool": "ontology.query", "reason": "cover missing dimension", "object_id": "obj-9", "clause_id": "clause-rev", "ticker": "TEST"}
            ],
            "exact_precise_query_candidates": [
                {
                    "clause_id": "clause-rev",
                    "ticker": "TEST",
                    "topic": "other revenue detail",
                    "limit": 5,
                    "response_detail": "compact"
                }
            ],
            "orientation_vocabulary": [
                {"term": "Component Procurement"}
            ]
        }))
        .unwrap();
        let output = compact(&CompactionInput {
            boundary: &boundary,
            state_artifact: &artifact,
            source_messages: &[ProviderMessage::assistant("settled")],
            ledger: &EvidenceLedger::default(),
            calculations: &BTreeMap::new(),
            research_projection: Some(&projection),
            analyst_judgment: &[],
            max_context_bytes: DEFAULT_MAX_COMPACTED_CONTEXT_BYTES,
        })
        .unwrap();

        let composer = output.context.view_for_role(ROLE_COMPOSER).unwrap();
        let canonical = composer.canonical();
        // The map: clause retrieval text, clause-level status, calculation
        // linkage, and missing-part prose all survive.
        assert!(canonical.contains("TEST consumer subscription revenue CY2025 vs CY2024"));
        assert!(canonical.contains("goal-clause-rev"));
        assert!(canonical.contains("partial"));
        assert!(canonical.contains("calc-share"));
        assert!(canonical.contains("신규 상품 개별 금액은 공시에서 분리되지 않음"));
        // Execution vocabulary and identifiers never reach the writer.
        assert!(!canonical.contains("goal-clause-rev-growth"));
        assert!(!canonical.contains("ontology.query"));
        assert!(!canonical.contains("obj-9"));
        assert!(!canonical.contains("other revenue detail"));
        assert!(!canonical.contains("Component Procurement"));
        assert!(!canonical.contains("dimension:product"));
        assert!(!canonical.contains("metric_lineage"));
        assert!(!canonical.contains("ev-1"));
        // The analyst still sees the full projection, tools and all.
        let analyst = output.context.view_for_role(ROLE_ANALYST).unwrap();
        assert!(analyst.canonical().contains("ontology.query"));
        assert!(analyst.canonical().contains("goal-clause-rev-growth"));
    }

    #[test]
    fn analyst_judgment_notes_reach_only_the_composer_and_are_receipt_bound() {
        use krw_agent_evidence::AnalystJudgmentNote;
        let (artifact, boundary) = artifact_and_boundary();
        let notes = vec![AnalystJudgmentNote {
            position: "신규 상품 기여는 아직 미미하고 관련 항목은 축소 중".into(),
            basis: "구독·서비스 매출 감소와 기타 거래 매출 감소가 함께 관찰됨".into(),
            confidence: "medium".into(),
            competing_reading: Some("분류 변경 효과로 일부 감소가 설명될 수 있음".into()),
        }];
        let output = compact(&CompactionInput {
            boundary: &boundary,
            state_artifact: &artifact,
            source_messages: &[ProviderMessage::assistant("settled")],
            ledger: &EvidenceLedger::default(),
            calculations: &BTreeMap::new(),
            research_projection: None,
            analyst_judgment: &notes,
            max_context_bytes: DEFAULT_MAX_COMPACTED_CONTEXT_BYTES,
        })
        .unwrap();

        // The writer receives the handoff notes verbatim.
        let composer = output.context.view_for_role(ROLE_COMPOSER).unwrap();
        assert!(composer.canonical().contains("신규 상품 기여는 아직 미미하고"));
        assert!(composer.canonical().contains("competing_reading"));
        // The analyst and repair views never see them again.
        let analyst = output.context.view_for_role(ROLE_ANALYST).unwrap();
        assert!(!analyst.canonical().contains("신규 상품 기여는 아직 미미하고"));
        let repair = output.context.view_for_role(ROLE_REPAIR).unwrap();
        assert!(!repair.canonical().contains("신규 상품 기여는 아직 미미하고"));
        // The receipt pins the note set (Some hash, not None).
        assert!(output.receipt.analyst_judgment_hash.is_some());
        output.verify().unwrap();
        // Tampering with a note breaks the receipt.
        let mut tampered = output.context.clone();
        tampered.analyst_judgment[0].position = "완전히 다른 판단".into();
        let tampered_canonical = serde_jcs::to_vec(&tampered).unwrap();
        assert_ne!(
            ContentHash::sha256(&tampered_canonical),
            output.receipt.compacted_context_hash
        );
    }

    #[test]
    fn noteless_runs_keep_the_pre_b_receipt_shape() {
        use krw_agent_evidence::AnalystJudgmentNote;
        let (artifact, boundary) = artifact_and_boundary();
        let output = compact(&CompactionInput {
            boundary: &boundary,
            state_artifact: &artifact,
            source_messages: &[ProviderMessage::assistant("settled")],
            ledger: &EvidenceLedger::default(),
            calculations: &BTreeMap::new(),
            research_projection: None,
            analyst_judgment: &[AnalystJudgmentNote {
                position: "p".into(),
                basis: "b".into(),
                confidence: "low".into(),
                competing_reading: None,
            }],
            max_context_bytes: DEFAULT_MAX_COMPACTED_CONTEXT_BYTES,
        })
        .unwrap();
        assert!(output.receipt.analyst_judgment_hash.is_some());
        // A pre-B receipt JSON (no analyst_judgment_hash key) still
        // deserializes and verifies against a noteless context.
        let mut receipt_json = serde_json::to_value(&output.receipt).unwrap();
        assert!(receipt_json
            .as_object_mut()
            .unwrap()
            .remove("analyst_judgment_hash")
            .is_some());
        let legacy_receipt: CompactionReceipt = serde_json::from_value(receipt_json).unwrap();
        let mut legacy_context = output.context.clone();
        legacy_context.analyst_judgment.clear();
        legacy_receipt.verify().unwrap();
        let _ = legacy_context;
    }
}
