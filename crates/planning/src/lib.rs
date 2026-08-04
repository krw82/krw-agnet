//! Deterministic evidence-goal DAG and conservative best-first action policy.
//!
//! The model may propose research actions, but it does not assign their
//! production utility. Trusted adapters derive the bounded features below from
//! canonical `ResearchState`, the evidence ledger, and deployment telemetry.
//! All arithmetic is integer/fixed-point so replay and differential tests are
//! bit-for-bit deterministic.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use krw_agent_protocol::ContentHash;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const PPM: u32 = 1_000_000;
const MAX_GOALS: usize = 128;
const MAX_GOAL_LINKS: usize = 32;
const MAX_ACTIONS: usize = 64;
const MAX_ACTION_GOALS: usize = 32;
const MAX_CONFLICT_KEYS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Unresolved,
    Partial,
    Satisfied,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectnessRequirement {
    Related,
    MetricLineage,
    Direct,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionBenefit {
    ServerRecommended,
    RaisesDirectnessCeiling,
    FillsCalculationCoverage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionEffect {
    ReadOnly,
    Mutating,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionConcurrency {
    Serial,
    ParallelSafe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthIsolation {
    Shared,
    Isolated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceGoal {
    pub goal_id: String,
    pub required: bool,
    /// Relative importance; the graph normalizes the sum during utility math.
    pub weight: u32,
    pub dependencies: Vec<String>,
    pub directness: DirectnessRequirement,
    pub calculation_required: bool,
    pub status: GoalStatus,
    /// 0..=1,000,000. Satisfied goals must be exactly one million.
    pub coverage_ppm: u32,
    pub evidence_ids: Vec<String>,
    pub calculation_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoalProgressUpdate {
    pub goal_id: String,
    pub expected_status: GoalStatus,
    pub status: GoalStatus,
    pub coverage_ppm: u32,
    pub evidence_ids: Vec<String>,
    pub calculation_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoalDelta {
    pub expected_version: u64,
    pub additions: Vec<EvidenceGoal>,
    pub progress: Vec<GoalProgressUpdate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceGoalGraph {
    version: u64,
    goals: BTreeMap<String, EvidenceGoal>,
    original_order: Vec<String>,
}

impl EvidenceGoalGraph {
    pub fn new(goals: Vec<EvidenceGoal>) -> Result<Self, PlanningError> {
        let mut graph = Self {
            version: 1,
            goals: BTreeMap::new(),
            original_order: Vec::with_capacity(goals.len()),
        };
        graph.insert_additions(goals)?;
        graph.validate_graph()?;
        Ok(graph)
    }

    pub const fn version(&self) -> u64 {
        self.version
    }

    pub fn goal(&self, goal_id: &str) -> Option<&EvidenceGoal> {
        self.goals.get(goal_id)
    }

    pub fn goals(&self) -> impl Iterator<Item = &EvidenceGoal> {
        self.original_order
            .iter()
            .filter_map(|goal_id| self.goals.get(goal_id))
    }

    /// Revalidates a deserialized durable graph before it is admitted into a
    /// recovered run. Serde cannot enforce constructor invariants, so recovery
    /// must check the version, deterministic order index, every goal, and the
    /// complete DAG again.
    pub fn validate_recovered(&self) -> Result<(), PlanningError> {
        if self.version == 0
            || self.original_order.len() != self.goals.len()
            || self.goals.len() > MAX_GOALS
        {
            return Err(PlanningError::InvalidRecoveredGraph);
        }
        let mut ordered = BTreeSet::new();
        for goal_id in &self.original_order {
            if !ordered.insert(goal_id.as_str()) || !self.goals.contains_key(goal_id) {
                return Err(PlanningError::InvalidRecoveredGraph);
            }
        }
        for goal in self.goals.values() {
            validate_goal(goal)?;
        }
        self.validate_graph()
    }

    /// Apply a versioned delta transactionally. Existing goal definitions and
    /// dependency edges are immutable; replanning appends goals and monotonic
    /// progress instead of silently rewriting prior intent.
    pub fn apply(&mut self, delta: GoalDelta) -> Result<(), PlanningError> {
        if delta.expected_version != self.version {
            return Err(PlanningError::VersionMismatch {
                expected: self.version,
                observed: delta.expected_version,
            });
        }
        if delta.additions.is_empty() && delta.progress.is_empty() {
            return Err(PlanningError::EmptyDelta);
        }
        let mut candidate = self.clone();
        candidate.insert_additions(delta.additions)?;
        let mut updated = BTreeSet::new();
        for update in delta.progress {
            validate_identifier(&update.goal_id)?;
            if !updated.insert(update.goal_id.clone()) {
                return Err(PlanningError::DuplicateProgress(update.goal_id));
            }
            let goal = candidate
                .goals
                .get_mut(&update.goal_id)
                .ok_or_else(|| PlanningError::UnknownGoal(update.goal_id.clone()))?;
            if goal.status != update.expected_status {
                return Err(PlanningError::ProgressConflict(update.goal_id));
            }
            validate_progress_transition(goal, &update)?;
            goal.status = update.status;
            goal.coverage_ppm = update.coverage_ppm;
            merge_identifiers(&mut goal.evidence_ids, update.evidence_ids)?;
            merge_identifiers(&mut goal.calculation_ids, update.calculation_ids)?;
        }
        candidate.validate_graph()?;
        candidate.version = candidate
            .version
            .checked_add(1)
            .ok_or(PlanningError::VersionOverflow)?;
        *self = candidate;
        Ok(())
    }

    /// Ready unresolved goals in deterministic best-first order. A dependency
    /// must be satisfied, not merely partial.
    pub fn frontier(&self) -> Vec<&EvidenceGoal> {
        let order = self
            .original_order
            .iter()
            .enumerate()
            .map(|(index, id)| (id.as_str(), index))
            .collect::<BTreeMap<_, _>>();
        let mut goals = self
            .goals()
            .filter(|goal| matches!(goal.status, GoalStatus::Unresolved | GoalStatus::Partial))
            .filter(|goal| {
                goal.dependencies.iter().all(|dependency| {
                    self.goals
                        .get(dependency)
                        .is_some_and(|goal| goal.status == GoalStatus::Satisfied)
                })
            })
            .collect::<Vec<_>>();
        goals.sort_by_key(|goal| {
            (
                !goal.required,
                std::cmp::Reverse(goal.directness),
                !goal.calculation_required,
                std::cmp::Reverse(goal.weight),
                order
                    .get(goal.goal_id.as_str())
                    .copied()
                    .unwrap_or(usize::MAX),
            )
        });
        goals
    }

    /// Weighted coverage less explicit contradiction/scope penalties.
    pub fn utility_ppm(
        &self,
        contradiction_penalty_ppm: u32,
        unresolved_scope_penalty_ppm: u32,
    ) -> Result<i64, PlanningError> {
        if contradiction_penalty_ppm > PPM || unresolved_scope_penalty_ppm > PPM {
            return Err(PlanningError::InvalidPenalty);
        }
        let total_weight = self
            .goals
            .values()
            .try_fold(0_u128, |sum, goal| sum.checked_add(u128::from(goal.weight)))
            .ok_or(PlanningError::ArithmeticOverflow)?;
        if total_weight == 0 {
            return Err(PlanningError::ZeroTotalWeight);
        }
        let covered = self.goals.values().try_fold(0_u128, |sum, goal| {
            sum.checked_add(u128::from(goal.weight) * u128::from(goal.coverage_ppm))
        });
        let covered = covered.ok_or(PlanningError::ArithmeticOverflow)? / total_weight;
        let covered = i64::try_from(covered).map_err(|_| PlanningError::ArithmeticOverflow)?;
        Ok(
            covered
                - i64::from(contradiction_penalty_ppm)
                - i64::from(unresolved_scope_penalty_ppm),
        )
    }

    fn insert_additions(&mut self, additions: Vec<EvidenceGoal>) -> Result<(), PlanningError> {
        if self.goals.len().saturating_add(additions.len()) > MAX_GOALS {
            return Err(PlanningError::GoalLimit);
        }
        for goal in additions {
            validate_goal(&goal)?;
            if self.goals.contains_key(&goal.goal_id) {
                return Err(PlanningError::DuplicateGoal(goal.goal_id));
            }
            self.original_order.push(goal.goal_id.clone());
            self.goals.insert(goal.goal_id.clone(), goal);
        }
        Ok(())
    }

    fn validate_graph(&self) -> Result<(), PlanningError> {
        if self.goals.is_empty() {
            return Err(PlanningError::EmptyGraph);
        }
        for goal in self.goals.values() {
            if goal
                .dependencies
                .iter()
                .any(|dependency| !self.goals.contains_key(dependency))
            {
                return Err(PlanningError::UnknownDependency(goal.goal_id.clone()));
            }
        }
        let mut indegree = self
            .goals
            .iter()
            .map(|(id, goal)| (id.clone(), goal.dependencies.len()))
            .collect::<BTreeMap<_, _>>();
        let mut dependents = BTreeMap::<&str, Vec<&str>>::new();
        for goal in self.goals.values() {
            for dependency in &goal.dependencies {
                dependents
                    .entry(dependency)
                    .or_default()
                    .push(&goal.goal_id);
            }
        }
        let mut queue = indegree
            .iter()
            .filter_map(|(id, degree)| (*degree == 0).then_some(id.clone()))
            .collect::<VecDeque<_>>();
        let mut visited = 0_usize;
        while let Some(id) = queue.pop_front() {
            visited += 1;
            for dependent in dependents.get(id.as_str()).into_iter().flatten() {
                let degree = indegree
                    .get_mut(*dependent)
                    .ok_or_else(|| PlanningError::UnknownDependency((*dependent).into()))?;
                *degree = degree.saturating_sub(1);
                if *degree == 0 {
                    queue.push_back((*dependent).into());
                }
            }
        }
        if visited != self.goals.len() {
            return Err(PlanningError::DependencyCycle);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionCandidate {
    pub action_id: String,
    pub capability_id: String,
    pub action_fingerprint: ContentHash,
    pub goal_ids: Vec<String>,
    /// Derived from canonical `recommended_actions`, never model confidence.
    pub benefits: BTreeSet<ActionBenefit>,
    pub historical_success_lower_ppm: u32,
    pub expected_duplicate_ppm: u32,
    pub failure_risk_upper_ppm: u32,
    pub expected_latency_ms: u32,
    pub expected_tokens: u32,
    pub expected_tool_cost_micros: u32,
    pub expected_result_bytes: u32,
    pub effect: ActionEffect,
    pub concurrency: ActionConcurrency,
    pub auth_isolation: AuthIsolation,
    pub conflict_keys: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScoringWeights {
    pub required_goal_bonus_ppm: u32,
    pub server_recommended_bonus_ppm: u32,
    pub directness_bonus_ppm: u32,
    pub calculation_bonus_ppm: u32,
    pub latency_penalty_per_ms: u32,
    pub token_penalty: u32,
    pub tool_cost_penalty: u32,
    pub result_kib_penalty: u32,
    pub duplicate_penalty_ppm: u32,
    pub risk_penalty_ppm: u32,
}

impl Default for ScoringWeights {
    fn default() -> Self {
        Self {
            required_goal_bonus_ppm: 220_000,
            server_recommended_bonus_ppm: 120_000,
            directness_bonus_ppm: 160_000,
            calculation_bonus_ppm: 160_000,
            latency_penalty_per_ms: 4,
            token_penalty: 2,
            tool_cost_penalty: 1,
            result_kib_penalty: 20,
            duplicate_penalty_ppm: 500_000,
            risk_penalty_ppm: 500_000,
        }
    }
}

impl ScoringWeights {
    /// Rejects release policy values that would disable every cost signal or
    /// escape the fixed-point probability domain. This is run once while an
    /// immutable image is resolved, never in the hot scoring loop.
    pub fn validate(self) -> Result<Self, PlanningError> {
        let bounded = [
            self.required_goal_bonus_ppm,
            self.server_recommended_bonus_ppm,
            self.directness_bonus_ppm,
            self.calculation_bonus_ppm,
            self.duplicate_penalty_ppm,
            self.risk_penalty_ppm,
        ]
        .into_iter()
        .all(|value| value <= PPM);
        let has_cost_signal = self.latency_penalty_per_ms > 0
            || self.token_penalty > 0
            || self.tool_cost_penalty > 0
            || self.result_kib_penalty > 0;
        if !bounded
            || !has_cost_signal
            || self.duplicate_penalty_ppm == 0
            || self.risk_penalty_ppm == 0
        {
            return Err(PlanningError::InvalidScoringWeights);
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoredAction<'a> {
    pub candidate: &'a ActionCandidate,
    pub score: i64,
}

/// Score only features derived by trusted code. Unknown goals, duplicate
/// fingerprints, unsafe effects, and actions with no ready goal fail closed.
pub fn score_actions<'a>(
    graph: &EvidenceGoalGraph,
    candidates: &'a [ActionCandidate],
    completed_fingerprints: &BTreeSet<ContentHash>,
    weights: ScoringWeights,
) -> Result<Vec<ScoredAction<'a>>, PlanningError> {
    if candidates.is_empty() || candidates.len() > MAX_ACTIONS {
        return Err(PlanningError::ActionLimit);
    }
    let frontier = graph
        .frontier()
        .into_iter()
        .map(|goal| (goal.goal_id.as_str(), goal))
        .collect::<BTreeMap<_, _>>();
    let total_weight = graph
        .goals
        .values()
        .try_fold(0_u128, |sum, goal| sum.checked_add(u128::from(goal.weight)))
        .ok_or(PlanningError::ArithmeticOverflow)?;
    if total_weight == 0 {
        return Err(PlanningError::ZeroTotalWeight);
    }
    let mut action_ids = BTreeSet::new();
    let mut fingerprints = BTreeSet::new();
    let mut scored = Vec::new();
    for candidate in candidates {
        validate_action(candidate)?;
        if !action_ids.insert(candidate.action_id.as_str()) {
            return Err(PlanningError::DuplicateAction(candidate.action_id.clone()));
        }
        if !fingerprints.insert(&candidate.action_fingerprint) {
            return Err(PlanningError::DuplicateFingerprint);
        }
        if completed_fingerprints.contains(&candidate.action_fingerprint)
            || candidate.effect != ActionEffect::ReadOnly
        {
            continue;
        }
        let goals = candidate
            .goal_ids
            .iter()
            .filter_map(|id| frontier.get(id.as_str()).copied())
            .collect::<Vec<_>>();
        if goals.len() != candidate.goal_ids.len() || goals.is_empty() {
            continue;
        }
        let remaining_weighted = goals.iter().try_fold(0_u128, |sum, goal| {
            let remaining = PPM.saturating_sub(goal.coverage_ppm);
            sum.checked_add(u128::from(goal.weight) * u128::from(remaining))
        });
        let remaining_weighted =
            remaining_weighted.ok_or(PlanningError::ArithmeticOverflow)? / total_weight;
        let success_adjusted = remaining_weighted
            .checked_mul(u128::from(candidate.historical_success_lower_ppm))
            .ok_or(PlanningError::ArithmeticOverflow)?
            / u128::from(PPM);
        let mut value =
            i128::try_from(success_adjusted).map_err(|_| PlanningError::ArithmeticOverflow)?;
        if goals.iter().any(|goal| goal.required) {
            value += i128::from(weights.required_goal_bonus_ppm);
        }
        if candidate
            .benefits
            .contains(&ActionBenefit::ServerRecommended)
        {
            value += i128::from(weights.server_recommended_bonus_ppm);
        }
        if candidate
            .benefits
            .contains(&ActionBenefit::RaisesDirectnessCeiling)
            && goals
                .iter()
                .any(|goal| goal.directness != DirectnessRequirement::Related)
        {
            value += i128::from(weights.directness_bonus_ppm);
        }
        if candidate
            .benefits
            .contains(&ActionBenefit::FillsCalculationCoverage)
            && goals.iter().any(|goal| goal.calculation_required)
        {
            value += i128::from(weights.calculation_bonus_ppm);
        }
        value -=
            i128::from(candidate.expected_latency_ms) * i128::from(weights.latency_penalty_per_ms);
        value -= i128::from(candidate.expected_tokens) * i128::from(weights.token_penalty);
        value -=
            i128::from(candidate.expected_tool_cost_micros) * i128::from(weights.tool_cost_penalty);
        let result_kib = candidate.expected_result_bytes.saturating_add(1_023) / 1_024;
        value -= i128::from(result_kib) * i128::from(weights.result_kib_penalty);
        value -= scaled_penalty(
            candidate.expected_duplicate_ppm,
            weights.duplicate_penalty_ppm,
        );
        value -= scaled_penalty(candidate.failure_risk_upper_ppm, weights.risk_penalty_ppm);
        let score = i64::try_from(value).map_err(|_| PlanningError::ArithmeticOverflow)?;
        scored.push(ScoredAction { candidate, score });
    }
    scored.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| {
                left.candidate
                    .capability_id
                    .cmp(&right.candidate.capability_id)
            })
            .then_with(|| left.candidate.action_id.cmp(&right.candidate.action_id))
    });
    Ok(scored)
}

pub fn select_best<'a>(scored: &'a [ScoredAction<'a>]) -> Option<&'a ActionCandidate> {
    scored
        .first()
        .filter(|action| action.score > 0)
        .map(|action| action.candidate)
}

/// Greedy deterministic safe batch. It is intentionally bounded and selects
/// only positive-score, read-only, explicitly parallel-safe actions whose
/// conflict keys do not overlap.
pub fn select_parallel_batch<'a>(
    scored: &'a [ScoredAction<'a>],
    max_width: usize,
    max_total_result_bytes: u64,
) -> Result<Vec<&'a ActionCandidate>, PlanningError> {
    if !(1..=8).contains(&max_width) || max_total_result_bytes == 0 {
        return Err(PlanningError::InvalidParallelBudget);
    }
    let mut selected = Vec::with_capacity(max_width);
    let mut conflicts = BTreeSet::new();
    let mut bytes = 0_u64;
    for action in scored.iter().filter(|action| action.score > 0) {
        let candidate = action.candidate;
        if candidate.effect != ActionEffect::ReadOnly
            || candidate.concurrency != ActionConcurrency::ParallelSafe
            || candidate.auth_isolation != AuthIsolation::Isolated
        {
            continue;
        }
        if candidate
            .conflict_keys
            .iter()
            .any(|key| conflicts.contains(key))
        {
            continue;
        }
        let next_bytes = bytes.saturating_add(u64::from(candidate.expected_result_bytes));
        if next_bytes > max_total_result_bytes {
            continue;
        }
        conflicts.extend(candidate.conflict_keys.iter().cloned());
        bytes = next_bytes;
        selected.push(candidate);
        if selected.len() == max_width {
            break;
        }
    }
    Ok(selected)
}

fn scaled_penalty(observed_ppm: u32, weight_ppm: u32) -> i128 {
    i128::from(observed_ppm) * i128::from(weight_ppm) / i128::from(PPM)
}

fn validate_goal(goal: &EvidenceGoal) -> Result<(), PlanningError> {
    validate_identifier(&goal.goal_id)?;
    if goal.weight == 0
        || goal.dependencies.len() > MAX_GOAL_LINKS
        || goal.evidence_ids.len() > MAX_GOAL_LINKS
        || goal.calculation_ids.len() > MAX_GOAL_LINKS
        || goal.coverage_ppm > PPM
        || !status_matches_coverage(goal.status, goal.coverage_ppm)
    {
        return Err(PlanningError::InvalidGoal(goal.goal_id.clone()));
    }
    validate_unique_identifiers(&goal.dependencies)?;
    validate_unique_identifiers(&goal.evidence_ids)?;
    validate_unique_identifiers(&goal.calculation_ids)?;
    if goal.dependencies.iter().any(|id| id == &goal.goal_id) {
        return Err(PlanningError::DependencyCycle);
    }
    Ok(())
}

fn validate_progress_transition(
    current: &EvidenceGoal,
    update: &GoalProgressUpdate,
) -> Result<(), PlanningError> {
    if update.coverage_ppm > PPM
        || update.coverage_ppm < current.coverage_ppm
        || !status_matches_coverage(update.status, update.coverage_ppm)
        || !matches!(
            (current.status, update.status),
            (
                GoalStatus::Unresolved | GoalStatus::Partial,
                GoalStatus::Partial | GoalStatus::Satisfied | GoalStatus::Blocked
            ) | (GoalStatus::Satisfied, GoalStatus::Satisfied)
        )
    {
        return Err(PlanningError::InvalidProgress(update.goal_id.clone()));
    }
    validate_unique_identifiers(&update.evidence_ids)?;
    validate_unique_identifiers(&update.calculation_ids)
}

const fn status_matches_coverage(status: GoalStatus, coverage_ppm: u32) -> bool {
    match status {
        GoalStatus::Unresolved | GoalStatus::Blocked => coverage_ppm == 0,
        GoalStatus::Partial => coverage_ppm > 0 && coverage_ppm < PPM,
        GoalStatus::Satisfied => coverage_ppm == PPM,
    }
}

fn merge_identifiers(
    destination: &mut Vec<String>,
    additions: Vec<String>,
) -> Result<(), PlanningError> {
    if destination.len().saturating_add(additions.len()) > MAX_GOAL_LINKS {
        return Err(PlanningError::IdentifierLimit);
    }
    for addition in additions {
        validate_identifier(&addition)?;
        if !destination.contains(&addition) {
            destination.push(addition);
        }
    }
    Ok(())
}

fn validate_action(candidate: &ActionCandidate) -> Result<(), PlanningError> {
    validate_identifier(&candidate.action_id)?;
    validate_identifier(&candidate.capability_id)?;
    if candidate.goal_ids.is_empty()
        || candidate.goal_ids.len() > MAX_ACTION_GOALS
        || candidate.conflict_keys.len() > MAX_CONFLICT_KEYS
        || candidate.historical_success_lower_ppm > PPM
        || candidate.expected_duplicate_ppm > PPM
        || candidate.failure_risk_upper_ppm > PPM
    {
        return Err(PlanningError::InvalidAction(candidate.action_id.clone()));
    }
    validate_unique_identifiers(&candidate.goal_ids)?;
    validate_unique_identifiers(&candidate.conflict_keys)
}

fn validate_unique_identifiers(values: &[String]) -> Result<(), PlanningError> {
    let mut unique = BTreeSet::new();
    for value in values {
        validate_identifier(value)?;
        if !unique.insert(value) {
            return Err(PlanningError::DuplicateIdentifier(value.clone()));
        }
    }
    Ok(())
}

fn validate_identifier(value: &str) -> Result<(), PlanningError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err(PlanningError::InvalidIdentifier);
    }
    Ok(())
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PlanningError {
    #[error("evidence goal graph cannot be empty")]
    EmptyGraph,
    #[error("evidence goal graph exceeded its goal limit")]
    GoalLimit,
    #[error("duplicate evidence goal: {0}")]
    DuplicateGoal(String),
    #[error("invalid evidence goal: {0}")]
    InvalidGoal(String),
    #[error("unknown dependency for evidence goal: {0}")]
    UnknownDependency(String),
    #[error("evidence goal dependency graph contains a cycle")]
    DependencyCycle,
    #[error("deserialized evidence goal graph violates durable invariants")]
    InvalidRecoveredGraph,
    #[error("goal delta is empty")]
    EmptyDelta,
    #[error("goal graph version mismatch: expected {expected}, observed {observed}")]
    VersionMismatch { expected: u64, observed: u64 },
    #[error("goal graph version overflow")]
    VersionOverflow,
    #[error("duplicate progress update: {0}")]
    DuplicateProgress(String),
    #[error("unknown evidence goal: {0}")]
    UnknownGoal(String),
    #[error("evidence goal progress conflicted with current state: {0}")]
    ProgressConflict(String),
    #[error("invalid evidence goal progress: {0}")]
    InvalidProgress(String),
    #[error("invalid bounded identifier")]
    InvalidIdentifier,
    #[error("duplicate identifier: {0}")]
    DuplicateIdentifier(String),
    #[error("identifier collection exceeded its bound")]
    IdentifierLimit,
    #[error("utility penalty is outside fixed-point bounds")]
    InvalidPenalty,
    #[error("goal graph has zero total weight")]
    ZeroTotalWeight,
    #[error("action candidate count is outside bounds")]
    ActionLimit,
    #[error("invalid action candidate: {0}")]
    InvalidAction(String),
    #[error("action scoring weights violate fixed-point policy bounds")]
    InvalidScoringWeights,
    #[error("duplicate action candidate: {0}")]
    DuplicateAction(String),
    #[error("duplicate action fingerprint")]
    DuplicateFingerprint,
    #[error("invalid parallel action budget")]
    InvalidParallelBudget,
    #[error("fixed-point planning arithmetic overflow")]
    ArithmeticOverflow,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn goal(
        id: &str,
        required: bool,
        weight: u32,
        dependencies: &[&str],
        directness: DirectnessRequirement,
        calculation_required: bool,
    ) -> EvidenceGoal {
        EvidenceGoal {
            goal_id: id.into(),
            required,
            weight,
            dependencies: dependencies.iter().map(ToString::to_string).collect(),
            directness,
            calculation_required,
            status: GoalStatus::Unresolved,
            coverage_ppm: 0,
            evidence_ids: Vec::new(),
            calculation_ids: Vec::new(),
        }
    }

    fn candidate(id: &str, goal_ids: &[&str]) -> ActionCandidate {
        ActionCandidate {
            action_id: id.into(),
            capability_id: "ontology.query".into(),
            action_fingerprint: ContentHash::sha256(id),
            goal_ids: goal_ids.iter().map(ToString::to_string).collect(),
            benefits: BTreeSet::new(),
            historical_success_lower_ppm: 700_000,
            expected_duplicate_ppm: 0,
            failure_risk_upper_ppm: 10_000,
            expected_latency_ms: 20,
            expected_tokens: 20,
            expected_tool_cost_micros: 0,
            expected_result_bytes: 1_024,
            effect: ActionEffect::ReadOnly,
            concurrency: ActionConcurrency::ParallelSafe,
            auth_isolation: AuthIsolation::Isolated,
            conflict_keys: vec![id.into()],
        }
    }

    #[test]
    fn dag_rejects_cycles_and_only_exposes_ready_goals() {
        let cycle = vec![
            goal("a", true, 10, &["b"], DirectnessRequirement::Direct, false),
            goal("b", true, 10, &["a"], DirectnessRequirement::Direct, false),
        ];
        assert_eq!(
            EvidenceGoalGraph::new(cycle),
            Err(PlanningError::DependencyCycle)
        );

        let mut graph = EvidenceGoalGraph::new(vec![
            goal(
                "premise",
                true,
                20,
                &[],
                DirectnessRequirement::Direct,
                false,
            ),
            goal(
                "conclusion",
                true,
                10,
                &["premise"],
                DirectnessRequirement::Related,
                false,
            ),
        ])
        .unwrap();
        assert_eq!(graph.frontier()[0].goal_id, "premise");
        graph
            .apply(GoalDelta {
                expected_version: 1,
                additions: Vec::new(),
                progress: vec![GoalProgressUpdate {
                    goal_id: "premise".into(),
                    expected_status: GoalStatus::Unresolved,
                    status: GoalStatus::Satisfied,
                    coverage_ppm: PPM,
                    evidence_ids: vec!["e:1".into()],
                    calculation_ids: Vec::new(),
                }],
            })
            .unwrap();
        assert_eq!(graph.frontier()[0].goal_id, "conclusion");
    }

    #[test]
    fn deltas_are_transactional_versioned_and_monotonic() {
        let mut graph = EvidenceGoalGraph::new(vec![goal(
            "revenue",
            true,
            10,
            &[],
            DirectnessRequirement::MetricLineage,
            true,
        )])
        .unwrap();
        let before = graph.clone();
        let error = graph.apply(GoalDelta {
            expected_version: 1,
            additions: Vec::new(),
            progress: vec![GoalProgressUpdate {
                goal_id: "revenue".into(),
                expected_status: GoalStatus::Partial,
                status: GoalStatus::Satisfied,
                coverage_ppm: PPM,
                evidence_ids: Vec::new(),
                calculation_ids: Vec::new(),
            }],
        });
        assert!(matches!(error, Err(PlanningError::ProgressConflict(_))));
        assert_eq!(graph, before);
    }

    #[test]
    fn recovered_graph_revalidates_private_serde_state() {
        let graph = EvidenceGoalGraph::new(vec![goal(
            "revenue",
            true,
            10,
            &[],
            DirectnessRequirement::Direct,
            false,
        )])
        .unwrap();
        graph.validate_recovered().unwrap();

        let mut encoded = serde_json::to_value(&graph).unwrap();
        encoded["original_order"] = serde_json::json!(["missing"]);
        let corrupted: EvidenceGoalGraph = serde_json::from_value(encoded).unwrap();
        assert_eq!(
            corrupted.validate_recovered(),
            Err(PlanningError::InvalidRecoveredGraph)
        );
    }

    #[test]
    fn conservative_scoring_prefers_direct_required_coverage_and_stops_below_zero() {
        let graph = EvidenceGoalGraph::new(vec![goal(
            "margin_cause",
            true,
            100,
            &[],
            DirectnessRequirement::Direct,
            false,
        )])
        .unwrap();
        let mut direct = candidate("trace", &["margin_cause"]);
        direct
            .benefits
            .insert(ActionBenefit::RaisesDirectnessCeiling);
        direct.benefits.insert(ActionBenefit::ServerRecommended);
        let mut broad = candidate("broad", &["margin_cause"]);
        broad.expected_duplicate_ppm = 900_000;
        broad.expected_result_bytes = 2_000_000;
        let candidates = vec![broad, direct];
        let scored = score_actions(
            &graph,
            &candidates,
            &BTreeSet::new(),
            ScoringWeights::default(),
        )
        .unwrap();
        assert_eq!(select_best(&scored).unwrap().action_id, "trace");

        let mut costly = candidate("costly", &["margin_cause"]);
        costly.expected_tool_cost_micros = 2_000_000;
        let costly_candidates = [costly];
        let scored = score_actions(
            &graph,
            &costly_candidates,
            &BTreeSet::new(),
            ScoringWeights::default(),
        )
        .unwrap();
        assert!(select_best(&scored).is_none());
    }

    #[test]
    fn scoring_policy_cannot_disable_all_cost_and_risk_signals() {
        let disabled = ScoringWeights {
            latency_penalty_per_ms: 0,
            token_penalty: 0,
            tool_cost_penalty: 0,
            result_kib_penalty: 0,
            duplicate_penalty_ppm: 0,
            risk_penalty_ppm: 0,
            ..ScoringWeights::default()
        };
        assert_eq!(
            disabled.validate(),
            Err(PlanningError::InvalidScoringWeights)
        );
    }

    #[test]
    fn parallel_batch_respects_conflicts_auth_and_byte_budget() {
        let graph = EvidenceGoalGraph::new(vec![
            goal("a", true, 10, &[], DirectnessRequirement::Related, false),
            goal("b", true, 10, &[], DirectnessRequirement::Related, false),
            goal("c", true, 10, &[], DirectnessRequirement::Related, false),
        ])
        .unwrap();
        let first = candidate("first", &["a"]);
        let mut conflict = candidate("conflict", &["b"]);
        conflict.conflict_keys = first.conflict_keys.clone();
        let mut unsafe_auth = candidate("unsafe", &["c"]);
        unsafe_auth.auth_isolation = AuthIsolation::Shared;
        let candidates = vec![first, conflict, unsafe_auth];
        let scored = score_actions(
            &graph,
            &candidates,
            &BTreeSet::new(),
            ScoringWeights::default(),
        )
        .unwrap();
        let selected = select_parallel_batch(&scored, 3, 10_000).unwrap();
        assert_eq!(selected.len(), 1);
    }
}
