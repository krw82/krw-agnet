//! Typed mapping from canonical `ResearchState v2` into generic evidence records.

use std::collections::{BTreeMap, BTreeSet};

use krw_agent_evidence::{
    Answerability, Calculation, Directness, EvidenceGrade, EvidenceRecord, EvidenceScope,
    EvidenceSource, NormalizedFact, PublicCitation,
};
use krw_agent_planning::{DirectnessRequirement, EvidenceGoal, EvidenceGoalGraph, GoalStatus, PPM};
use krw_agent_protocol::ContentHash;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use zeroize::Zeroizing;

const MAX_SUPPLEMENTAL_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
const MAX_SUPPLEMENTAL_RECORDS: usize = 256;
const MAX_SUPPLEMENTAL_FACTS: usize = 16;
const MAX_RESEARCH_FACTS_PER_RECORD: usize = 128;
const MAX_RESEARCH_CALCULATIONS: usize = 64;
const MAX_NORMALIZED_RECORD_BYTES: usize = 256 * 1024;
const MAX_PLANNING_GAPS: usize = 256;

#[derive(Debug, Clone, PartialEq, Deserialize)]
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
    pub metric_series_pack: Option<Value>,
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Continuation {
    pub has_more: bool,
    pub omitted_evidence_count: u32,
    pub reason: Option<String>,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClausePlanningBinding {
    pub clause_id: String,
    pub retrieval_query: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchPlanningProjection {
    pub graph: EvidenceGoalGraph,
    pub clauses: Vec<ClausePlanningBinding>,
    pub missing_parts: Vec<MissingPart>,
    pub recommended_actions: Vec<RecommendedAction>,
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

pub fn derive_research_planning_projection(
    state: &ResearchStateV2,
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
    })
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
            evidence_ids: coverage.evidence_ids.clone(),
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
            calculation_ids: coverage.calculation_ids.clone(),
        });
    }
    EvidenceGoalGraph::new(goals).map_err(AdapterError::Planning)
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
        let period = clean_optional(unit.period.as_deref(), 128);
        let normalized_unit = clean_optional(unit.unit.as_deref(), 64);
        let subject = entity.clone().unwrap_or_else(|| "company".into());
        let predicate = safe_single_line(
            unit.metric.as_deref().unwrap_or("filing_evidence"),
            128,
            "filing_evidence",
        );
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
                    period: clean_optional(Some(point.period.as_str()), 128),
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
            as_of: None,
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
                period,
            },
            facts,
            supports: unit.supports_clause_ids.clone(),
            refutes: Vec::new(),
            qualifies: Vec::new(),
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
        return Ok(empty_supplemental_delta());
    }
    let Some(results) = payload.get("results") else {
        return Ok(empty_supplemental_delta());
    };
    let results = results
        .as_array()
        .ok_or(AdapterError::InvalidSupplementalPayload("results"))?;
    if results.len() > MAX_SUPPLEMENTAL_RECORDS {
        return Err(AdapterError::SupplementalItemLimit);
    }
    let records = results
        .iter()
        .map(|item| supplemental_record(item, item.get("evidence"), context))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SupplementalEvidenceDelta {
        records,
        calculations: Vec::new(),
    })
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
        return Ok(empty_supplemental_delta());
    }
    let object = payload
        .get("object")
        .ok_or(AdapterError::InvalidSupplementalPayload("object"))?;
    let record = supplemental_record(object, payload.get("evidence"), context)?;
    Ok(SupplementalEvidenceDelta {
        records: vec![record],
        calculations: Vec::new(),
    })
}

fn empty_supplemental_delta() -> SupplementalEvidenceDelta {
    SupplementalEvidenceDelta {
        records: Vec::new(),
        calculations: Vec::new(),
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
    let period = bounded_optional(object_map.get("period"), 128);
    let document_type = bounded_optional(object_map.get("document_type"), 128);
    let subject = entity.clone().unwrap_or_else(|| "company".into());
    let mut facts = Vec::with_capacity(MAX_SUPPLEMENTAL_FACTS);
    if let Some(value) = first_display_value(object_map) {
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
            period,
        },
        facts,
        supports: Vec::new(),
        refutes: Vec::new(),
        qualifies: Vec::new(),
    };
    ensure_record_bound(&record)?;
    Ok(record)
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
        let projection = derive_research_planning_projection(&state).unwrap();
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
            derive_research_planning_projection(&state),
            Err(AdapterError::InvalidPlanningProjection(
                "planning gap limit"
            ))
        ));
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
    fn trace_errors_are_control_results_not_evidence() {
        let payload = serde_json::json!({"error": "not_found"});
        let delta = map_trace(&payload, &context("ontology.trace")).unwrap();
        assert!(delta.records.is_empty());
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
}
