//! Trusted adaptive research policy between model proposals and capability dispatch.
//!
//! `DeepSeek` decides what may be worth investigating. This crate maps those
//! proposals to server-observed evidence gaps, applies a deterministic
//! value-of-information/cost policy, rejects duplicate work, and selects at
//! most one action for the current serial statechart. Model confidence is
//! deliberately not an input.

mod initial_plan;

pub use initial_plan::{
    InitialPlanError, InitialPlanScope, ResearchIntentCompilation, ResearchIntentReceipt,
    ResearchPlanRequester, compile_research_proposal,
};

use std::collections::{BTreeMap, BTreeSet};

use krw_agent_planning::{
    ActionBenefit, ActionCandidate, EvidenceGoal, GoalDelta, GoalProgressUpdate, GoalStatus, PPM,
    score_actions, select_best,
};
pub use krw_agent_planning::{ActionConcurrency, ActionEffect, AuthIsolation, ScoringWeights};
use krw_agent_protocol::ContentHash;
use krw_ontology_adapter::{
    ClauseCoverageProgress, ClausePlanningBinding, MAX_ORIENTATION_VOCABULARY,
    MAX_SUPPLEMENTAL_READ_STATUSES, OrientationTerm, ResearchPlanningProjection, ResearchStateV2,
    SupplementalReadStatus, derive_clause_coverage_progress, derive_research_planning_projection,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

const MAX_PROPOSALS: usize = 64;
const MAX_COMPLETED_FINGERPRINTS: usize = 256;
const MAX_REJECTED_CONTEXT_FINGERPRINTS: usize = 32;
const MAX_CONTEXT_PLAN_HASHES: usize = 64;
const MAX_RETRIEVAL_STATUS_WARNINGS: usize = 32;
// This is still a small bounded context, but it must cover a complex question
// plus one adaptive follow-up without turning a valid research plan into a
// terminal repair loop.
const MAX_CONTEXT_CLAUSES: usize = 18;
const PLANNER_CHECKPOINT_SCHEMA_VERSION: u16 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CandidateEstimate {
    pub historical_success_lower_ppm: u32,
    pub expected_duplicate_ppm: u32,
    pub failure_risk_upper_ppm: u32,
    pub expected_latency_ms: u32,
    pub expected_tokens: u32,
    pub expected_tool_cost_micros: u32,
    pub expected_result_bytes: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CandidateProposal {
    /// Provider tool-call id or another run-local opaque identifier.
    pub proposal_id: String,
    pub capability_id: String,
    /// Semantic action kind derived from the immutable `AgentImage`
    /// typed result-ingest declaration, never guessed from the provider-visible name.
    pub kind: ResearchActionKind,
    pub fingerprint: ContentHash,
    pub arguments: Value,
    pub estimate: CandidateEstimate,
    pub effect: ActionEffect,
    pub concurrency: ActionConcurrency,
    pub auth_isolation: AuthIsolation,
    pub conflict_keys: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResearchActionKind {
    QueryContext,
    TargetedQuery,
    Trace,
    /// Advisory observation read (ticker-scoped openbb plane). Mappable
    /// exactly when a frontier clause shares the proposal's trusted ticker:
    /// the observation informs the clause, the filing read resolves it.
    Observation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionReason {
    InitialContext,
    PositiveExpectedValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlannerDecision {
    Execute {
        proposal_id: String,
        score: Option<i64>,
        reason: SelectionReason,
        evaluated: u16,
    },
    NoPositiveValue {
        evaluated: u16,
        reason: NoPositiveReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoPositiveReason {
    /// The canonical evidence graph has no ready unresolved goal.
    NoFrontier,
    /// This exact action already has a committed durable result.
    DuplicateCompleted,
    /// The proposal does not match a canonical gap or recommendation.
    ProposalUnmapped,
    /// A mapped action does not clear the trusted cost/benefit threshold.
    NonPositiveScore,
}

#[derive(Debug, Clone, Default)]
pub struct ResearchPlanner {
    weights: ScoringWeights,
    projection: Option<ResearchPlanningProjection>,
    intent_projection: Option<IntentPlanningProjection>,
    completed_fingerprints: BTreeSet<ContentHash>,
    rejected_context_fingerprints: BTreeSet<ContentHash>,
    initial_context_fingerprint: Option<ContentHash>,
    confirmed_context_plan: Option<Value>,
    confirmed_context_plan_hashes: BTreeSet<ContentHash>,
}

/// Model-declared evidence goals after the kernel has accepted their compiled
/// `SearchPlan`. This is a separate semantic projection from the ontology
/// server's clause graph: the former preserves *why* a clause was selected;
/// the latter preserves *what* evidence the server found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentPlanningProjection {
    pub anchor_hash: ContentHash,
    pub graph: krw_agent_planning::EvidenceGoalGraph,
    pub clause_goal_ids: BTreeMap<String, Vec<String>>,
}

/// Exact durable state for the trusted research planner. The run engine
/// rebuilds this state from committed capability results during recovery and
/// compares the canonical hash with its checkpoint, so a stale or divergent
/// replan cannot be silently resumed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchPlannerCheckpointV4 {
    pub schema_version: u16,
    pub weights: ScoringWeights,
    pub projection: Option<ResearchPlanningProjection>,
    pub intent_projection: Option<IntentPlanningProjection>,
    pub completed_fingerprints: BTreeSet<ContentHash>,
    pub rejected_context_fingerprints: BTreeSet<ContentHash>,
    pub initial_context_fingerprint: Option<ContentHash>,
    pub confirmed_context_plan: Option<Value>,
    pub confirmed_context_plan_hashes: BTreeSet<ContentHash>,
}

impl ResearchPlanner {
    pub fn new(weights: ScoringWeights) -> Result<Self, ResearchPlannerError> {
        Ok(Self {
            weights: weights.validate()?,
            projection: None,
            intent_projection: None,
            completed_fingerprints: BTreeSet::new(),
            rejected_context_fingerprints: BTreeSet::new(),
            initial_context_fingerprint: None,
            confirmed_context_plan: None,
            confirmed_context_plan_hashes: BTreeSet::new(),
        })
    }

    pub fn projection(&self) -> Option<&ResearchPlanningProjection> {
        self.projection.as_ref()
    }

    /// The immutable user-linked evidence objective graph, when this run was
    /// entered through a `ResearchIntent` capability. It contains no raw user
    /// question or retrieved text and is safe for deterministic checkpointing.
    pub fn intent_projection(&self) -> Option<&IntentPlanningProjection> {
        self.intent_projection.as_ref()
    }

    /// Return whether the trusted, current evidence objective has no
    /// unresolved frontier. `None` means that no canonical `ResearchState`
    /// has been accepted yet, so the initial context action is still needed.
    ///
    /// This is deliberately derived from the server-normalized coverage graph
    /// (or the kernel's user-linked projection of it), never from a model's
    /// confidence. The run kernel uses it to stop advertising read actions
    /// once further evidence cannot advance a required goal.
    pub fn objective_frontier_is_exhausted(&self) -> Option<bool> {
        let projection = self.projection.as_ref()?;
        let objective = self
            .intent_projection
            .as_ref()
            .map_or(&projection.graph, |intent| &intent.graph);
        Some(objective.frontier().is_empty())
    }

    /// Exact normalized `SearchPlan` returned by the latest accepted context
    /// action. It is read-only trusted state used by the intent compiler to
    /// build an append-only follow-up; it is never provider-authored replay
    /// material.
    pub fn confirmed_context_plan(&self) -> Option<&Value> {
        self.confirmed_context_plan.as_ref()
    }

    pub fn checkpoint(&self) -> ResearchPlannerCheckpointV4 {
        ResearchPlannerCheckpointV4 {
            schema_version: PLANNER_CHECKPOINT_SCHEMA_VERSION,
            weights: self.weights,
            projection: self.projection.clone(),
            intent_projection: self.intent_projection.clone(),
            completed_fingerprints: self.completed_fingerprints.clone(),
            rejected_context_fingerprints: self.rejected_context_fingerprints.clone(),
            initial_context_fingerprint: self.initial_context_fingerprint.clone(),
            confirmed_context_plan: self.confirmed_context_plan.clone(),
            confirmed_context_plan_hashes: self.confirmed_context_plan_hashes.clone(),
        }
    }

    pub fn checkpoint_hash(&self) -> Result<ContentHash, ResearchPlannerError> {
        Ok(ContentHash::sha256(serde_jcs::to_vec(&self.checkpoint())?))
    }

    pub fn restore(
        checkpoint: ResearchPlannerCheckpointV4,
        expected_weights: ScoringWeights,
    ) -> Result<Self, ResearchPlannerError> {
        if checkpoint.schema_version != PLANNER_CHECKPOINT_SCHEMA_VERSION {
            return Err(ResearchPlannerError::CheckpointVersion);
        }
        if checkpoint.weights != expected_weights {
            return Err(ResearchPlannerError::ScoringPolicyMismatch);
        }
        expected_weights.validate()?;
        if checkpoint.completed_fingerprints.len() > MAX_COMPLETED_FINGERPRINTS {
            return Err(ResearchPlannerError::InvalidCheckpoint);
        }
        if checkpoint.rejected_context_fingerprints.len() > MAX_REJECTED_CONTEXT_FINGERPRINTS
            || checkpoint
                .rejected_context_fingerprints
                .iter()
                .any(|fingerprint| checkpoint.completed_fingerprints.contains(fingerprint))
        {
            return Err(ResearchPlannerError::InvalidCheckpoint);
        }
        if checkpoint
            .initial_context_fingerprint
            .as_ref()
            .is_some_and(|fingerprint| !checkpoint.completed_fingerprints.contains(fingerprint))
        {
            return Err(ResearchPlannerError::InvalidCheckpoint);
        }
        if checkpoint.confirmed_context_plan_hashes.len() > MAX_CONTEXT_PLAN_HASHES
            || checkpoint.confirmed_context_plan.is_some() != checkpoint.projection.is_some()
        {
            return Err(ResearchPlannerError::InvalidCheckpoint);
        }
        if let Some(plan) = &checkpoint.confirmed_context_plan {
            validate_plan_shape(plan)?;
            let hash = plan_hash(plan)?;
            if !checkpoint.confirmed_context_plan_hashes.contains(&hash) {
                return Err(ResearchPlannerError::InvalidCheckpoint);
            }
        } else if !checkpoint.confirmed_context_plan_hashes.is_empty() {
            return Err(ResearchPlannerError::InvalidCheckpoint);
        }
        if let Some(projection) = &checkpoint.projection {
            validate_projection(projection)?;
        }
        match (
            &checkpoint.intent_projection,
            &checkpoint.confirmed_context_plan,
        ) {
            (Some(intent), Some(plan)) => validate_intent_projection(intent, plan)?,
            (Some(_), None) => return Err(ResearchPlannerError::InvalidCheckpoint),
            (None, _) => {}
        }
        Ok(Self {
            weights: checkpoint.weights,
            projection: checkpoint.projection,
            intent_projection: checkpoint.intent_projection,
            completed_fingerprints: checkpoint.completed_fingerprints,
            rejected_context_fingerprints: checkpoint.rejected_context_fingerprints,
            initial_context_fingerprint: checkpoint.initial_context_fingerprint,
            confirmed_context_plan: checkpoint.confirmed_context_plan,
            confirmed_context_plan_hashes: checkpoint.confirmed_context_plan_hashes,
        })
    }

    /// Ingest a new canonical `ResearchState`. Existing goal definitions may
    /// not disappear or change; progress is merged monotonically. The
    /// `orientation_vocabulary` is the advisory company-orientation terms
    /// distilled from the committed evidence ledger; it is unioned into the
    /// projection so the wording bridge survives later bounded snapshots.
    pub fn ingest_research_state(
        &mut self,
        state: &ResearchStateV2,
        orientation_vocabulary: &[OrientationTerm],
    ) -> Result<(), ResearchPlannerError> {
        self.ingest_research_state_inner(state, orientation_vocabulary)?;
        Ok(())
    }

    /// Commit a valid server `ResearchState` together with the exact semantic
    /// receipt which produced its dispatched root `SearchPlan`. Both the
    /// server projection and the user-linked goal graph advance atomically in
    /// memory, so recovery cannot observe one without the other.
    pub fn ingest_research_state_for_intent(
        &mut self,
        state: &ResearchStateV2,
        receipt: &ResearchIntentReceipt,
        orientation_vocabulary: &[OrientationTerm],
    ) -> Result<(), ResearchPlannerError> {
        let mut next = self.clone();
        next.merge_intent_receipt(receipt)?;
        next.ingest_research_state_inner(state, orientation_vocabulary)?;
        next.apply_intent_progress(state)?;
        *self = next;
        Ok(())
    }

    fn ingest_research_state_inner(
        &mut self,
        state: &ResearchStateV2,
        orientation_vocabulary: &[OrientationTerm],
    ) -> Result<(), ResearchPlannerError> {
        let observed = derive_research_planning_projection(state, orientation_vocabulary)?;
        validate_plan_shape(&state.plan)?;
        if let Some(current) = &self.confirmed_context_plan {
            validate_append_only_plan(current, &state.plan, false)?;
        }
        let observed_plan_hash = plan_hash(&state.plan)?;
        if !self
            .confirmed_context_plan_hashes
            .contains(&observed_plan_hash)
            && self.confirmed_context_plan_hashes.len() >= MAX_CONTEXT_PLAN_HASHES
        {
            return Err(ResearchPlannerError::ContextPlanLimit);
        }
        match &mut self.projection {
            None => self.projection = Some(observed),
            Some(current) => merge_projection(current, observed)?,
        }
        self.confirmed_context_plan = Some(state.plan.clone());
        self.confirmed_context_plan_hashes
            .insert(observed_plan_hash);
        Ok(())
    }

    fn merge_intent_receipt(
        &mut self,
        receipt: &ResearchIntentReceipt,
    ) -> Result<(), ResearchPlannerError> {
        receipt
            .validate_recovered()
            .map_err(|_| ResearchPlannerError::IntentReceipt)?;
        match &mut self.intent_projection {
            None => {
                ensure_fresh_intent_goals(&receipt.intent_graph)?;
                self.intent_projection = Some(IntentPlanningProjection {
                    anchor_hash: receipt.anchor_hash.clone(),
                    graph: receipt.intent_graph.clone(),
                    clause_goal_ids: receipt.clause_goal_ids.clone(),
                });
            }
            Some(current) => {
                if current.anchor_hash != receipt.anchor_hash {
                    return Err(ResearchPlannerError::IntentAnchorMismatch);
                }
                let mut additions = Vec::new();
                for goal in receipt.intent_graph.goals() {
                    match current.graph.goal(&goal.goal_id) {
                        Some(existing) if same_goal_definition(existing, goal) => {}
                        Some(_) => {
                            // A provider may restate an already committed
                            // objective on a later turn (for example,
                            // changing directness while asking for the next
                            // piece of evidence).  The first accepted
                            // definition remains the trusted contract; keep
                            // it and continue with any new clause bindings.
                            // This prevents a harmless rephrasing from
                            // terminating an otherwise valid research run.
                        }
                        None => {
                            ensure_fresh_intent_goal(goal)?;
                            additions.push(goal.clone());
                        }
                    }
                }
                if !additions.is_empty() {
                    current.graph.apply(GoalDelta {
                        expected_version: current.graph.version(),
                        additions,
                        progress: Vec::new(),
                    })?;
                }
                for (clause_id, goal_ids) in &receipt.clause_goal_ids {
                    match current.clause_goal_ids.get(clause_id) {
                        Some(existing) if existing == goal_ids => {}
                        Some(_) => return Err(ResearchPlannerError::IntentClauseBindingDrift),
                        None => {
                            if goal_ids
                                .iter()
                                .any(|goal_id| current.graph.goal(goal_id).is_none())
                            {
                                return Err(ResearchPlannerError::IntentReceipt);
                            }
                            current
                                .clause_goal_ids
                                .insert(clause_id.clone(), goal_ids.clone());
                        }
                    }
                }
                validate_intent_projection_without_plan(current)?;
            }
        }
        Ok(())
    }

    fn apply_intent_progress(
        &mut self,
        state: &ResearchStateV2,
    ) -> Result<(), ResearchPlannerError> {
        let Some(intent) = &mut self.intent_projection else {
            return Ok(());
        };
        let observations = derive_clause_coverage_progress(state)?;
        let required_missing_clauses = required_missing_clause_ids(state);
        let mut observed_goals = BTreeMap::<String, SemanticGoalObservation>::new();
        for (clause_id, goal_ids) in &intent.clause_goal_ids {
            let clause = observations
                .get(clause_id)
                .ok_or(ResearchPlannerError::IntentClauseBindingDrift)?;
            for goal_id in goal_ids {
                let goal = intent
                    .graph
                    .goal(goal_id)
                    .ok_or(ResearchPlannerError::IntentReceipt)?;
                let observation = semantic_observation_for_clause(
                    clause,
                    goal,
                    required_missing_clauses.contains(clause_id.as_str()),
                );
                observed_goals
                    .entry(goal_id.clone())
                    .and_modify(|current| current.merge(&observation))
                    .or_insert(observation);
            }
        }
        let mut progress = Vec::new();
        for goal in intent.graph.goals() {
            let observed = observed_goals
                .get(&goal.goal_id)
                .ok_or(ResearchPlannerError::IntentReceipt)?;
            let new_evidence = observed
                .evidence_ids
                .iter()
                .filter(|id| !goal.evidence_ids.contains(id))
                .cloned()
                .collect::<Vec<_>>();
            let new_calculations = observed
                .calculation_ids
                .iter()
                .filter(|id| !goal.calculation_ids.contains(id))
                .cloned()
                .collect::<Vec<_>>();
            if observed.coverage_ppm < goal.coverage_ppm
                || (goal.status == GoalStatus::Satisfied
                    && observed.status != GoalStatus::Satisfied)
                || goal.status == GoalStatus::Blocked
            {
                return Err(ResearchPlannerError::IntentGoalProgressDrift);
            }
            if observed.status == GoalStatus::Unresolved {
                if goal.status != GoalStatus::Unresolved {
                    return Err(ResearchPlannerError::IntentGoalProgressDrift);
                }
                // A goal that is still unresolved but already has new evidence is
                // in progress, not drifting. With multiple clauses a single
                // capability call may cover some goals and leave others
                // unresolved; the agent must be allowed to make further calls to
                // resolve them. Previously, any new evidence on an unresolved
                // goal was treated as drift and aborted the whole run.
                continue;
            }
            if observed.evidence_ids.is_empty()
                || (goal.calculation_required && observed.calculation_ids.is_empty())
            {
                return Err(ResearchPlannerError::IntentCoverageProvenanceMissing);
            }
            if goal.status == observed.status
                && goal.coverage_ppm == observed.coverage_ppm
                && new_evidence.is_empty()
                && new_calculations.is_empty()
            {
                continue;
            }
            progress.push(GoalProgressUpdate {
                goal_id: goal.goal_id.clone(),
                expected_status: goal.status,
                status: observed.status,
                coverage_ppm: observed.coverage_ppm,
                evidence_ids: new_evidence,
                calculation_ids: new_calculations,
            });
        }
        if !progress.is_empty() {
            intent.graph.apply(GoalDelta {
                expected_version: intent.graph.version(),
                additions: Vec::new(),
                progress,
            })?;
        }
        Ok(())
    }

    pub fn record_completed(
        &mut self,
        fingerprint: ContentHash,
    ) -> Result<(), ResearchPlannerError> {
        if self.rejected_context_fingerprints.contains(&fingerprint) {
            return Err(ResearchPlannerError::RejectedContextConflict);
        }
        if !self.completed_fingerprints.contains(&fingerprint)
            && self.completed_fingerprints.len() >= MAX_COMPLETED_FINGERPRINTS
        {
            return Err(ResearchPlannerError::CompletedFingerprintLimit);
        }
        self.completed_fingerprints.insert(fingerprint);
        Ok(())
    }

    /// Keep a kernel-owned result classification for a supplemental read.
    /// It is control context rather than evidence and never makes a user
    /// answer fail; it only prevents raw-tool compaction from turning a
    /// failed exact read into an apparent company non-disclosure.
    pub fn record_supplemental_retrieval_warning(&mut self, warning: &str) {
        if !warning.starts_with("supplemental_") {
            return;
        }
        let Some(projection) = self.projection.as_mut() else {
            return;
        };
        let mut warnings = projection
            .retrieval_status
            .warnings
            .iter()
            .filter(|current| !current.is_empty())
            .cloned()
            .collect::<BTreeSet<_>>();
        warnings.insert(warning.to_owned());
        projection.retrieval_status.warnings = warnings
            .into_iter()
            .take(MAX_RETRIEVAL_STATUS_WARNINGS)
            .collect();
    }

    /// Keep a bounded, typed outcome for a supplemental read. This is control
    /// context only: it never changes goal coverage or final-answer validity.
    pub fn record_supplemental_retrieval_status(&mut self, status: SupplementalReadStatus) {
        let Some(projection) = self.projection.as_mut() else {
            return;
        };
        if projection.retrieval_status.supplemental_reads.last() == Some(&status) {
            return;
        }
        projection.retrieval_status.supplemental_reads.push(status);
        if projection.retrieval_status.supplemental_reads.len() > MAX_SUPPLEMENTAL_READ_STATUSES {
            let excess = projection.retrieval_status.supplemental_reads.len()
                - MAX_SUPPLEMENTAL_READ_STATUSES;
            projection
                .retrieval_status
                .supplemental_reads
                .drain(..excess);
        }
    }

    /// Remember a canonical query-context input that the server rejected.
    /// It is not a completed bootstrap: a different corrected plan remains
    /// eligible, while an exact invalid replay fails closed before dispatch.
    pub fn record_rejected_context(
        &mut self,
        fingerprint: ContentHash,
    ) -> Result<(), ResearchPlannerError> {
        if self.completed_fingerprints.contains(&fingerprint)
            || self.initial_context_fingerprint.as_ref() == Some(&fingerprint)
        {
            return Err(ResearchPlannerError::RejectedContextConflict);
        }
        if !self.rejected_context_fingerprints.contains(&fingerprint)
            && self.rejected_context_fingerprints.len() >= MAX_REJECTED_CONTEXT_FINGERPRINTS
        {
            return Err(ResearchPlannerError::RejectedContextLimit);
        }
        self.rejected_context_fingerprints.insert(fingerprint);
        Ok(())
    }

    /// Record the one bootstrap context action. A second context action before
    /// a typed `ResearchState` exists is never silently retried.
    pub fn record_initial_context(
        &mut self,
        fingerprint: ContentHash,
    ) -> Result<(), ResearchPlannerError> {
        match &self.initial_context_fingerprint {
            Some(existing) if existing != &fingerprint => {
                Err(ResearchPlannerError::InitialContextAlreadyCompleted)
            }
            Some(_) => Ok(()),
            None => {
                self.record_completed(fingerprint.clone())?;
                self.initial_context_fingerprint = Some(fingerprint);
                Ok(())
            }
        }
    }

    pub fn select(
        &self,
        proposals: &[CandidateProposal],
    ) -> Result<PlannerDecision, ResearchPlannerError> {
        if proposals.is_empty() || proposals.len() > MAX_PROPOSALS {
            return Err(ResearchPlannerError::ProposalLimit);
        }
        validate_proposal_ids(proposals)?;
        let evaluated = u16::try_from(proposals.len()).unwrap_or(u16::MAX);
        let Some(projection) = &self.projection else {
            if self.initial_context_fingerprint.is_some() {
                return Err(ResearchPlannerError::InitialContextAlreadyCompleted);
            }
            if proposals.len() == 1 && proposals[0].kind == ResearchActionKind::QueryContext {
                if self
                    .rejected_context_fingerprints
                    .contains(&proposals[0].fingerprint)
                {
                    return Err(ResearchPlannerError::RejectedContextAlreadyAttempted);
                }
                return Ok(PlannerDecision::Execute {
                    proposal_id: proposals[0].proposal_id.clone(),
                    score: None,
                    reason: SelectionReason::InitialContext,
                    evaluated,
                });
            }
            return Err(ResearchPlannerError::InitialContextRequired);
        };

        if proposals.len() == 1 && proposals[0].kind == ResearchActionKind::QueryContext {
            let proposal = &proposals[0];
            if self
                .rejected_context_fingerprints
                .contains(&proposal.fingerprint)
            {
                return Err(ResearchPlannerError::RejectedContextAlreadyAttempted);
            }
            if self.completed_fingerprints.contains(&proposal.fingerprint) {
                return Ok(PlannerDecision::NoPositiveValue {
                    evaluated,
                    reason: NoPositiveReason::DuplicateCompleted,
                });
            }
            let current = self
                .confirmed_context_plan
                .as_ref()
                .ok_or(ResearchPlannerError::InvalidCheckpoint)?;
            let appended = validate_append_only_plan(current, &proposal.arguments, true)?;
            let hash = plan_hash(&proposal.arguments)?;
            if self.confirmed_context_plan_hashes.contains(&hash) {
                return Ok(PlannerDecision::NoPositiveValue {
                    evaluated,
                    reason: NoPositiveReason::DuplicateCompleted,
                });
            }
            let score = score_context_replan(appended, proposal.estimate)?;
            return if score > 0 {
                Ok(PlannerDecision::Execute {
                    proposal_id: proposal.proposal_id.clone(),
                    score: Some(score),
                    reason: SelectionReason::PositiveExpectedValue,
                    evaluated,
                })
            } else {
                Ok(PlannerDecision::NoPositiveValue {
                    evaluated,
                    reason: NoPositiveReason::NonPositiveScore,
                })
            };
        }

        let objective_graph = self
            .intent_projection
            .as_ref()
            .map_or(&projection.graph, |intent| &intent.graph);
        if objective_graph.frontier().is_empty() {
            return Ok(PlannerDecision::NoPositiveValue {
                evaluated,
                reason: NoPositiveReason::NoFrontier,
            });
        }

        let mut action_ids = BTreeMap::new();
        let mut seen_fingerprints = BTreeSet::new();
        let mut candidates = Vec::new();
        let mut saw_uncompleted = false;
        for proposal in proposals {
            if !seen_fingerprints.insert(proposal.fingerprint.clone()) {
                continue;
            }
            if self.completed_fingerprints.contains(&proposal.fingerprint) {
                continue;
            }
            saw_uncompleted = true;
            let Some((goal_ids, benefits)) =
                map_goals(projection, self.intent_projection.as_ref(), proposal)
            else {
                continue;
            };
            let action_id = planner_action_id(&proposal.fingerprint);
            action_ids.insert(action_id.clone(), proposal.proposal_id.clone());
            candidates.push(ActionCandidate {
                action_id,
                capability_id: proposal.capability_id.clone(),
                action_fingerprint: proposal.fingerprint.clone(),
                goal_ids,
                benefits,
                historical_success_lower_ppm: proposal.estimate.historical_success_lower_ppm,
                expected_duplicate_ppm: proposal.estimate.expected_duplicate_ppm,
                failure_risk_upper_ppm: proposal.estimate.failure_risk_upper_ppm,
                expected_latency_ms: proposal.estimate.expected_latency_ms,
                expected_tokens: proposal.estimate.expected_tokens,
                expected_tool_cost_micros: proposal.estimate.expected_tool_cost_micros,
                expected_result_bytes: proposal.estimate.expected_result_bytes,
                effect: proposal.effect,
                concurrency: proposal.concurrency,
                auth_isolation: proposal.auth_isolation,
                conflict_keys: proposal.conflict_keys.clone(),
            });
        }
        if candidates.is_empty() {
            return Ok(PlannerDecision::NoPositiveValue {
                evaluated,
                reason: if saw_uncompleted {
                    NoPositiveReason::ProposalUnmapped
                } else {
                    NoPositiveReason::DuplicateCompleted
                },
            });
        }
        let scored = score_actions(
            objective_graph,
            &candidates,
            &self.completed_fingerprints,
            self.weights,
        )?;
        let Some(selected) = select_best(&scored) else {
            return Ok(PlannerDecision::NoPositiveValue {
                evaluated,
                reason: NoPositiveReason::NonPositiveScore,
            });
        };
        let score = scored
            .iter()
            .find(|action| action.candidate.action_id == selected.action_id)
            .map(|action| action.score)
            .ok_or(ResearchPlannerError::SelectionInvariant)?;
        let proposal_id = action_ids
            .get(&selected.action_id)
            .cloned()
            .ok_or(ResearchPlannerError::SelectionInvariant)?;
        Ok(PlannerDecision::Execute {
            proposal_id,
            score: Some(score),
            reason: SelectionReason::PositiveExpectedValue,
            evaluated,
        })
    }
}

#[derive(Debug, Clone)]
struct SemanticGoalObservation {
    status: GoalStatus,
    coverage_ppm: u32,
    evidence_ids: Vec<String>,
    calculation_ids: Vec<String>,
}

impl SemanticGoalObservation {
    fn merge(&mut self, other: &Self) {
        let current_rank = semantic_status_rank(self.status);
        let other_rank = semantic_status_rank(other.status);
        if other_rank > current_rank
            || (other_rank == current_rank && other.coverage_ppm > self.coverage_ppm)
        {
            self.status = other.status;
            self.coverage_ppm = other.coverage_ppm;
        }
        self.evidence_ids.extend(other.evidence_ids.iter().cloned());
        self.evidence_ids.sort();
        self.evidence_ids.dedup();
        self.calculation_ids
            .extend(other.calculation_ids.iter().cloned());
        self.calculation_ids.sort();
        self.calculation_ids.dedup();
    }
}

fn semantic_observation_for_clause(
    clause: &ClauseCoverageProgress,
    goal: &EvidenceGoal,
    has_required_gap: bool,
) -> SemanticGoalObservation {
    let (mut status, mut coverage_ppm, calculation_ids) = if goal.calculation_required {
        match (clause.status, clause.calculation_status) {
            (GoalStatus::Satisfied, Some(GoalStatus::Satisfied))
                if !clause.calculation_ids.is_empty() =>
            {
                (GoalStatus::Satisfied, PPM, clause.calculation_ids.clone())
            }
            (GoalStatus::Blocked, _) | (_, Some(GoalStatus::Blocked)) => {
                (GoalStatus::Blocked, 0, Vec::new())
            }
            _ => {
                let evidence_coverage = clause.coverage_ppm;
                let calculation_coverage = clause.calculation_coverage_ppm.unwrap_or(0);
                let coverage_ppm = evidence_coverage.min(calculation_coverage);
                if coverage_ppm > 0 {
                    (
                        GoalStatus::Partial,
                        coverage_ppm.clamp(1, PPM - 1),
                        clause.calculation_ids.clone(),
                    )
                } else {
                    (GoalStatus::Unresolved, 0, Vec::new())
                }
            }
        }
    } else {
        (clause.status, clause.coverage_ppm, Vec::new())
    };
    // The ontology can return a usable raw observation while also reporting
    // that a required comparison, period, or calculation for this exact
    // clause remains unresolved.  Do not mark the user-linked goal complete
    // in that split state: the analyst may already have chosen an advertised
    // precise follow-up which can close the remaining gap.  This is a
    // progress projection only; it neither forces a tool call nor blocks the
    // eventual answer if the precise read remains unavailable.
    if has_required_gap && status == GoalStatus::Satisfied {
        status = GoalStatus::Partial;
        coverage_ppm = coverage_ppm.min(PPM - 1);
    }
    SemanticGoalObservation {
        status,
        coverage_ppm,
        evidence_ids: clause.evidence_ids.clone(),
        calculation_ids,
    }
}

/// Return only server-reported gaps for clauses that the normalized plan
/// itself marks as required.  Incidental/non-required server gaps must not
/// reopen an otherwise complete user objective.
fn required_missing_clause_ids(state: &ResearchStateV2) -> BTreeSet<&str> {
    let required = state
        .plan
        .get("clauses")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|clause| {
            clause
                .get("required")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .filter_map(|clause| clause.get("clause_id").and_then(Value::as_str))
        .collect::<BTreeSet<_>>();
    state
        .missing_parts
        .iter()
        .filter_map(|missing| missing.clause_id.as_deref())
        .filter(|clause_id| required.contains(clause_id))
        .collect()
}

const fn semantic_status_rank(status: GoalStatus) -> u8 {
    match status {
        GoalStatus::Blocked => 0,
        GoalStatus::Unresolved => 1,
        GoalStatus::Partial => 2,
        GoalStatus::Satisfied => 3,
    }
}

fn ensure_fresh_intent_goals(
    graph: &krw_agent_planning::EvidenceGoalGraph,
) -> Result<(), ResearchPlannerError> {
    for goal in graph.goals() {
        ensure_fresh_intent_goal(goal)?;
    }
    Ok(())
}

fn ensure_fresh_intent_goal(goal: &EvidenceGoal) -> Result<(), ResearchPlannerError> {
    if goal.status != GoalStatus::Unresolved
        || goal.coverage_ppm != 0
        || !goal.evidence_ids.is_empty()
        || !goal.calculation_ids.is_empty()
    {
        return Err(ResearchPlannerError::IntentReceipt);
    }
    Ok(())
}

fn validate_intent_projection_without_plan(
    projection: &IntentPlanningProjection,
) -> Result<(), ResearchPlannerError> {
    projection.graph.validate_recovered()?;
    if projection.clause_goal_ids.is_empty()
        || projection.clause_goal_ids.len() > MAX_CONTEXT_CLAUSES
    {
        return Err(ResearchPlannerError::InvalidCheckpoint);
    }
    for (clause_id, goal_ids) in &projection.clause_goal_ids {
        if !valid_planner_identifier(clause_id)
            || goal_ids.is_empty()
            || goal_ids.len() > 16
            || goal_ids.windows(2).any(|pair| pair[0] >= pair[1])
            || goal_ids
                .iter()
                .any(|goal_id| projection.graph.goal(goal_id).is_none())
        {
            return Err(ResearchPlannerError::InvalidCheckpoint);
        }
    }
    Ok(())
}

fn validate_intent_projection(
    projection: &IntentPlanningProjection,
    plan: &Value,
) -> Result<(), ResearchPlannerError> {
    validate_intent_projection_without_plan(projection)?;
    let clause_ids = plan
        .get("clauses")
        .and_then(Value::as_array)
        .ok_or(ResearchPlannerError::InvalidCheckpoint)?
        .iter()
        .map(|clause| clause.get("clause_id").and_then(Value::as_str))
        .collect::<Option<BTreeSet<_>>>()
        .ok_or(ResearchPlannerError::InvalidCheckpoint)?;
    if projection
        .clause_goal_ids
        .keys()
        .any(|clause_id| !clause_ids.contains(clause_id.as_str()))
    {
        return Err(ResearchPlannerError::InvalidCheckpoint);
    }
    Ok(())
}

fn valid_planner_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn validate_plan_shape(plan: &Value) -> Result<(), ResearchPlannerError> {
    let object = plan
        .as_object()
        .ok_or(ResearchPlannerError::ContextPlanDrift)?;
    let clauses = object
        .get("clauses")
        .and_then(Value::as_array)
        .ok_or(ResearchPlannerError::ContextPlanDrift)?;
    if !(1..=MAX_CONTEXT_CLAUSES).contains(&clauses.len())
        || serde_jcs::to_vec(plan)?.len() > 256 * 1024
    {
        return Err(ResearchPlannerError::ContextPlanLimit);
    }
    let tickers = object
        .get("tickers")
        .and_then(Value::as_array)
        .ok_or(ResearchPlannerError::ContextPlanDrift)?;
    let mut ticker_set = BTreeSet::new();
    if tickers.iter().any(|ticker| {
        ticker.as_str().is_none_or(|ticker| {
            ticker.is_empty() || ticker.len() > 32 || !ticker_set.insert(ticker)
        })
    }) {
        return Err(ResearchPlannerError::ContextPlanDrift);
    }
    let mut clause_ids = BTreeSet::new();
    for clause in clauses {
        let clause = clause
            .as_object()
            .ok_or(ResearchPlannerError::ContextPlanDrift)?;
        let clause_id = clause
            .get("clause_id")
            .and_then(Value::as_str)
            .ok_or(ResearchPlannerError::ContextPlanDrift)?;
        if clause_id.is_empty()
            || clause_id.len() > 64
            || !clause_ids.insert(clause_id)
            || clause
                .get("tickers")
                .and_then(Value::as_array)
                .is_some_and(|values| {
                    values.iter().any(|ticker| {
                        ticker
                            .as_str()
                            .is_none_or(|ticker| !ticker_set.contains(ticker))
                    })
                })
        {
            return Err(ResearchPlannerError::ContextPlanDrift);
        }
    }
    Ok(())
}

/// Verify that a canonical server-emitted `SearchPlan` is the normalized form
/// of the exact plan sent by the provider. Pydantic/FastMCP may add declared
/// defaults and normalize bounded string sets, so raw JSON equality is too
/// strict. This guard nevertheless forbids scope changes, clause insertion,
/// clause removal, clause reordering, or any semantic rewrite.
pub fn validate_normalized_plan_exchange(
    dispatched: &Value,
    normalized: &Value,
) -> Result<(), ResearchPlannerError> {
    canonicalize_normalized_plan_exchange(dispatched, normalized).map(|_| ())
}

/// Verify a server-normalized `SearchPlan` and return its canonical form.
///
/// The server may omit a field only when the dispatched value is exactly that
/// field's documented default.  Filling those omissions keeps the persisted
/// planner state canonical while still rejecting a dropped non-default scope,
/// proof requirement, or retrieval constraint.
pub fn canonicalize_normalized_plan_exchange(
    dispatched: &Value,
    normalized: &Value,
) -> Result<Value, ResearchPlannerError> {
    const PLAN_KEYS: &[&str] = &[
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
    ];
    const CLAUSE_KEYS: &[&str] = &[
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
    ];

    if serde_jcs::to_vec(dispatched)?.len() > 256 * 1024 {
        return Err(normalized_plan_mismatch(
            NormalizedPlanMismatchKind::DispatchedPayloadTooLarge,
        ));
    }
    let mut expected = dispatched.clone();
    let sent = expected
        .as_object_mut()
        .ok_or_else(|| normalized_plan_mismatch(NormalizedPlanMismatchKind::DispatchedPlanShape))?;
    fill_omitted_plan_defaults(sent);
    let sent_clauses = sent
        .get_mut("clauses")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| normalized_plan_mismatch(NormalizedPlanMismatchKind::ClauseCount))?;
    for sent_clause in sent_clauses {
        let sent_clause = sent_clause.as_object_mut().ok_or_else(|| {
            normalized_plan_mismatch(NormalizedPlanMismatchKind::DispatchedClauseShape)
        })?;
        fill_omitted_clause_defaults(sent_clause);
    }
    let sent = expected
        .as_object()
        .ok_or_else(|| normalized_plan_mismatch(NormalizedPlanMismatchKind::DispatchedPlanShape))?;
    let mut canonical = normalized.clone();
    let observed = canonical
        .as_object_mut()
        .ok_or_else(|| normalized_plan_mismatch(NormalizedPlanMismatchKind::ObservedPlanShape))?;
    fill_omitted_plan_defaults(observed);
    let sent_clauses = sent
        .get("clauses")
        .and_then(Value::as_array)
        .filter(|clauses| (1..=MAX_CONTEXT_CLAUSES).contains(&clauses.len()))
        .ok_or_else(|| normalized_plan_mismatch(NormalizedPlanMismatchKind::ClauseCount))?;
    let observed_clauses = observed
        .get_mut("clauses")
        .and_then(Value::as_array_mut)
        .filter(|clauses| clauses.len() == sent_clauses.len())
        .ok_or_else(|| normalized_plan_mismatch(NormalizedPlanMismatchKind::ClauseCount))?;
    for (sent_clause, observed_clause) in sent_clauses.iter().zip(observed_clauses.iter_mut()) {
        sent_clause.as_object().ok_or_else(|| {
            normalized_plan_mismatch(NormalizedPlanMismatchKind::DispatchedClauseShape)
        })?;
        let observed_clause = observed_clause.as_object_mut().ok_or_else(|| {
            normalized_plan_mismatch(NormalizedPlanMismatchKind::ObservedClauseShape)
        })?;
        fill_omitted_clause_defaults(observed_clause);
    }
    validate_plan_shape(&canonical)
        .map_err(|_| normalized_plan_mismatch(NormalizedPlanMismatchKind::ObservedPlanShape))?;
    let observed = canonical
        .as_object()
        .ok_or_else(|| normalized_plan_mismatch(NormalizedPlanMismatchKind::ObservedPlanShape))?;
    if !keys_are_subset(sent, PLAN_KEYS) {
        return Err(normalized_plan_mismatch(
            NormalizedPlanMismatchKind::DispatchedPlanKeys,
        ));
    }
    if !keys_are_exact(observed, PLAN_KEYS) {
        return Err(normalized_plan_mismatch(
            NormalizedPlanMismatchKind::ObservedPlanKeys,
        ));
    }
    if !matches!(sent.get("question"), Some(Value::String(value)) if !value.trim().is_empty()) {
        return Err(normalized_plan_mismatch(
            NormalizedPlanMismatchKind::PlanField("question"),
        ));
    }
    if !matches!(sent.get("intent"), Some(Value::String(value)) if !value.trim().is_empty()) {
        return Err(normalized_plan_mismatch(
            NormalizedPlanMismatchKind::PlanField("intent"),
        ));
    }

    for key in PLAN_KEYS {
        if *key == "clauses" {
            continue;
        }
        match *key {
            "tickers" | "document_types" | "periods" | "comparison_axes" => {
                let uppercase = matches!(*key, "tickers" | "document_types" | "periods");
                let expected = match sent.get(*key) {
                    Some(value) => normalize_string_set(
                        value,
                        uppercase,
                        NormalizedPlanMismatchKind::PlanField(key),
                    )?,
                    None if *key == "tickers" => Vec::new(),
                    None => continue,
                };
                let actual = normalize_string_set(
                    observed.get(*key).ok_or_else(|| {
                        normalized_plan_mismatch(NormalizedPlanMismatchKind::PlanField(key))
                    })?,
                    uppercase,
                    NormalizedPlanMismatchKind::PlanField(key),
                )?;
                if expected != actual {
                    return Err(normalized_plan_mismatch(
                        NormalizedPlanMismatchKind::PlanField(key),
                    ));
                }
            }
            "universe" if !sent.contains_key(*key) => {
                if !observed.get(*key).is_some_and(Value::is_null) {
                    return Err(normalized_plan_mismatch(
                        NormalizedPlanMismatchKind::PlanField("universe"),
                    ));
                }
            }
            _ => {
                if let Some(expected) = sent.get(*key) {
                    let actual = observed.get(*key).ok_or_else(|| {
                        normalized_plan_mismatch(NormalizedPlanMismatchKind::PlanField(key))
                    })?;
                    if !normalized_scalar_equal(expected, actual) {
                        return Err(normalized_plan_mismatch(
                            NormalizedPlanMismatchKind::PlanField(key),
                        ));
                    }
                }
            }
        }
    }

    let observed_clauses = observed
        .get("clauses")
        .and_then(Value::as_array)
        .filter(|clauses| clauses.len() == sent_clauses.len())
        .ok_or_else(|| normalized_plan_mismatch(NormalizedPlanMismatchKind::ClauseCount))?;
    for (sent_clause, observed_clause) in sent_clauses.iter().zip(observed_clauses) {
        let sent_clause = sent_clause.as_object().ok_or_else(|| {
            normalized_plan_mismatch(NormalizedPlanMismatchKind::DispatchedClauseShape)
        })?;
        let observed_clause = observed_clause.as_object().ok_or_else(|| {
            normalized_plan_mismatch(NormalizedPlanMismatchKind::ObservedClauseShape)
        })?;
        if !keys_are_subset(sent_clause, CLAUSE_KEYS) {
            return Err(normalized_plan_mismatch(
                NormalizedPlanMismatchKind::DispatchedClauseKeys,
            ));
        }
        if !keys_are_exact(observed_clause, CLAUSE_KEYS) {
            return Err(normalized_plan_mismatch(
                NormalizedPlanMismatchKind::ObservedClauseKeys,
            ));
        }
        if !sent_clause.contains_key("clause_id") {
            return Err(normalized_plan_mismatch(
                NormalizedPlanMismatchKind::ClauseField("clause_id"),
            ));
        }
        if !sent_clause.contains_key("retrieval_query") {
            return Err(normalized_plan_mismatch(
                NormalizedPlanMismatchKind::ClauseField("retrieval_query"),
            ));
        }
        for key in CLAUSE_KEYS {
            match *key {
                "tickers"
                | "metric_dimensions"
                | "metrics"
                | "object_types"
                | "required_concepts"
                | "required_predicates" => {
                    let expected = match sent_clause.get(*key) {
                        Some(value) => normalize_string_set(
                            value,
                            *key == "tickers",
                            NormalizedPlanMismatchKind::ClauseField(key),
                        )?,
                        None if *key == "tickers" => Vec::new(),
                        None => continue,
                    };
                    let actual = normalize_string_set(
                        observed_clause.get(*key).ok_or_else(|| {
                            normalized_plan_mismatch(NormalizedPlanMismatchKind::ClauseField(key))
                        })?,
                        *key == "tickers",
                        NormalizedPlanMismatchKind::ClauseField(key),
                    )?;
                    if expected != actual {
                        return Err(normalized_plan_mismatch(
                            NormalizedPlanMismatchKind::ClauseField(key),
                        ));
                    }
                }
                _ => {
                    if let Some(expected) = sent_clause.get(*key) {
                        let actual = observed_clause.get(*key).ok_or_else(|| {
                            normalized_plan_mismatch(NormalizedPlanMismatchKind::ClauseField(key))
                        })?;
                        if !normalized_scalar_equal(expected, actual) {
                            return Err(normalized_plan_mismatch(
                                NormalizedPlanMismatchKind::ClauseField(key),
                            ));
                        }
                    }
                }
            }
        }
    }
    Ok(canonical)
}

fn fill_omitted_plan_defaults(plan: &mut serde_json::Map<String, Value>) {
    for (key, value) in [
        ("tickers", serde_json::json!([])),
        ("document_types", serde_json::json!([])),
        ("periods", serde_json::json!([])),
        ("universe", Value::Null),
        ("comparison_axes", serde_json::json!([])),
        ("answer_scope", Value::String("direct".into())),
        ("uncertainty", Value::String("low".into())),
        ("limit_results", serde_json::json!(12)),
        ("limit_tickers", serde_json::json!(20)),
    ] {
        plan.entry(key).or_insert(value);
    }
}

fn fill_omitted_clause_defaults(clause: &mut serde_json::Map<String, Value>) {
    for (key, value) in [
        ("required_concepts", serde_json::json!([])),
        ("required_predicates", serde_json::json!([])),
        ("required", Value::Bool(true)),
        ("tickers", serde_json::json!([])),
        ("directness", Value::String("direct_preferred".into())),
        ("object_types", serde_json::json!([])),
        ("metrics", serde_json::json!([])),
        ("metric_dimensions", serde_json::json!([])),
        ("metric_scope", Value::String("company_total".into())),
        ("calculation_window", Value::Null),
    ] {
        clause.entry(key).or_insert(value);
    }
}

fn keys_are_subset(object: &serde_json::Map<String, Value>, allowed: &[&str]) -> bool {
    object.keys().all(|key| allowed.contains(&key.as_str()))
}

fn keys_are_exact(object: &serde_json::Map<String, Value>, required: &[&str]) -> bool {
    object.len() == required.len() && required.iter().all(|key| object.contains_key(*key))
}

fn normalize_string_set(
    value: &Value,
    uppercase: bool,
    mismatch: NormalizedPlanMismatchKind,
) -> Result<Vec<String>, ResearchPlannerError> {
    let values = value
        .as_array()
        .ok_or_else(|| normalized_plan_mismatch(mismatch))?;
    let mut seen = BTreeSet::new();
    let mut normalized = Vec::with_capacity(values.len());
    for value in values {
        let value = value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| normalized_plan_mismatch(mismatch))?;
        let value = if uppercase {
            value.to_ascii_uppercase()
        } else {
            value.to_owned()
        };
        if seen.insert(value.clone()) {
            normalized.push(value);
        }
    }
    Ok(normalized)
}

fn normalized_plan_mismatch(kind: NormalizedPlanMismatchKind) -> ResearchPlannerError {
    ResearchPlannerError::NormalizedPlanMismatch(kind)
}

fn normalized_scalar_equal(expected: &Value, actual: &Value) -> bool {
    match (expected, actual) {
        (Value::String(expected), Value::String(actual)) => expected.trim() == actual,
        _ => expected == actual,
    }
}

fn validate_append_only_plan(
    current: &Value,
    next: &Value,
    require_append: bool,
) -> Result<usize, ResearchPlannerError> {
    validate_plan_shape(current)?;
    validate_plan_shape(next)?;
    let current = current
        .as_object()
        .ok_or(ResearchPlannerError::ContextPlanDrift)?;
    let next = next
        .as_object()
        .ok_or(ResearchPlannerError::ContextPlanDrift)?;
    if current.len() != next.len()
        || current.iter().any(|(key, value)| {
            key != "clauses" && next.get(key).is_none_or(|next_value| next_value != value)
        })
    {
        return Err(ResearchPlannerError::ContextPlanDrift);
    }
    let current_clauses = current
        .get("clauses")
        .and_then(Value::as_array)
        .ok_or(ResearchPlannerError::ContextPlanDrift)?;
    let next_clauses = next
        .get("clauses")
        .and_then(Value::as_array)
        .ok_or(ResearchPlannerError::ContextPlanDrift)?;
    if next_clauses.len() < current_clauses.len() || !next_clauses.starts_with(current_clauses) {
        return Err(ResearchPlannerError::ContextPlanDrift);
    }
    let appended = next_clauses.len() - current_clauses.len();
    if require_append && appended == 0 {
        return Err(ResearchPlannerError::ContextPlanDrift);
    }
    Ok(appended)
}

fn plan_hash(plan: &Value) -> Result<ContentHash, ResearchPlannerError> {
    Ok(ContentHash::sha256(serde_jcs::to_vec(plan)?))
}

/// Temporary conservative baseline until deployment telemetry publishes a
/// signed estimate registry. No provider/model confidence enters this score.
fn score_context_replan(
    appended_clauses: usize,
    estimate: CandidateEstimate,
) -> Result<i64, ResearchPlannerError> {
    if appended_clauses == 0
        || estimate.historical_success_lower_ppm == 0
        || estimate.historical_success_lower_ppm > krw_agent_planning::PPM
        || estimate.expected_duplicate_ppm > krw_agent_planning::PPM
        || estimate.failure_risk_upper_ppm > krw_agent_planning::PPM
        || estimate.expected_latency_ms == 0
        || estimate.expected_tokens == 0
        || estimate.expected_tool_cost_micros == 0
        || estimate.expected_result_bytes == 0
    {
        return Err(ResearchPlannerError::InvalidEstimate);
    }
    let appended =
        i128::try_from(appended_clauses).map_err(|_| ResearchPlannerError::ContextPlanLimit)?;
    let gross = appended
        .checked_mul(400_000)
        .and_then(|value| value.checked_mul(i128::from(estimate.historical_success_lower_ppm)))
        .and_then(|value| value.checked_div(i128::from(krw_agent_planning::PPM)))
        .ok_or(ResearchPlannerError::SelectionInvariant)?;
    let result_kib = u64::from(estimate.expected_result_bytes).div_ceil(1024);
    let cost = i128::from(estimate.expected_duplicate_ppm / 2)
        + i128::from(estimate.failure_risk_upper_ppm / 2)
        + i128::from(estimate.expected_latency_ms) * 4
        + i128::from(estimate.expected_tokens) * 2
        + i128::from(estimate.expected_tool_cost_micros)
        + i128::from(result_kib) * 20;
    i64::try_from(gross - cost).map_err(|_| ResearchPlannerError::SelectionInvariant)
}

fn validate_projection(
    projection: &ResearchPlanningProjection,
) -> Result<(), ResearchPlannerError> {
    projection.graph.validate_recovered()?;
    if projection.clauses.is_empty()
        || projection.clauses.len() > 128
        || projection.missing_parts.len() > 256
        || projection.recommended_actions.len() > 256
        || projection.exact_precise_query_candidates.len() > 12
        || projection.orientation_vocabulary.len() > MAX_ORIENTATION_VOCABULARY
    {
        return Err(ResearchPlannerError::InvalidCheckpoint);
    }
    let mut clause_ids = BTreeSet::new();
    for clause in &projection.clauses {
        if clause.clause_id.is_empty()
            || clause.clause_id.len() > 64
            || clause.retrieval_query.is_empty()
            || clause.retrieval_query.len() > 1_000
            || !clause_ids.insert(clause.clause_id.as_str())
            || projection
                .graph
                .goal(&format!("clause:{}", clause.clause_id))
                .is_none()
        {
            return Err(ResearchPlannerError::InvalidCheckpoint);
        }
    }
    for missing in &projection.missing_parts {
        if missing.code.is_empty()
            || missing.code.len() > 128
            || missing.detail.is_empty()
            || missing.detail.len() > 2_000
            || missing
                .clause_id
                .as_deref()
                .is_some_and(|id| !clause_ids.contains(id))
            || missing
                .ticker
                .as_ref()
                .is_some_and(|ticker| ticker.len() > 32)
        {
            return Err(ResearchPlannerError::InvalidCheckpoint);
        }
    }
    for action in &projection.recommended_actions {
        if action.tool.is_empty()
            || action.tool.len() > 128
            || action.reason.is_empty()
            || action.reason.len() > 2_000
            || action
                .clause_id
                .as_deref()
                .is_some_and(|id| !clause_ids.contains(id))
            || action
                .object_id
                .as_ref()
                .is_some_and(|object_id| object_id.is_empty() || object_id.len() > 512)
            || action
                .ticker
                .as_ref()
                .is_some_and(|ticker| ticker.len() > 32)
        {
            return Err(ResearchPlannerError::InvalidCheckpoint);
        }
    }
    let mut exact_candidates = BTreeSet::new();
    for candidate in &projection.exact_precise_query_candidates {
        if candidate.clause_id.is_empty()
            || candidate.clause_id.len() > 64
            || !clause_ids.contains(candidate.clause_id.as_str())
            || candidate.ticker.is_empty()
            || candidate.ticker.len() > 32
            || candidate.topic.is_empty()
            || candidate.topic.len() > 512
            // `compact` is the advertised default depth of both candidate
            // emitters (the adapter projection and the run-engine gap hint);
            // the analyst may raise the dispatched read to `full`. Any other
            // value is not a depth this contract ever publishes.
            || !matches!(candidate.response_detail.as_str(), "full" | "compact")
            || candidate.limit == 0
            || candidate.limit > 50
            || candidate.document_types.len() > 16
            || candidate.periods.len() > 16
            || candidate.object_types.len() > 16
            || candidate.document_types.iter().any(|value| {
                value.is_empty() || value.len() > 128 || value.chars().any(char::is_control)
            })
            || candidate.periods.iter().any(|value| {
                value.is_empty() || value.len() > 128 || value.chars().any(char::is_control)
            })
            || candidate.object_types.iter().any(|value| {
                value.is_empty() || value.len() > 128 || value.chars().any(char::is_control)
            })
            || !exact_candidates.insert((
                candidate.clause_id.as_str(),
                candidate.ticker.as_str(),
                candidate.topic.as_str(),
            ))
        {
            return Err(ResearchPlannerError::InvalidCheckpoint);
        }
    }
    // The orientation vocabulary is advisory only, but a restored checkpoint
    // is still a trust boundary: it must carry the same bounded, duplicate-free
    // shape the adapter derives from the ledger.
    let mut orientation_terms = BTreeSet::new();
    for term in &projection.orientation_vocabulary {
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
            || !orientation_terms.insert((
                term.term.as_str(),
                term.document_type.as_deref(),
                term.period.as_deref(),
            ))
        {
            return Err(ResearchPlannerError::InvalidCheckpoint);
        }
    }
    Ok(())
}

fn map_goals(
    projection: &ResearchPlanningProjection,
    intent_projection: Option<&IntentPlanningProjection>,
    proposal: &CandidateProposal,
) -> Option<(Vec<String>, BTreeSet<ActionBenefit>)> {
    let objective_graph = intent_projection.map_or(&projection.graph, |intent| &intent.graph);
    let frontier = objective_graph
        .frontier()
        .into_iter()
        .map(|goal| goal.goal_id.as_str())
        .collect::<BTreeSet<_>>();
    if frontier.is_empty() {
        return None;
    }
    let mut benefits = BTreeSet::new();
    let clause_id = if proposal.kind == ResearchActionKind::Trace {
        let object_id = proposal
            .arguments
            .get("object_id")
            .and_then(Value::as_str)?;
        let ticker = proposal.arguments.get("ticker").and_then(Value::as_str);
        let recommendation = projection
            .recommended_actions
            .iter()
            .find(|recommendation| {
                normalize_tool(&recommendation.tool) == "trace"
                    && recommendation.object_id.as_deref() == Some(object_id)
                    && recommendation.ticker.as_deref() == ticker
            })?;
        benefits.insert(ActionBenefit::ServerRecommended);
        benefits.insert(ActionBenefit::RaisesDirectnessCeiling);
        recommendation.clause_id.as_deref()?
    } else if proposal.kind == ResearchActionKind::TargetedQuery {
        if proposal.capability_id.ends_with("chain") {
            // A chain traversal starts from one observed object, not from a
            // free-text topic. Admit it only when the trusted ontology result
            // recommended this exact `(ticker, object_id)` mechanism lookup.
            // This keeps chain useful for causal analysis without letting the
            // model invent a graph root or turn every evidence item into work.
            let object_id = proposal
                .arguments
                .get("object_id")
                .and_then(Value::as_str)?;
            let ticker = proposal.arguments.get("ticker").and_then(Value::as_str);
            let recommendation = projection
                .recommended_actions
                .iter()
                .find(|recommendation| {
                    normalize_tool(&recommendation.tool) == "chain"
                        && recommendation.object_id.as_deref() == Some(object_id)
                        && recommendation.ticker.as_deref() == ticker
                })?;
            benefits.insert(ActionBenefit::ServerRecommended);
            recommendation.clause_id.as_deref()?
        } else {
            let topic = proposal.arguments.get("topic").and_then(Value::as_str)?;
            projection
                .clauses
                .iter()
                .find(|clause| clause.retrieval_query == topic)
                .map(|clause| clause.clause_id.as_str())?
        }
    } else if proposal.kind == ResearchActionKind::Observation {
        // The observation lane is ticker-scoped by construction: the openbb
        // model requests carry a trusted ticker, and the only honest clause
        // mapping is an exact query candidate scoped to the same ticker.
        // The model proposed this read autonomously and the capability
        // carries a one-visit state bound, so admit the first candidate for
        // the ticker that still serves a frontier goal. Live plans map only
        // a subset of clauses in the intent receipt, so when the intent
        // mapping yields nothing the projection graph's `clause:*` goals are
        // the honest fallback — without it the model's only market-data
        // request is dropped as unmapped (production GLM DCF run: an
        // `openbb.balance_statement` proposal with the filing query already
        // completed was discarded and the composer had to hedge the answer).
        let ticker = proposal.arguments.get("ticker").and_then(Value::as_str)?;
        let mut goal_ids = projection
            .exact_precise_query_candidates
            .iter()
            .filter(|candidate| candidate.ticker == ticker)
            .find_map(|candidate| {
                resolve_clause_frontier_goals(
                    projection,
                    intent_projection,
                    &frontier,
                    candidate.clause_id.as_str(),
                    true,
                    &mut benefits,
                )
            });
        if goal_ids.is_none() {
            // P7 (2026-09-03, live VIPS runs): a trusted ticker the ontology
            // catalog does not cover has no filing-evidence lane at all —
            // the strict mapping above only serves calculation goals, so a
            // qualitative frontier (e.g. "list this issuer's recent
            // filings") discarded the only remaining research surface as
            // unmapped. When the server reports the ticker unavailable and
            // the plan still expects it (an exact candidate exists), the
            // observation plane is the only lane left: admit the read
            // against every still-open goal. The advisory-only result, the
            // one-visit state bound, and the ticker filter keep it bounded.
            let ticker_unavailable = projection
                .retrieval_status
                .warnings
                .iter()
                .any(|warning| warning == "ticker_not_available")
                && projection
                    .exact_precise_query_candidates
                    .iter()
                    .any(|candidate| candidate.ticker == ticker);
            if ticker_unavailable {
                goal_ids =
                    Some(frontier.iter().map(|goal_id| (*goal_id).to_owned()).collect());
            }
        }
        if goal_ids.is_none() && projection.exact_precise_query_candidates.is_empty() {
            // Free-door universe plans (question_only runs) carry no
            // ticker-scoped precise candidates and never raise
            // ticker_not_available for an issuer the question itself names,
            // so the P7 lane above cannot fire — and without this fallback
            // the model's only market-plane read for the subject company is
            // discarded as unmapped (live 2026-09-04, run_745d938b: every
            // openbb PLTR read came back proposal_unmapped and the answer
            // had to refuse all figures). The MarketPlane scope binding
            // still admits only canonical tickers, and the one-visit state
            // bounds plus the action_limits caps keep the reads few.
            goal_ids = Some(frontier.iter().map(|goal_id| (*goal_id).to_owned()).collect());
        }
        let mut goal_ids = goal_ids?;
        goal_ids.sort();
        goal_ids.dedup();
        return Some((goal_ids, benefits));
    } else {
        return None;
    };
    let mut goal_ids = resolve_clause_frontier_goals(
        projection,
        intent_projection,
        &frontier,
        clause_id,
        false,
        &mut benefits,
    )?;
    goal_ids.sort();
    (!goal_ids.is_empty()).then_some((goal_ids, benefits))
}

/// Resolve one clause to the frontier goals an action would serve. With an
/// intent projection the mapping is intent-only (the strict, pre-existing
/// contract for filing reads); `allow_projection_fallback` additionally
/// falls back to the projection graph's `clause:*` goals when the intent
/// receipt does not cover the clause. `benefits` accumulates
/// `FillsCalculationCoverage` when any served goal requires a calculation.
fn resolve_clause_frontier_goals(
    projection: &ResearchPlanningProjection,
    intent_projection: Option<&IntentPlanningProjection>,
    frontier: &BTreeSet<&str>,
    clause_id: &str,
    allow_projection_fallback: bool,
    benefits: &mut BTreeSet<ActionBenefit>,
) -> Option<Vec<String>> {
    let mut goal_ids = Vec::new();
    if let Some(intent) = intent_projection {
        goal_ids = intent
            .clause_goal_ids
            .get(clause_id)
            .into_iter()
            .flatten()
            .filter(|goal_id| frontier.contains(goal_id.as_str()))
            .filter_map(|goal_id| intent.graph.goal(goal_id))
            .map(|goal| {
                if goal.calculation_required {
                    benefits.insert(ActionBenefit::FillsCalculationCoverage);
                }
                goal.goal_id.clone()
            })
            .collect::<Vec<_>>();
    }
    if goal_ids.is_empty() && (intent_projection.is_none() || allow_projection_fallback) {
        if let Some(intent) = intent_projection {
            // Under an intent projection the scoreable goal space is the
            // intent graph's own IDs, so a projection `clause:*` fallback
            // could never survive `score_actions`. The honest intent-space
            // fallback for a ticker-scoped observation is the intent's open
            // calculation goals: market and statement reads are calculation
            // inputs, not filing substitutes. Qualitative-only frontiers
            // keep the observation unmapped.
            goal_ids = intent
                .graph
                .frontier()
                .into_iter()
                .filter(|goal| goal.calculation_required)
                .map(|goal| {
                    benefits.insert(ActionBenefit::FillsCalculationCoverage);
                    goal.goal_id.clone()
                })
                .collect::<Vec<_>>();
        } else {
            let clause_goal = format!("clause:{clause_id}");
            goal_ids = projection
                .graph
                .goals()
                .filter(|goal| {
                    frontier.contains(goal.goal_id.as_str())
                        && (goal.goal_id == clause_goal
                            || goal
                                .dependencies
                                .iter()
                                .any(|dependency| dependency == &clause_goal))
                })
                .map(|goal| {
                    if goal.calculation_required {
                        benefits.insert(ActionBenefit::FillsCalculationCoverage);
                    }
                    goal.goal_id.clone()
                })
                .collect::<Vec<_>>();
        }
    }
    (!goal_ids.is_empty()).then_some(goal_ids)
}

fn merge_projection(
    current: &mut ResearchPlanningProjection,
    observed: ResearchPlanningProjection,
) -> Result<(), ResearchPlannerError> {
    let current_goals = current
        .graph
        .goals()
        .map(|goal| (goal.goal_id.clone(), goal.clone()))
        .collect::<BTreeMap<_, _>>();
    let observed_goals = observed
        .graph
        .goals()
        .map(|goal| (goal.goal_id.clone(), goal.clone()))
        .collect::<BTreeMap<_, _>>();
    if current_goals
        .keys()
        .any(|id| !observed_goals.contains_key(id))
    {
        return Err(ResearchPlannerError::GoalDefinitionDrift);
    }
    let mut additions = Vec::new();
    let mut progress = Vec::new();
    for (id, next) in &observed_goals {
        let Some(previous) = current_goals.get(id) else {
            additions.push(next.clone());
            continue;
        };
        if !same_goal_definition(previous, next) {
            return Err(ResearchPlannerError::GoalDefinitionDrift);
        }
        if let Some(update) = monotonic_projection_progress(id, previous, next) {
            progress.push(update);
        }
    }
    if !additions.is_empty() || !progress.is_empty() {
        current.graph.apply(GoalDelta {
            expected_version: current.graph.version(),
            additions,
            progress,
        })?;
    }
    merge_clause_bindings(&mut current.clauses, &observed.clauses)?;
    current.missing_parts = observed.missing_parts;
    current.recommended_actions = observed.recommended_actions;
    current.exact_precise_query_candidates = observed.exact_precise_query_candidates;
    // The orientation vocabulary is advisory wording distilled from the
    // committed ledger, which only grows. A later bounded research snapshot
    // that was derived without orientation records must not erase terms a
    // restored checkpoint or an earlier commit already carried, so join the
    // two deduplicated sets instead of replacing them.
    let mut orientation_vocabulary: BTreeSet<_> =
        current.orientation_vocabulary.drain(..).collect();
    orientation_vocabulary.extend(observed.orientation_vocabulary);
    current.orientation_vocabulary = orientation_vocabulary
        .into_iter()
        .take(MAX_ORIENTATION_VOCABULARY)
        .collect();
    // `ResearchState` is a bounded retrieval snapshot. A later context query
    // may legitimately contain fewer rows than an earlier one, while its
    // diagnostics (pagination, current-document anchors, input warnings) are
    // authoritative for that latest query.
    let preserved_supplemental_warnings = current
        .retrieval_status
        .warnings
        .iter()
        .filter(|warning| warning.starts_with("supplemental_"))
        .cloned()
        .collect::<BTreeSet<_>>();
    let preserved_supplemental_reads = current.retrieval_status.supplemental_reads.clone();
    current.retrieval_status = observed.retrieval_status;
    let mut warnings = current
        .retrieval_status
        .warnings
        .iter()
        .filter(|warning| !warning.is_empty())
        .cloned()
        .collect::<BTreeSet<_>>();
    warnings.extend(preserved_supplemental_warnings);
    current.retrieval_status.warnings = warnings
        .into_iter()
        .take(MAX_RETRIEVAL_STATUS_WARNINGS)
        .collect();
    let observed_supplemental_reads = current.retrieval_status.supplemental_reads.clone();
    current.retrieval_status.supplemental_reads = preserved_supplemental_reads;
    current
        .retrieval_status
        .supplemental_reads
        .extend(observed_supplemental_reads);
    if current.retrieval_status.supplemental_reads.len() > MAX_SUPPLEMENTAL_READ_STATUSES {
        let excess =
            current.retrieval_status.supplemental_reads.len() - MAX_SUPPLEMENTAL_READ_STATUSES;
        current.retrieval_status.supplemental_reads.drain(..excess);
    }
    Ok(())
}

/// A `ResearchState` is a bounded view of a release-pinned corpus, not a
/// replacement for evidence already committed during this run. Replanning can
/// therefore omit a previously covered calculation simply because a different
/// clause consumed the response budget. Treating that omission as a progress
/// regression aborts valid research and makes the model report an absence that
/// was never observed.
///
/// Goal definitions remain immutable, while goal progress is joined
/// monotonically. A later snapshot may advance progress or attach additional
/// provenance at the same level; it can never retract known coverage.
fn monotonic_projection_progress(
    goal_id: &str,
    previous: &EvidenceGoal,
    observed: &EvidenceGoal,
) -> Option<GoalProgressUpdate> {
    // A lower coverage number in a later bounded snapshot means "not returned
    // in this page", not that the previously returned evidence disappeared.
    if observed.coverage_ppm < previous.coverage_ppm
        || (previous.status == GoalStatus::Satisfied && observed.status != GoalStatus::Satisfied)
        || previous.status == GoalStatus::Blocked
    {
        return None;
    }

    // `EvidenceGoalGraph` intentionally does not accept an
    // `Unresolved -> Unresolved` update. Such an observation cannot advance
    // trusted coverage, even if the provider happened to attach a weak ID.
    if observed.status == GoalStatus::Unresolved {
        return None;
    }

    let new_evidence = observed
        .evidence_ids
        .iter()
        .filter(|id| !previous.evidence_ids.contains(id))
        .cloned()
        .collect::<Vec<_>>();
    let new_calculations = observed
        .calculation_ids
        .iter()
        .filter(|id| !previous.calculation_ids.contains(id))
        .cloned()
        .collect::<Vec<_>>();
    if previous.status == observed.status
        && previous.coverage_ppm == observed.coverage_ppm
        && new_evidence.is_empty()
        && new_calculations.is_empty()
    {
        return None;
    }

    Some(GoalProgressUpdate {
        goal_id: goal_id.to_owned(),
        expected_status: previous.status,
        status: observed.status,
        coverage_ppm: observed.coverage_ppm,
        evidence_ids: new_evidence,
        calculation_ids: new_calculations,
    })
}

fn same_goal_definition(left: &EvidenceGoal, right: &EvidenceGoal) -> bool {
    left.goal_id == right.goal_id
        && left.required == right.required
        && left.weight == right.weight
        && left.dependencies == right.dependencies
        && left.directness == right.directness
        && left.calculation_required == right.calculation_required
}

fn merge_clause_bindings(
    current: &mut Vec<ClausePlanningBinding>,
    observed: &[ClausePlanningBinding],
) -> Result<(), ResearchPlannerError> {
    let observed_by_id = observed
        .iter()
        .map(|clause| (clause.clause_id.as_str(), clause))
        .collect::<BTreeMap<_, _>>();
    for existing in current.iter() {
        if observed_by_id
            .get(existing.clause_id.as_str())
            .is_none_or(|next| next.retrieval_query != existing.retrieval_query)
        {
            return Err(ResearchPlannerError::ClauseDefinitionDrift);
        }
    }
    let known = current
        .iter()
        .map(|clause| clause.clause_id.clone())
        .collect::<BTreeSet<_>>();
    current.extend(
        observed
            .iter()
            .filter(|clause| !known.contains(&clause.clause_id))
            .cloned(),
    );
    Ok(())
}

fn validate_proposal_ids(proposals: &[CandidateProposal]) -> Result<(), ResearchPlannerError> {
    let mut ids = BTreeSet::new();
    for proposal in proposals {
        if proposal.proposal_id.is_empty()
            || proposal.proposal_id.len() > 256
            || !proposal.arguments.is_object()
            || !ids.insert(proposal.proposal_id.as_str())
        {
            return Err(ResearchPlannerError::InvalidProposal);
        }
    }
    Ok(())
}

fn planner_action_id(fingerprint: &ContentHash) -> String {
    format!(
        "candidate:{}",
        fingerprint.as_str().trim_start_matches("sha256:")
    )
}

fn normalize_tool(tool: &str) -> &str {
    if tool.ends_with("trace") {
        "trace"
    } else if tool.ends_with("query_context") || tool.ends_with("query_context_universe") {
        "query_context"
    } else if tool.ends_with("query") || tool.ends_with("query_universe") {
        "query"
    } else if tool.ends_with("chain") {
        "chain"
    } else {
        "unknown"
    }
}

/// A content-free location for a failed `SearchPlan` normalization proof.
///
/// The MCP response and the dispatched plan can contain user research terms,
/// so this type deliberately carries only fixed contract field names.  It is
/// safe to use in durable diagnostics and lets cross-language conformance
/// failures be repaired at the ABI boundary rather than by inspecting a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormalizedPlanMismatchKind {
    DispatchedPayloadTooLarge,
    DispatchedPlanShape,
    ObservedPlanShape,
    DispatchedPlanKeys,
    ObservedPlanKeys,
    PlanField(&'static str),
    ClauseCount,
    DispatchedClauseShape,
    ObservedClauseShape,
    DispatchedClauseKeys,
    ObservedClauseKeys,
    ClauseField(&'static str),
}

impl NormalizedPlanMismatchKind {
    /// A stable, content-free diagnostic category.
    pub const fn diagnostic_kind(self) -> &'static str {
        match self {
            Self::DispatchedPayloadTooLarge => "research_planner_normalized_plan_dispatched_size",
            Self::DispatchedPlanShape => "research_planner_normalized_plan_dispatched_shape",
            Self::ObservedPlanShape => "research_planner_normalized_plan_observed_shape",
            Self::DispatchedPlanKeys => "research_planner_normalized_plan_dispatched_keys",
            Self::ObservedPlanKeys => "research_planner_normalized_plan_observed_keys",
            Self::PlanField(_) => "research_planner_normalized_plan_field",
            Self::ClauseCount => "research_planner_normalized_plan_clause_count",
            Self::DispatchedClauseShape => {
                "research_planner_normalized_plan_dispatched_clause_shape"
            }
            Self::ObservedClauseShape => "research_planner_normalized_plan_observed_clause_shape",
            Self::DispatchedClauseKeys => "research_planner_normalized_plan_dispatched_clause_keys",
            Self::ObservedClauseKeys => "research_planner_normalized_plan_observed_clause_keys",
            Self::ClauseField(_) => "research_planner_normalized_plan_clause_field",
        }
    }

    /// The field identifier is immutable program metadata, never run content.
    pub const fn diagnostic_identifier(self) -> &'static str {
        match self {
            Self::PlanField(field) | Self::ClauseField(field) => field,
            Self::DispatchedPayloadTooLarge => "dispatched_payload_too_large",
            Self::DispatchedPlanShape => "dispatched_plan_shape",
            Self::ObservedPlanShape => "observed_plan_shape",
            Self::DispatchedPlanKeys => "dispatched_plan_keys",
            Self::ObservedPlanKeys => "observed_plan_keys",
            Self::ClauseCount => "clause_count",
            Self::DispatchedClauseShape => "dispatched_clause_shape",
            Self::ObservedClauseShape => "observed_clause_shape",
            Self::DispatchedClauseKeys => "dispatched_clause_keys",
            Self::ObservedClauseKeys => "observed_clause_keys",
        }
    }
}

#[derive(Debug, Error)]
pub enum ResearchPlannerError {
    #[error("candidate proposal count is outside bounds")]
    ProposalLimit,
    #[error("candidate proposal is malformed or duplicated")]
    InvalidProposal,
    #[error("initial research requires exactly one query-context proposal")]
    InitialContextRequired,
    #[error("the one bootstrap query-context action was already completed")]
    InitialContextAlreadyCompleted,
    #[error("the server already rejected this exact query-context plan")]
    RejectedContextAlreadyAttempted,
    #[error("rejected query-context fingerprint history exceeds its fixed bound")]
    RejectedContextLimit,
    #[error("a query-context fingerprint cannot be both rejected and completed")]
    RejectedContextConflict,
    #[error("query-context replan is not a strict append-only plan in the trusted scope")]
    ContextPlanDrift,
    #[error("server-normalized SearchPlan is not semantically linked to the dispatched plan")]
    NormalizedPlanMismatch(NormalizedPlanMismatchKind),
    #[error("query-context plan or plan history exceeds its fixed bound")]
    ContextPlanLimit,
    #[error("trusted runtime candidate estimate is invalid or disables a cost signal")]
    InvalidEstimate,
    #[error("completed action fingerprint limit exceeded")]
    CompletedFingerprintLimit,
    #[error("canonical evidence goal definition drifted during replanning")]
    GoalDefinitionDrift,
    #[error("canonical clause definition drifted during replanning")]
    ClauseDefinitionDrift,
    #[error("compiled ResearchIntent receipt is malformed or not fresh")]
    IntentReceipt,
    #[error("a later ResearchIntent used a different trusted question anchor")]
    IntentAnchorMismatch,
    #[error("a later ResearchIntent attempted to rewrite an existing evidence goal")]
    IntentGoalDefinitionDrift,
    #[error("a later ResearchIntent attempted to rewrite a clause-to-goal binding")]
    IntentClauseBindingDrift,
    #[error("canonical coverage attempted to regress a user-linked evidence goal")]
    IntentGoalProgressDrift,
    #[error("canonical coverage marked a user-linked goal without required provenance")]
    IntentCoverageProvenanceMissing,
    #[error("planner selected an action that is absent from the proposal map")]
    SelectionInvariant,
    #[error("research planner checkpoint schema version is unsupported")]
    CheckpointVersion,
    #[error("research planner checkpoint uses a different scoring policy")]
    ScoringPolicyMismatch,
    #[error("research planner checkpoint violates bounded invariants")]
    InvalidCheckpoint,
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Adapter(#[from] krw_ontology_adapter::AdapterError),
    #[error(transparent)]
    Planning(#[from] krw_agent_planning::PlanningError),
}

#[cfg(test)]
mod tests {
    use krw_agent_planning::{ActionConcurrency, ActionEffect, AuthIsolation};
    use krw_agent_protocol::RunContextV1;
    use krw_ontology_adapter::{
        CalculationCoverage, ClauseCoverage, MissingPart, RecommendedAction,
        SupplementalReadKind, parse_research_state,
    };

    use super::*;

    fn fixture() -> ResearchStateV2 {
        parse_research_state(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/vertical-slice/v1/mcp/research-state-answerable.json"
        )))
        .unwrap()
    }

    fn partial_fixture() -> ResearchStateV2 {
        let mut state = fixture();
        let coverage = &mut state.clause_coverage[0];
        coverage.status = "partial".into();
        coverage.strong_claim_ready = false;
        coverage.covered_tickers = vec!["VG".into()];
        coverage.missing_tickers = vec!["XOM".into()];
        state.answerability.status = "partial".into();
        state.answerability.strong_claim_allowed = false;
        state.recommended_actions = vec![RecommendedAction {
            tool: "krw_ontology_trace".into(),
            reason: "verify filing lineage".into(),
            object_id: Some("claim:vg:cash-generation:2025".into()),
            clause_id: Some("cash_generation".into()),
            ticker: Some("VG".into()),
        }];
        state
    }

    /// Partial coverage plus a server-reported missing part for the
    /// `cash_generation` clause — the realistic shape an observation lane
    /// sees: the exact query candidates exist, so a ticker-scoped openbb
    /// read has an in-scope clause to inform.
    fn observation_fixture() -> ResearchStateV2 {
        let mut state = partial_fixture();
        state.missing_parts.push(MissingPart {
            code: "clause_not_covered".into(),
            detail: "cash generation evidence incomplete".into(),
            clause_id: Some("cash_generation".into()),
            ticker: Some("VG".into()),
        });
        state
    }

    fn fixture_proposal() -> Value {
        serde_json::json!({
            "intent":"company_research",
            "answer_scope":"direct",
            "uncertainty":"low",
            "document_types":["10-K"],
            "periods":[],
            "objectives":[{
                "priority":"required",
                "alternatives":[{"terms":["VG", "cash generation"]}],
                "directness":"direct_required",
                "object_types":[],
                "goal": {
                    "kind":"qualitative_evidence",
                    "concepts":["cash generation"],
                    "predicates":[]
                }
            }]
        })
    }

    fn fixture_intent_receipt() -> ResearchIntentReceipt {
        let context = RunContextV1::CompanyTickerSet {
            tickers: vec!["VG".into()],
        };
        compile_research_proposal(
            &fixture_proposal(),
            InitialPlanScope {
                question: "Does VG generate cash according to its filing?",
                context: &context,
                derived_tickers: None,
                max_discovery_tickers: 1,
                prior_plan: None,
                requester: ResearchPlanRequester::CompanyQueryContext,
            },
        )
        .unwrap()
        .receipt
    }

    /// A metric-change intent receipt plus a server state that echoes the
    /// receipt's compiled plan with PARTIAL clause coverage, so the intent
    /// graph keeps an open calculation-required goal on the frontier and the
    /// echoed clause is an exact-candidate source for the VG ticker.
    fn calculation_intent_fixture() -> (ResearchIntentReceipt, ResearchStateV2) {
        let proposal = serde_json::json!({
            "intent":"company_research",
            "answer_scope":"direct",
            "uncertainty":"low",
            "document_types":["10-K"],
            "periods":[],
            "objectives":[{
                "priority":"required",
                "alternatives":[{"terms":["VG", "operating cash flow"]}],
                "directness":"direct_required",
                "object_types":[],
                "goal": {
                    "kind":"metric_change",
                    "metric":"operating_cash_flow",
                    "metric_dimensions":[],
                    "change":"growth_rate",
                    "window":"year_over_year"
                }
            }]
        });
        let context = RunContextV1::CompanyTickerSet {
            tickers: vec!["VG".into()],
        };
        let compiled = compile_research_proposal(
            &proposal,
            InitialPlanScope {
                question: "How fast did VG operating cash flow grow?",
                context: &context,
                derived_tickers: None,
                max_discovery_tickers: 1,
                prior_plan: None,
                requester: ResearchPlanRequester::CompanyQueryContext,
            },
        )
        .unwrap();
        let clause_id = compiled.search_plan["clauses"][0]["clause_id"]
            .as_str()
            .expect("compiled clause id")
            .to_owned();
        let mut value: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/vertical-slice/v1/mcp/research-state-answerable.json"
        )))
        .unwrap();
        value["plan"] = compiled.search_plan.clone();
        replace_exact_clause_reference(&mut value, "cash_generation", &clause_id);
        let mut state: ResearchStateV2 = serde_json::from_value(value).unwrap();
        state.clause_coverage[0].status = "partial".into();
        state.clause_coverage[0].strong_claim_ready = false;
        state.answerability.status = "partial".into();
        state.answerability.strong_claim_allowed = false;
        state.missing_parts.push(MissingPart {
            code: "clause_not_covered".into(),
            detail: "calculation input incomplete".into(),
            clause_id: Some(clause_id),
            ticker: Some("VG".into()),
        });
        (compiled.receipt, state)
    }

    /// Existing ontology fixtures carry semantic clause names. The provider
    /// ABI no longer owns those names, so tests materialize the same fixture
    /// against the kernel-generated plan and rewrite only exact clause
    /// references. This mirrors an actual `query_context` response, whose
    /// normalized plan echoes the dispatched kernel plan.
    fn fixture_for_proposal() -> ResearchStateV2 {
        let context = RunContextV1::CompanyTickerSet {
            tickers: vec!["VG".into()],
        };
        let plan = compile_research_proposal(
            &fixture_proposal(),
            InitialPlanScope {
                question: "Does VG generate cash according to its filing?",
                context: &context,
                derived_tickers: None,
                max_discovery_tickers: 1,
                prior_plan: None,
                requester: ResearchPlanRequester::CompanyQueryContext,
            },
        )
        .unwrap()
        .search_plan;
        let clause_id = plan["clauses"][0]["clause_id"]
            .as_str()
            .expect("compiled clause id")
            .to_owned();
        let mut value: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/vertical-slice/v1/mcp/research-state-answerable.json"
        )))
        .unwrap();
        value["plan"] = plan;
        replace_exact_clause_reference(&mut value, "cash_generation", &clause_id);
        serde_json::from_value(value).unwrap()
    }

    fn only_goal_id(receipt: &ResearchIntentReceipt) -> String {
        receipt
            .clause_goal_ids
            .values()
            .next()
            .and_then(|goal_ids| goal_ids.first())
            .cloned()
            .expect("single-objective receipt")
    }

    fn replace_exact_clause_reference(value: &mut Value, from: &str, to: &str) {
        match value {
            Value::String(current) if current == from => *current = to.into(),
            Value::Array(values) => values
                .iter_mut()
                .for_each(|value| replace_exact_clause_reference(value, from, to)),
            Value::Object(values) => values
                .values_mut()
                .for_each(|value| replace_exact_clause_reference(value, from, to)),
            _ => {}
        }
    }

    fn estimate(tool_cost: u32) -> CandidateEstimate {
        CandidateEstimate {
            historical_success_lower_ppm: 800_000,
            expected_duplicate_ppm: 0,
            failure_risk_upper_ppm: 20_000,
            expected_latency_ms: 25,
            expected_tokens: 50,
            expected_tool_cost_micros: tool_cost,
            expected_result_bytes: 4_096,
        }
    }

    fn proposal(
        id: &str,
        capability_id: &str,
        arguments: Value,
        tool_cost: u32,
    ) -> CandidateProposal {
        CandidateProposal {
            proposal_id: id.into(),
            capability_id: capability_id.into(),
            kind: if capability_id.ends_with("query_context") {
                ResearchActionKind::QueryContext
            } else if capability_id.ends_with("trace") {
                ResearchActionKind::Trace
            } else if capability_id.starts_with("openbb.") {
                ResearchActionKind::Observation
            } else {
                ResearchActionKind::TargetedQuery
            },
            fingerprint: ContentHash::sha256(format!("{capability_id}:{arguments}")),
            arguments,
            estimate: estimate(tool_cost),
            effect: ActionEffect::ReadOnly,
            concurrency: ActionConcurrency::Serial,
            auth_isolation: AuthIsolation::Isolated,
            conflict_keys: vec!["ticker:VG".into()],
        }
    }

    #[test]
    fn bootstrap_requires_one_query_context_action() {
        let planner = ResearchPlanner::default();
        let context = proposal(
            "context",
            "ontology.query_context",
            serde_json::json!({"question": "q"}),
            0,
        );
        assert_eq!(
            planner.select(std::slice::from_ref(&context)).unwrap(),
            PlannerDecision::Execute {
                proposal_id: "context".into(),
                score: None,
                reason: SelectionReason::InitialContext,
                evaluated: 1,
            }
        );
        let query = proposal(
            "query",
            "ontology.query",
            serde_json::json!({"topic": "VG cash generation"}),
            0,
        );
        assert!(matches!(
            planner.select(&[context, query]),
            Err(ResearchPlannerError::InitialContextRequired)
        ));
    }

    #[test]
    fn answerable_context_closes_the_trusted_research_frontier() {
        let mut planner = ResearchPlanner::default();
        planner
            .ingest_research_state_for_intent(
                &fixture_for_proposal(),
                &fixture_intent_receipt(),
                &[],
            )
            .unwrap();

        assert_eq!(planner.objective_frontier_is_exhausted(), Some(true));
    }

    #[test]
    fn partial_context_keeps_the_trusted_research_frontier_open() {
        let mut planner = ResearchPlanner::default();
        planner
            .ingest_research_state(&partial_fixture(), &[])
            .unwrap();

        assert_eq!(planner.objective_frontier_is_exhausted(), Some(false));
    }

    #[test]
    fn trusted_server_recommendation_beats_costly_broad_query() {
        let mut planner = ResearchPlanner::default();
        planner
            .ingest_research_state(&partial_fixture(), &[])
            .unwrap();
        let broad = proposal(
            "broad",
            "ontology.query",
            serde_json::json!({"topic": "VG cash generation"}),
            900_000,
        );
        let trace = proposal(
            "trace",
            "ontology.trace",
            serde_json::json!({
                "object_id": "claim:vg:cash-generation:2025",
                "ticker": "VG"
            }),
            0,
        );
        let decision = planner.select(&[broad, trace.clone()]).unwrap();
        assert!(matches!(
            decision,
            PlannerDecision::Execute {
                proposal_id,
                score: Some(score),
                reason: SelectionReason::PositiveExpectedValue,
                evaluated: 2,
            } if proposal_id == "trace" && score > 0
        ));

        planner.record_completed(trace.fingerprint.clone()).unwrap();
        assert!(
            !planner.projection().unwrap().graph.frontier().is_empty(),
            "a supplemental tool result is not itself a canonical ResearchState progress update"
        );
        assert_eq!(
            planner.select(std::slice::from_ref(&trace)).unwrap(),
            PlannerDecision::NoPositiveValue {
                evaluated: 1,
                reason: NoPositiveReason::DuplicateCompleted,
            }
        );

        let wrong_topic = proposal(
            "wrong-topic",
            "ontology.query",
            serde_json::json!({"topic": "unrelated acquisition rumor", "ticker": "VG"}),
            0,
        );
        assert_eq!(
            planner.select(&[wrong_topic]).unwrap(),
            PlannerDecision::NoPositiveValue {
                evaluated: 1,
                reason: NoPositiveReason::ProposalUnmapped,
            }
        );
        let wrong_object = proposal(
            "wrong-object",
            "ontology.trace",
            serde_json::json!({"object_id": "claim:vg:other", "ticker": "VG"}),
            0,
        );
        assert_eq!(
            planner.select(&[wrong_object]).unwrap(),
            PlannerDecision::NoPositiveValue {
                evaluated: 1,
                reason: NoPositiveReason::ProposalUnmapped,
            }
        );
    }

    #[test]
    fn server_recommended_chain_is_eligible_for_causal_follow_up() {
        let mut state = partial_fixture();
        state.recommended_actions[0].tool = "krw_ontology_chain".into();
        state.recommended_actions[0].reason = "expand causal mechanism".into();

        let mut planner = ResearchPlanner::default();
        planner.ingest_research_state(&state, &[]).unwrap();
        let chain = proposal(
            "chain",
            "ontology.chain",
            serde_json::json!({
                "object_id": "claim:vg:cash-generation:2025",
                "ticker": "VG"
            }),
            0,
        );

        assert!(matches!(
            planner.select(std::slice::from_ref(&chain)).unwrap(),
            PlannerDecision::Execute {
                proposal_id,
                score: Some(score),
                reason: SelectionReason::PositiveExpectedValue,
                evaluated: 1,
            } if proposal_id == "chain" && score > 0
        ));
    }

    #[test]
    fn observation_maps_when_a_frontier_clause_shares_the_trusted_ticker() {
        let mut planner = ResearchPlanner::default();
        planner
            .ingest_research_state(&observation_fixture(), &[])
            .unwrap();
        let observation = proposal(
            "quote",
            "openbb.quote",
            serde_json::json!({"ticker": "VG"}),
            100,
        );
        assert!(matches!(
            planner.select(std::slice::from_ref(&observation)).unwrap(),
            PlannerDecision::Execute {
                proposal_id,
                score: Some(score),
                reason: SelectionReason::PositiveExpectedValue,
                evaluated: 1,
            } if proposal_id == "quote" && score > 0
        ));
    }

    #[test]
    fn observation_with_a_foreign_ticker_is_unmapped() {
        let mut planner = ResearchPlanner::default();
        planner
            .ingest_research_state(&observation_fixture(), &[])
            .unwrap();
        let observation = proposal(
            "quote",
            "openbb.quote",
            serde_json::json!({"ticker": "AAPL"}),
            0,
        );
        assert_eq!(
            planner.select(std::slice::from_ref(&observation)).unwrap(),
            PlannerDecision::NoPositiveValue {
                evaluated: 1,
                reason: NoPositiveReason::ProposalUnmapped,
            }
        );
    }

    #[test]
    fn observation_maps_for_a_qualitative_frontier_when_the_ticker_is_uncovered() {
        // P7 live regression (VIPS filings runs): the ontology reports the
        // trusted ticker unavailable, the plan still expects it (an exact
        // candidate exists), and the frontier is qualitative-only — the
        // strict calculation-goal fallback left the openbb.filings proposal
        // unmapped twice. The observation lane is then the only research
        // surface and must be admitted.
        let mut state = observation_fixture();
        state.warnings.push("ticker_not_available".into());
        let mut planner = ResearchPlanner::default();
        planner.ingest_research_state(&state, &[]).unwrap();
        let filings = proposal(
            "filings",
            "openbb.filings",
            serde_json::json!({"ticker": "VG"}),
            100,
        );
        assert!(matches!(
            planner.select(std::slice::from_ref(&filings)).unwrap(),
            PlannerDecision::Execute { proposal_id, .. } if proposal_id == "filings"
        ));
    }

    #[test]
    fn uncovered_ticker_fallback_still_rejects_foreign_tickers() {
        let mut state = observation_fixture();
        state.warnings.push("ticker_not_available".into());
        let mut planner = ResearchPlanner::default();
        planner.ingest_research_state(&state, &[]).unwrap();
        let foreign = proposal(
            "filings",
            "openbb.filings",
            serde_json::json!({"ticker": "AAPL"}),
            100,
        );
        assert_eq!(
            planner.select(std::slice::from_ref(&foreign)).unwrap(),
            PlannerDecision::NoPositiveValue {
                evaluated: 1,
                reason: NoPositiveReason::ProposalUnmapped,
            }
        );
    }

    #[test]
    fn universe_plan_admits_the_subject_ticker_observation_lane() {
        // Free-door (question_only) live regression (2026-09-04,
        // run_745d938b): a universe discovery plan has no ticker-scoped
        // precise candidates and never raises ticker_not_available for the
        // issuer the question itself names, so every openbb read for the
        // subject came back proposal_unmapped and the answer had to refuse
        // all figures. With the precise-candidate lane empty, the
        // observation plane is the only lane left: admit it. Company runs
        // always carry exact candidates, and the dispatch-level MarketPlane
        // binding still polices ticker canonicality and membership.
        let state = observation_fixture();
        let mut planner = ResearchPlanner::default();
        planner.ingest_research_state(&state, &[]).unwrap();
        planner
            .projection
            .as_mut()
            .expect("projection ingested")
            .exact_precise_query_candidates
            .clear();
        let income = proposal(
            "income",
            "openbb.income_statement",
            serde_json::json!({"ticker": "PLTR"}),
            100,
        );
        assert!(matches!(
            planner.select(std::slice::from_ref(&income)).unwrap(),
            PlannerDecision::Execute { proposal_id, .. } if proposal_id == "income"
        ));
    }

    #[test]
    fn filing_targeted_query_outcompetes_observation_on_the_same_clause() {
        let mut planner = ResearchPlanner::default();
        planner
            .ingest_research_state(&observation_fixture(), &[])
            .unwrap();
        let query = proposal(
            "filing",
            "ontology.query",
            serde_json::json!({"topic": "VG cash generation"}),
            100,
        );
        let observation = proposal(
            "quote",
            "openbb.quote",
            serde_json::json!({"ticker": "VG"}),
            100,
        );
        // The compass: the observation informs the clause, the filing read
        // resolves it — same clause, same cost, the filing evidence path
        // must win the selection.
        assert!(matches!(
            planner.select(&[query, observation]).unwrap(),
            PlannerDecision::Execute { proposal_id, .. } if proposal_id == "filing"
        ));
    }

    #[test]
    fn observation_serves_open_calculation_goals_when_intent_lacks_the_clause() {
        // A metric-change intent keeps an open calculation goal, and the
        // echoed plan clause carries the ticker: the model's openbb
        // statement read must be selectable end-to-end.
        let (receipt, state) = calculation_intent_fixture();
        let mut planner = ResearchPlanner::default();
        planner
            .record_initial_context(ContentHash::sha256("fixture-context"))
            .unwrap();
        planner
            .ingest_research_state_for_intent(&state, &receipt, &[])
            .unwrap();
        let observation = proposal(
            "balance",
            "openbb.balance_statement",
            serde_json::json!({"ticker": "VG"}),
            100,
        );
        assert!(matches!(
            planner.select(std::slice::from_ref(&observation)).unwrap(),
            PlannerDecision::Execute {
                proposal_id,
                score: Some(score),
                reason: SelectionReason::PositiveExpectedValue,
                evaluated: 1,
            } if proposal_id == "balance" && score > 0
        ));
    }

    #[test]
    fn observation_skips_candidates_whose_clause_already_resolved() {
        // The incident shape without an intent projection: the ticker's
        // FIRST exact candidate belongs to a clause the filing read already
        // resolved, while a later candidate still has an open clause. The
        // observation must bind to the open one instead of dropping out.
        let mut state = fixture();
        let first_clause_id = state.plan["clauses"][0]["clause_id"]
            .as_str()
            .expect("fixture clause id")
            .to_owned();
        let mut second_clause = state.plan["clauses"][0].clone();
        second_clause["clause_id"] = Value::String("clause-open-calc".into());
        second_clause["retrieval_query"] = Value::String("total debt".into());
        state
            .plan
            ["clauses"]
            .as_array_mut()
            .expect("clauses array")
            .push(second_clause);
        state.clause_coverage.push(ClauseCoverage {
            clause_id: "clause-open-calc".into(),
            required: true,
            directness_required: "direct".into(),
            status: "partial".into(),
            evidence_ids: Vec::new(),
            covered_tickers: vec!["VG".into()],
            missing_tickers: Vec::new(),
            best_directness: Some("direct".into()),
            best_evidence_grade: Some("strong".into()),
            strong_claim_ready: false,
            reason: None,
        });
        // Both clauses are exact-candidate sources (required + missing), but
        // the first clause's coverage stays satisfied, so only the second
        // resolves to a frontier goal.
        state.missing_parts.push(MissingPart {
            code: "clause_not_covered".into(),
            detail: "cash generation evidence incomplete".into(),
            clause_id: Some(first_clause_id),
            ticker: Some("VG".into()),
        });
        state.missing_parts.push(MissingPart {
            code: "clause_not_covered".into(),
            detail: "total debt evidence incomplete".into(),
            clause_id: Some("clause-open-calc".into()),
            ticker: Some("VG".into()),
        });
        let mut planner = ResearchPlanner::default();
        planner.ingest_research_state(&state, &[]).unwrap();
        let observation = proposal(
            "balance",
            "openbb.balance_statement",
            serde_json::json!({"ticker": "VG"}),
            100,
        );
        assert!(matches!(
            planner.select(std::slice::from_ref(&observation)).unwrap(),
            PlannerDecision::Execute {
                proposal_id,
                score: Some(score),
                reason: SelectionReason::PositiveExpectedValue,
                evaluated: 1,
            } if proposal_id == "balance" && score > 0
        ));
    }

    #[test]
    fn satisfied_graph_stops_even_when_the_model_requests_more() {
        let mut planner = ResearchPlanner::default();
        planner.ingest_research_state(&fixture(), &[]).unwrap();
        let query = proposal(
            "query",
            "ontology.query",
            serde_json::json!({"topic": "VG cash generation"}),
            0,
        );
        assert_eq!(
            planner.select(&[query]).unwrap(),
            PlannerDecision::NoPositiveValue {
                evaluated: 1,
                reason: NoPositiveReason::NoFrontier,
            }
        );
    }

    #[test]
    fn rejected_bootstrap_does_not_block_a_corrected_plan_and_survives_recovery() {
        let weights = ScoringWeights::default();
        let mut planner = ResearchPlanner::new(weights).unwrap();
        let rejected = proposal(
            "bad-context",
            "ontology.query_context",
            serde_json::json!({"question": "VG mixed clause"}),
            0,
        );
        assert!(matches!(
            planner.select(std::slice::from_ref(&rejected)),
            Ok(PlannerDecision::Execute {
                reason: SelectionReason::InitialContext,
                ..
            })
        ));
        planner
            .record_rejected_context(rejected.fingerprint.clone())
            .unwrap();
        assert!(matches!(
            planner.select(std::slice::from_ref(&rejected)),
            Err(ResearchPlannerError::RejectedContextAlreadyAttempted)
        ));

        let corrected = proposal(
            "corrected-context",
            "ontology.query_context",
            fixture().plan,
            0,
        );
        assert!(matches!(
            planner.select(&[corrected]),
            Ok(PlannerDecision::Execute {
                reason: SelectionReason::InitialContext,
                ..
            })
        ));

        let restored = ResearchPlanner::restore(planner.checkpoint(), weights).unwrap();
        assert!(matches!(
            restored.select(std::slice::from_ref(&rejected)),
            Err(ResearchPlannerError::RejectedContextAlreadyAttempted)
        ));
    }

    #[test]
    fn sparse_search_plan_accepts_only_its_canonical_normalized_form() {
        let canonical = fixture().plan;
        let sparse = serde_json::json!({
            "question": canonical["question"],
            "intent": canonical["intent"],
            "tickers": [" vg ", "VG"],
            "document_types": ["10-k", "10-K"],
            "comparison_axes": ["directness", "directness"],
            "limit_results": 8,
            "clauses": [{
                "clause_id": "cash_generation",
                "retrieval_query": "VG cash generation",
                "required": true,
                "directness": "direct_required",
                "required_concepts": ["cash generation"],
                "tickers": ["vg"]
            }]
        });
        validate_normalized_plan_exchange(&sparse, &canonical).unwrap();

        let mut wrong_scope = canonical.clone();
        wrong_scope["tickers"] = serde_json::json!(["XOM"]);
        assert!(matches!(
            validate_normalized_plan_exchange(&sparse, &wrong_scope),
            Err(ResearchPlannerError::NormalizedPlanMismatch(_))
        ));
        let mut rewritten_clause = canonical;
        rewritten_clause["clauses"][0]["retrieval_query"] =
            Value::String("VG rewritten meaning".into());
        assert!(matches!(
            validate_normalized_plan_exchange(&sparse, &rewritten_clause),
            Err(ResearchPlannerError::NormalizedPlanMismatch(_))
        ));
    }

    #[test]
    fn normalized_plan_restores_only_omitted_server_defaults() {
        let canonical = fixture().plan;
        let mut sparse_dispatched = canonical.clone();
        let clause = sparse_dispatched["clauses"][0].as_object_mut().unwrap();
        for key in [
            "object_types",
            "metrics",
            "metric_dimensions",
            "calculation_window",
        ] {
            clause.remove(key);
        }
        let sparse_observed = sparse_dispatched.clone();

        let restored = canonicalize_normalized_plan_exchange(&sparse_dispatched, &sparse_observed)
            .expect("server may omit only exact default values");
        assert_eq!(restored, canonical);

        let mut missing_constraint = sparse_dispatched;
        missing_constraint["clauses"][0]
            .as_object_mut()
            .unwrap()
            .remove("required_concepts");
        assert!(matches!(
            canonicalize_normalized_plan_exchange(&canonical, &missing_constraint),
            Err(ResearchPlannerError::NormalizedPlanMismatch(
                NormalizedPlanMismatchKind::ClauseField("required_concepts")
            ))
        ));
    }

    #[test]
    fn replanning_is_monotonic_and_rejects_definition_drift() {
        let mut planner = ResearchPlanner::default();
        planner
            .ingest_research_state(&partial_fixture(), &[])
            .unwrap();
        planner.ingest_research_state(&fixture(), &[]).unwrap();

        // A context response is a bounded page of the same release-pinned
        // corpus. Its omission of an earlier row must not retract progress
        // already observed by this run.
        let regressed = partial_fixture();
        planner.ingest_research_state(&regressed, &[]).unwrap();
        let goal = planner
            .projection()
            .and_then(|projection| projection.graph.goal("clause:cash_generation"))
            .unwrap();
        assert_eq!(goal.status, GoalStatus::Satisfied);
        assert_eq!(goal.coverage_ppm, PPM);

        let mut regressed = fixture();
        regressed.plan["clauses"][0]["retrieval_query"] = Value::String("changed query".into());
        assert!(matches!(
            planner.ingest_research_state(&regressed, &[]),
            Err(ResearchPlannerError::ContextPlanDrift)
        ));
    }

    #[test]
    fn replanning_keeps_a_prior_supplemental_retrieval_warning() {
        // A targeted/trace outcome happens after the initial ResearchState.
        // A later bounded context page must not erase its safe warning and
        // make the composer reinterpret a failed exact read as company
        // non-disclosure.
        let mut planner = ResearchPlanner::default();
        let mut initial = partial_fixture();
        initial.warnings = vec!["supplemental_targeted_query_not_found".into()];
        planner.ingest_research_state(&initial, &[]).unwrap();

        let mut later = partial_fixture();
        later.warnings = vec!["planned_evidence_truncated".into()];
        planner.ingest_research_state(&later, &[]).unwrap();

        assert_eq!(
            planner.projection().unwrap().retrieval_status.warnings,
            vec![
                "planned_evidence_truncated".to_owned(),
                "supplemental_targeted_query_not_found".to_owned(),
            ]
        );
    }

    #[test]
    fn replanning_keeps_bounded_typed_supplemental_statuses() {
        let mut planner = ResearchPlanner::default();
        planner
            .ingest_research_state(&partial_fixture(), &[])
            .unwrap();
        for offset in 0..6 {
            planner.record_supplemental_retrieval_status(SupplementalReadStatus {
                kind: SupplementalReadKind::Retrieved,
                result_count: 1,
                has_more: true,
                next_offset: Some(offset),
                warning_codes: vec!["supplemental_truncated".into()],
            });
        }

        let statuses = &planner
            .projection()
            .unwrap()
            .retrieval_status
            .supplemental_reads;
        assert_eq!(statuses.len(), MAX_SUPPLEMENTAL_READ_STATUSES);
        assert_eq!(statuses[0].next_offset, Some(2));
        assert_eq!(statuses[3].next_offset, Some(5));
    }

    #[test]
    fn replanning_does_not_retract_a_previously_covered_calculation() {
        let mut initial = partial_fixture();
        initial.calculation_coverage.push(CalculationCoverage {
            clause_id: "cash_generation".into(),
            metric: "operating_cash_flow".into(),
            axis: "growth_rate".into(),
            metric_scope: "company_total".into(),
            metric_dimensions: Vec::new(),
            status: "partial".into(),
            required_tickers: vec!["VG".into(), "XOM".into()],
            covered_tickers: vec!["VG".into()],
            calculation_ids: vec!["calc:vg:ocf-growth".into()],
            reason: Some("comparison period unavailable".into()),
        });

        let mut planner = ResearchPlanner::default();
        planner.ingest_research_state(&initial, &[]).unwrap();
        let calculation_id = planner
            .projection()
            .unwrap()
            .graph
            .goals()
            .find(|goal| goal.calculation_required)
            .unwrap()
            .goal_id
            .clone();

        // A later replan may be truncated before it returns this calculation.
        // That is an incomplete snapshot, not negative evidence.
        let mut later = initial;
        let calculation = &mut later.calculation_coverage[0];
        calculation.status = "missing".into();
        calculation.covered_tickers.clear();
        calculation.calculation_ids.clear();
        planner.ingest_research_state(&later, &[]).unwrap();

        let goal = planner
            .projection()
            .and_then(|projection| projection.graph.goal(&calculation_id))
            .unwrap();
        assert_eq!(goal.status, GoalStatus::Partial);
        assert_eq!(goal.coverage_ppm, PPM / 2);
        assert_eq!(goal.calculation_ids, ["calc:vg:ocf-growth"]);
    }

    #[test]
    fn context_replan_is_strictly_append_only_and_scope_immutable() {
        let mut planner = ResearchPlanner::default();
        planner
            .ingest_research_state(&partial_fixture(), &[])
            .unwrap();
        let mut appended = fixture().plan;
        appended["clauses"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "clause_id": "new_risk",
                "retrieval_query": "VG new filing risk",
                "required_concepts": ["filing risk"],
                "required_predicates": [],
                "required": false,
                "tickers": ["VG"],
                "directness": "direct_preferred",
                "object_types": [],
                "metrics": [],
                "metric_dimensions": [],
                "metric_scope": "company_total",
                "calculation_window": null
            }));
        let context_proposal = proposal(
            "context-replan",
            "ontology.query_context",
            appended.clone(),
            2_500,
        );
        assert!(matches!(
            planner.select(&[context_proposal]),
            Ok(PlannerDecision::Execute {
                score: Some(score),
                reason: SelectionReason::PositiveExpectedValue,
                ..
            }) if score > 0
        ));

        let mut rewritten = appended.clone();
        rewritten["tickers"] = serde_json::json!(["XOM"]);
        let scope_proposal = proposal("scope-rewrite", "ontology.query_context", rewritten, 2_500);
        assert!(matches!(
            planner.select(&[scope_proposal]),
            Err(ResearchPlannerError::ContextPlanDrift)
        ));

        let mut rewritten = appended;
        rewritten["clauses"][0]["retrieval_query"] = Value::String("rewritten".into());
        let rewrite_proposal =
            proposal("clause-rewrite", "ontology.query_context", rewritten, 2_500);
        assert!(matches!(
            planner.select(&[rewrite_proposal]),
            Err(ResearchPlannerError::ContextPlanDrift)
        ));
    }

    #[test]
    fn intent_receipt_turns_clause_coverage_into_user_goal_progress() {
        let weights = ScoringWeights::default();
        let receipt = fixture_intent_receipt();
        let goal_id = only_goal_id(&receipt);
        let mut planner = ResearchPlanner::new(weights).unwrap();
        planner
            .record_initial_context(ContentHash::sha256("fixture-context"))
            .unwrap();
        planner
            .ingest_research_state_for_intent(&fixture_for_proposal(), &receipt, &[])
            .unwrap();
        let intent = planner.intent_projection().expect("intent projection");
        let goal = intent.graph.goal(&goal_id).unwrap();
        assert_eq!(goal.status, GoalStatus::Satisfied);
        assert_eq!(goal.coverage_ppm, PPM);
        assert_eq!(goal.evidence_ids, vec!["ev-vg-cash-001"]);

        let checkpoint = planner.checkpoint();
        let bytes = serde_jcs::to_vec(&checkpoint).unwrap();
        let restored = ResearchPlanner::restore(
            serde_json::from_slice::<ResearchPlannerCheckpointV4>(&bytes).unwrap(),
            weights,
        )
        .unwrap();
        assert_eq!(
            restored.checkpoint_hash().unwrap(),
            planner.checkpoint_hash().unwrap()
        );
        assert_eq!(
            restored
                .intent_projection()
                .and_then(|projection| projection.graph.goal(&goal_id))
                .map(|goal| goal.status),
            Some(GoalStatus::Satisfied)
        );
    }

    #[test]
    fn intent_goal_cannot_be_satisfied_without_evidence_provenance() {
        let receipt = fixture_intent_receipt();
        let mut state = fixture_for_proposal();
        state.clause_coverage[0].evidence_ids.clear();
        let mut planner = ResearchPlanner::default();
        planner
            .record_initial_context(ContentHash::sha256("fixture-context"))
            .unwrap();
        assert!(matches!(
            planner.ingest_research_state_for_intent(&state, &receipt, &[]),
            Err(ResearchPlannerError::IntentCoverageProvenanceMissing)
        ));
        assert!(
            planner.intent_projection().is_none(),
            "failed ingest is atomic"
        );
    }

    #[test]
    fn orientation_vocabulary_survives_checkpoints_and_old_projections_still_validate() {
        let weights = ScoringWeights::default();
        let mut planner = ResearchPlanner::new(weights).unwrap();
        planner
            .record_initial_context(ContentHash::sha256("fixture-context"))
            .unwrap();
        let terms = vec![
            OrientationTerm {
                term: "Component Procurement".into(),
                document_type: Some("10-K".into()),
                period: Some("2026년".into()),
            },
            OrientationTerm {
                term: "Services Growth".into(),
                document_type: None,
                period: None,
            },
        ];
        planner.ingest_research_state(&fixture(), &terms).unwrap();
        assert_eq!(planner.projection().unwrap().orientation_vocabulary, terms);

        // A later bounded research snapshot derived without orientation
        // records must not erase the committed advisory vocabulary.
        planner.ingest_research_state(&fixture(), &[]).unwrap();
        assert_eq!(planner.projection().unwrap().orientation_vocabulary, terms);

        let checkpoint = planner.checkpoint();
        let bytes = serde_jcs::to_vec(&checkpoint).unwrap();
        let restored = ResearchPlanner::restore(
            serde_json::from_slice::<ResearchPlannerCheckpointV4>(&bytes).unwrap(),
            weights,
        )
        .unwrap();
        assert_eq!(restored.projection().unwrap().orientation_vocabulary, terms);

        // Checkpoints serialized before the field existed still validate and
        // restore with an empty vocabulary.
        let mut old = serde_json::from_slice::<Value>(&bytes).unwrap();
        old.get_mut("projection")
            .expect("checkpoint carries a projection")
            .as_object_mut()
            .expect("projection is an object")
            .remove("orientation_vocabulary");
        let restored_old = ResearchPlanner::restore(
            serde_json::from_value::<ResearchPlannerCheckpointV4>(old).unwrap(),
            weights,
        )
        .unwrap();
        assert!(
            restored_old
                .projection()
                .unwrap()
                .orientation_vocabulary
                .is_empty()
        );

        // The checkpoint trust boundary keeps the vocabulary bounded and
        // duplicate-free.
        let mut oversized = serde_json::from_slice::<Value>(&bytes).unwrap();
        let vocabulary = oversized
            .get_mut("projection")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .get_mut("orientation_vocabulary")
            .unwrap()
            .as_array_mut()
            .unwrap();
        vocabulary.clear();
        for index in 0..=MAX_ORIENTATION_VOCABULARY {
            vocabulary.push(serde_json::json!({
                "term": format!("Topic {index:02}"),
                "document_type": "10-K",
                "period": "2026년"
            }));
        }
        assert!(matches!(
            ResearchPlanner::restore(
                serde_json::from_value::<ResearchPlannerCheckpointV4>(oversized).unwrap(),
                weights,
            ),
            Err(ResearchPlannerError::InvalidCheckpoint)
        ));
    }

    #[test]
    fn later_intent_restatement_keeps_committed_goal_definition() {
        let mut planner = ResearchPlanner::default();
        let receipt = fixture_intent_receipt();
        planner.merge_intent_receipt(&receipt).unwrap();
        let mut rewritten = fixture_proposal();
        rewritten["objectives"][0]["directness"] = serde_json::json!("any");
        let context = RunContextV1::CompanyTickerSet {
            tickers: vec!["VG".into()],
        };
        let rewritten_receipt = compile_research_proposal(
            &rewritten,
            InitialPlanScope {
                question: "Does VG generate cash according to its filing?",
                context: &context,
                derived_tickers: None,
                max_discovery_tickers: 1,
                prior_plan: None,
                requester: ResearchPlanRequester::CompanyQueryContext,
            },
        )
        .unwrap()
        .receipt;
        planner.merge_intent_receipt(&rewritten_receipt).unwrap();
        let goal = planner
            .intent_projection()
            .and_then(|projection| projection.graph.goals().next())
            .expect("committed goal");
        assert_eq!(
            goal.directness,
            krw_agent_planning::DirectnessRequirement::Direct
        );
    }

    #[test]
    fn semantic_goal_completion_stops_targeted_research_even_if_server_has_a_gap() {
        let receipt = fixture_intent_receipt();
        let mut planner = ResearchPlanner::default();
        planner
            .record_initial_context(ContentHash::sha256("fixture-context"))
            .unwrap();
        planner
            .ingest_research_state_for_intent(&fixture_for_proposal(), &receipt, &[])
            .unwrap();

        // This is deliberately a direct planner test: a real run additionally
        // rejects server-added clauses at the normalized-plan exchange. It
        // proves that action selection follows the user-linked semantic graph,
        // not an incidental server clause which was never selected for a goal.
        let mut with_unmapped_gap = fixture_for_proposal();
        with_unmapped_gap.plan["clauses"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "clause_id":"unmapped_gap",
                "retrieval_query":"VG incidental unresolved topic",
                "required_concepts":["incidental unresolved topic"],
                "required_predicates":[],
                "required":false,
                "tickers":["VG"],
                "directness":"any",
                "object_types":[],
                "metrics":[],
                "metric_dimensions":[],
                "metric_scope":"company_total",
                "calculation_window":null
            }));
        let mut coverage = with_unmapped_gap.clause_coverage[0].clone();
        coverage.clause_id = "unmapped_gap".into();
        coverage.required = false;
        coverage.status = "missing".into();
        coverage.evidence_ids.clear();
        coverage.covered_tickers.clear();
        coverage.missing_tickers = vec!["VG".into()];
        with_unmapped_gap.clause_coverage.push(coverage);
        planner
            .ingest_research_state_for_intent(&with_unmapped_gap, &receipt, &[])
            .unwrap();

        let query = proposal(
            "unmapped-query",
            "ontology.query",
            serde_json::json!({"topic":"VG cash generation", "ticker":"VG"}),
            0,
        );
        assert_eq!(
            planner.select(&[query]).unwrap(),
            PlannerDecision::NoPositiveValue {
                evaluated: 1,
                reason: NoPositiveReason::NoFrontier,
            }
        );
    }

    #[test]
    fn required_mapped_gap_keeps_a_completed_semantic_goal_eligible_for_targeted_read() {
        let receipt = fixture_intent_receipt();
        let mut state = fixture_for_proposal();
        let clause_id = state.plan["clauses"][0]["clause_id"]
            .as_str()
            .expect("compiled clause id")
            .to_owned();
        let topic = state.plan["clauses"][0]["retrieval_query"]
            .as_str()
            .expect("compiled retrieval query")
            .to_owned();
        // A raw observation may exist while the ontology explicitly reports a
        // required comparison/calculation gap for the same user-linked
        // clause. That gap must remain eligible for the analyst's precise
        // follow-up instead of being treated as an incidental extra clause.
        state.missing_parts.push(MissingPart {
            code: "metric_calculation_unavailable".into(),
            detail: "comparison evidence remains incomplete".into(),
            clause_id: Some(clause_id),
            ticker: Some("VG".into()),
        });

        let mut planner = ResearchPlanner::default();
        planner
            .record_initial_context(ContentHash::sha256("fixture-context"))
            .unwrap();
        planner
            .ingest_research_state_for_intent(&state, &receipt, &[])
            .unwrap();

        let query = proposal(
            "required-gap-query",
            "ontology.query",
            serde_json::json!({"ticker":"VG", "topic":topic}),
            0,
        );
        assert!(matches!(
            planner.select(std::slice::from_ref(&query)).unwrap(),
            PlannerDecision::Execute {
                proposal_id,
                score: Some(score),
                reason: SelectionReason::PositiveExpectedValue,
                evaluated: 1,
            } if proposal_id == "required-gap-query" && score > 0
        ));
    }

    #[test]
    fn planner_checkpoint_round_trips_and_rejects_private_state_tamper() {
        let weights = ScoringWeights::default();
        let mut planner = ResearchPlanner::new(weights).unwrap();
        planner
            .ingest_research_state(&partial_fixture(), &[])
            .unwrap();
        planner
            .record_completed(ContentHash::sha256("completed"))
            .unwrap();
        planner
            .record_rejected_context(ContentHash::sha256("rejected"))
            .unwrap();
        let expected_hash = planner.checkpoint_hash().unwrap();
        let bytes = serde_jcs::to_vec(&planner.checkpoint()).unwrap();
        let checkpoint: ResearchPlannerCheckpointV4 = serde_json::from_slice(&bytes).unwrap();
        let recovered = ResearchPlanner::restore(checkpoint, weights).unwrap();
        assert_eq!(recovered.checkpoint_hash().unwrap(), expected_hash);

        let mut tampered = serde_json::to_value(planner.checkpoint()).unwrap();
        tampered["projection"]["graph"]["original_order"] = serde_json::json!(["clause:missing"]);
        let checkpoint: ResearchPlannerCheckpointV4 = serde_json::from_value(tampered).unwrap();
        assert!(matches!(
            ResearchPlanner::restore(checkpoint, weights),
            Err(ResearchPlannerError::Planning(
                krw_agent_planning::PlanningError::InvalidRecoveredGraph
            ))
        ));
    }

    #[test]
    fn checkpoint_restore_accepts_compact_exact_query_candidates() {
        let weights = ScoringWeights::default();
        let mut state = partial_fixture();
        let clause_id = state.plan["clauses"][0]["clause_id"]
            .as_str()
            .expect("fixture clause id")
            .to_owned();
        // Both candidate emitters (the adapter projection and the run-engine
        // gap hint) advertise `response_detail: "compact"` as the safe
        // default depth; the analyst may raise a dispatched read to `full`.
        // A checkpoint holding such a projection must restore instead of
        // failing `InvalidCheckpoint` and killing the recovered run.
        state.missing_parts.push(MissingPart {
            code: "direct_lineage_gap".into(),
            detail: "trace the filing claim".into(),
            clause_id: Some(clause_id),
            ticker: Some("VG".into()),
        });

        let mut planner = ResearchPlanner::new(weights).unwrap();
        planner.ingest_research_state(&state, &[]).unwrap();
        let candidates = &planner
            .projection()
            .expect("projection")
            .exact_precise_query_candidates;
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].response_detail, "compact");

        let expected_hash = planner.checkpoint_hash().unwrap();
        let bytes = serde_jcs::to_vec(&planner.checkpoint()).unwrap();
        let checkpoint: ResearchPlannerCheckpointV4 = serde_json::from_slice(&bytes).unwrap();
        let recovered = ResearchPlanner::restore(checkpoint, weights).unwrap();
        assert_eq!(recovered.checkpoint_hash().unwrap(), expected_hash);
        assert_eq!(
            recovered
                .projection()
                .unwrap()
                .exact_precise_query_candidates[0]
                .response_detail,
            "compact"
        );

        // `full` remains valid and any other depth is still rejected.
        let mut full = planner.checkpoint();
        full.projection
            .as_mut()
            .unwrap()
            .exact_precise_query_candidates[0]
            .response_detail = "full".into();
        assert!(ResearchPlanner::restore(full, weights).is_ok());

        let mut unbounded = planner.checkpoint();
        unbounded
            .projection
            .as_mut()
            .unwrap()
            .exact_precise_query_candidates[0]
            .response_detail = "unbounded".into();
        assert!(matches!(
            ResearchPlanner::restore(unbounded, weights),
            Err(ResearchPlannerError::InvalidCheckpoint)
        ));
    }
}
