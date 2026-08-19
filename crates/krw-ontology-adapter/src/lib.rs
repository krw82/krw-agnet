//! Typed mapping from canonical `ResearchState v2` into generic evidence records.

use std::collections::{BTreeMap, BTreeSet};

use krw_agent_evidence::{
    Answerability, Calculation, Directness, EvidenceGrade, EvidenceRecord, EvidenceScope,
    EvidenceSource, NormalizedFact, PublicCitation,
};
use krw_agent_planning::{
    DirectnessRequirement, EvidenceGoal, EvidenceGoalGraph, GoalStatus, MAX_EVIDENCE_GOAL_LINKS,
    PPM,
};
use krw_agent_protocol::{ContentHash, is_canonical_ticker};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use zeroize::Zeroizing;

const MAX_SUPPLEMENTAL_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
const MAX_SUPPLEMENTAL_RECORDS: usize = 256;
const MAX_SUPPLEMENTAL_FACTS: usize = 16;
const MAX_COMPANY_CONTEXT_TOPICS: usize = 8;
const MAX_MARKET_SNAPSHOT_METRICS: usize = 6;
const MAX_MARKET_METRIC_ABS: f64 = 1.0e18;
const MAX_RESEARCH_FACTS_PER_RECORD: usize = 128;
const MAX_RESEARCH_CALCULATIONS: usize = 64;
const MAX_NORMALIZED_RECORD_BYTES: usize = 256 * 1024;
const MAX_PLANNING_GAPS: usize = 256;
// A compacted analyst turn must retain the exact, server-normalized follow-up
// reads for required gaps.  The raw ResearchState is deliberately removed at
// every settled boundary, so keeping this small list prevents the analyst from
// seeing only "missing" without the safe query it can actually issue.
// A SearchPlan is capped at twelve clauses. Retain every missing required
// clause's follow-up candidate so an item near the end of a broad plan is not
// hidden merely by clause order.
const MAX_EXACT_TARGETED_QUERY_CANDIDATES: usize = 12;
const MAX_EXACT_TARGETED_QUERY_FILTERS: usize = 16;
// The advisory orientation vocabulary must stay small enough to ride inside
// every compacted planner/analyst turn without crowding out goal state, while
// still covering the per-company topic cap across a small multi-ticker scope.
pub const MAX_ORIENTATION_VOCABULARY: usize = 12;
/// Evidence predicate of a `company_context` orientation fact. The fact value
/// is the sanitized topic label; the record itself stays `Unverified`.
pub const COMPANY_TOPIC_ORIENTATION_PREDICATE: &str = "company_topic_orientation";
const MAX_SOURCE_ANCHORS: usize = 32;
const MAX_RESEARCH_WARNINGS: usize = 32;
pub const MAX_SUPPLEMENTAL_READ_STATUSES: usize = 4;
const MAX_CHAIN_CONTEXT_ITEMS: usize = 16;
const MAX_CHAIN_CONTEXT_DEPTH: usize = 4;
const MAX_CHAIN_CONTEXT_STRING_BYTES: usize = 512;

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchStateV2 {
    pub contract_version: String,
    pub release_id: Option<String>,
    pub plan: Value,
    pub resolved_scope: Value,
    #[serde(default)]
    pub source_anchors: Vec<Value>,
    pub answerability: ResearchAnswerability,
    pub clause_coverage: Vec<ClauseCoverage>,
    pub evidence_units: Vec<EvidenceUnit>,
    #[serde(default)]
    pub computed_values: Vec<ComputedValue>,
    #[serde(default)]
    pub calculation_coverage: Vec<CalculationCoverage>,
    #[serde(default)]
    pub missing_parts: Vec<MissingPart>,
    #[serde(default)]
    pub recommended_actions: Vec<RecommendedAction>,
    pub continuation: Option<Continuation>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchAnswerability {
    pub status: String,
    pub strong_claim_allowed: bool,
    pub required_clause_count: u16,
    pub covered_required_clause_count: u16,
    pub requires_direct_evidence: bool,
    #[serde(default)]
    pub reason_codes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClauseCoverage {
    pub clause_id: String,
    pub required: bool,
    pub directness_required: String,
    pub status: String,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    #[serde(default)]
    pub covered_tickers: Vec<String>,
    #[serde(default)]
    pub missing_tickers: Vec<String>,
    pub best_directness: Option<String>,
    pub best_evidence_grade: Option<String>,
    pub strong_claim_ready: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceUnit {
    pub evidence_id: String,
    pub object_id: Option<String>,
    pub object_type: String,
    pub ticker: Option<String>,
    pub period: Option<String>,
    pub document_type: Option<String>,
    pub title: String,
    pub summary: String,
    pub match_mode: String,
    pub directness: String,
    pub evidence_grade: String,
    pub materiality: Option<Value>,
    pub metric: Option<String>,
    pub unit: Option<String>,
    pub currency: Option<String>,
    #[serde(default)]
    pub dimensions: BTreeMap<String, String>,
    pub metric_scope: Option<String>,
    #[serde(default)]
    pub metric_points: Vec<MetricPoint>,
    #[serde(default)]
    pub supports_clause_ids: Vec<String>,
    #[serde(default)]
    pub clause_matches: Vec<ClauseEvidenceMatch>,
    pub source: UnitSource,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClauseEvidenceMatch {
    pub clause_id: String,
    pub match_mode: String,
    pub directness: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UnitSource {
    #[serde(default)]
    pub object_ids: Vec<String>,
    #[serde(default)]
    pub quote_ids: Vec<String>,
    #[serde(default)]
    pub span_ids: Vec<String>,
    pub source_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MetricPoint {
    pub period: String,
    pub value: Option<Value>,
    pub formatted_value: Option<String>,
    pub object_id: Option<String>,
    pub period_type: Option<String>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub conflict_value_count: u16,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ComputedValue {
    pub calculation_id: String,
    pub kind: String,
    pub label: Option<String>,
    pub metric: Option<String>,
    #[serde(default)]
    pub tickers: Vec<String>,
    pub period: Option<String>,
    pub from_period: Option<String>,
    pub unit: Option<String>,
    pub currency: Option<String>,
    #[serde(default)]
    pub dimensions: BTreeMap<String, String>,
    pub metric_scope: Option<String>,
    pub period_basis: Option<String>,
    pub duration_basis: Option<String>,
    pub calculation_window: Option<String>,
    pub value: Option<Value>,
    pub numerator: Option<Value>,
    pub denominator: Option<Value>,
    #[serde(default)]
    pub source_object_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CalculationCoverage {
    pub clause_id: String,
    pub metric: String,
    pub axis: String,
    pub metric_scope: String,
    #[serde(default)]
    pub metric_dimensions: Vec<String>,
    pub status: String,
    #[serde(default)]
    pub required_tickers: Vec<String>,
    #[serde(default)]
    pub covered_tickers: Vec<String>,
    #[serde(default)]
    pub calculation_ids: Vec<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MissingPart {
    pub code: String,
    pub detail: String,
    pub clause_id: Option<String>,
    pub ticker: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecommendedAction {
    pub tool: String,
    pub reason: String,
    pub object_id: Option<String>,
    pub clause_id: Option<String>,
    pub ticker: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Continuation {
    pub has_more: bool,
    pub omitted_evidence_count: u32,
    pub reason: Option<String>,
}

/// Safe, bounded filing provenance retained after raw MCP output is compacted.
/// This is control context, not evidence: it tells later roles whether a
/// current-driver document exists, whether pagination omitted rows, or whether
/// a retrieval failed. It cannot itself support a factual claim.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchSourceAnchor {
    pub ticker: Option<String>,
    pub period: Option<String>,
    pub document_type: Option<String>,
    pub role: String,
    pub source_label: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchRetrievalStatus {
    #[serde(default)]
    pub source_anchors: Vec<ResearchSourceAnchor>,
    #[serde(default)]
    pub continuation: Option<Continuation>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub supplemental_reads: Vec<SupplementalReadStatus>,
}

#[derive(Debug, Clone)]
pub struct MappingContext {
    pub capability_id: String,
    pub action_key: String,
    pub server_build: String,
    pub normalized_contract_hash: ContentHash,
    pub server_schema_bundle_hash: ContentHash,
    pub data_release_hash: ContentHash,
    pub scope: EvidenceScope,
    pub payload_ref: ContentHash,
}

#[derive(Debug, Clone)]
pub struct EvidenceDelta {
    pub answerability: Answerability,
    pub records: Vec<EvidenceRecord>,
    pub calculations: Vec<Calculation>,
}

#[derive(Debug, Clone)]
pub struct SupplementalEvidenceDelta {
    pub records: Vec<EvidenceRecord>,
    pub calculations: Vec<Calculation>,
    pub status: SupplementalReadStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SupplementalReadKind {
    Retrieved,
    Empty,
    NotFound,
    InputRejected,
    Ambiguous,
    ApplicationError,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SupplementalReadStatus {
    pub kind: SupplementalReadKind,
    pub result_count: u16,
    pub has_more: bool,
    pub next_offset: Option<u32>,
    #[serde(default)]
    pub warning_codes: Vec<String>,
}

/// A safe, compact projection of the per-company topic store. This is
/// deliberately orientation-only: it may help select a later evidence query,
/// but its records are never eligible to support a factual or strong claim.
#[derive(Debug, Clone)]
pub struct CompanyContextDelta {
    pub provider_content: Value,
    pub records: Vec<EvidenceRecord>,
}

/// Safe projection of a timestamped, non-filing market-data snapshot. It is
/// deliberately not represented as evidence: a current quote or multiple can
/// orient a question, but never proves a filing claim or recommendation.
#[derive(Debug, Clone)]
pub struct MarketSnapshotDelta {
    pub provider_content: Value,
    /// A deliberately unverified orientation record retains the compact
    /// timestamped values across transcript compaction. It is not filing
    /// evidence and cannot support a strong claim.
    pub records: Vec<EvidenceRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClausePlanningBinding {
    pub clause_id: String,
    pub retrieval_query: String,
}

/// One exact, bounded `ontology.query` input distilled from a required clause
/// that the ontology reports as missing.  This is control context, not
/// evidence: it cannot support an answer claim, but it lets the analyst make
/// a precise follow-up instead of inventing a paraphrased query after raw MCP
/// output has been compacted away.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExactTargetedQueryCandidate {
    pub clause_id: String,
    pub ticker: String,
    pub topic: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub document_types: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub periods: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub object_types: Vec<String>,
    /// A known required gap must be read using its full phrase. This public
    /// MCP field selects the model's evidence depth for the bounded read;
    /// scope and answer-candidate semantics remain kernel-owned.
    #[serde(default = "exact_targeted_query_answer_candidate_only")]
    pub answer_candidate_only: bool,
    pub response_detail: String,
    pub limit: u16,
}

const fn exact_targeted_query_answer_candidate_only() -> bool {
    true
}

/// One advisory company-orientation term distilled from a committed
/// `company_topic_orientation` ledger fact (`term` is the topic label). This is
/// vocabulary, not evidence: it may steer the wording of a later evidence
/// query, and never upgrades directness or strong-claim eligibility.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OrientationTerm {
    pub term: String,
    #[serde(default)]
    pub document_type: Option<String>,
    #[serde(default)]
    pub period: Option<String>,
}

/// Distill the advisory orientation vocabulary from committed ledger records.
/// The ledger stays the single source of truth: only records carrying the
/// `company_topic_orientation` predicate contribute, and their `Unverified`
/// advisory-only semantics are preserved by never touching the records
/// themselves.
pub fn company_orientation_vocabulary(records: &[EvidenceRecord]) -> Vec<OrientationTerm> {
    records
        .iter()
        .filter(|record| {
            record
                .facts
                .iter()
                .any(|fact| fact.predicate == COMPANY_TOPIC_ORIENTATION_PREDICATE)
        })
        .filter_map(|record| {
            let fact = record
                .facts
                .iter()
                .find(|fact| fact.predicate == COMPANY_TOPIC_ORIENTATION_PREDICATE)?;
            Some(OrientationTerm {
                term: fact.value.as_str()?.to_owned(),
                document_type: record.citation.document_type.clone(),
                period: record.period.clone(),
            })
        })
        .collect()
}

/// Bound, deduplicate, and order the advisory vocabulary so the projection —
/// and therefore every checkpoint and compacted role view derived from it —
/// stays deterministic. Malformed entries are omitted rather than failing a
/// valid research state, mirroring the exact-candidate policy.
fn normalize_orientation_vocabulary(terms: &[OrientationTerm]) -> Vec<OrientationTerm> {
    let mut bounded = BTreeSet::new();
    for term in terms {
        let valid = |value: &str, max_bytes: usize| {
            !value.is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
        };
        if !valid(&term.term, 256)
            || term
                .document_type
                .as_deref()
                .is_some_and(|value| !valid(value, 128))
            || term
                .period
                .as_deref()
                .is_some_and(|value| !valid(value, 128))
        {
            continue;
        }
        bounded.insert(term.clone());
    }
    bounded
        .into_iter()
        .take(MAX_ORIENTATION_VOCABULARY)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchPlanningProjection {
    pub graph: EvidenceGoalGraph,
    pub clauses: Vec<ClausePlanningBinding>,
    pub missing_parts: Vec<MissingPart>,
    pub recommended_actions: Vec<RecommendedAction>,
    /// Exact full-detail reads for the currently missing required clauses.
    /// Kept separately from prose hints so the candidate survives the trusted
    /// compaction boundary and remains copyable by the analyst.
    #[serde(default)]
    pub exact_precise_query_candidates: Vec<ExactTargetedQueryCandidate>,
    /// Advisory company-orientation terms distilled from committed
    /// `company_topic_orientation` ledger facts. Like the exact candidates
    /// above, this rides the projection across the trusted compaction
    /// boundary; unlike evidence, it only suggests canonical filing
    /// vocabulary for later queries and never upgrades directness or
    /// strong-claim eligibility (the source records stay `Unverified`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub orientation_vocabulary: Vec<OrientationTerm>,
    #[serde(default)]
    pub retrieval_status: ResearchRetrievalStatus,
}

/// Canonical per-clause progress distilled from a `ResearchState`. This is
/// deliberately exported by the ontology adapter so the kernel does not
/// duplicate the server's coverage-status interpretation when it maps a
/// clause back to a model-declared evidence goal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClauseCoverageProgress {
    pub clause_id: String,
    pub status: GoalStatus,
    pub coverage_ppm: u32,
    pub evidence_ids: Vec<String>,
    /// `None` means that the server reported no calculation coverage for this
    /// clause. A semantic goal which requires a calculation must treat that as
    /// insufficient rather than inferring success from text coverage alone.
    pub calculation_status: Option<GoalStatus>,
    pub calculation_coverage_ppm: Option<u32>,
    pub calculation_ids: Vec<String>,
}

/// Convert `ResearchState` clause and calculation coverage into a single
/// canonical observation per `SearchPlan` clause. The function validates that
/// the server covered every declared clause exactly once and that calculation
/// coverage cannot refer to an unknown clause.
pub fn derive_clause_coverage_progress(
    state: &ResearchStateV2,
) -> Result<BTreeMap<String, ClauseCoverageProgress>, AdapterError> {
    let clauses = state
        .plan
        .get("clauses")
        .and_then(Value::as_array)
        .ok_or(AdapterError::InvalidPlanningProjection("plan.clauses"))?;
    let mut declared = BTreeSet::new();
    for clause in clauses {
        let clause = clause
            .as_object()
            .ok_or(AdapterError::InvalidPlanningProjection("plan.clauses[]"))?;
        let clause_id = bounded_required_string(clause.get("clause_id"), 64, "clause_id")?;
        if !declared.insert(clause_id) {
            return Err(AdapterError::InvalidPlanningProjection(
                "duplicate clause_id",
            ));
        }
    }

    let mut observations = BTreeMap::new();
    for coverage in &state.clause_coverage {
        if !declared.contains(&coverage.clause_id) {
            return Err(AdapterError::InvalidPlanningProjection(
                "coverage references unknown clause",
            ));
        }
        let (status, coverage_ppm) = coverage_progress(
            &coverage.status,
            coverage.covered_tickers.len(),
            coverage.missing_tickers.len(),
        )?;
        let mut evidence_ids = coverage.evidence_ids.clone();
        evidence_ids.sort();
        evidence_ids.dedup();
        if observations
            .insert(
                coverage.clause_id.clone(),
                ClauseCoverageProgress {
                    clause_id: coverage.clause_id.clone(),
                    status,
                    coverage_ppm,
                    evidence_ids,
                    calculation_status: None,
                    calculation_coverage_ppm: None,
                    calculation_ids: Vec::new(),
                },
            )
            .is_some()
        {
            return Err(AdapterError::InvalidPlanningProjection(
                "duplicate clause coverage",
            ));
        }
    }
    if observations.len() != declared.len() {
        return Err(AdapterError::InvalidPlanningProjection(
            "clause coverage is incomplete",
        ));
    }

    let mut calculations = BTreeMap::<String, Vec<(GoalStatus, u32, Vec<String>)>>::new();
    for coverage in &state.calculation_coverage {
        if !declared.contains(&coverage.clause_id) {
            return Err(AdapterError::InvalidPlanningProjection(
                "calculation references unknown clause",
            ));
        }
        let missing = coverage
            .required_tickers
            .len()
            .saturating_sub(coverage.covered_tickers.len());
        let (status, coverage_ppm) =
            coverage_progress(&coverage.status, coverage.covered_tickers.len(), missing)?;
        let mut ids = coverage.calculation_ids.clone();
        ids.sort();
        ids.dedup();
        calculations
            .entry(coverage.clause_id.clone())
            .or_default()
            .push((status, coverage_ppm, ids));
    }
    for (clause_id, values) in calculations {
        let all_satisfied = values
            .iter()
            .all(|(status, _, _)| *status == GoalStatus::Satisfied);
        let any_progress = values
            .iter()
            .any(|(status, _, _)| matches!(status, GoalStatus::Partial | GoalStatus::Satisfied));
        let status = if all_satisfied {
            GoalStatus::Satisfied
        } else if any_progress {
            GoalStatus::Partial
        } else {
            GoalStatus::Unresolved
        };
        let coverage_ppm = match status {
            GoalStatus::Satisfied => PPM,
            GoalStatus::Partial => values
                .iter()
                .map(|(_, coverage_ppm, _)| *coverage_ppm)
                .max()
                .unwrap_or(PPM / 2)
                .clamp(1, PPM - 1),
            GoalStatus::Unresolved | GoalStatus::Blocked => 0,
        };
        let mut ids = values
            .into_iter()
            .flat_map(|(_, _, ids)| ids)
            .collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        let observation =
            observations
                .get_mut(&clause_id)
                .ok_or(AdapterError::InvalidPlanningProjection(
                    "calculation missing clause observation",
                ))?;
        observation.calculation_status = Some(status);
        observation.calculation_coverage_ppm = Some(coverage_ppm);
        observation.calculation_ids = ids;
    }
    Ok(observations)
}

/// Build the planning projection for a committed research state. The
/// `orientation_vocabulary` input is the advisory company-orientation
/// vocabulary distilled from the evidence ledger (see
/// [`company_orientation_vocabulary`]); passing an empty slice yields a
/// projection without orientation terms, which is exactly the pre-orientation
/// shape.
pub fn derive_research_planning_projection(
    state: &ResearchStateV2,
    orientation_vocabulary: &[OrientationTerm],
) -> Result<ResearchPlanningProjection, AdapterError> {
    if state.missing_parts.len() > MAX_PLANNING_GAPS
        || state.recommended_actions.len() > MAX_PLANNING_GAPS
    {
        return Err(AdapterError::InvalidPlanningProjection(
            "planning gap limit",
        ));
    }
    let graph = derive_evidence_goal_graph(state)?;
    let clauses = state
        .plan
        .get("clauses")
        .and_then(Value::as_array)
        .ok_or(AdapterError::InvalidPlanningProjection("plan.clauses"))?
        .iter()
        .map(|clause| {
            let clause = clause
                .as_object()
                .ok_or(AdapterError::InvalidPlanningProjection("plan.clauses[]"))?;
            Ok(ClausePlanningBinding {
                clause_id: bounded_required_string(clause.get("clause_id"), 64, "clause_id")?,
                retrieval_query: bounded_required_string(
                    clause.get("retrieval_query"),
                    1_000,
                    "retrieval_query",
                )?,
            })
        })
        .collect::<Result<Vec<_>, AdapterError>>()?;
    Ok(ResearchPlanningProjection {
        graph,
        clauses,
        missing_parts: state.missing_parts.clone(),
        recommended_actions: state.recommended_actions.clone(),
        exact_precise_query_candidates: exact_precise_query_candidates(state),
        orientation_vocabulary: normalize_orientation_vocabulary(orientation_vocabulary),
        retrieval_status: research_retrieval_status(state),
    })
}

/// Preserve the exact candidate shape the kernel previously attached only to
/// the raw `query_context` tool result.  The raw result is intentionally
/// scrubbed before the analyst turn, so derive the same bounded inputs into
/// the durable projection instead.  Bad optional filters are omitted rather
/// than making a valid research state fail; the resulting query becomes
/// broader and remains subject to the normal trusted-scope check at dispatch.
fn exact_precise_query_candidates(state: &ResearchStateV2) -> Vec<ExactTargetedQueryCandidate> {
    let Some(plan) = state.plan.as_object() else {
        return Vec::new();
    };
    let missing_clause_ids = state
        .missing_parts
        .iter()
        .filter_map(|part| part.clause_id.as_deref())
        .collect::<BTreeSet<_>>();
    if missing_clause_ids.is_empty() {
        return Vec::new();
    }

    let fallback_ticker = bounded_plan_values(plan.get("tickers"), 32)
        .into_iter()
        .find(|ticker| valid_targeted_ticker(ticker));
    let document_types = bounded_plan_values(plan.get("document_types"), 128);
    let periods = bounded_plan_values(plan.get("periods"), 128);
    let Some(clauses) = plan.get("clauses").and_then(Value::as_array) else {
        return Vec::new();
    };

    let mut candidates = Vec::new();
    for clause in clauses {
        if candidates.len() >= MAX_EXACT_TARGETED_QUERY_CANDIDATES {
            break;
        }
        let Some(clause) = clause.as_object() else {
            continue;
        };
        if !clause
            .get("required")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }
        let Some(clause_id) = bounded_optional(clause.get("clause_id"), 64) else {
            continue;
        };
        if !missing_clause_ids.contains(clause_id.as_str()) {
            continue;
        }
        let Some(retrieval_query) = bounded_optional(clause.get("retrieval_query"), 512) else {
            continue;
        };
        let ticker = bounded_plan_values(clause.get("tickers"), 32)
            .into_iter()
            .find(|ticker| valid_targeted_ticker(ticker))
            .or_else(|| fallback_ticker.clone());
        let Some(ticker) = ticker else {
            continue;
        };
        let topic = focused_targeted_query_topic(clause, &retrieval_query);
        candidates.push(ExactTargetedQueryCandidate {
            clause_id,
            ticker,
            topic,
            document_types: document_types.clone(),
            periods: periods.clone(),
            object_types: bounded_plan_values(clause.get("object_types"), 128),
            answer_candidate_only: true,
            response_detail: "compact".into(),
            limit: 20,
        });
    }
    candidates
}

/// `query_context` can use a canonical metric clause plus multiple discovery
/// aliases, but the targeted-query ABI has only one FTS `topic` field.  The
/// bundled runtime treats that topic as a strict AND when
/// `answer_candidate_only=true`. Passing every alias therefore asks one
/// filing object to contain mutually redundant spellings such as `R&D`,
/// `research and development`, and `rd_expense`, which makes an existing fact
/// look absent. For ordinary company-total metrics, use one stable filing
/// phrase and retain the original query for named/dimensioned breakdowns.
/// This narrows only the follow-up text; ticker, period, document, and object
/// type filters remain unchanged.
fn focused_targeted_query_topic(clause: &serde_json::Map<String, Value>, fallback: &str) -> String {
    let metric_scope = clause
        .get("metric_scope")
        .and_then(Value::as_str)
        .unwrap_or("any");
    if metric_scope == "dimensioned" {
        return fallback.to_owned();
    }
    let metrics = bounded_plan_values(clause.get("metrics"), 128);
    let [metric] = metrics.as_slice() else {
        return fallback.to_owned();
    };
    let phrase = match metric.as_str() {
        "revenue" => "net sales",
        "gross_margin" => "gross margin",
        "operating_margin" => "operating margin",
        "research_and_development" => "research and development",
        "selling_general_and_admin" => "selling general administrative",
        "operating_cash_flow" => "cash from operating activities",
        "free_cash_flow" => "free cash flow",
        "capital_expenditures" => "capital expenditures",
        "cash_and_equivalents" => "cash and cash equivalents",
        "total_debt" => "total debt",
        _ => return fallback.to_owned(),
    };
    phrase.to_owned()
}

fn bounded_plan_values(value: Option<&Value>, max_bytes: usize) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|value| safe_single_line(value, max_bytes, ""))
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(MAX_EXACT_TARGETED_QUERY_FILTERS)
        .collect()
}

fn valid_targeted_ticker(ticker: &str) -> bool {
    !ticker.is_empty()
        && ticker.len() <= 32
        && ticker.bytes().all(|byte| {
            byte.is_ascii_uppercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
        })
}

fn research_retrieval_status(state: &ResearchStateV2) -> ResearchRetrievalStatus {
    let mut anchors = state
        .source_anchors
        .iter()
        .filter_map(Value::as_object)
        .filter_map(|anchor| {
            let role = bounded_optional(anchor.get("role"), 64)
                .unwrap_or_else(|| "retrieved_evidence".into());
            let raw_period = bounded_optional(anchor.get("period"), 128);
            let document_type = bounded_optional(anchor.get("document_type"), 64);
            Some(ResearchSourceAnchor {
                ticker: bounded_optional(anchor.get("ticker"), 32),
                // `CY2026Q1` is an ontology routing bucket, not a statement
                // that this issuer calls the filing its fiscal Q1.  Keep the
                // user-facing control context neutral until an observed
                // financial period/date can establish a fiscal label.
                period: presentation_document_period(
                    raw_period.as_deref(),
                    document_type.as_deref(),
                ),
                document_type: document_type.clone(),
                role,
                source_label: presentation_source_label(
                    bounded_optional(anchor.get("source_label"), 512).as_deref(),
                    raw_period.as_deref(),
                    document_type.as_deref(),
                ),
            })
        })
        .collect::<Vec<_>>();
    anchors.sort_by(|left, right| {
        (
            left.ticker.as_deref().unwrap_or(""),
            left.role.as_str(),
            left.period.as_deref().unwrap_or(""),
            left.document_type.as_deref().unwrap_or(""),
        )
            .cmp(&(
                right.ticker.as_deref().unwrap_or(""),
                right.role.as_str(),
                right.period.as_deref().unwrap_or(""),
                right.document_type.as_deref().unwrap_or(""),
            ))
    });
    anchors.dedup();
    anchors.truncate(MAX_SOURCE_ANCHORS);

    let warnings = state
        .warnings
        .iter()
        .map(|warning| safe_single_line(warning, 256, ""))
        .filter(|warning| !warning.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(MAX_RESEARCH_WARNINGS)
        .collect();
    ResearchRetrievalStatus {
        source_anchors: anchors,
        continuation: state.continuation.clone(),
        warnings,
        supplemental_reads: Vec::new(),
    }
}

/// Compile canonical server coverage into the kernel's deterministic research
/// objective graph. The model may add a bounded replan delta later, but it
/// cannot rewrite these server-observed goals or claim more progress than the
/// canonical `ResearchState` reports.
pub fn derive_evidence_goal_graph(
    state: &ResearchStateV2,
) -> Result<EvidenceGoalGraph, AdapterError> {
    let clauses = state
        .plan
        .get("clauses")
        .and_then(Value::as_array)
        .ok_or(AdapterError::InvalidPlanningProjection("plan.clauses"))?;
    let mut clause_definitions = BTreeMap::new();
    for clause in clauses {
        let clause = clause
            .as_object()
            .ok_or(AdapterError::InvalidPlanningProjection("plan.clauses[]"))?;
        let clause_id = bounded_required_string(clause.get("clause_id"), 64, "clause_id")?;
        let required = clause
            .get("required")
            .and_then(Value::as_bool)
            .ok_or(AdapterError::InvalidPlanningProjection("clause.required"))?;
        let directness = clause
            .get("directness")
            .and_then(Value::as_str)
            .ok_or(AdapterError::InvalidPlanningProjection("clause.directness"))?;
        let directness = match directness {
            "any" => DirectnessRequirement::Related,
            "direct_preferred" | "direct_required" => DirectnessRequirement::Direct,
            _ => {
                return Err(AdapterError::InvalidPlanningProjection(
                    "clause.directness value",
                ));
            }
        };
        if clause_definitions
            .insert(clause_id, (required, directness))
            .is_some()
        {
            return Err(AdapterError::InvalidPlanningProjection(
                "duplicate clause_id",
            ));
        }
    }

    let mut goals = Vec::with_capacity(
        state
            .clause_coverage
            .len()
            .saturating_add(state.calculation_coverage.len()),
    );
    for coverage in &state.clause_coverage {
        let Some((declared_required, directness)) =
            clause_definitions.get(&coverage.clause_id).copied()
        else {
            return Err(AdapterError::InvalidPlanningProjection(
                "coverage references unknown clause",
            ));
        };
        if declared_required != coverage.required {
            return Err(AdapterError::InvalidPlanningProjection(
                "coverage required flag drift",
            ));
        }
        let (status, coverage_ppm) = coverage_progress(
            &coverage.status,
            coverage.covered_tickers.len(),
            coverage.missing_tickers.len(),
        )?;
        goals.push(EvidenceGoal {
            goal_id: clause_goal_id(&coverage.clause_id),
            required: coverage.required,
            weight: if coverage.required { 1_000 } else { 250 },
            dependencies: Vec::new(),
            directness,
            calculation_required: false,
            status,
            coverage_ppm,
            evidence_ids: bounded_goal_links(&coverage.evidence_ids),
            calculation_ids: Vec::new(),
        });
    }
    if goals.len() != clause_definitions.len() {
        return Err(AdapterError::InvalidPlanningProjection(
            "clause coverage is incomplete",
        ));
    }

    for coverage in &state.calculation_coverage {
        let Some((required, _)) = clause_definitions.get(&coverage.clause_id).copied() else {
            return Err(AdapterError::InvalidPlanningProjection(
                "calculation references unknown clause",
            ));
        };
        let (status, coverage_ppm) = coverage_progress(
            &coverage.status,
            coverage.covered_tickers.len(),
            coverage
                .required_tickers
                .len()
                .saturating_sub(coverage.covered_tickers.len()),
        )?;
        goals.push(EvidenceGoal {
            goal_id: calculation_goal_id(coverage),
            required,
            weight: if required { 600 } else { 150 },
            dependencies: vec![clause_goal_id(&coverage.clause_id)],
            directness: DirectnessRequirement::MetricLineage,
            calculation_required: true,
            status,
            coverage_ppm,
            evidence_ids: Vec::new(),
            calculation_ids: bounded_goal_links(&coverage.calculation_ids),
        });
    }
    EvidenceGoalGraph::new(goals).map_err(AdapterError::Planning)
}

/// A ResearchState may retain many matching records for one clause. They all
/// remain in the EvidenceLedger; the planning graph only needs a bounded,
/// deterministic witness set to decide which unresolved goal can advance.
/// Preserve server order because it already reflects selector priority, while
/// removing repeated references before the graph's fixed link cap is applied.
fn bounded_goal_links(values: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    values
        .iter()
        .filter(|value| seen.insert(value.as_str()))
        .take(MAX_EVIDENCE_GOAL_LINKS)
        .cloned()
        .collect()
}

fn clause_goal_id(clause_id: &str) -> String {
    format!("clause:{clause_id}")
}

fn calculation_goal_id(coverage: &CalculationCoverage) -> String {
    let fingerprint = ContentHash::sha256(
        serde_jcs::to_vec(&(
            &coverage.clause_id,
            &coverage.metric,
            &coverage.axis,
            &coverage.metric_scope,
            &coverage.metric_dimensions,
        ))
        .expect("serializing borrowed strings cannot fail"),
    );
    let suffix = fingerprint
        .as_str()
        .trim_start_matches("sha256:")
        .chars()
        .take(48)
        .collect::<String>();
    format!("calculation:{}:{suffix}", coverage.clause_id)
}

fn coverage_progress(
    status: &str,
    covered_count: usize,
    missing_count: usize,
) -> Result<(GoalStatus, u32), AdapterError> {
    match status {
        "covered" => Ok((GoalStatus::Satisfied, PPM)),
        "missing" => Ok((GoalStatus::Unresolved, 0)),
        "partial" => {
            let total = covered_count.saturating_add(missing_count);
            let ratio = if total == 0 {
                PPM / 2
            } else {
                let numerator = u64::try_from(covered_count)
                    .unwrap_or(u64::MAX)
                    .saturating_mul(u64::from(PPM));
                let denominator = u64::try_from(total).unwrap_or(u64::MAX).max(1);
                u32::try_from(numerator / denominator).unwrap_or(PPM - 1)
            }
            .clamp(1, PPM - 1);
            Ok((GoalStatus::Partial, ratio))
        }
        _ => Err(AdapterError::InvalidPlanningProjection("coverage status")),
    }
}

fn bounded_required_string(
    value: Option<&Value>,
    max_bytes: usize,
    field: &'static str,
) -> Result<String, AdapterError> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= max_bytes)
        .map(ToOwned::to_owned)
        .ok_or(AdapterError::InvalidPlanningProjection(field))
}

pub fn parse_research_state(bytes: &[u8]) -> Result<ResearchStateV2, AdapterError> {
    if bytes.len() > MAX_SUPPLEMENTAL_PAYLOAD_BYTES {
        return Err(AdapterError::ResearchStateLimit);
    }
    let state: ResearchStateV2 = serde_json::from_slice(bytes)?;
    if state.contract_version != "research-state/v2" {
        return Err(AdapterError::UnsupportedContract(state.contract_version));
    }
    Ok(state)
}

/// Remove explicit foreign-company material from a fixed-company ResearchState
/// before it can become either ledger evidence or provider-visible context.
///
/// The MCP result is still a successful read: a bad unit is withheld and the
/// remaining in-scope material is returned to the analyst.  This deliberately
/// does not turn a remote scope defect into a terminal run failure or request
/// another model turn.  Wide discovery has no pre-authorized ticker set and
/// therefore remains untouched.
pub fn sanitize_research_state_scope(
    state: &ResearchStateV2,
    expected_tickers: &[String],
) -> ResearchStateV2 {
    const WITHHELD_WARNING: &str = "out_of_scope_evidence_withheld";

    let expected = expected_tickers
        .iter()
        .filter(|ticker| is_canonical_ticker(ticker))
        .cloned()
        .collect::<BTreeSet<_>>();
    if expected.is_empty() {
        return state.clone();
    }

    let mut sanitized = state.clone();
    let mut withheld = false;
    let ticker_in_scope =
        |ticker: Option<&str>| ticker.is_none_or(|ticker| expected.contains(ticker));

    let evidence_before = sanitized.evidence_units.len();
    sanitized
        .evidence_units
        .retain(|unit| ticker_in_scope(unit.ticker.as_deref()));
    withheld |= sanitized.evidence_units.len() != evidence_before;
    let retained_evidence_ids = sanitized
        .evidence_units
        .iter()
        .map(|unit| unit.evidence_id.as_str())
        .collect::<BTreeSet<_>>();

    let calculations_before = sanitized.computed_values.len();
    sanitized
        .computed_values
        .retain(|value| value.tickers.iter().all(|ticker| expected.contains(ticker)));
    withheld |= sanitized.computed_values.len() != calculations_before;

    let source_anchors_before = sanitized.source_anchors.len();
    sanitized
        .source_anchors
        .retain(|anchor| ticker_in_scope(anchor.get("ticker").and_then(Value::as_str)));
    withheld |= sanitized.source_anchors.len() != source_anchors_before;

    let missing_before = sanitized.missing_parts.len();
    sanitized
        .missing_parts
        .retain(|part| ticker_in_scope(part.ticker.as_deref()));
    let actions_before = sanitized.recommended_actions.len();
    sanitized
        .recommended_actions
        .retain(|action| ticker_in_scope(action.ticker.as_deref()));
    withheld |= sanitized.missing_parts.len() != missing_before
        || sanitized.recommended_actions.len() != actions_before;

    for coverage in &mut sanitized.clause_coverage {
        let evidence_before = coverage.evidence_ids.len();
        coverage
            .evidence_ids
            .retain(|evidence_id| retained_evidence_ids.contains(evidence_id.as_str()));
        let covered_before = coverage.covered_tickers.len();
        coverage
            .covered_tickers
            .retain(|ticker| expected.contains(ticker));
        let missing_before = coverage.missing_tickers.len();
        coverage
            .missing_tickers
            .retain(|ticker| expected.contains(ticker));
        let coverage_withheld = coverage.evidence_ids.len() != evidence_before
            || coverage.covered_tickers.len() != covered_before
            || coverage.missing_tickers.len() != missing_before;
        if coverage_withheld {
            withheld = true;
            coverage.strong_claim_ready = false;
            coverage.best_directness = None;
            coverage.best_evidence_grade = None;
            coverage.status = if coverage.evidence_ids.is_empty()
                || (covered_before > 0 && coverage.covered_tickers.is_empty())
            {
                "missing".into()
            } else {
                "partial".into()
            };
            coverage.reason = Some(WITHHELD_WARNING.into());
        }
    }

    for coverage in &mut sanitized.calculation_coverage {
        let required_before = coverage.required_tickers.len();
        let covered_before = coverage.covered_tickers.len();
        coverage
            .required_tickers
            .retain(|ticker| expected.contains(ticker));
        coverage
            .covered_tickers
            .retain(|ticker| expected.contains(ticker));
        if coverage.required_tickers.len() != required_before
            || coverage.covered_tickers.len() != covered_before
        {
            withheld = true;
            coverage.status = if coverage.covered_tickers.is_empty() {
                "missing".into()
            } else {
                "partial".into()
            };
            coverage.reason = Some(WITHHELD_WARNING.into());
        }
    }

    if withheld {
        let required = sanitized
            .clause_coverage
            .iter()
            .filter(|coverage| coverage.required)
            .count();
        let covered = sanitized
            .clause_coverage
            .iter()
            .filter(|coverage| coverage.required && coverage.status == "covered")
            .count();
        sanitized.answerability.required_clause_count = u16::try_from(required).unwrap_or(u16::MAX);
        sanitized.answerability.covered_required_clause_count =
            u16::try_from(covered).unwrap_or(u16::MAX);
        sanitized.answerability.strong_claim_allowed = false;
        if covered < required {
            sanitized.answerability.status = if covered == 0 {
                "not_answerable".into()
            } else {
                "partial".into()
            };
        }
        sanitized
            .answerability
            .reason_codes
            .push(WITHHELD_WARNING.into());
        sanitized.answerability.reason_codes.sort();
        sanitized.answerability.reason_codes.dedup();
        sanitized.warnings.push(WITHHELD_WARNING.into());
        sanitized.warnings.sort();
        sanitized.warnings.dedup();
        sanitized.warnings.truncate(MAX_RESEARCH_WARNINGS);
    }

    sanitized
}

pub fn map_research_state(
    state: &ResearchStateV2,
    context: &MappingContext,
) -> Result<EvidenceDelta, AdapterError> {
    if state.evidence_units.len() > MAX_SUPPLEMENTAL_RECORDS
        || state.computed_values.len() > MAX_RESEARCH_CALCULATIONS
        || state.evidence_units.iter().any(|unit| {
            unit.metric_points.len() > MAX_RESEARCH_FACTS_PER_RECORD
                || unit.supports_clause_ids.len() > MAX_RESEARCH_FACTS_PER_RECORD
                || unit.metric_points.iter().any(|point| {
                    point.value.as_ref().is_some_and(|value| {
                        serde_json::to_vec(value).map_or(true, |v| v.len() > 64 * 1024)
                    })
                })
        })
        || state.computed_values.iter().any(|value| {
            value.value.as_ref().is_some_and(|value| {
                serde_json::to_vec(value).map_or(true, |v| v.len() > 64 * 1024)
            })
        })
    {
        return Err(AdapterError::ResearchStateLimit);
    }
    let required_clauses = state
        .clause_coverage
        .iter()
        .filter(|coverage| coverage.required)
        .collect::<Vec<_>>();
    let all_required_strong_ready = !required_clauses.is_empty()
        && required_clauses.len() == usize::from(state.answerability.required_clause_count)
        && required_clauses
            .iter()
            .all(|coverage| coverage.status == "covered" && coverage.strong_claim_ready);
    let ready_clauses = state
        .clause_coverage
        .iter()
        .filter(|coverage| {
            coverage.required && coverage.status == "covered" && coverage.strong_claim_ready
        })
        .collect::<Vec<_>>();
    let mut object_to_evidence = BTreeMap::new();
    let mut records = Vec::with_capacity(state.evidence_units.len());
    for unit in &state.evidence_units {
        if !valid_identifier(&unit.evidence_id)
            || unit
                .supports_clause_ids
                .iter()
                .any(|clause_id| !valid_identifier(clause_id))
        {
            return Err(AdapterError::InvalidEvidenceIdentifier);
        }
        let directness = parse_directness(&unit.directness)?;
        let grade = parse_grade(&unit.evidence_grade)?;
        let content_hash = ContentHash::sha256(serde_jcs::to_vec(unit)?);
        let supports_ready_clause = unit.supports_clause_ids.iter().any(|clause| {
            ready_clauses.iter().any(|coverage| {
                coverage.clause_id == *clause
                    && coverage
                        .evidence_ids
                        .iter()
                        .any(|evidence_id| evidence_id == &unit.evidence_id)
            })
        });
        let strong_claim_allowed = state.answerability.strong_claim_allowed
            && all_required_strong_ready
            && supports_ready_clause
            && matches!(directness, Directness::Direct | Directness::MetricLineage);
        if let Some(object_id) = &unit.object_id {
            object_to_evidence.insert(object_id.clone(), unit.evidence_id.clone());
        }
        for object_id in &unit.source.object_ids {
            object_to_evidence.insert(object_id.clone(), unit.evidence_id.clone());
        }
        let entity = clean_optional(unit.ticker.as_deref(), 128);
        // `unit.period` labels the source document's ontology bucket.  It is
        // not necessarily the financial observation period: for example, a
        // document indexed as CY2025 can contain Apple's FY2025 year-to-date
        // cash-flow point.  Keep the source label for the citation, but use a
        // single unambiguous metric-point period for the evidence record.
        // When a record contains several observation periods, deliberately do
        // not invent one record-wide period; every retained fact still carries
        // its own period.
        let raw_document_period = clean_optional(unit.period.as_deref(), 128);
        let document_period = presentation_document_period(
            raw_document_period.as_deref(),
            unit.document_type.as_deref(),
        );
        let raw_period = research_metric_record_period(unit, raw_document_period.clone());
        let period = if let Some(point) = unit.metric_points.first() {
            presentation_observation_period(
                raw_period.as_deref(),
                point.period_type.as_deref(),
                point.end_date.as_deref(),
            )
        } else {
            presentation_document_period(raw_period.as_deref(), unit.document_type.as_deref())
        };
        let as_of = research_metric_as_of(unit);
        let normalized_unit = clean_optional(unit.unit.as_deref(), 64);
        let subject = entity.clone().unwrap_or_else(|| "company".into());
        let predicate = safe_single_line(
            unit.metric.as_deref().unwrap_or("filing_evidence"),
            128,
            "filing_evidence",
        );
        let mut source_object_ids = Vec::new();
        let mut source_seen = BTreeSet::new();
        if let Some(object_id) = &unit.object_id
            && source_seen.insert(object_id.clone())
        {
            source_object_ids.push(object_id.clone());
        }
        for object_id in &unit.source.object_ids {
            if source_seen.insert(object_id.clone()) {
                source_object_ids.push(object_id.clone());
            }
        }
        let mut facts = Vec::new();
        if unit.metric_points.is_empty() {
            facts.push(NormalizedFact {
                subject: safe_single_line(&subject, 256, "company"),
                predicate: predicate.clone(),
                value: Value::String(safe_single_line(
                    &unit.summary,
                    60 * 1024,
                    "filing evidence",
                )),
                unit: normalized_unit.clone(),
                period: period.clone(),
            });
        } else {
            for point in &unit.metric_points {
                facts.push(NormalizedFact {
                    subject: safe_single_line(&subject, 256, "company"),
                    predicate: safe_single_line(&predicate, 128, "metric"),
                    value: point.value.clone().unwrap_or(Value::Null),
                    unit: normalized_unit.clone(),
                    period: presentation_observation_period(
                        Some(point.period.as_str()),
                        point.period_type.as_deref(),
                        point.end_date.as_deref(),
                    ),
                });
            }
            // Preserve the basis that turns a raw number into a meaningful
            // observation (annual vs. year-to-date vs. quarter, date window,
            // currency, scope, and dimensions).  This is one bounded context
            // fact per evidence record rather than a new research requirement;
            // it lets the analyst distinguish compatible comparisons without
            // expanding the ontology schema or adding a model turn.
            if facts.len() < MAX_RESEARCH_FACTS_PER_RECORD
                && let Some(context_value) =
                    research_metric_context(unit, raw_document_period.as_deref())
            {
                facts.push(NormalizedFact {
                    subject: safe_single_line(&subject, 256, "company"),
                    predicate: "metric_context".into(),
                    value: context_value,
                    unit: None,
                    period: period.clone(),
                });
            }
        }
        let record = EvidenceRecord {
            evidence_id: unit.evidence_id.clone(),
            content_hash,
            source: EvidenceSource {
                capability_id: context.capability_id.clone(),
                action_key: context.action_key.clone(),
                server_build: context.server_build.clone(),
                normalized_contract_hash: context.normalized_contract_hash.clone(),
                server_schema_bundle_hash: context.server_schema_bundle_hash.clone(),
                data_release_hash: context.data_release_hash.clone(),
            },
            scope: context.scope.clone(),
            entity,
            period: period.clone(),
            as_of,
            directness,
            grade,
            strong_claim_allowed,
            payload_ref: context.payload_ref.clone(),
            citation: PublicCitation {
                title: unit.source.source_label.as_deref().map_or_else(
                    || safe_single_line(&unit.title, 512, "KRW ontology evidence"),
                    |title| safe_single_line(title, 512, "KRW ontology evidence"),
                ),
                document_type: clean_optional(unit.document_type.as_deref(), 128),
                // Citation period identifies the filing bucket; the evidence
                // record and its facts above identify the economic period.
                period: document_period,
            },
            facts,
            supports: unit.supports_clause_ids.clone(),
            refutes: Vec::new(),
            qualifies: Vec::new(),
            source_object_ids,
        };
        ensure_record_bound(&record)?;
        records.push(record);
    }
    if state
        .computed_values
        .iter()
        .any(|value| !valid_identifier(&value.calculation_id))
    {
        return Err(AdapterError::InvalidCalculationIdentifier);
    }
    let calculations = state
        .computed_values
        .iter()
        .filter_map(|value| {
            let input_evidence_ids = value
                .source_object_ids
                .iter()
                .filter_map(|object_id| object_to_evidence.get(object_id).cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            if input_evidence_ids.is_empty() {
                return None;
            }
            Some(Calculation {
                calculation_id: value.calculation_id.clone(),
                expression: safe_single_line(&value.kind, 256, "calculation"),
                input_evidence_ids,
                output: value.value.clone().unwrap_or(Value::Null),
                unit: clean_optional(value.unit.as_deref(), 64),
                rounding: None,
                subject: clean_optional(value.tickers.first().map(String::as_str), 256),
                metric: clean_optional(value.metric.as_deref(), 128),
                period: clean_optional(value.period.as_deref(), 128),
                currency: clean_optional(value.currency.as_deref(), 16),
            })
        })
        .collect::<Vec<_>>();
    let all_required_backed_by_strong_evidence = ready_clauses.iter().all(|coverage| {
        records.iter().any(|record| {
            record.strong_claim_allowed
                && record
                    .supports
                    .iter()
                    .any(|clause_id| clause_id == &coverage.clause_id)
                && coverage
                    .evidence_ids
                    .iter()
                    .any(|evidence_id| evidence_id == &record.evidence_id)
        })
    });
    let answerability = match (
        state.answerability.status.as_str(),
        state.answerability.strong_claim_allowed
            && all_required_strong_ready
            && all_required_backed_by_strong_evidence,
    ) {
        ("answerable", true) => Answerability::StrongAllowed,
        ("not_answerable" | "supporting_context_only", _) => Answerability::NotAnswerable,
        _ => Answerability::QualifiedOnly,
    };
    Ok(EvidenceDelta {
        answerability,
        records,
        calculations,
    })
}

/// Returns the single economic period shared by a metric record, if there is
/// one. `EvidenceUnit.period` is a source-document label and must not override
/// a fiscal observation period carried by the metric point itself.
fn research_metric_record_period(
    unit: &EvidenceUnit,
    document_period: Option<String>,
) -> Option<String> {
    if unit.metric_points.is_empty() {
        return document_period;
    }
    let mut periods = BTreeSet::new();
    for point in &unit.metric_points {
        if let Some(period) = clean_optional(Some(point.period.as_str()), 128) {
            periods.insert(period);
        }
    }
    match periods.len() {
        0 => document_period,
        1 => periods.into_iter().next(),
        // A multi-period record is intentionally record-period-less. Each
        // fact retains its own period, which is safer than choosing an
        // arbitrary first or latest point.
        _ => None,
    }
}

/// A date is useful as a recency anchor only when every retained metric point
/// agrees on it. Mixed annual/quarterly/YTD records deliberately expose no
/// record-wide `as_of` date.
fn research_metric_as_of(unit: &EvidenceUnit) -> Option<String> {
    if unit.metric_points.is_empty() {
        return None;
    }
    let mut dates = BTreeSet::new();
    for point in &unit.metric_points {
        if let Some(date) = clean_optional(point.end_date.as_deref(), 128) {
            dates.insert(date);
        }
    }
    (dates.len() == 1)
        .then(|| dates.into_iter().next())
        .flatten()
}

/// The ontology uses `CY2026Q1`-style strings as stable index buckets.  They
/// are useful for routing, but they do not establish an issuer's fiscal
/// quarter: Apple's filing indexed as `CY2026Q1`, for example, ends on March
/// 28 and is not safe to present as "Apple fiscal Q1".  Model-facing evidence
/// therefore receives a neutral document-year label.  Genuine `FY...` and
/// observed date labels pass through unchanged.
fn presentation_document_period(
    period: Option<&str>,
    _document_type: Option<&str>,
) -> Option<String> {
    let period = clean_optional(period, 128)?;
    cy_bucket_year(&period)
        .map(|year| format!("{year}년"))
        .or(Some(period))
}

/// Use the observed end date and basis when a metric is carried in a calendar
/// routing bucket.  This avoids guessing a fiscal-quarter number while still
/// giving the analyst an exact, comparable reporting reference.
fn presentation_observation_period(
    period: Option<&str>,
    period_type: Option<&str>,
    end_date: Option<&str>,
) -> Option<String> {
    let period = clean_optional(period, 128)?;
    if cy_bucket_year(&period).is_none() {
        return Some(period);
    }
    let basis = presentation_period_basis(period_type);
    if let Some(end_date) = clean_optional(end_date, 128) {
        return Some(match basis {
            Some(basis) => format!("{end_date} 종료 {basis}"),
            None => format!("{end_date} 종료"),
        });
    }
    let year = cy_bucket_year(&period).expect("checked above");
    Some(match basis {
        Some(basis) => format!("{year}년 {basis}"),
        None => format!("{year}년"),
    })
}

fn presentation_source_label(
    source_label: Option<&str>,
    raw_period: Option<&str>,
    document_type: Option<&str>,
) -> Option<String> {
    let source_label = clean_optional(source_label, 512)?;
    let Some(raw_period) = clean_optional(raw_period, 128) else {
        return Some(source_label);
    };
    let Some(display_period) = presentation_document_period(Some(&raw_period), document_type)
    else {
        return Some(source_label);
    };
    if raw_period == display_period {
        Some(source_label)
    } else {
        Some(source_label.replace(&raw_period, &display_period))
    }
}

fn cy_bucket_year(value: &str) -> Option<&str> {
    let suffix = value.strip_prefix("CY")?;
    let year = suffix.get(..4)?;
    if !year.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let remainder = suffix.get(4..)?;
    if remainder.is_empty()
        || matches!(remainder.as_bytes(), [b'Q', b'1'..=b'4'])
        || matches!(remainder.as_bytes(), [b' ', b'Q', b'1'..=b'4'])
    {
        Some(year)
    } else {
        None
    }
}

fn presentation_period_basis(period_type: Option<&str>) -> Option<&'static str> {
    let period_type = period_type?.trim().to_ascii_lowercase();
    match period_type.as_str() {
        "annual" | "year" | "yearly" => Some("연간"),
        "quarter" | "quarterly" | "three_months" | "three_month" => Some("분기"),
        "year_to_date" | "ytd" | "six_months" | "nine_months" | "cumulative" => Some("누적"),
        _ => None,
    }
}

/// Retain the observation basis once per primary ResearchState record. The
/// source ontology already supplies this metadata, but raw capability content
/// is compacted away after ingestion; without this compact fact an annual
/// source label can be mistaken for a fiscal quarter or a year-to-date value.
fn research_metric_context(unit: &EvidenceUnit, document_period: Option<&str>) -> Option<Value> {
    const MAX_OBSERVATIONS: usize = 16;
    const MAX_DIMENSIONS: usize = 16;

    let mut context = serde_json::Map::new();
    if let Some(period) =
        presentation_document_period(document_period, unit.document_type.as_deref())
    {
        context.insert("source_document_period".into(), Value::String(period));
    }
    if let Some(document_type) = clean_optional(unit.document_type.as_deref(), 128) {
        context.insert("source_document_type".into(), Value::String(document_type));
    }
    if let Some(currency) = clean_optional(unit.currency.as_deref(), 16) {
        context.insert("currency".into(), Value::String(currency));
    }
    if let Some(scope) = clean_optional(unit.metric_scope.as_deref(), 128) {
        context.insert("metric_scope".into(), Value::String(scope));
    }
    if !unit.dimensions.is_empty() {
        let mut dimensions = serde_json::Map::new();
        for (key, value) in unit.dimensions.iter().take(MAX_DIMENSIONS) {
            let key = safe_single_line(key, 128, "");
            let value = safe_single_line(value, 256, "");
            if !key.is_empty() && !value.is_empty() {
                dimensions.insert(key, Value::String(value));
            }
        }
        if !dimensions.is_empty() {
            context.insert("dimensions".into(), Value::Object(dimensions));
        }
    }

    let observations = unit
        .metric_points
        .iter()
        .filter_map(|point| {
            let mut observation = serde_json::Map::new();
            if let Some(period) = presentation_observation_period(
                Some(point.period.as_str()),
                point.period_type.as_deref(),
                point.end_date.as_deref(),
            ) {
                observation.insert("period".into(), Value::String(period));
            }
            if let Some(period_type) = clean_optional(point.period_type.as_deref(), 64) {
                observation.insert("period_type".into(), Value::String(period_type));
            }
            if let Some(start_date) = clean_optional(point.start_date.as_deref(), 128) {
                observation.insert("start_date".into(), Value::String(start_date));
            }
            if let Some(end_date) = clean_optional(point.end_date.as_deref(), 128) {
                observation.insert("end_date".into(), Value::String(end_date));
            }
            if point.conflict_value_count > 0 {
                observation.insert(
                    "conflict_value_count".into(),
                    Value::from(point.conflict_value_count),
                );
            }
            (!observation.is_empty()).then_some(Value::Object(observation))
        })
        .take(MAX_OBSERVATIONS)
        .collect::<Vec<_>>();
    if !observations.is_empty() {
        context.insert("observations".into(), Value::Array(observations));
    }
    (!context.is_empty()).then_some(Value::Object(context))
}

/// Conservatively project the untyped search response into supplemental
/// evidence. Search results can improve coverage, but they never grant a
/// load-bearing strong-claim permission; only canonical `ResearchState v2`
/// can do that.
pub fn map_targeted_query(
    payload: &Value,
    context: &MappingContext,
) -> Result<SupplementalEvidenceDelta, AdapterError> {
    validate_supplemental_payload(payload)?;
    if payload.get("error").is_some() {
        return Ok(empty_supplemental_delta(supplemental_read_status(
            payload, 0, true,
        )));
    }
    let Some(results) = payload.get("results") else {
        return Ok(empty_supplemental_delta(supplemental_read_status(
            payload, 0, false,
        )));
    };
    let results = results
        .as_array()
        .ok_or(AdapterError::InvalidSupplementalPayload("results"))?;
    if results.len() > MAX_SUPPLEMENTAL_RECORDS {
        return Err(AdapterError::SupplementalItemLimit);
    }
    let records = results
        .iter()
        .map(|item| {
            let object = targeted_result_object(item)?;
            supplemental_record(&object, targeted_result_evidence(item), context)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SupplementalEvidenceDelta {
        records,
        calculations: Vec::new(),
        status: supplemental_read_status(payload, results.len(), true),
    })
}

/// Targeted-query results use an envelope for pagination and source context,
/// while the actual ontology object may be nested under ``object``.  Preserve
/// that object's typed value and identity, filling only harmless envelope
/// metadata that it omits.  Treating the envelope itself as evidence used to
/// replace a numeric observation with its display text.
fn targeted_result_object(item: &Value) -> Result<Value, AdapterError> {
    let item_map = item
        .as_object()
        .ok_or(AdapterError::InvalidSupplementalPayload("result item"))?;
    let Some(object) = item_map.get("object").and_then(Value::as_object) else {
        return Ok(item.clone());
    };
    let mut normalized = object.clone();
    for field in ["ticker", "period", "document_type", "section", "text"] {
        if !normalized.contains_key(field)
            && let Some(value) = item_map.get(field)
        {
            normalized.insert(field.to_owned(), value.clone());
        }
    }
    Ok(Value::Object(normalized))
}

fn targeted_result_evidence(item: &Value) -> Option<&Value> {
    item.get("evidence")
        .or_else(|| item.get("object").and_then(|object| object.get("evidence")))
}

/// Project one trace response. A trace may establish directness or metric
/// lineage for a supplemental premise, but deliberately keeps
/// `strong_claim_allowed=false` because it has no clause-coverage verdict.
pub fn map_trace(
    payload: &Value,
    context: &MappingContext,
) -> Result<SupplementalEvidenceDelta, AdapterError> {
    validate_supplemental_payload(payload)?;
    if payload.get("error").is_some() {
        return Ok(empty_supplemental_delta(supplemental_read_status(
            payload, 0, true,
        )));
    }
    let object = payload
        .get("object")
        .ok_or(AdapterError::InvalidSupplementalPayload("object"))?;
    let record = supplemental_record(object, payload.get("evidence"), context)?;
    let chain_record = chain_context_record(payload, &record, context)?;
    let mut records = vec![record];
    if let Some(record) = chain_record {
        records.push(record);
    }
    Ok(SupplementalEvidenceDelta {
        records,
        calculations: Vec::new(),
        status: supplemental_read_status(payload, 1, true),
    })
}

fn chain_context_record(
    payload: &Value,
    root: &EvidenceRecord,
    context: &MappingContext,
) -> Result<Option<EvidenceRecord>, AdapterError> {
    let Some(chain) = payload.get("chain") else {
        return Ok(None);
    };
    let Some(value) = bounded_chain_context_value(chain) else {
        return Ok(None);
    };
    let content_hash = ContentHash::sha256(serde_jcs::to_vec(&value)?);
    let evidence_id = format!(
        "chain:{}",
        content_hash
            .as_str()
            .trim_start_matches("sha256:")
            .chars()
            .take(40)
            .collect::<String>()
    );
    let mut source_object_ids = root
        .source_object_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    collect_chain_object_ids(chain, &mut source_object_ids, 0);
    let record = EvidenceRecord {
        evidence_id,
        content_hash,
        source: EvidenceSource {
            capability_id: context.capability_id.clone(),
            action_key: context.action_key.clone(),
            server_build: context.server_build.clone(),
            normalized_contract_hash: context.normalized_contract_hash.clone(),
            server_schema_bundle_hash: context.server_schema_bundle_hash.clone(),
            data_release_hash: context.data_release_hash.clone(),
        },
        scope: context.scope.clone(),
        entity: root.entity.clone(),
        period: root.period.clone(),
        as_of: root.as_of.clone(),
        directness: Directness::Related,
        grade: EvidenceGrade::Unverified,
        strong_claim_allowed: false,
        payload_ref: context.payload_ref.clone(),
        citation: PublicCitation {
            title: "KRW ontology relationship context".into(),
            document_type: root.citation.document_type.clone(),
            period: root.citation.period.clone(),
        },
        facts: vec![NormalizedFact {
            subject: root.entity.clone().unwrap_or_else(|| "company".into()),
            predicate: "ontology_chain_context".into(),
            value,
            unit: None,
            period: root.period.clone(),
        }],
        supports: Vec::new(),
        refutes: Vec::new(),
        qualifies: Vec::new(),
        source_object_ids: source_object_ids
            .into_iter()
            .take(MAX_CHAIN_CONTEXT_ITEMS)
            .collect(),
    };
    ensure_record_bound(&record)?;
    Ok(Some(record))
}

fn bounded_chain_context_value(value: &Value) -> Option<Value> {
    let map = value.as_object()?;
    let mut result = serde_json::Map::new();
    for key in [
        "evidence_chain",
        "semantic_neighbors",
        "temporal_context",
        "edge_paths",
    ] {
        let Some(raw) = map.get(key) else {
            continue;
        };
        if let Some(value) = bounded_chain_value(raw, 0) {
            result.insert(key.to_owned(), value);
        }
    }
    (!result.is_empty()).then_some(Value::Object(result))
}

fn bounded_chain_value(value: &Value, depth: usize) -> Option<Value> {
    bounded_chain_value_for_field(value, depth, None)
}

fn bounded_chain_value_for_field(
    value: &Value,
    depth: usize,
    field_name: Option<&str>,
) -> Option<Value> {
    if depth > MAX_CHAIN_CONTEXT_DEPTH {
        return None;
    }
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => Some(value.clone()),
        Value::String(text) => {
            let value = safe_single_line(text, MAX_CHAIN_CONTEXT_STRING_BYTES, "");
            if value.is_empty() {
                return None;
            }
            if field_name.is_some_and(is_chain_period_field) {
                return presentation_document_period(Some(&value), None).map(Value::String);
            }
            Some(Value::String(value))
        }
        Value::Array(values) => {
            let values = values
                .iter()
                .filter_map(|item| bounded_chain_value_for_field(item, depth + 1, None))
                .take(MAX_CHAIN_CONTEXT_ITEMS)
                .collect::<Vec<_>>();
            (!values.is_empty()).then_some(Value::Array(values))
        }
        Value::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort();
            let mut result = serde_json::Map::new();
            for key in keys.into_iter().take(MAX_CHAIN_CONTEXT_ITEMS) {
                if let Some(value) =
                    bounded_chain_value_for_field(&values[key], depth + 1, Some(key.as_str()))
                {
                    result.insert(key.clone(), value);
                }
            }
            (!result.is_empty()).then_some(Value::Object(result))
        }
    }
}

fn is_chain_period_field(field_name: &str) -> bool {
    let field_name = field_name.trim().to_ascii_lowercase();
    field_name == "period" || field_name.ends_with("_period")
}

fn collect_chain_object_ids(value: &Value, identifiers: &mut BTreeSet<String>, depth: usize) {
    if depth > MAX_CHAIN_CONTEXT_DEPTH {
        return;
    }
    match value {
        Value::Array(values) => values
            .iter()
            .take(MAX_CHAIN_CONTEXT_ITEMS)
            .for_each(|value| collect_chain_object_ids(value, identifiers, depth + 1)),
        Value::Object(values) => {
            for (key, value) in values {
                let key = key.to_ascii_lowercase();
                let id_field = key == "id" || key.ends_with("_id");
                let ids_field = key.ends_with("_ids");
                if id_field {
                    if let Some(identifier) = value.as_str().filter(|value| valid_identifier(value))
                    {
                        identifiers.insert(identifier.to_owned());
                    }
                } else if ids_field {
                    for identifier in value
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .filter(|value| valid_identifier(value))
                    {
                        identifiers.insert(identifier.to_owned());
                    }
                }
                collect_chain_object_ids(value, identifiers, depth + 1);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

/// Sanitize the untyped company-topic response before it can reach the model
/// transcript or compacted evidence context. The source store exposes routing
/// metadata and broad topic payloads; neither belongs in a user-facing agent
/// context. Keep only a bounded label-level orientation map for the exact
/// trusted ticker and mark every resulting record as unverified.
pub fn map_company_context(
    payload: &Value,
    expected_ticker: &str,
    context: &MappingContext,
) -> Result<CompanyContextDelta, AdapterError> {
    let bytes = serde_json::to_vec(payload)?;
    if bytes.len() > MAX_SUPPLEMENTAL_PAYLOAD_BYTES {
        return Err(AdapterError::SupplementalPayloadLimit);
    }
    let Some(root) = payload.as_object() else {
        return Ok(unavailable_company_context(expected_ticker));
    };
    if root.get("error").is_some()
        || root.get("ticker").and_then(Value::as_str) != Some(expected_ticker)
    {
        return Ok(unavailable_company_context(expected_ticker));
    }
    let Some(topics) = root.get("company_topics").and_then(Value::as_array) else {
        return Ok(unavailable_company_context(expected_ticker));
    };

    // The source topic index may return several raw entries that collapse to
    // the same safe, label-level orientation record once private metadata is
    // removed.  Keep the first occurrence only.  Otherwise two identical
    // normalized records have the same content hash and are rightly rejected
    // by the capability evidence-lineage boundary, turning harmless source
    // duplication into a failed research run.
    let mut seen_topics = BTreeSet::new();
    let topics = topics
        .iter()
        .filter_map(|topic| company_context_topic(topic, expected_ticker))
        .filter(|topic| {
            seen_topics.insert((
                topic.topic_label.clone(),
                topic.period.clone(),
                topic.document_type.clone(),
                topic.trace_status.clone(),
            ))
        })
        .take(MAX_COMPANY_CONTEXT_TOPICS)
        .collect::<Vec<_>>();
    let records = topics
        .iter()
        .map(|topic| company_context_record(topic, expected_ticker, context))
        .collect::<Result<Vec<_>, _>>()?;
    let status = if topics.is_empty() {
        "empty"
    } else {
        "available"
    };
    let provider_content = serde_json::json!({
        "format": "company-context-orientation/v1",
        "ticker": expected_ticker,
        "status": status,
        "advisory_only": true,
        "topics": topics,
        "usage": "Use these labels only to narrow a later evidence query; do not cite them as company facts."
    });
    Ok(CompanyContextDelta {
        provider_content,
        records,
    })
}

/// Sanitize the fixed market router response before the model sees it. This
/// adapter does not promote volatile values into filing evidence or a
/// strong-claim path, does not expose upstream errors, and accepts only the
/// trusted in-scope ticker.
pub fn map_market_snapshot(
    payload: &Value,
    expected_ticker: &str,
    context: &MappingContext,
) -> Result<MarketSnapshotDelta, AdapterError> {
    if serde_json::to_vec(payload)?.len() > MAX_SUPPLEMENTAL_PAYLOAD_BYTES {
        return Err(AdapterError::SupplementalPayloadLimit);
    }
    let Some(root) = payload.as_object() else {
        return Ok(unavailable_market_snapshot(expected_ticker));
    };
    if root.get("format").and_then(Value::as_str) != Some("market-snapshot/v1")
        || root.get("ticker").and_then(Value::as_str) != Some(expected_ticker)
        || root.get("source").and_then(Value::as_str) != Some("fmp")
        || root.get("source_usage").and_then(Value::as_str) != Some("research_only")
        || root.get("advisory_only").and_then(Value::as_bool) != Some(true)
    {
        return Ok(unavailable_market_snapshot(expected_ticker));
    }
    let status = root.get("status").and_then(Value::as_str);
    if !matches!(status, Some("available" | "unavailable")) {
        return Ok(unavailable_market_snapshot(expected_ticker));
    }
    let metrics = market_snapshot_metrics(root.get("metrics"));
    let status = if status == Some("available") && !metrics.is_empty() {
        "available"
    } else {
        "unavailable"
    };
    let provider_content = serde_json::json!({
        "format": "market-snapshot-context/v1",
        "ticker": expected_ticker,
        "status": status,
        "source": "fmp",
        "source_usage": "research_only",
        "fetched_at": market_timestamp(root.get("fetched_at")),
        "as_of": market_timestamp(root.get("as_of")),
        "currency": market_currency(root.get("currency")),
        "metrics": metrics,
        "advisory_only": true,
        "usage": "Timestamped advisory market context only. Do not treat it as filing evidence or support a recommendation with it."
    });
    let records = if status == "available" {
        vec![market_snapshot_record(
            &provider_content,
            expected_ticker,
            context,
        )?]
    } else {
        Vec::new()
    };
    Ok(MarketSnapshotDelta {
        provider_content,
        records,
    })
}

fn unavailable_market_snapshot(expected_ticker: &str) -> MarketSnapshotDelta {
    MarketSnapshotDelta {
        provider_content: serde_json::json!({
            "format": "market-snapshot-context/v1",
            "ticker": expected_ticker,
            "status": "unavailable",
            "source": "fmp",
            "source_usage": "research_only",
            "fetched_at": null,
            "as_of": null,
            "currency": null,
            "metrics": {},
            "advisory_only": true,
            "usage": "No safe current market snapshot was available. Continue with filing-derived research."
        }),
        records: Vec::new(),
    }
}

fn market_snapshot_record(
    provider_content: &Value,
    expected_ticker: &str,
    context: &MappingContext,
) -> Result<EvidenceRecord, AdapterError> {
    let metrics = provider_content
        .get("metrics")
        .and_then(Value::as_object)
        .ok_or(AdapterError::InvalidSupplementalPayload("market metrics"))?;
    let facts = metrics
        .iter()
        .filter_map(|(metric, value)| {
            value.as_f64().map(|number| NormalizedFact {
                subject: expected_ticker.to_owned(),
                predicate: format!("market_snapshot_{metric}"),
                value: Value::from(number),
                unit: if matches!(
                    metric.as_str(),
                    "last_price" | "previous_close" | "market_cap"
                ) {
                    provider_content
                        .get("currency")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                } else {
                    None
                },
                period: None,
            })
        })
        .collect::<Vec<_>>();
    if facts.is_empty() || facts.len() > MAX_MARKET_SNAPSHOT_METRICS {
        return Err(AdapterError::InvalidSupplementalPayload("market metrics"));
    }
    let content_hash = ContentHash::sha256(serde_jcs::to_vec(provider_content)?);
    let evidence_id = format!(
        "market-advisory:{}",
        content_hash
            .as_str()
            .trim_start_matches("sha256:")
            .chars()
            .take(40)
            .collect::<String>()
    );
    let as_of = provider_content
        .get("as_of")
        .and_then(Value::as_str)
        .or_else(|| provider_content.get("fetched_at").and_then(Value::as_str))
        .map(ToOwned::to_owned);
    let record = EvidenceRecord {
        evidence_id,
        content_hash,
        source: EvidenceSource {
            capability_id: context.capability_id.clone(),
            action_key: context.action_key.clone(),
            server_build: context.server_build.clone(),
            normalized_contract_hash: context.normalized_contract_hash.clone(),
            server_schema_bundle_hash: context.server_schema_bundle_hash.clone(),
            data_release_hash: context.data_release_hash.clone(),
        },
        scope: context.scope.clone(),
        entity: Some(expected_ticker.to_owned()),
        period: None,
        as_of,
        directness: Directness::Unverified,
        grade: EvidenceGrade::Unverified,
        strong_claim_allowed: false,
        payload_ref: context.payload_ref.clone(),
        citation: PublicCitation {
            title: "Timestamped FMP research snapshot (advisory only)".into(),
            document_type: Some("market_snapshot".into()),
            period: None,
        },
        facts,
        supports: Vec::new(),
        refutes: Vec::new(),
        qualifies: Vec::new(),
        source_object_ids: Vec::new(),
    };
    ensure_record_bound(&record)?;
    Ok(record)
}

fn market_snapshot_metrics(value: Option<&Value>) -> BTreeMap<String, f64> {
    let Some(values) = value.and_then(Value::as_object) else {
        return BTreeMap::new();
    };
    [
        "last_price",
        "previous_close",
        "market_cap",
        "trailing_pe",
        "forward_pe",
        "price_to_book",
    ]
    .into_iter()
    .filter_map(|field| {
        let value = values.get(field)?.as_f64()?;
        (value.is_finite() && value.abs() <= MAX_MARKET_METRIC_ABS)
            .then(|| (field.to_owned(), value))
    })
    .take(MAX_MARKET_SNAPSHOT_METRICS)
    .collect()
}

fn market_timestamp(value: Option<&Value>) -> Option<String> {
    let value = value.and_then(Value::as_str)?;
    if !(1..=64).contains(&value.len())
        || !value.is_ascii()
        || !value.bytes().all(|byte| {
            byte.is_ascii_digit() || matches!(byte, b'-' | b':' | b'.' | b'+' | b'T' | b'Z')
        })
    {
        return None;
    }
    Some(value.to_owned())
}

fn market_currency(value: Option<&Value>) -> Option<String> {
    let value = value.and_then(Value::as_str)?;
    if !(1..=8).contains(&value.len()) || !value.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return None;
    }
    Some(value.to_owned())
}

fn unavailable_company_context(expected_ticker: &str) -> CompanyContextDelta {
    CompanyContextDelta {
        provider_content: serde_json::json!({
            "format": "company-context-orientation/v1",
            "ticker": expected_ticker,
            "status": "unavailable",
            "advisory_only": true,
            "topics": [],
            "usage": "No safe orientation data was available. Continue with question-specific evidence retrieval."
        }),
        records: Vec::new(),
    }
}

#[derive(Debug, Clone, Serialize)]
struct CompanyContextTopic {
    topic_label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    period: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    document_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trace_status: Option<String>,
}

fn company_context_topic(topic: &Value, expected_ticker: &str) -> Option<CompanyContextTopic> {
    let topic = topic.as_object()?;
    if topic
        .get("ticker")
        .and_then(Value::as_str)
        .is_some_and(|ticker| ticker != expected_ticker)
    {
        return None;
    }
    let topic_label = public_orientation_text(topic.get("topic_label"), 256)?;
    let document_type = public_orientation_text(topic.get("document_type"), 128);
    let raw_period = public_orientation_text(topic.get("period"), 128);
    Some(CompanyContextTopic {
        topic_label,
        period: presentation_document_period(raw_period.as_deref(), document_type.as_deref()),
        document_type,
        trace_status: public_orientation_text(topic.get("trace_status"), 128),
    })
}

fn public_orientation_text(value: Option<&Value>, max_bytes: usize) -> Option<String> {
    let text = value.and_then(Value::as_str)?;
    let text = safe_single_line(text, max_bytes, "");
    if text.is_empty()
        || ["/Users/", "/home/", "/tmp/", "file://", "\\Users\\"]
            .iter()
            .any(|marker| text.contains(marker))
    {
        return None;
    }
    Some(text)
}

fn company_context_record(
    topic: &CompanyContextTopic,
    expected_ticker: &str,
    context: &MappingContext,
) -> Result<EvidenceRecord, AdapterError> {
    let content_hash = ContentHash::sha256(serde_jcs::to_vec(topic)?);
    let evidence_id = format!(
        "orientation:{}",
        content_hash
            .as_str()
            .trim_start_matches("sha256:")
            .chars()
            .take(40)
            .collect::<String>()
    );
    let record = EvidenceRecord {
        evidence_id,
        content_hash,
        source: EvidenceSource {
            capability_id: context.capability_id.clone(),
            action_key: context.action_key.clone(),
            server_build: context.server_build.clone(),
            normalized_contract_hash: context.normalized_contract_hash.clone(),
            server_schema_bundle_hash: context.server_schema_bundle_hash.clone(),
            data_release_hash: context.data_release_hash.clone(),
        },
        scope: context.scope.clone(),
        entity: Some(expected_ticker.to_owned()),
        period: topic.period.clone(),
        as_of: None,
        directness: Directness::Unverified,
        grade: EvidenceGrade::Unverified,
        strong_claim_allowed: false,
        payload_ref: context.payload_ref.clone(),
        citation: PublicCitation {
            title: format!("KRW ontology company orientation: {}", topic.topic_label),
            document_type: topic.document_type.clone(),
            period: topic.period.clone(),
        },
        facts: vec![NormalizedFact {
            subject: expected_ticker.to_owned(),
            predicate: COMPANY_TOPIC_ORIENTATION_PREDICATE.into(),
            value: Value::String(topic.topic_label.clone()),
            unit: None,
            period: topic.period.clone(),
        }],
        supports: Vec::new(),
        refutes: Vec::new(),
        qualifies: Vec::new(),
        source_object_ids: Vec::new(),
    };
    ensure_record_bound(&record)?;
    Ok(record)
}

fn empty_supplemental_delta(status: SupplementalReadStatus) -> SupplementalEvidenceDelta {
    SupplementalEvidenceDelta {
        records: Vec::new(),
        calculations: Vec::new(),
        status,
    }
}

fn supplemental_read_status(
    payload: &Value,
    result_count: usize,
    results_present: bool,
) -> SupplementalReadStatus {
    let pagination = payload.get("pagination").and_then(Value::as_object);
    let has_more = pagination
        .and_then(|value| value.get("has_more"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let next_offset = pagination.and_then(|value| {
        value
            .get("next_offset")
            .and_then(Value::as_u64)
            .or_else(|| {
                value
                    .get("next_cursor")
                    .and_then(Value::as_str)
                    .and_then(|cursor| cursor.parse::<u64>().ok())
            })
            .and_then(|offset| u32::try_from(offset).ok())
    });
    let bounded_count = result_count.min(MAX_SUPPLEMENTAL_RECORDS);
    let mut warning_codes = Vec::new();

    let kind = if let Some(error) = payload.get("error") {
        let code = supplemental_error_code(error);
        let (kind, warning) = classify_supplemental_error(&code);
        warning_codes.push(warning.to_owned());
        kind
    } else if !results_present {
        warning_codes.push("supplemental_results_missing".to_owned());
        SupplementalReadKind::ApplicationError
    } else if bounded_count == 0 {
        SupplementalReadKind::Empty
    } else {
        if has_more {
            warning_codes.push("supplemental_truncated".to_owned());
        }
        if payload
            .get("results")
            .and_then(Value::as_array)
            .is_some_and(|results| {
                results.iter().any(|result| {
                    result
                        .get("evidence_summary")
                        .and_then(|summary| summary.get("truncated"))
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                })
            })
        {
            warning_codes.push("compact_evidence_truncated".to_owned());
        }
        SupplementalReadKind::Retrieved
    };

    warning_codes.sort();
    warning_codes.dedup();
    SupplementalReadStatus {
        kind,
        result_count: u16::try_from(bounded_count).unwrap_or(u16::MAX),
        has_more,
        next_offset,
        warning_codes,
    }
}

pub fn supplemental_status_for_targeted_payload(
    payload: &Value,
) -> Result<SupplementalReadStatus, AdapterError> {
    validate_supplemental_payload(payload)?;
    if payload.get("error").is_some() {
        return Ok(supplemental_read_status(payload, 0, true));
    }
    let Some(results) = payload.get("results") else {
        return Ok(supplemental_read_status(payload, 0, false));
    };
    let results = results
        .as_array()
        .ok_or(AdapterError::InvalidSupplementalPayload("results"))?;
    Ok(supplemental_read_status(payload, results.len(), true))
}

pub fn supplemental_status_for_trace_payload(
    payload: &Value,
) -> Result<SupplementalReadStatus, AdapterError> {
    validate_supplemental_payload(payload)?;
    if payload.get("error").is_some() {
        return Ok(supplemental_read_status(payload, 0, true));
    }
    let object_present = payload.get("object").is_some();
    Ok(supplemental_read_status(
        payload,
        if object_present { 1 } else { 0 },
        object_present,
    ))
}

fn supplemental_error_code(error: &Value) -> String {
    error
        .as_str()
        .or_else(|| error.get("code").and_then(Value::as_str))
        .or_else(|| error.get("status").and_then(Value::as_str))
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn classify_supplemental_error(code: &str) -> (SupplementalReadKind, &'static str) {
    if code.contains("not_found") || code.contains("not found") {
        (SupplementalReadKind::NotFound, "supplemental_not_found")
    } else if code.contains("ambiguous") {
        (SupplementalReadKind::Ambiguous, "supplemental_ambiguous")
    } else if code.contains("invalid") || code.contains("input") || code.contains("validation") {
        (
            SupplementalReadKind::InputRejected,
            "supplemental_input_rejected",
        )
    } else {
        (
            SupplementalReadKind::ApplicationError,
            "supplemental_unclassified_error",
        )
    }
}

fn validate_supplemental_payload(payload: &Value) -> Result<(), AdapterError> {
    if !payload.is_object() {
        return Err(AdapterError::InvalidSupplementalPayload("root"));
    }
    if serde_json::to_vec(payload)?.len() > MAX_SUPPLEMENTAL_PAYLOAD_BYTES {
        return Err(AdapterError::SupplementalPayloadLimit);
    }
    Ok(())
}

fn supplemental_record(
    object: &Value,
    evidence: Option<&Value>,
    context: &MappingContext,
) -> Result<EvidenceRecord, AdapterError> {
    let object_map = object
        .as_object()
        .ok_or(AdapterError::InvalidSupplementalPayload("evidence object"))?;
    let evidence_map = evidence.and_then(Value::as_object);
    let content_hash = ContentHash::sha256(serde_jcs::to_vec(&SupplementalFingerprint {
        object,
        evidence,
    })?);
    let evidence_id = format!(
        "supp:{}",
        content_hash
            .as_str()
            .trim_start_matches("sha256:")
            .chars()
            .take(40)
            .collect::<String>()
    );
    let quotes = evidence_map
        .and_then(|value| value.get("quotes"))
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let spans = evidence_map
        .and_then(|value| value.get("spans"))
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let metric_lineage = evidence_map
        .and_then(|value| value.get("metric_lineage"))
        .filter(|value| valid_metric_lineage(value));
    let has_source_excerpt = quotes.iter().chain(spans).any(valid_source_excerpt);
    let directness = if has_source_excerpt {
        Directness::Direct
    } else if metric_lineage.is_some() {
        Directness::MetricLineage
    } else {
        Directness::Related
    };
    let grade = object_map
        .get("quality")
        .and_then(Value::as_object)
        .and_then(|quality| quality.get("evidence_grade"))
        .or_else(|| object_map.get("evidence_grade"))
        .and_then(Value::as_str)
        .map_or(EvidenceGrade::Unverified, conservative_grade);
    let entity = bounded_optional(object_map.get("ticker"), 128);
    let raw_period = bounded_optional(object_map.get("period"), 128);
    let document_type = bounded_optional(object_map.get("document_type"), 128);
    let period = presentation_observation_period(
        raw_period.as_deref(),
        object_map.get("period_type").and_then(Value::as_str),
        object_map.get("end_date").and_then(Value::as_str),
    );
    let citation_period =
        presentation_document_period(raw_period.as_deref(), document_type.as_deref());
    let subject = entity.clone().unwrap_or_else(|| "company".into());
    let mut facts = Vec::with_capacity(MAX_SUPPLEMENTAL_FACTS);
    let metric_name = object_map
        .get("metric_name")
        .or_else(|| object_map.get("canonical_metric"))
        .or_else(|| object_map.get("metric"))
        .and_then(Value::as_str)
        .map(|value| safe_single_line(value, 128, "metric"))
        .filter(|value| !value.is_empty());
    if let (Some(metric_name), Some(value)) = (
        metric_name.as_deref(),
        object_map.get("value").filter(|value| !value.is_null()),
    ) {
        facts.push(NormalizedFact {
            subject: safe_single_line(&subject, 256, "company"),
            predicate: metric_name.to_owned(),
            value: value.clone(),
            unit: bounded_optional(object_map.get("unit"), 64),
            period: period.clone(),
        });
        if let Some(context_value) = supplemental_metric_context(object_map) {
            facts.push(NormalizedFact {
                subject: safe_single_line(&subject, 256, "company"),
                predicate: "metric_context".into(),
                value: context_value,
                unit: None,
                period: period.clone(),
            });
        }
    } else if let Some(value) = first_display_value(object_map) {
        facts.push(NormalizedFact {
            subject: safe_single_line(&subject, 256, "company"),
            predicate: "ontology_object".into(),
            value,
            unit: bounded_optional(object_map.get("unit"), 64),
            period: period.clone(),
        });
    }
    for quote in quotes
        .iter()
        .chain(spans)
        .take(MAX_SUPPLEMENTAL_FACTS - facts.len())
    {
        let Some(text) = quote
            .as_object()
            .and_then(|item| item.get("text"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        facts.push(NormalizedFact {
            subject: safe_single_line(&subject, 256, "company"),
            predicate: "source_excerpt".into(),
            value: Value::String(safe_single_line(text, 8 * 1024, "source excerpt")),
            unit: None,
            period: period.clone(),
        });
    }
    if facts.is_empty() {
        facts.push(NormalizedFact {
            subject: safe_single_line(&subject, 256, "company"),
            predicate: "ontology_reference".into(),
            value: Value::String("supplemental ontology object".into()),
            unit: None,
            period: period.clone(),
        });
    }
    let title = object_map
        .get("section")
        .or_else(|| object_map.get("type"))
        .and_then(Value::as_str)
        .map_or_else(
            || "KRW ontology evidence".into(),
            |value| safe_single_line(value, 512, "KRW ontology evidence"),
        );
    let record = EvidenceRecord {
        evidence_id,
        content_hash,
        source: EvidenceSource {
            capability_id: context.capability_id.clone(),
            action_key: context.action_key.clone(),
            server_build: context.server_build.clone(),
            normalized_contract_hash: context.normalized_contract_hash.clone(),
            server_schema_bundle_hash: context.server_schema_bundle_hash.clone(),
            data_release_hash: context.data_release_hash.clone(),
        },
        scope: context.scope.clone(),
        entity,
        period: period.clone(),
        as_of: None,
        directness,
        grade,
        strong_claim_allowed: false,
        payload_ref: context.payload_ref.clone(),
        citation: PublicCitation {
            title,
            document_type,
            period: citation_period,
        },
        facts,
        supports: Vec::new(),
        refutes: Vec::new(),
        qualifies: Vec::new(),
        source_object_ids: supplemental_source_object_ids(object_map, evidence_map),
    };
    ensure_record_bound(&record)?;
    Ok(record)
}

fn supplemental_source_object_ids(
    object: &serde_json::Map<String, Value>,
    evidence: Option<&serde_json::Map<String, Value>>,
) -> Vec<String> {
    let mut identifiers = BTreeSet::new();
    for source in [Some(object), evidence] {
        let Some(source) = source else {
            continue;
        };
        for field in ["id", "object_id"] {
            if let Some(identifier) = source.get(field).and_then(Value::as_str)
                && valid_identifier(identifier)
            {
                identifiers.insert(identifier.to_owned());
            }
        }
        for field in ["object_ids", "source_object_ids"] {
            let Some(values) = source.get(field).and_then(Value::as_array) else {
                continue;
            };
            for identifier in values.iter().filter_map(Value::as_str) {
                if valid_identifier(identifier) {
                    identifiers.insert(identifier.to_owned());
                }
            }
        }
    }
    identifiers
        .into_iter()
        .take(MAX_SUPPLEMENTAL_FACTS)
        .collect()
}

fn supplemental_metric_context(object: &serde_json::Map<String, Value>) -> Option<Value> {
    let mut context = serde_json::Map::new();
    for field in [
        "currency",
        "metric_scope",
        "period_type",
        "start_date",
        "end_date",
        "dimensions",
    ] {
        let Some(value) = object.get(field) else {
            continue;
        };
        if value.is_null() || !serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= 8 * 1024)
        {
            continue;
        }
        context.insert(field.to_owned(), value.clone());
    }
    (!context.is_empty()).then_some(Value::Object(context))
}

fn ensure_record_bound(record: &EvidenceRecord) -> Result<(), AdapterError> {
    let bytes = Zeroizing::new(serde_json::to_vec(record)?);
    if bytes.len() > MAX_NORMALIZED_RECORD_BYTES {
        return Err(AdapterError::NormalizedRecordLimit);
    }
    Ok(())
}

#[derive(Serialize)]
struct SupplementalFingerprint<'a> {
    object: &'a Value,
    evidence: Option<&'a Value>,
}

fn valid_source_excerpt(value: &Value) -> bool {
    value
        .as_object()
        .and_then(|item| item.get("text"))
        .and_then(Value::as_str)
        .is_some_and(|text| !text.trim().is_empty() && text.len() <= 64 * 1024)
}

fn valid_metric_lineage(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    object.values().any(|value| match value {
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
        Value::String(text) => !text.trim().is_empty(),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    })
}

fn conservative_grade(value: &str) -> EvidenceGrade {
    match value {
        "strong" => EvidenceGrade::Strong,
        "medium" => EvidenceGrade::Medium,
        "weak" => EvidenceGrade::Weak,
        _ => EvidenceGrade::Unverified,
    }
}

fn first_display_value(object: &serde_json::Map<String, Value>) -> Option<Value> {
    for field in [
        "text",
        "claim_text",
        "quote_text",
        "description",
        "name",
        "value",
    ] {
        let Some(value) = object.get(field) else {
            continue;
        };
        if let Some(text) = value.as_str() {
            if !text.trim().is_empty() {
                return Some(Value::String(safe_single_line(text, 32 * 1024, "evidence")));
            }
        } else if !value.is_null()
            && serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= 32 * 1024)
        {
            return Some(value.clone());
        }
    }
    None
}

fn bounded_optional(value: Option<&Value>, max_bytes: usize) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(|value| safe_single_line(value, max_bytes, ""))
}

fn clean_optional(value: Option<&str>, max_bytes: usize) -> Option<String> {
    value
        .filter(|value| !value.trim().is_empty())
        .map(|value| safe_single_line(value, max_bytes, ""))
        .filter(|value| !value.is_empty())
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn safe_single_line(value: &str, max_bytes: usize, fallback: &str) -> String {
    let collapsed = value
        .split_whitespace()
        .filter(|part| !part.chars().any(char::is_control))
        .collect::<Vec<_>>()
        .join(" ");
    let source = if collapsed.is_empty() {
        fallback
    } else {
        &collapsed
    };
    let mut output = String::with_capacity(source.len().min(max_bytes));
    for character in source.chars() {
        if output.len() + character.len_utf8() > max_bytes {
            break;
        }
        output.push(character);
    }
    output
}

fn parse_directness(value: &str) -> Result<Directness, AdapterError> {
    match value {
        "direct" => Ok(Directness::Direct),
        "metric_lineage" => Ok(Directness::MetricLineage),
        "related" => Ok(Directness::Related),
        "unverified" => Ok(Directness::Unverified),
        other => Err(AdapterError::UnknownDirectness(other.into())),
    }
}

fn parse_grade(value: &str) -> Result<EvidenceGrade, AdapterError> {
    match value {
        "strong" => Ok(EvidenceGrade::Strong),
        "medium" => Ok(EvidenceGrade::Medium),
        "weak" => Ok(EvidenceGrade::Weak),
        "unverified" => Ok(EvidenceGrade::Unverified),
        other => Err(AdapterError::UnknownGrade(other.into())),
    }
}

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("invalid ResearchState JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported ResearchState contract: {0}")]
    UnsupportedContract(String),
    #[error("unknown evidence directness: {0}")]
    UnknownDirectness(String),
    #[error("unknown evidence grade: {0}")]
    UnknownGrade(String),
    #[error("evidence or clause identifier is outside the normalized ABI")]
    InvalidEvidenceIdentifier,
    #[error("calculation identifier is outside the normalized ABI")]
    InvalidCalculationIdentifier,
    #[error("ResearchState exceeds the normalized evidence ABI bounds")]
    ResearchStateLimit,
    #[error("normalized evidence record exceeds its byte bound")]
    NormalizedRecordLimit,
    #[error("invalid supplemental ontology payload field: {0}")]
    InvalidSupplementalPayload(&'static str),
    #[error("supplemental ontology payload exceeded its byte limit")]
    SupplementalPayloadLimit,
    #[error("supplemental ontology payload exceeded its item limit")]
    SupplementalItemLimit,
    #[error("invalid ResearchState planning projection: {0}")]
    InvalidPlanningProjection(&'static str),
    #[error("invalid evidence-goal graph: {0}")]
    Planning(#[from] krw_agent_planning::PlanningError),
}

#[cfg(test)]
mod tests {
    use krw_agent_evidence::EvidenceLedger;
    use krw_agent_protocol::AuthScope;

    use super::*;

    fn context(capability_id: &str) -> MappingContext {
        MappingContext {
            capability_id: capability_id.into(),
            action_key: "action:1".into(),
            server_build: "build-1".into(),
            normalized_contract_hash: ContentHash::sha256("normalized"),
            server_schema_bundle_hash: ContentHash::sha256("server-schema"),
            data_release_hash: ContentHash::sha256("release"),
            scope: EvidenceScope {
                auth_scope: AuthScope::Tenant,
                scope_hash: ContentHash::sha256("tenant"),
            },
            payload_ref: ContentHash::sha256("payload"),
        }
    }

    fn answerable_fixture() -> ResearchStateV2 {
        parse_research_state(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/vertical-slice/v1/mcp/research-state-answerable.json"
        )))
        .unwrap()
    }

    #[test]
    fn canonical_coverage_compiles_to_a_satisfied_goal() {
        let state = answerable_fixture();
        let projection = derive_research_planning_projection(&state, &[]).unwrap();
        assert_eq!(projection.clauses.len(), 1);
        assert_eq!(projection.clauses[0].clause_id, "cash_generation");
        assert_eq!(projection.clauses[0].retrieval_query, "VG cash generation");
        let graph = projection.graph;
        assert_eq!(graph.goals().count(), 1);
        let goal = graph.goal("clause:cash_generation").unwrap();
        assert!(goal.required);
        assert_eq!(goal.status, GoalStatus::Satisfied);
        assert_eq!(goal.coverage_ppm, PPM);
        assert_eq!(goal.directness, DirectnessRequirement::Direct);
        assert!(graph.frontier().is_empty());
    }

    #[test]
    fn fixed_company_scope_withholds_foreign_evidence_without_rejecting_the_read() {
        let mut state = answerable_fixture();
        state.evidence_units[0].ticker = Some("MSFT".into());
        state.clause_coverage[0].covered_tickers = vec!["MSFT".into()];
        state.clause_coverage[0].missing_tickers = Vec::new();

        let sanitized = sanitize_research_state_scope(&state, &["VG".into()]);

        assert!(sanitized.evidence_units.is_empty());
        assert_eq!(sanitized.clause_coverage[0].status, "missing");
        assert!(!sanitized.answerability.strong_claim_allowed);
        assert!(
            sanitized
                .warnings
                .iter()
                .any(|warning| warning == "out_of_scope_evidence_withheld")
        );
        let delta = map_research_state(&sanitized, &context("ontology.query_context"))
            .expect("scope cleanup remains a valid partial research result");
        assert!(delta.records.is_empty());
        assert_eq!(delta.answerability, Answerability::NotAnswerable);
    }

    #[test]
    fn planning_projection_keeps_exact_full_detail_query_for_required_gap() {
        let mut state = answerable_fixture();
        state.missing_parts = vec![MissingPart {
            code: "metric_calculation_unavailable".into(),
            detail: "upstream metric retrieval was truncated".into(),
            clause_id: Some("cash_generation".into()),
            ticker: Some("VG".into()),
        }];
        state.continuation = Some(Continuation {
            has_more: true,
            omitted_evidence_count: 17,
            reason: Some("response evidence limit reached".into()),
        });
        let projection = derive_research_planning_projection(&state, &[]).unwrap();

        assert_eq!(projection.exact_precise_query_candidates.len(), 1);
        assert_eq!(
            projection.exact_precise_query_candidates[0],
            ExactTargetedQueryCandidate {
                clause_id: "cash_generation".into(),
                ticker: "VG".into(),
                topic: "VG cash generation".into(),
                document_types: vec!["10-K".into()],
                periods: Vec::new(),
                object_types: Vec::new(),
                answer_candidate_only: true,
                response_detail: "compact".into(),
                limit: 20,
            }
        );
    }

    #[test]
    fn exact_gap_candidates_use_one_filing_phrase_for_company_cost_metrics() {
        let mut state = answerable_fixture();
        state.plan = serde_json::json!({
            "tickers": ["AAPL"],
            "document_types": [],
            "periods": [],
            "clauses": [
                {
                    "clause_id": "rd",
                    "required": true,
                    "tickers": ["AAPL"],
                    "retrieval_query": "AAPL R&D expense research and development rd_expense",
                    "metrics": ["research_and_development"],
                    "metric_scope": "company_total",
                    "object_types": ["MetricObservation", "XBRLFact"]
                },
                {
                    "clause_id": "sga",
                    "required": true,
                    "tickers": ["AAPL"],
                    "retrieval_query": "AAPL SG&A expense selling general administrative sga",
                    "metrics": ["selling_general_and_admin"],
                    "metric_scope": "company_total",
                    "object_types": ["MetricObservation", "XBRLFact"]
                }
            ]
        });
        state.missing_parts = vec![
            MissingPart {
                code: "metric_calculation_unavailable".into(),
                detail: "needs a focused follow-up".into(),
                clause_id: Some("rd".into()),
                ticker: Some("AAPL".into()),
            },
            MissingPart {
                code: "metric_calculation_unavailable".into(),
                detail: "needs a focused follow-up".into(),
                clause_id: Some("sga".into()),
                ticker: Some("AAPL".into()),
            },
        ];

        let candidates = exact_precise_query_candidates(&state);
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].topic, "research and development");
        assert_eq!(candidates[1].topic, "selling general administrative");
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.answer_candidate_only)
        );
    }

    #[test]
    fn exact_gap_candidates_keep_all_twelve_required_clauses() {
        let mut state = answerable_fixture();
        let clauses = (0..12)
            .map(|index| {
                serde_json::json!({
                    "clause_id": format!("metric-{index}"),
                    "required": true,
                    "tickers": ["AAPL"],
                    "retrieval_query": format!("AAPL metric {index}"),
                    "metrics": [],
                    "metric_scope": "any",
                    "object_types": []
                })
            })
            .collect::<Vec<_>>();
        state.plan = serde_json::json!({"tickers": ["AAPL"], "clauses": clauses});
        state.missing_parts = (0..12)
            .map(|index| MissingPart {
                code: "missing".into(),
                detail: "needs a focused follow-up".into(),
                clause_id: Some(format!("metric-{index}")),
                ticker: Some("AAPL".into()),
            })
            .collect();

        assert_eq!(exact_precise_query_candidates(&state).len(), 12);
    }

    #[test]
    fn planning_projection_rejects_unbounded_server_gap_lists() {
        let mut state = answerable_fixture();
        state.missing_parts = (0..=MAX_PLANNING_GAPS)
            .map(|index| MissingPart {
                code: format!("gap_{index}"),
                detail: "bounded gap".into(),
                clause_id: Some("cash_generation".into()),
                ticker: Some("VG".into()),
            })
            .collect();
        assert!(matches!(
            derive_research_planning_projection(&state, &[]),
            Err(AdapterError::InvalidPlanningProjection(
                "planning gap limit"
            ))
        ));
    }

    #[test]
    fn dense_clause_coverage_is_bounded_for_the_planning_graph_without_failing_the_run() {
        let mut state = answerable_fixture();
        state.clause_coverage[0].evidence_ids =
            (0..40).map(|index| format!("ev-dense-{index}")).collect();

        let graph = derive_evidence_goal_graph(&state).unwrap();
        let goal = graph.goal("clause:cash_generation").unwrap();

        assert_eq!(goal.status, GoalStatus::Satisfied);
        assert_eq!(goal.evidence_ids.len(), 32);
        assert_eq!(goal.evidence_ids.first().unwrap(), "ev-dense-0");
        assert_eq!(goal.evidence_ids.last().unwrap(), "ev-dense-31");
    }

    #[test]
    fn partial_clause_and_calculation_progress_stays_conservative() {
        let mut state = answerable_fixture();
        let clause = &mut state.clause_coverage[0];
        clause.status = "partial".into();
        clause.covered_tickers = vec!["VG".into()];
        clause.missing_tickers = vec!["XOM".into()];
        clause.strong_claim_ready = false;
        state.calculation_coverage.push(CalculationCoverage {
            clause_id: "cash_generation".into(),
            metric: "operating_cash_flow".into(),
            axis: "growth_rate".into(),
            metric_scope: "company_total".into(),
            metric_dimensions: Vec::new(),
            status: "partial".into(),
            required_tickers: vec!["VG".into(), "XOM".into()],
            covered_tickers: vec!["VG".into()],
            calculation_ids: vec!["calc:vg".into()],
            reason: Some("missing aligned XOM period".into()),
        });

        let graph = derive_evidence_goal_graph(&state).unwrap();
        assert_eq!(graph.goals().count(), 2);
        let clause = graph.goal("clause:cash_generation").unwrap();
        assert_eq!(clause.status, GoalStatus::Partial);
        assert_eq!(clause.coverage_ppm, PPM / 2);
        let calculation = graph
            .goals()
            .find(|goal| goal.goal_id.starts_with("calculation:cash_generation:"))
            .unwrap();
        assert_eq!(calculation.status, GoalStatus::Partial);
        assert_eq!(calculation.coverage_ppm, PPM / 2);
        assert_eq!(calculation.dependencies, ["clause:cash_generation"]);
        assert_eq!(graph.frontier().len(), 1);
        assert_eq!(graph.frontier()[0].goal_id, "clause:cash_generation");
    }

    #[test]
    fn targeted_quotes_are_direct_but_never_grant_strong_claims() {
        let payload = serde_json::json!({
            "results": [{
                "id": "claim:AAPL:1",
                "type": "ResearchClaim",
                "ticker": "AAPL",
                "document_type": "10-Q",
                "period": "FY2026Q2",
                "section": "Management discussion",
                "text": "Revenue\ncontinued to grow.",
                "quality": {"evidence_grade": "strong"},
                "evidence": {
                    "quotes": [{"text": "Revenue grew 8%."}],
                    "spans": [],
                    "metric_lineage": null
                }
            }]
        });
        let delta = map_targeted_query(&payload, &context("ontology.query")).unwrap();
        assert_eq!(delta.records.len(), 1);
        let record = &delta.records[0];
        assert_eq!(record.directness, Directness::Direct);
        assert_eq!(record.grade, EvidenceGrade::Strong);
        assert!(!record.strong_claim_allowed);
        assert_eq!(record.source.capability_id, "ontology.query");
        assert!(record.facts.iter().all(|fact| {
            fact.value
                .as_str()
                .is_none_or(|text| !text.contains(['\n', '\r']))
        }));
        EvidenceLedger::from_records(delta.records).unwrap();
    }

    #[test]
    fn metric_lineage_is_preserved_without_being_upgraded() {
        let payload = serde_json::json!({
            "results": [{
                "id": "metric:AAPL:revenue",
                "ticker": "AAPL",
                "value": 100,
                "evidence": {
                    "quotes": [],
                    "spans": [],
                    "metric_lineage": {"source_document_ids": ["doc:1"]}
                }
            }]
        });
        let delta = map_targeted_query(&payload, &context("ontology.query")).unwrap();
        assert_eq!(delta.records[0].directness, Directness::MetricLineage);
        assert!(!delta.records[0].strong_claim_allowed);
    }

    #[test]
    fn targeted_metric_envelope_preserves_the_nested_numeric_object_and_lineage_anchor() {
        let payload = serde_json::json!({
            "results": [{
                "ticker": "AVGO",
                "period": "CY2026Q1",
                "document_type": "10-Q",
                "text": "Net revenue observation",
                "object": {
                    "id": "metric:AVGO:revenue:CY2026Q1",
                    "type": "MetricObservation",
                    "metric_name": "revenue",
                    "value": 15400,
                    "unit": "USD_millions",
                    "currency": "USD",
                    "dimensions": {"scope": "company_total"}
                },
                "evidence": {
                    "quotes": [],
                    "spans": [],
                    "metric_lineage": {"source_document_ids": ["doc:AVGO:1"]}
                }
            }]
        });

        let delta = map_targeted_query(&payload, &context("ontology.query")).unwrap();
        let record = &delta.records[0];
        assert_eq!(record.entity.as_deref(), Some("AVGO"));
        assert_eq!(record.period.as_deref(), Some("2026년"));
        assert_eq!(record.source_object_ids, ["metric:AVGO:revenue:CY2026Q1"]);
        assert!(record.facts.iter().any(|fact| {
            fact.predicate == "revenue"
                && fact.value == serde_json::json!(15400)
                && fact.unit.as_deref() == Some("USD_millions")
                && fact.period.as_deref() == Some("2026년")
        }));
    }

    #[test]
    fn company_context_is_sanitized_and_never_becomes_claim_evidence() {
        let payload = serde_json::json!({
            "ticker": "AAPL",
            "company_topics": [{
                "ticker": "AAPL",
                "topic_label": "Component Procurement",
                "topic_summary": "Contains noisy LNG terms and must not be shown as evidence.",
                "period": "CY2026Q1",
                "document_type": "10-K",
                "trace_status": "traceable",
                "object_ids": ["private:object:1"]
            }],
            "routing": {
                "index_path": "/Users/private/index.sqlite",
                "internal_ids": ["private:route:1"]
            }
        });

        let delta =
            map_company_context(&payload, "AAPL", &context("ontology.company_context")).unwrap();

        assert_eq!(delta.provider_content["status"], "available");
        assert_eq!(
            delta.provider_content["topics"][0]["topic_label"],
            "Component Procurement"
        );
        assert!(delta.provider_content.get("routing").is_none());
        assert!(!delta.provider_content.to_string().contains("LNG"));
        assert!(!delta.provider_content.to_string().contains("/Users/"));
        assert!(!delta.provider_content.to_string().contains("CY2026Q1"));
        assert_eq!(delta.records.len(), 1);
        assert_eq!(delta.records[0].directness, Directness::Unverified);
        assert_eq!(delta.records[0].grade, EvidenceGrade::Unverified);
        assert!(!delta.records[0].strong_claim_allowed);
        EvidenceLedger::from_records(delta.records).unwrap();
    }

    #[test]
    fn company_context_deduplicates_topics_that_collapse_after_sanitization() {
        let payload = serde_json::json!({
            "ticker": "AAPL",
            "company_topics": [
                {
                    "ticker": "AAPL",
                    "topic_label": "Inflation",
                    "period": "FY2025",
                    "document_type": "10-K",
                    "trace_status": "traceable",
                    "topic_summary": "First private source summary.",
                    "private_source_id": "topic:one"
                },
                {
                    "ticker": "AAPL",
                    "topic_label": "Inflation",
                    "period": "FY2025",
                    "document_type": "10-K",
                    "trace_status": "traceable",
                    "topic_summary": "Different private source summary.",
                    "private_source_id": "topic:two"
                }
            ]
        });

        let delta =
            map_company_context(&payload, "AAPL", &context("ontology.company_context")).unwrap();

        assert_eq!(
            delta.provider_content["topics"].as_array().unwrap().len(),
            1
        );
        assert_eq!(delta.records.len(), 1);
        EvidenceLedger::from_records(delta.records).unwrap();
    }

    #[test]
    fn cross_ticker_company_context_is_withheld() {
        let payload = serde_json::json!({
            "ticker": "MSFT",
            "company_topics": [{"topic_label": "Cloud"}]
        });
        let delta =
            map_company_context(&payload, "AAPL", &context("ontology.company_context")).unwrap();
        assert_eq!(delta.provider_content["status"], "unavailable");
        assert!(delta.records.is_empty());
    }

    #[test]
    fn planning_projection_surfaces_company_orientation_vocabulary() {
        let payload = serde_json::json!({
            "ticker": "AAPL",
            "company_topics": [
                {
                    "ticker": "AAPL",
                    "topic_label": "Component Procurement",
                    "period": "CY2026Q1",
                    "document_type": "10-K",
                    "trace_status": "traceable"
                },
                {
                    "ticker": "AAPL",
                    "topic_label": "Services Growth"
                }
            ]
        });

        let delta =
            map_company_context(&payload, "AAPL", &context("ontology.company_context")).unwrap();
        // The vocabulary is promoted from ledger facts, so the advisory-only
        // record semantics must survive the promotion unchanged.
        assert!(
            delta
                .records
                .iter()
                .all(|record| record.directness == Directness::Unverified
                    && !record.strong_claim_allowed)
        );
        // Two committed reads of the same orientation must not double-report a
        // term: dedup happens on the (term, document_type, period) triple.
        let mut records = delta.records.clone();
        records.extend(delta.records);
        let vocabulary = company_orientation_vocabulary(&records);

        let state = answerable_fixture();
        let projection =
            derive_research_planning_projection(&state, &vocabulary).expect("valid projection");

        assert_eq!(projection.orientation_vocabulary.len(), 2);
        assert_eq!(
            projection.orientation_vocabulary[0],
            OrientationTerm {
                term: "Component Procurement".into(),
                document_type: Some("10-K".into()),
                period: Some("2026년".into()),
            }
        );
        assert_eq!(
            projection.orientation_vocabulary[1],
            OrientationTerm {
                term: "Services Growth".into(),
                document_type: None,
                period: None,
            }
        );
    }

    #[test]
    fn orientation_vocabulary_is_capped_at_a_bounded_deterministic_front() {
        let topic_payload = |ticker: &str, offset: usize| {
            serde_json::json!({
                "ticker": ticker,
                "company_topics": (0..8)
                    .map(|index| {
                        serde_json::json!({
                            "ticker": ticker,
                            "topic_label": format!("Topic {:02}", offset + index),
                            "document_type": "10-K",
                            "period": "CY2025"
                        })
                    })
                    .collect::<Vec<_>>()
            })
        };
        let first = map_company_context(
            &topic_payload("AAPL", 0),
            "AAPL",
            &context("ontology.company_context"),
        )
        .unwrap();
        let second = map_company_context(
            &topic_payload("MSFT", 8),
            "MSFT",
            &context("ontology.company_context"),
        )
        .unwrap();
        assert_eq!(first.records.len() + second.records.len(), 16);

        let mut records = first.records;
        records.extend(second.records);
        let vocabulary = company_orientation_vocabulary(&records);
        let state = answerable_fixture();
        let projection = derive_research_planning_projection(&state, &vocabulary).unwrap();

        assert_eq!(
            projection.orientation_vocabulary.len(),
            MAX_ORIENTATION_VOCABULARY
        );
        let terms = projection
            .orientation_vocabulary
            .iter()
            .map(|term| term.term.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            terms,
            (0..MAX_ORIENTATION_VOCABULARY)
                .map(|index| format!("Topic {index:02}"))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn market_snapshot_is_sanitized_and_never_becomes_claim_evidence() {
        let payload = serde_json::json!({
            "format": "market-snapshot/v1",
            "ticker": "AAPL",
            "status": "available",
            "source": "fmp",
            "source_usage": "research_only",
            "fetched_at": "2026-08-10T10:00:00Z",
            "as_of": "2026-08-10T09:59:00Z",
            "currency": "USD",
            "metrics": {
                "last_price": 210.5,
                "trailing_pe": 31.2,
                "private_router_key": "must not cross"
            },
            "advisory_only": true,
            "private_error": "must not cross"
        });

        let delta = map_market_snapshot(&payload, "AAPL", &context("market.snapshot")).unwrap();

        assert_eq!(delta.provider_content["status"], "available");
        assert_eq!(delta.provider_content["metrics"]["last_price"], 210.5);
        assert!(delta.provider_content.get("private_error").is_none());
        assert!(
            delta.provider_content["metrics"]
                .get("private_router_key")
                .is_none()
        );
        assert_eq!(delta.provider_content["advisory_only"], true);
        assert_eq!(delta.records.len(), 1);
        assert_eq!(delta.records[0].directness, Directness::Unverified);
        assert_eq!(delta.records[0].grade, EvidenceGrade::Unverified);
        assert!(!delta.records[0].strong_claim_allowed);
        EvidenceLedger::from_records(delta.records).unwrap();
    }

    #[test]
    fn cross_ticker_or_untrusted_market_snapshot_is_withheld() {
        let payload = serde_json::json!({
            "format": "market-snapshot/v1",
            "ticker": "MSFT",
            "status": "available",
            "source": "fmp",
            "source_usage": "research_only",
            "fetched_at": "2026-08-10T10:00:00Z",
            "as_of": null,
            "currency": "USD",
            "metrics": {"last_price": 1.0},
            "advisory_only": true
        });

        let delta = map_market_snapshot(&payload, "AAPL", &context("market.snapshot")).unwrap();

        assert_eq!(delta.provider_content["status"], "unavailable");
        assert_eq!(delta.provider_content["metrics"], serde_json::json!({}));
        assert!(delta.records.is_empty());
    }

    #[test]
    fn trace_errors_are_control_results_not_evidence() {
        let payload = serde_json::json!({"error": "not_found"});
        let delta = map_trace(&payload, &context("ontology.trace")).unwrap();
        assert!(delta.records.is_empty());
    }

    #[test]
    fn chain_keeps_bounded_relationship_paths_after_trace_compaction() {
        let payload = serde_json::json!({
            "object": {"id":"claim:AVGO:ai-demand","ticker":"AVGO","text":"AI demand"},
            "evidence": {"quotes": [{"text":"AI demand supported networking revenue."}]},
            "chain": {
                "evidence_chain": [{"id":"quote:AVGO:ai-demand","text":"Demand increased."}],
                "semantic_neighbors": [{"id":"claim:AVGO:networking","label":"Networking revenue"}],
                "temporal_context": [{"period":"CY2026Q1","label":"Current filing"}],
                "edge_paths": [{"from_id":"claim:AVGO:ai-demand","to_id":"claim:AVGO:networking","relation":"supports"}]
            }
        });

        let delta = map_trace(&payload, &context("ontology.chain")).unwrap();
        assert!(delta.records.iter().any(|record| {
            record
                .facts
                .iter()
                .any(|fact| fact.predicate == "ontology_chain_context")
        }));
        let context_record = delta
            .records
            .iter()
            .find(|record| {
                record
                    .facts
                    .iter()
                    .any(|fact| fact.predicate == "ontology_chain_context")
            })
            .unwrap();
        assert!(
            context_record
                .source_object_ids
                .contains(&"claim:AVGO:networking".into())
        );
        assert!(
            !context_record.facts[0]
                .value
                .to_string()
                .contains("CY2026Q1")
        );
        assert!(!context_record.strong_claim_allowed);
    }

    #[test]
    fn malformed_result_arrays_fail_closed() {
        let payload = serde_json::json!({"results": {"not": "an array"}});
        assert!(matches!(
            map_targeted_query(&payload, &context("ontology.query")),
            Err(AdapterError::InvalidSupplementalPayload("results"))
        ));
    }

    #[test]
    fn targeted_input_error_is_not_normalized_as_empty_evidence() {
        let delta = map_targeted_query(
            &serde_json::json!({
                "error": {"code": "input_invalid"}
            }),
            &context("ontology.query"),
        )
        .unwrap();

        assert_eq!(delta.records.len(), 0);
        assert_eq!(delta.status.kind, SupplementalReadKind::InputRejected);
        assert_eq!(
            delta.status.warning_codes,
            vec!["supplemental_input_rejected".to_owned()]
        );
    }

    #[test]
    fn targeted_empty_and_truncated_results_have_distinct_status() {
        let empty = map_targeted_query(
            &serde_json::json!({"results": []}),
            &context("ontology.query"),
        )
        .unwrap();
        assert_eq!(empty.status.kind, SupplementalReadKind::Empty);
        assert_eq!(empty.status.result_count, 0);
        assert!(!empty.status.has_more);

        let truncated = map_targeted_query(
            &serde_json::json!({
                "results": [{
                    "id": "claim:AAPL:1",
                    "ticker": "AAPL",
                    "text": "Revenue grew.",
                    "evidence": {"quotes": []}
                }],
                "pagination": {
                    "has_more": true,
                    "next_offset": 20
                }
            }),
            &context("ontology.query"),
        )
        .unwrap();
        assert_eq!(truncated.status.kind, SupplementalReadKind::Retrieved);
        assert_eq!(truncated.status.result_count, 1);
        assert!(truncated.status.has_more);
        assert_eq!(truncated.status.next_offset, Some(20));
    }

    #[test]
    fn compact_targeted_result_preserves_truncation_as_status_only() {
        let delta = map_targeted_query(
            &serde_json::json!({
                "results": [{
                    "id": "claim:AAPL:1",
                    "ticker": "AAPL",
                    "text": "Revenue grew.",
                    "evidence_summary": {
                        "claim_count": 4,
                        "quote_count": 4,
                        "span_count": 0,
                        "related_object_count": 0,
                        "truncated": true
                    },
                    "evidence": {"quotes": [{"id": "quote:1", "text": "Revenue grew."}]}
                }]
            }),
            &context("ontology.query"),
        )
        .unwrap();

        assert_eq!(delta.records.len(), 1);
        assert_eq!(
            delta.status.warning_codes,
            vec!["compact_evidence_truncated".to_owned()]
        );
    }

    #[test]
    fn unknown_trace_application_error_is_not_normalized_as_not_found() {
        let delta = map_trace(
            &serde_json::json!({
                "error": {"code": "upstream_index_unavailable"}
            }),
            &context("ontology.trace"),
        )
        .unwrap();
        assert_eq!(delta.records.len(), 0);
        assert_eq!(delta.status.kind, SupplementalReadKind::ApplicationError);
        assert_eq!(
            delta.status.warning_codes,
            vec!["supplemental_unclassified_error".to_owned()]
        );
    }

    #[test]
    fn empty_quote_objects_do_not_upgrade_directness() {
        let payload = serde_json::json!({
            "results": [{
                "id": "claim:AAPL:1",
                "ticker": "AAPL",
                "text": "Related context",
                "evidence": {
                    "quotes": [{"text": "   "}],
                    "spans": [],
                    "metric_lineage": null
                }
            }]
        });
        let delta = map_targeted_query(&payload, &context("ontology.query")).unwrap();
        assert_eq!(delta.records[0].directness, Directness::Related);
        assert!(!delta.records[0].strong_claim_allowed);
    }

    #[test]
    fn trace_identity_binds_the_evidence_payload_not_only_the_object() {
        let first = serde_json::json!({
            "object": {"id": "claim:AAPL:1", "ticker": "AAPL", "text": "Claim"},
            "evidence": {"quotes": [{"text": "First filing quote"}]}
        });
        let second = serde_json::json!({
            "object": {"id": "claim:AAPL:1", "ticker": "AAPL", "text": "Claim"},
            "evidence": {"quotes": [{"text": "Different filing quote"}]}
        });
        let first_record = map_trace(&first, &context("ontology.trace"))
            .unwrap()
            .records
            .remove(0);
        let second_record = map_trace(&second, &context("ontology.trace"))
            .unwrap()
            .records
            .remove(0);
        assert_ne!(first_record.content_hash, second_record.content_hash);
        assert_ne!(first_record.evidence_id, second_record.evidence_id);
    }

    #[test]
    fn global_flag_cannot_upgrade_an_optional_or_inconsistent_clause() {
        let mut state = parse_research_state(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/vertical-slice/v1/mcp/research-state-answerable.json"
        )))
        .expect("ResearchState fixture");
        state.clause_coverage[0].required = false;
        let delta = map_research_state(&state, &context("ontology.query_context")).unwrap();
        assert_eq!(delta.answerability, Answerability::QualifiedOnly);
        assert!(
            delta
                .records
                .iter()
                .all(|record| !record.strong_claim_allowed)
        );
    }

    #[test]
    fn metric_series_sidecar_is_not_ingested_as_evidence() {
        // Chart-ready series leave through the private presentation channel
        // (MCP `_meta`), never through the model-visible ResearchState. The
        // mapped delta therefore contains only primary filing evidence.
        let state = answerable_fixture();

        let delta = map_research_state(&state, &context("ontology.query_context")).unwrap();
        assert!(
            delta
                .records
                .iter()
                .all(|record| !record.evidence_id.starts_with("metric-series:"))
        );
        assert_eq!(delta.records.len(), state.evidence_units.len());
        EvidenceLedger::from_records(delta.records).unwrap();
    }

    #[test]
    fn research_state_rejects_a_smuggled_metric_series_pack() {
        // deny_unknown_fields makes the cutover fail closed: a stale server
        // cannot push chart data back into the model path under the old key.
        let state = answerable_fixture();
        let mut payload = serde_json::to_value(&state).unwrap();
        payload["metric_series_pack"] = serde_json::json!({
            "mode": "chart_series_sidecar",
            "series": []
        });
        let error = serde_json::from_value::<ResearchStateV2>(payload)
            .expect_err("metric_series_pack is not part of ResearchStateV2");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn primary_metric_evidence_keeps_fiscal_period_and_ytd_basis_separate_from_document_label() {
        let mut state = answerable_fixture();
        let unit = &mut state.evidence_units[0];
        unit.period = Some("CY2025".into());
        unit.document_type = Some("10-Q".into());
        unit.metric = Some("operating_cash_flow".into());
        unit.unit = Some("USD".into());
        unit.currency = Some("USD".into());
        unit.metric_scope = Some("company_total".into());
        unit.dimensions
            .insert("scope".into(), "company_total".into());
        unit.metric_points = vec![MetricPoint {
            period: "FY2025".into(),
            value: Some(serde_json::json!(53_887_000_000_u64)),
            formatted_value: Some("$53.887B".into()),
            object_id: Some("metric:VG:operating-cash-flow:FY2025".into()),
            period_type: Some("year_to_date".into()),
            start_date: Some("2024-09-29".into()),
            end_date: Some("2025-03-29".into()),
            conflict_value_count: 0,
        }];
        let evidence_id = unit.evidence_id.clone();

        let delta = map_research_state(&state, &context("ontology.query_context")).unwrap();
        let record = delta
            .records
            .iter()
            .find(|record| record.evidence_id == evidence_id)
            .expect("primary metric record");
        assert_eq!(record.period.as_deref(), Some("FY2025"));
        assert_eq!(record.citation.period.as_deref(), Some("2025년"));
        assert_eq!(record.as_of.as_deref(), Some("2025-03-29"));
        assert!(record.facts.iter().any(|fact| {
            fact.predicate == "operating_cash_flow"
                && fact.period.as_deref() == Some("FY2025")
                && fact.value == serde_json::json!(53_887_000_000_u64)
        }));
        let context = record
            .facts
            .iter()
            .find(|fact| fact.predicate == "metric_context")
            .expect("metric observation basis");
        assert_eq!(context.value["source_document_period"], "2025년");
        assert_eq!(
            context.value["observations"][0]["period_type"],
            "year_to_date"
        );
        assert_eq!(context.value["observations"][0]["start_date"], "2024-09-29");
        assert_eq!(context.value["observations"][0]["end_date"], "2025-03-29");
        EvidenceLedger::from_records(delta.records).unwrap();
    }

    #[test]
    fn calendar_routing_bucket_uses_observed_date_without_guessing_fiscal_quarter() {
        assert_eq!(
            presentation_observation_period(
                Some("CY2026Q1"),
                Some("quarterly"),
                Some("2026-03-28"),
            )
            .as_deref(),
            Some("2026-03-28 종료 분기")
        );
        assert_eq!(
            presentation_document_period(Some("CY2026Q1"), Some("10-Q")).as_deref(),
            Some("2026년")
        );
        assert_eq!(
            presentation_source_label(Some("AAPL CY2026Q1 10-Q"), Some("CY2026Q1"), Some("10-Q"),)
                .as_deref(),
            Some("AAPL 2026년 10-Q")
        );
        assert_eq!(
            presentation_observation_period(Some("FY2026Q2"), Some("quarterly"), None).as_deref(),
            Some("FY2026Q2")
        );
    }

    #[test]
    fn research_state_maps_primary_evidence_without_a_sidecar_pack() {
        let state = answerable_fixture();

        let delta = map_research_state(&state, &context("ontology.query_context")).unwrap();
        assert_eq!(delta.records.len(), state.evidence_units.len());
    }
}
