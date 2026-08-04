//! Deterministic compilation of a model proposal into a root `SearchPlan`.
//!
//! The provider may identify user-linked answer goals and candidate retrieval
//! clauses, but it cannot choose the production plan by simply emitting a
//! large `SearchPlan`. This module validates the proposal against the
//! authenticated question/scope, removes dominated candidates, and chooses a
//! minimum sufficient set before the first ontology call or a bounded append.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use krw_agent_contracts::{RESEARCH_PROPOSAL_V4, SEARCH_PLAN_V2, validate_value};
use krw_agent_planning::{DirectnessRequirement, EvidenceGoal, EvidenceGoalGraph, GoalStatus};
use krw_agent_protocol::{ContentHash, RunContextV1};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

const MAX_SELECTED_CLAUSES: usize = 12;
const MAX_EXACT_SEARCH_NODES: usize = 65_536;
const RESEARCH_INTENT_RECEIPT_SCHEMA_VERSION: u16 = 1;

/// Trusted inputs that a model is not allowed to restate or widen in its
/// `ResearchProposal`. The owner is the run engine, not this planner.
#[derive(Debug, Clone, Copy)]
pub struct InitialPlanScope<'a> {
    pub question: &'a str,
    pub context: &'a RunContextV1,
    /// A ticker set derived from an earlier committed typed capability result
    /// (for example a selected feed context). It is trusted kernel state, not
    /// provider-authored input, and takes precedence over a context which has
    /// no intrinsic ticker set.
    pub derived_tickers: Option<&'a [String]>,
    /// The selected entrypoint's covered-universe cardinality cap. It has no
    /// effect for ticker-bound research.
    pub max_discovery_tickers: u8,
    /// The exact canonical plan already accepted by `query_context`, if this
    /// proposal is a follow-up investigation. The model never reconstructs
    /// or rewrites this state: the compiler preserves it and appends only
    /// genuinely selected new clauses.
    pub prior_plan: Option<&'a Value>,
}

/// The deterministic boundary receipt between a model-authored proposal and
/// the canonical MCP input. `intent_graph` is retained by the caller as the
/// semantic proof of why each selected clause exists; `search_plan` is the
/// only payload that crosses into the ontology capability.
#[derive(Debug, Clone)]
pub struct ResearchIntentCompilation {
    pub search_plan: Value,
    /// Non-sensitive semantic receipt retained by the kernel. It binds the
    /// canonical clauses chosen below to the immutable evidence-goal graph;
    /// raw provider arguments and the user question are intentionally absent.
    pub receipt: ResearchIntentReceipt,
    pub selected_clause_ids: Vec<String>,
    pub appended_clause_count: usize,
}

/// Durable semantic boundary between a model proposal and a canonical MCP
/// `SearchPlan`. A receipt contains only opaque hashes, goal definitions, and
/// clause-to-goal links. It never retains the user question or retrieval text.
///
/// The run engine commits this only after a valid `ResearchState` is accepted.
/// That ordering means an input-correction response cannot alter the durable
/// goal graph or unlock a follow-up action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchIntentReceipt {
    pub schema_version: u16,
    pub anchor_hash: ContentHash,
    pub compiled_plan_hash: ContentHash,
    pub intent_graph: EvidenceGoalGraph,
    /// Each selected canonical clause maps to one or more model-declared
    /// evidence goals. The values are sorted and unique for deterministic
    /// recovery and content hashing.
    pub clause_goal_ids: BTreeMap<String, Vec<String>>,
}

impl ResearchIntentReceipt {
    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }

    pub fn validate_recovered(&self) -> Result<(), InitialPlanError> {
        if self.schema_version != RESEARCH_INTENT_RECEIPT_SCHEMA_VERSION
            || self.clause_goal_ids.is_empty()
            || self.clause_goal_ids.len() > MAX_SELECTED_CLAUSES
        {
            return Err(InitialPlanError::Receipt);
        }
        self.intent_graph
            .validate_recovered()
            .map_err(|_| InitialPlanError::Receipt)?;
        for (clause_id, goal_ids) in &self.clause_goal_ids {
            if !valid_identifier(clause_id)
                || goal_ids.is_empty()
                || goal_ids.len() > 16
                || goal_ids.windows(2).any(|pair| pair[0] >= pair[1])
                || goal_ids
                    .iter()
                    .any(|goal_id| self.intent_graph.goal(goal_id).is_none())
            {
                return Err(InitialPlanError::Receipt);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum GoalKind {
    AnswerClaim,
    EvidenceDependency,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Directness {
    Any,
    DirectPreferred,
    DirectRequired,
}

impl Directness {
    const fn rank(self) -> u8 {
        match self {
            Self::Any => 0,
            Self::DirectPreferred => 1,
            Self::DirectRequired => 2,
        }
    }

    const fn as_search_plan(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::DirectPreferred => "direct_preferred",
            Self::DirectRequired => "direct_required",
        }
    }

    const fn as_requirement(self) -> DirectnessRequirement {
        match self {
            Self::Any => DirectnessRequirement::Related,
            Self::DirectPreferred | Self::DirectRequired => DirectnessRequirement::Direct,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MetricScope {
    CompanyTotal,
    Dimensioned,
    Any,
}

impl MetricScope {
    const fn as_search_plan(self) -> &'static str {
        match self {
            Self::CompanyTotal => "company_total",
            Self::Dimensioned => "dimensioned",
            Self::Any => "any",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CalculationWindow {
    PeriodOverPeriod,
    YearOverYear,
}

impl CalculationWindow {
    const fn as_search_plan(self) -> &'static str {
        match self {
            Self::PeriodOverPeriod => "period_over_period",
            Self::YearOverYear => "year_over_year",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IntentGoal {
    goal_id: String,
    kind: GoalKind,
    user_span: Option<String>,
    required: bool,
    dependencies: Vec<String>,
    directness: Directness,
    calculation_required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidateClause {
    candidate_id: String,
    covers_goal_ids: Vec<String>,
    retrieval_query: String,
    required_concepts: Vec<String>,
    required_predicates: Vec<String>,
    directness: Directness,
    object_types: Vec<String>,
    metrics: Vec<String>,
    metric_dimensions: Vec<String>,
    metric_scope: MetricScope,
    calculation_window: Option<CalculationWindow>,
}

/// Candidate semantics before kernel-owned physical identifiers and goal
/// links are attached. Identical alternatives from independent objectives are
/// merged deterministically, allowing the exact-cover stage to select one
/// clause that proves more than one required objective.
#[derive(Debug, Clone, Serialize)]
struct CandidateClauseShape {
    retrieval_query: String,
    required_concepts: Vec<String>,
    required_predicates: Vec<String>,
    directness: Directness,
    object_types: Vec<String>,
    metrics: Vec<String>,
    metric_dimensions: Vec<String>,
    metric_scope: MetricScope,
    calculation_window: Option<CalculationWindow>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResearchIntent {
    intent: String,
    answer_scope: String,
    uncertainty: String,
    document_types: Vec<String>,
    periods: Vec<String>,
    comparison_axes: Vec<String>,
    limit_results: u64,
    limit_tickers: u64,
    goals: Vec<IntentGoal>,
    candidate_clauses: Vec<CandidateClause>,
}

/// The deliberately small provider-facing proposal.  It says what evidence
/// would answer the question, but never constructs opaque IDs, a goal graph,
/// a physical retrieval query, tickers, or execution limits.  Those are
/// kernel-owned lowering details.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResearchProposal {
    intent: String,
    answer_scope: String,
    uncertainty: String,
    document_types: Vec<String>,
    periods: Vec<String>,
    objectives: Vec<ResearchProposalObjective>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResearchProposalObjective {
    priority: ResearchProposalPriority,
    alternatives: Vec<ResearchProposalAlternative>,
    directness: Directness,
    object_types: Vec<String>,
    goal: ResearchProposalGoal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResearchProposalAlternative {
    terms: Vec<String>,
}

/// Stable answer-claim identity deliberately excludes proof strength,
/// retrieval alternatives, and physical object filters. Those values change
/// how evidence is collected, not what the user is asking to establish. A
/// later proposal therefore cannot silently rewrite a committed claim by
/// weakening its directness or swapping search wording.
#[derive(Debug, Clone, Serialize)]
struct ResearchObjectiveIdentity {
    kind: String,
    metric: Option<String>,
    metric_dimensions: Vec<String>,
    calculation_window: Option<CalculationWindow>,
    concepts: Vec<String>,
    predicates: Vec<String>,
}

/// The model selects which objectives are necessary for the answer now and
/// which are deliberately deferred until the committed evidence frontier shows
/// a real gap. The compiler never silently upgrades a deferred objective.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ResearchProposalPriority {
    Required,
    Deferred,
}

/// One discriminated semantic goal. The model therefore cannot accidentally
/// combine a raw time series, a period-over-period calculation, and a
/// qualitative proof condition as unrelated top-level fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ResearchProposalGoal {
    MetricObservation {
        metric: String,
        metric_dimensions: Vec<String>,
    },
    MetricTimeSeries {
        metric: String,
        metric_dimensions: Vec<String>,
    },
    MetricChange {
        metric: String,
        metric_dimensions: Vec<String>,
        change: MetricChange,
        window: CalculationWindow,
    },
    MetricDifference {
        metric: String,
        metric_dimensions: Vec<String>,
    },
    QualitativeEvidence {
        concepts: Vec<String>,
        predicates: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MetricChange {
    AbsoluteChange,
    GrowthRate,
}

#[derive(Debug, Clone)]
struct PreparedCandidate {
    source: CandidateClause,
    covers_required: u32,
    canonical_bytes: usize,
    fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlanScore {
    clause_count: usize,
    canonical_bytes: usize,
    fingerprints: Vec<String>,
}

impl Ord for PlanScore {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.clause_count
            .cmp(&other.clause_count)
            .then_with(|| self.canonical_bytes.cmp(&other.canonical_bytes))
            .then_with(|| self.fingerprints.cmp(&other.fingerprints))
    }
}

impl PartialOrd for PlanScore {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Default)]
struct ExactSearch {
    nodes: usize,
    exhausted: bool,
    best: Option<(PlanScore, Vec<usize>)>,
}

#[derive(Debug, Error)]
pub enum InitialPlanError {
    #[error("research intent is not a valid pinned contract")]
    Contract,
    #[error("research intent cannot be decoded after contract validation")]
    Decode,
    #[error("initial research is unsupported for this immutable run scope")]
    UnsupportedScope,
    #[error("the trusted prior SearchPlan is not compatible with this immutable run scope")]
    PriorPlan,
    #[error("a follow-up intent attempted to reuse an existing SearchPlan clause identifier")]
    DuplicateClause,
    #[error("a claimed user goal is not linked to the user question")]
    UnlinkedUserGoal,
    #[error("a dependency goal is not required by any user-linked answer goal")]
    UnlinkedDependency,
    #[error("research intent goal graph is invalid")]
    GoalGraph,
    #[error("candidate clause does not satisfy the goal it claims to cover")]
    CandidateCoverage,
    #[error("required research goal has no valid candidate clause")]
    UncoverableGoal,
    #[error("minimum sufficient plan exceeds the canonical SearchPlan clause bound")]
    PlanTooLarge,
    #[error("compiled initial SearchPlan is not a valid canonical contract")]
    SearchPlanContract,
    #[error("research intent receipt violates durable invariants")]
    Receipt,
    #[error("planner canonicalization failed")]
    Canonicalization,
}

/// Compile the only provider-facing research planning ABI into a canonical
/// root `SearchPlan`. The result deliberately has no outer key: it is sent as
/// MCP arguments to `ontology.query_context` unchanged.  The model proposes
/// evidence needs; the kernel creates the goal graph, candidate identities,
/// query literals, scope and limits before selection.
pub fn compile_research_proposal(
    proposal: &Value,
    scope: InitialPlanScope<'_>,
) -> Result<ResearchIntentCompilation, InitialPlanError> {
    validate_value(RESEARCH_PROPOSAL_V4, proposal).map_err(|_| InitialPlanError::Contract)?;
    let proposal: ResearchProposal =
        serde_json::from_value(proposal.clone()).map_err(|_| InitialPlanError::Decode)?;
    let intent = lower_research_proposal(proposal, scope)?;
    compile_lowered_research_intent(intent, scope)
}

/// Lower a bounded model proposal into the private semantic IR.  This is the
/// root-cause boundary: the provider cannot manufacture a broken graph or a
/// candidate that merely *claims* to cover a goal because every relation is
/// generated together from one request.
fn lower_research_proposal(
    proposal: ResearchProposal,
    scope: InitialPlanScope<'_>,
) -> Result<ResearchIntent, InitialPlanError> {
    let user_span = bounded_question_anchor(scope.question)?;
    let mut goals = Vec::new();
    let mut seen_goal_ids = BTreeSet::new();
    let mut comparison_axes = BTreeSet::new();
    let mut grouped_candidates =
        BTreeMap::<String, (CandidateClauseShape, BTreeSet<String>)>::new();
    for objective in proposal
        .objectives
        .into_iter()
        .filter(|objective| objective.priority == ResearchProposalPriority::Required)
    {
        let goal_id = generated_identifier(
            "goal",
            scope.question,
            &research_objective_identity(&objective),
        )?;
        // Duplicating an identical objective cannot create a second user need.
        // Keep the first one rather than manufacturing two opaque goals that
        // require the same evidence.
        if !seen_goal_ids.insert(goal_id.clone()) {
            continue;
        }
        let lowered = lower_goal(&objective.goal);
        comparison_axes.insert(lowered.comparison_axis.to_owned());
        comparison_axes.insert("directness".to_owned());
        goals.push(IntentGoal {
            goal_id: goal_id.clone(),
            kind: GoalKind::AnswerClaim,
            user_span: Some(user_span.clone()),
            required: true,
            dependencies: Vec::new(),
            directness: objective.directness,
            calculation_required: lowered.calculation_window.is_some(),
        });
        for alternative in objective.alternatives {
            let semantic_terms = if lowered.metrics.is_empty() {
                // Qualitative clause: use concepts and predicates as prose
                // terms so the retrieval index can match them in filing text.
                lowered
                    .required_concepts
                    .iter()
                    .cloned()
                    .chain(lowered.required_predicates.iter().cloned())
                    .collect::<Vec<_>>()
            } else {
                // Metric clause: emit natural-language filing phrases, not the
                // snake_case metric identifiers. A token like `gross_margin`
                // never appears verbatim in a 10-K, so sending it to the
                // retrieval index caused zero evidence matches. The canonical
                // machine identifier is still preserved separately under the
                // clause `metrics` field for structured lookup.
                lowered
                    .metrics
                    .iter()
                    .filter_map(|m| {
                        let prose = metric_as_prose(m);
                        (!prose.is_empty()).then(|| prose.to_string())
                    })
                    .chain(lowered.metric_dimensions.iter().cloned())
                    .collect::<Vec<_>>()
            };
            let shape = CandidateClauseShape {
                retrieval_query: assemble_retrieval_query(alternative.terms, semantic_terms)?,
                required_concepts: lowered.required_concepts.clone(),
                required_predicates: lowered.required_predicates.clone(),
                directness: objective.directness,
                object_types: objective.object_types.clone(),
                metrics: lowered.metrics.clone(),
                metric_dimensions: lowered.metric_dimensions.clone(),
                metric_scope: lowered.metric_scope,
                calculation_window: lowered.calculation_window,
            };
            let canonical =
                serde_jcs::to_vec(&shape).map_err(|_| InitialPlanError::Canonicalization)?;
            let key = ContentHash::sha256(canonical).to_string();
            grouped_candidates
                .entry(key)
                .and_modify(|(_, goal_ids)| {
                    goal_ids.insert(goal_id.clone());
                })
                .or_insert_with(|| (shape, BTreeSet::from([goal_id.clone()])));
        }
    }
    let candidate_clauses = grouped_candidates
        .into_values()
        .map(|(shape, goal_ids)| {
            Ok(CandidateClause {
                candidate_id: generated_identifier("clause", scope.question, &shape)?,
                covers_goal_ids: goal_ids.into_iter().collect(),
                retrieval_query: shape.retrieval_query,
                required_concepts: shape.required_concepts,
                required_predicates: shape.required_predicates,
                directness: shape.directness,
                object_types: shape.object_types,
                metrics: shape.metrics,
                metric_scope: shape.metric_scope,
                metric_dimensions: shape.metric_dimensions,
                calculation_window: shape.calculation_window,
            })
        })
        .collect::<Result<Vec<_>, InitialPlanError>>()?;
    if goals.is_empty() || candidate_clauses.is_empty() {
        return Err(InitialPlanError::Contract);
    }
    Ok(ResearchIntent {
        intent: proposal.intent,
        answer_scope: proposal.answer_scope,
        uncertainty: proposal.uncertainty,
        document_types: proposal.document_types,
        periods: proposal.periods,
        comparison_axes: comparison_axes.into_iter().collect(),
        // Model authors never set execution resource limits. These are a
        // bounded kernel policy and trusted scope later narrows them further.
        limit_results: 12,
        limit_tickers: 12,
        goals,
        candidate_clauses,
    })
}

/// Opaque, deterministic identities prevent model-owned ordering and ID
/// choices from becoming durable kernel state. The question scopes the digest
/// so identical generic evidence terms in different runs cannot collide.
fn generated_identifier(
    prefix: &str,
    question: &str,
    semantic: &impl Serialize,
) -> Result<String, InitialPlanError> {
    let canonical = serde_jcs::to_vec(semantic).map_err(|_| InitialPlanError::Canonicalization)?;
    let mut material = Vec::with_capacity(question.len().saturating_add(canonical.len() + 1));
    material.extend_from_slice(question.as_bytes());
    material.push(0);
    material.extend_from_slice(&canonical);
    let digest = ContentHash::sha256(material);
    let suffix = digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or(InitialPlanError::Canonicalization)?;
    // `SearchPlan` constrains physical clause IDs to 64 ASCII characters.
    // Forty-eight hex characters retain 192 bits while leaving room for the
    // namespace prefix; the full canonical candidate fingerprint remains in
    // the receipt and action identity.
    let suffix_len = if prefix == "clause" { 48 } else { 64 };
    Ok(format!("{prefix}-{}", &suffix[..suffix_len]))
}

fn research_objective_identity(objective: &ResearchProposalObjective) -> ResearchObjectiveIdentity {
    let normalize = |values: &[String]| {
        let mut normalized = values
            .iter()
            .map(|value| value.trim().to_lowercase())
            .collect::<Vec<_>>();
        normalized.sort();
        normalized.dedup();
        normalized
    };
    match &objective.goal {
        ResearchProposalGoal::MetricObservation {
            metric,
            metric_dimensions,
        } => ResearchObjectiveIdentity {
            kind: "metric_observation".into(),
            metric: Some(metric.to_lowercase()),
            metric_dimensions: normalize(metric_dimensions),
            calculation_window: None,
            concepts: Vec::new(),
            predicates: Vec::new(),
        },
        ResearchProposalGoal::MetricTimeSeries {
            metric,
            metric_dimensions,
        } => ResearchObjectiveIdentity {
            kind: "metric_time_series".into(),
            metric: Some(metric.to_lowercase()),
            metric_dimensions: normalize(metric_dimensions),
            calculation_window: None,
            concepts: Vec::new(),
            predicates: Vec::new(),
        },
        ResearchProposalGoal::MetricChange {
            metric,
            metric_dimensions,
            change,
            window,
        } => ResearchObjectiveIdentity {
            kind: match change {
                MetricChange::AbsoluteChange => "metric_absolute_change",
                MetricChange::GrowthRate => "metric_growth_rate",
            }
            .into(),
            metric: Some(metric.to_lowercase()),
            metric_dimensions: normalize(metric_dimensions),
            calculation_window: Some(*window),
            concepts: Vec::new(),
            predicates: Vec::new(),
        },
        ResearchProposalGoal::MetricDifference {
            metric,
            metric_dimensions,
        } => ResearchObjectiveIdentity {
            kind: "metric_difference".into(),
            metric: Some(metric.to_lowercase()),
            metric_dimensions: normalize(metric_dimensions),
            calculation_window: None,
            concepts: Vec::new(),
            predicates: Vec::new(),
        },
        ResearchProposalGoal::QualitativeEvidence {
            concepts,
            predicates,
        } => ResearchObjectiveIdentity {
            kind: "qualitative_evidence".into(),
            metric: None,
            metric_dimensions: Vec::new(),
            calculation_window: None,
            concepts: normalize(concepts),
            predicates: normalize(predicates),
        },
    }
}

/// Private lowering from a model-visible semantic goal to the small set of
/// `SearchPlan` fields the ontology understands. The provider never chooses a
/// free-floating calculation window or global comparison axis; each is derived
/// from the tagged meaning of the objective.
struct LoweredGoal {
    required_concepts: Vec<String>,
    required_predicates: Vec<String>,
    metrics: Vec<String>,
    metric_scope: MetricScope,
    metric_dimensions: Vec<String>,
    calculation_window: Option<CalculationWindow>,
    comparison_axis: &'static str,
}

fn lower_goal(goal: &ResearchProposalGoal) -> LoweredGoal {
    let metric_goal = |metric: &String, metric_dimensions: &Vec<String>, axis| LoweredGoal {
        required_concepts: Vec::new(),
        required_predicates: Vec::new(),
        metrics: vec![metric.clone()],
        metric_scope: if metric_dimensions.is_empty() {
            MetricScope::CompanyTotal
        } else {
            MetricScope::Dimensioned
        },
        metric_dimensions: metric_dimensions.clone(),
        calculation_window: None,
        comparison_axis: axis,
    };
    match goal {
        ResearchProposalGoal::MetricObservation {
            metric,
            metric_dimensions,
        } => metric_goal(metric, metric_dimensions, "value"),
        ResearchProposalGoal::MetricTimeSeries {
            metric,
            metric_dimensions,
        } => metric_goal(metric, metric_dimensions, "value"),
        ResearchProposalGoal::MetricChange {
            metric,
            metric_dimensions,
            change,
            window,
        } => {
            let mut lowered = metric_goal(
                metric,
                metric_dimensions,
                match change {
                    MetricChange::AbsoluteChange => "absolute_change",
                    MetricChange::GrowthRate => "growth_rate",
                },
            );
            lowered.calculation_window = Some(*window);
            lowered
        }
        ResearchProposalGoal::MetricDifference {
            metric,
            metric_dimensions,
        } => metric_goal(metric, metric_dimensions, "value_difference"),
        ResearchProposalGoal::QualitativeEvidence {
            concepts,
            predicates,
        } => LoweredGoal {
            required_concepts: concepts.clone(),
            required_predicates: predicates.clone(),
            metrics: Vec::new(),
            metric_scope: MetricScope::Any,
            metric_dimensions: Vec::new(),
            calculation_window: None,
            comparison_axis: "directness",
        },
    }
}

/// Construct a physical retrieval phrase from model-provided search terms and
/// compiler-owned semantic requirements. The latter are appended rather than
/// trusted to the model's wording, so every `SearchPlan` literal requirement
/// is present by construction. This is independent of any individual metric
/// or ontology rule.
fn assemble_retrieval_query(
    terms: Vec<String>,
    semantic_terms: impl IntoIterator<Item = String>,
) -> Result<String, InitialPlanError> {
    let mut seen = BTreeSet::new();
    let mut parts = Vec::new();
    for term in terms.into_iter().chain(semantic_terms) {
        let normalized = term.trim();
        if normalized.is_empty() {
            return Err(InitialPlanError::Contract);
        }
        if seen.insert(normalized.to_lowercase()) {
            parts.push(normalized.to_owned());
        }
    }
    let query = parts.join(" ");
    if !(2..=1_000).contains(&query.len()) {
        return Err(InitialPlanError::PlanTooLarge);
    }
    Ok(query)
}

/// Map a canonical ResearchProposal v4 metric identifier to the natural
/// language phrase that actually appears in filings (10-K/10-Q). The MCP
/// retrieval index matches against filing prose, so a snake_case identifier
/// like `gross_margin` would never match `gross margin` in the actual 10-K
/// text. This is intentionally a one-way display/prose mapping — the
/// canonical machine identifier is still emitted separately under the
/// `metrics` clause field for structured metric lookups.
fn metric_as_prose(metric: &str) -> &'static str {
    match metric {
        "capital_expenditures" => "capital expenditures",
        "cash_and_equivalents" => "cash and cash equivalents",
        "cost_of_revenue" => "cost of revenue",
        "eps" => "earnings per share",
        "fcf_margin" => "free cash flow margin",
        "free_cash_flow" => "free cash flow",
        "gross_margin" => "gross margin",
        "gross_profit" => "gross profit",
        "net_income" => "net income",
        "net_margin" => "net margin",
        "operating_cash_flow" => "cash from operating activities",
        "operating_expense" => "operating expenses",
        "operating_income" => "operating income",
        "operating_margin" => "operating margin",
        "research_and_development" => "research and development expense",
        "revenue" => "revenue net sales",
        "revenue_growth" => "revenue growth",
        "roa" => "return on assets",
        "roe" => "return on equity",
        "segment_revenue" => "segment net sales",
        "selling_general_and_admin" => "selling general and administrative expense",
        "shareholders_equity" => "total shareholders equity",
        "total_assets" => "total assets",
        "total_debt" => "total debt",
        "total_liabilities" => "total liabilities",
        _ => "",
    }
}

fn bounded_question_anchor(question: &str) -> Result<String, InitialPlanError> {
    let mut anchor = String::new();
    for character in question.chars() {
        if anchor.len().saturating_add(character.len_utf8()) > 1_000 {
            break;
        }
        anchor.push(character);
    }
    (!anchor.trim().is_empty())
        .then_some(anchor)
        .ok_or(InitialPlanError::UnlinkedUserGoal)
}

/// Compile a private, kernel-owned semantic IR into a canonical root
/// `SearchPlan`. It is intentionally not exported as an AgentImage ABI.
fn compile_lowered_research_intent(
    intent: ResearchIntent,
    scope: InitialPlanScope<'_>,
) -> Result<ResearchIntentCompilation, InitialPlanError> {
    let (goals, intent_graph) = index_and_validate_goals(&intent.goals, scope.question)?;
    let required_ids = required_goal_closure(&goals)?;
    let goal_bits = required_ids
        .iter()
        .enumerate()
        .map(|(index, goal_id)| (goal_id.as_str(), 1_u32 << index))
        .collect::<BTreeMap<_, _>>();
    let required_mask = goal_bits.values().fold(0_u32, |mask, bit| mask | bit);
    let mut candidates = prepare_candidates(&intent.candidate_clauses, &goals, &goal_bits)?;
    candidates = remove_dominated(&candidates);
    let selected = select_minimum_sufficient(&candidates, required_mask)?;
    let selected = sort_selected_by_fingerprint(&candidates, selected);
    let selected_clause_ids = selected
        .iter()
        .map(|index| candidates[*index].source.candidate_id.clone())
        .collect::<Vec<_>>();
    if selected.len() > MAX_SELECTED_CLAUSES {
        return Err(InitialPlanError::PlanTooLarge);
    }
    let (tickers, universe, limit_tickers) = trusted_scope_values(&scope, intent.limit_tickers)?;
    let clause_goal_ids = selected
        .iter()
        .map(|index| {
            let candidate = &candidates[*index].source;
            let mut goal_ids = candidate.covers_goal_ids.clone();
            goal_ids.sort();
            goal_ids.dedup();
            (candidate.candidate_id.clone(), goal_ids)
        })
        .collect::<BTreeMap<_, _>>();
    let appended_clauses = selected
        .into_iter()
        .map(|index| search_plan_clause(&candidates[index].source, &tickers))
        .collect::<Result<Vec<_>, _>>()?;
    let appended_clause_count = appended_clauses.len();
    let plan = if let Some(prior_plan) = scope.prior_plan {
        validate_prior_plan(prior_plan, &scope, &tickers, &universe, limit_tickers)?;
        let mut next = prior_plan.clone();
        let clauses = next
            .get_mut("clauses")
            .and_then(Value::as_array_mut)
            .ok_or(InitialPlanError::PriorPlan)?;
        let existing_ids = clauses
            .iter()
            .filter_map(|clause| clause.get("clause_id").and_then(Value::as_str))
            .collect::<BTreeSet<_>>();
        if selected_clause_ids
            .iter()
            .any(|id| existing_ids.contains(id.as_str()))
        {
            return Err(InitialPlanError::DuplicateClause);
        }
        if clauses.len().saturating_add(appended_clauses.len()) > MAX_SELECTED_CLAUSES {
            return Err(InitialPlanError::PlanTooLarge);
        }
        clauses.extend(appended_clauses);
        next
    } else {
        json!({
            "question": scope.question,
            "intent": intent.intent,
            "tickers": tickers,
            "document_types": intent.document_types,
            "periods": intent.periods,
            "comparison_axes": intent.comparison_axes,
            "answer_scope": intent.answer_scope,
            "uncertainty": intent.uncertainty,
            "limit_results": intent.limit_results,
            "limit_tickers": limit_tickers,
            "universe": universe,
            "clauses": appended_clauses,
        })
    };
    validate_value(SEARCH_PLAN_V2, &plan).map_err(|_| InitialPlanError::SearchPlanContract)?;
    let receipt = ResearchIntentReceipt {
        schema_version: RESEARCH_INTENT_RECEIPT_SCHEMA_VERSION,
        anchor_hash: ContentHash::sha256(scope.question.as_bytes()),
        compiled_plan_hash: ContentHash::sha256(
            serde_jcs::to_vec(&plan).map_err(|_| InitialPlanError::Canonicalization)?,
        ),
        intent_graph,
        clause_goal_ids,
    };
    receipt.validate_recovered()?;
    Ok(ResearchIntentCompilation {
        search_plan: plan,
        receipt,
        selected_clause_ids,
        appended_clause_count,
    })
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn index_and_validate_goals(
    goals: &[IntentGoal],
    question: &str,
) -> Result<(BTreeMap<String, IntentGoal>, EvidenceGoalGraph), InitialPlanError> {
    let mut indexed = BTreeMap::new();
    let mut graph_goals = Vec::with_capacity(goals.len());
    for goal in goals {
        match goal.kind {
            GoalKind::AnswerClaim => {
                let Some(span) = &goal.user_span else {
                    return Err(InitialPlanError::UnlinkedUserGoal);
                };
                if !question.contains(span) || !goal.required {
                    return Err(InitialPlanError::UnlinkedUserGoal);
                }
            }
            GoalKind::EvidenceDependency => {
                if goal.user_span.is_some() || goal.required {
                    return Err(InitialPlanError::UnlinkedDependency);
                }
            }
        }
        if indexed.insert(goal.goal_id.clone(), goal.clone()).is_some() {
            return Err(InitialPlanError::GoalGraph);
        }
        graph_goals.push(EvidenceGoal {
            goal_id: goal.goal_id.clone(),
            required: matches!(goal.kind, GoalKind::AnswerClaim),
            weight: goal_weight(goal),
            dependencies: goal.dependencies.clone(),
            directness: goal.directness.as_requirement(),
            calculation_required: goal.calculation_required,
            status: GoalStatus::Unresolved,
            coverage_ppm: 0,
            evidence_ids: Vec::new(),
            calculation_ids: Vec::new(),
        });
    }
    let intent_graph =
        EvidenceGoalGraph::new(graph_goals).map_err(|_| InitialPlanError::GoalGraph)?;
    if !indexed
        .values()
        .any(|goal| matches!(goal.kind, GoalKind::AnswerClaim))
    {
        return Err(InitialPlanError::UnlinkedUserGoal);
    }
    for goal in indexed
        .values()
        .filter(|goal| matches!(goal.kind, GoalKind::EvidenceDependency))
    {
        if !dependency_reaches_answer_goal(&goal.goal_id, &indexed) {
            return Err(InitialPlanError::UnlinkedDependency);
        }
    }
    Ok((indexed, intent_graph))
}

fn goal_weight(goal: &IntentGoal) -> u32 {
    let base = match goal.kind {
        GoalKind::AnswerClaim => 1_000,
        GoalKind::EvidenceDependency => 250,
    };
    base + u32::from(goal.directness.rank()) * 100 + u32::from(goal.calculation_required) * 100
}

fn dependency_reaches_answer_goal(goal_id: &str, goals: &BTreeMap<String, IntentGoal>) -> bool {
    let mut dependents = BTreeMap::<&str, Vec<&str>>::new();
    for goal in goals.values() {
        for dependency in &goal.dependencies {
            dependents
                .entry(dependency.as_str())
                .or_default()
                .push(goal.goal_id.as_str());
        }
    }
    let mut pending = VecDeque::from([goal_id]);
    let mut visited = BTreeSet::new();
    while let Some(current) = pending.pop_front() {
        if !visited.insert(current) {
            continue;
        }
        if goals
            .get(current)
            .is_some_and(|goal| matches!(goal.kind, GoalKind::AnswerClaim))
        {
            return true;
        }
        if let Some(children) = dependents.get(current) {
            pending.extend(children.iter().copied());
        }
    }
    false
}

fn required_goal_closure(
    goals: &BTreeMap<String, IntentGoal>,
) -> Result<Vec<String>, InitialPlanError> {
    let mut required = BTreeSet::new();
    let mut pending = goals
        .values()
        .filter(|goal| matches!(goal.kind, GoalKind::AnswerClaim))
        .map(|goal| goal.goal_id.clone())
        .collect::<VecDeque<_>>();
    while let Some(goal_id) = pending.pop_front() {
        if !required.insert(goal_id.clone()) {
            continue;
        }
        let goal = goals.get(&goal_id).ok_or(InitialPlanError::GoalGraph)?;
        pending.extend(goal.dependencies.iter().cloned());
    }
    Ok(required.into_iter().collect())
}

fn prepare_candidates(
    candidates: &[CandidateClause],
    goals: &BTreeMap<String, IntentGoal>,
    required_bits: &BTreeMap<&str, u32>,
) -> Result<Vec<PreparedCandidate>, InitialPlanError> {
    let mut ids = BTreeSet::new();
    let mut prepared = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        if !ids.insert(candidate.candidate_id.as_str())
            || candidate
                .required_concepts
                .iter()
                .any(|concept| !literal_in_query(&candidate.retrieval_query, concept))
        {
            return Err(InitialPlanError::CandidateCoverage);
        }
        let mut covers_required = 0_u32;
        for goal_id in &candidate.covers_goal_ids {
            let goal = goals
                .get(goal_id)
                .ok_or(InitialPlanError::CandidateCoverage)?;
            if candidate.directness.rank() < goal.directness.rank()
                || (goal.calculation_required
                    && (candidate.calculation_window.is_none() || candidate.metrics.is_empty()))
            {
                return Err(InitialPlanError::CandidateCoverage);
            }
            if let Some(bit) = required_bits.get(goal_id.as_str()) {
                covers_required |= *bit;
            }
        }
        let canonical =
            serde_jcs::to_vec(candidate).map_err(|_| InitialPlanError::Canonicalization)?;
        let fingerprint = krw_agent_protocol::ContentHash::sha256(&canonical).to_string();
        prepared.push(PreparedCandidate {
            source: candidate.clone(),
            covers_required,
            canonical_bytes: canonical.len(),
            fingerprint,
        });
    }
    prepared.sort_by(|left, right| {
        left.fingerprint
            .cmp(&right.fingerprint)
            .then_with(|| left.source.candidate_id.cmp(&right.source.candidate_id))
    });
    Ok(prepared)
}

fn literal_in_query(query: &str, literal: &str) -> bool {
    query.to_lowercase().contains(&literal.to_lowercase())
}

fn remove_dominated(candidates: &[PreparedCandidate]) -> Vec<PreparedCandidate> {
    candidates
        .iter()
        .enumerate()
        .filter(|(index, candidate)| {
            !candidates.iter().enumerate().any(|(other_index, other)| {
                other_index != *index
                    && candidate.covers_required | other.covers_required == other.covers_required
                    && other.canonical_bytes <= candidate.canonical_bytes
                    && (other.canonical_bytes < candidate.canonical_bytes
                        || other.fingerprint < candidate.fingerprint)
            })
        })
        .map(|(_, candidate)| candidate.clone())
        .collect()
}

fn select_minimum_sufficient(
    candidates: &[PreparedCandidate],
    required_mask: u32,
) -> Result<Vec<usize>, InitialPlanError> {
    if candidates.is_empty()
        || candidates
            .iter()
            .fold(0_u32, |mask, candidate| mask | candidate.covers_required)
            != required_mask
    {
        return Err(InitialPlanError::UncoverableGoal);
    }
    let mut exact = ExactSearch::default();
    let mut selected = Vec::new();
    exact_cover(candidates, required_mask, 0, 0, &mut selected, &mut exact);
    let result = if exact.exhausted {
        greedy_cover(candidates, required_mask)?
    } else {
        exact
            .best
            .map(|(_, selected)| selected)
            .ok_or(InitialPlanError::UncoverableGoal)?
    };
    if result.len() > MAX_SELECTED_CLAUSES {
        return Err(InitialPlanError::PlanTooLarge);
    }
    Ok(result)
}

fn exact_cover(
    candidates: &[PreparedCandidate],
    required_mask: u32,
    covered: u32,
    used: u64,
    selected: &mut Vec<usize>,
    search: &mut ExactSearch,
) {
    if search.nodes >= MAX_EXACT_SEARCH_NODES {
        search.exhausted = true;
        return;
    }
    search.nodes = search.nodes.saturating_add(1);
    if selected.len() > MAX_SELECTED_CLAUSES {
        return;
    }
    if covered == required_mask {
        let score = selection_score(candidates, selected);
        if search.best.as_ref().is_none_or(|(best, _)| score < *best) {
            search.best = Some((score, selected.clone()));
        }
        return;
    }
    if let Some((best, _)) = &search.best
        && selected.len() >= best.clause_count
    {
        return;
    }
    let remaining = required_mask & !covered;
    let Some(goal_bit) = least_branching_goal(candidates, remaining, used) else {
        return;
    };
    for (index, candidate) in candidates.iter().enumerate() {
        let bit = 1_u64 << index;
        if used & bit != 0 || candidate.covers_required & goal_bit == 0 {
            continue;
        }
        selected.push(index);
        exact_cover(
            candidates,
            required_mask,
            covered | candidate.covers_required,
            used | bit,
            selected,
            search,
        );
        selected.pop();
        if search.exhausted {
            return;
        }
    }
}

fn least_branching_goal(
    candidates: &[PreparedCandidate],
    remaining: u32,
    used: u64,
) -> Option<u32> {
    let mut best: Option<(usize, u32)> = None;
    for bit_index in 0..u32::BITS {
        let bit = 1_u32 << bit_index;
        if remaining & bit == 0 {
            continue;
        }
        let count = candidates
            .iter()
            .enumerate()
            .filter(|(index, candidate)| {
                used & (1_u64 << index) == 0 && candidate.covers_required & bit != 0
            })
            .count();
        match best {
            None => best = Some((count, bit)),
            Some((best_count, _)) if count < best_count => best = Some((count, bit)),
            _ => {}
        }
    }
    best.and_then(|(count, bit)| (count > 0).then_some(bit))
}

fn greedy_cover(
    candidates: &[PreparedCandidate],
    required_mask: u32,
) -> Result<Vec<usize>, InitialPlanError> {
    let mut selected = Vec::new();
    let mut used = 0_u64;
    let mut covered = 0_u32;
    while covered != required_mask {
        let remaining = required_mask & !covered;
        let best = candidates
            .iter()
            .enumerate()
            .filter(|(index, _)| used & (1_u64 << index) == 0)
            .filter_map(|(index, candidate)| {
                let gain = (candidate.covers_required & remaining).count_ones();
                (gain > 0).then_some((index, candidate, gain))
            })
            .max_by(|left, right| {
                let left_ratio =
                    u64::from(left.2) * u64::try_from(right.1.canonical_bytes).unwrap_or(u64::MAX);
                let right_ratio =
                    u64::from(right.2) * u64::try_from(left.1.canonical_bytes).unwrap_or(u64::MAX);
                left_ratio
                    .cmp(&right_ratio)
                    .then_with(|| right.1.canonical_bytes.cmp(&left.1.canonical_bytes))
                    .then_with(|| right.1.fingerprint.cmp(&left.1.fingerprint))
            })
            .map(|(index, _, _)| index)
            .ok_or(InitialPlanError::UncoverableGoal)?;
        selected.push(best);
        used |= 1_u64 << best;
        covered |= candidates[best].covers_required;
        if selected.len() > MAX_SELECTED_CLAUSES {
            return Err(InitialPlanError::PlanTooLarge);
        }
    }
    selected.sort_by(|left, right| {
        candidates[*left]
            .fingerprint
            .cmp(&candidates[*right].fingerprint)
    });
    Ok(selected)
}

fn selection_score(candidates: &[PreparedCandidate], selected: &[usize]) -> PlanScore {
    let mut fingerprints = selected
        .iter()
        .map(|index| candidates[*index].fingerprint.clone())
        .collect::<Vec<_>>();
    fingerprints.sort();
    PlanScore {
        clause_count: selected.len(),
        canonical_bytes: selected
            .iter()
            .map(|index| candidates[*index].canonical_bytes)
            .sum(),
        fingerprints,
    }
}

fn sort_selected_by_fingerprint(
    candidates: &[PreparedCandidate],
    mut selected: Vec<usize>,
) -> Vec<usize> {
    selected.sort_by(|left, right| {
        candidates[*left]
            .fingerprint
            .cmp(&candidates[*right].fingerprint)
    });
    selected
}

fn validate_prior_plan(
    prior_plan: &Value,
    scope: &InitialPlanScope<'_>,
    tickers: &[String],
    universe: &Value,
    limit_tickers: u64,
) -> Result<(), InitialPlanError> {
    validate_value(SEARCH_PLAN_V2, prior_plan).map_err(|_| InitialPlanError::PriorPlan)?;
    let prior = prior_plan.as_object().ok_or(InitialPlanError::PriorPlan)?;
    if prior.get("question").and_then(Value::as_str) != Some(scope.question)
        || prior.get("tickers")
            != Some(&Value::Array(
                tickers.iter().cloned().map(Value::String).collect(),
            ))
        || prior.get("universe") != Some(universe)
        || prior.get("limit_tickers").and_then(Value::as_u64) != Some(limit_tickers)
    {
        return Err(InitialPlanError::PriorPlan);
    }
    Ok(())
}

fn trusted_scope_values(
    scope: &InitialPlanScope<'_>,
    requested_limit_tickers: u64,
) -> Result<(Vec<String>, Value, u64), InitialPlanError> {
    if let Some(tickers) = scope.derived_tickers {
        if tickers.is_empty()
            || tickers.len() > 16
            || tickers.iter().any(|ticker| !canonical_ticker(ticker))
            || tickers.iter().collect::<BTreeSet<_>>().len() != tickers.len()
        {
            return Err(InitialPlanError::UnsupportedScope);
        }
        return Ok((tickers.to_vec(), Value::Null, requested_limit_tickers));
    }
    match scope.context {
        RunContextV1::CompanyTickerSet { tickers } => {
            Ok((tickers.clone(), Value::Null, requested_limit_tickers))
        }
        RunContextV1::CoveredUniverse { .. } => {
            let cap = u64::from(scope.max_discovery_tickers);
            if cap == 0 {
                return Err(InitialPlanError::UnsupportedScope);
            }
            Ok((
                Vec::new(),
                Value::String("covered".into()),
                requested_limit_tickers.min(cap),
            ))
        }
        _ => Err(InitialPlanError::UnsupportedScope),
    }
}

fn canonical_ticker(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 16
        && value.bytes().all(|byte| {
            byte.is_ascii_uppercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
        })
}

fn search_plan_clause(
    candidate: &CandidateClause,
    tickers: &[String],
) -> Result<Value, InitialPlanError> {
    let retrieval_query = assemble_retrieval_query(
        tickers.to_vec(),
        std::iter::once(candidate.retrieval_query.clone()),
    )?;
    // Metric clauses are emitted as qualitative clauses driven by natural
    // filing language rather than structured metric identity.
    //
    // The ontology retrieval index has two clause paths:
    //   * metric identity — requires a matching `MetricObservation` with a
    //     complete lineage and is far stricter than filing prose;
    //   * qualitative prose — matches `required_concepts` against filing text
    //     and reliably returns strong-grade evidence.
    // Empirically a clause carrying both a natural filing phrase and a metric
    // identifier is scored against the metric-identity path first, so it is
    // reported as `direct_evidence_missing` / `metric_calculation_unavailable`
    // even when the prose path has ample evidence.
    //
    // To use the reliable prose path for metric clauses, the structured metric
    // identity fields (`metrics`, `metric_dimensions`, `metric_scope`,
    // `calculation_window`) are omitted from the wire payload, and the metric's
    // natural-language filing phrase is injected into `required_concepts`. The
    // ontology treats a clause without `metrics` as qualitative, which requires
    // at least one `required_concepts` entry, so the prose form satisfies both
    // the contract and the retrieval index. Canonical machine identifiers stay
    // on the candidate for internal tracking and canonical hashing.
    let has_metrics = !candidate.metrics.is_empty();
    let mut required_concepts = candidate.required_concepts.clone();
    if has_metrics {
        for metric in &candidate.metrics {
            let prose = metric_as_prose(metric);
            if !prose.is_empty() {
                let normalized = prose.to_string();
                if !required_concepts.iter().any(|c| c.eq_ignore_ascii_case(&normalized)) {
                    required_concepts.push(normalized);
                }
            }
        }
    }
    let metric_fields = if has_metrics {
        json!({})
    } else {
        json!({
            "metrics": candidate.metrics,
            "metric_dimensions": candidate.metric_dimensions,
            "metric_scope": candidate.metric_scope.as_search_plan(),
            "calculation_window": candidate.calculation_window.map(CalculationWindow::as_search_plan),
        })
    };
    let mut clause = json!({
        "clause_id": candidate.candidate_id,
        "retrieval_query": retrieval_query,
        "required_concepts": required_concepts,
        "required_predicates": candidate.required_predicates,
        "required": true,
        "tickers": tickers,
        "directness": candidate.directness.as_search_plan(),
        "object_types": candidate.object_types,
    });
    if let Some(obj) = clause.as_object_mut() {
        if let Some(metric_obj) = metric_fields.as_object() {
            obj.extend(metric_obj.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
    }
    Ok(clause)
}

#[cfg(test)]
mod tests {
    use super::*;
    use krw_agent_protocol::{CoveredUniverseMarker, RunContextV1};

    fn company_scope(question: &str) -> InitialPlanScope<'_> {
        static TICKERS: std::sync::OnceLock<RunContextV1> = std::sync::OnceLock::new();
        let context = TICKERS.get_or_init(|| RunContextV1::CompanyTickerSet {
            tickers: vec!["AAPL".into()],
        });
        InitialPlanScope {
            question,
            context,
            derived_tickers: None,
            max_discovery_tickers: 1,
            prior_plan: None,
        }
    }

    fn proposal() -> Value {
        json!({
            "intent":"company_research",
            "answer_scope":"direct",
            "uncertainty":"low",
            "document_types":["10-K"],
            "periods":[],
            "objectives":[{
                "priority":"required",
                "alternatives":[{"terms":["AAPL", "services growth"]}],
                "directness":"direct_preferred",
                "object_types":[],
                "goal": {
                    "kind":"qualitative_evidence",
                    "concepts":["services growth"],
                    "predicates":[]
                }
            }]
        })
    }

    #[test]
    fn lowers_one_required_objective_to_a_user_linked_root_search_plan() {
        let question = "How durable is AAPL services growth?";
        let compiled = compile_research_proposal(&proposal(), company_scope(question)).unwrap();
        let plan = compiled.search_plan;
        assert_eq!(plan["question"], question);
        assert_eq!(plan["tickers"], json!(["AAPL"]));
        assert_eq!(plan["universe"], Value::Null);
        assert_eq!(plan["clauses"].as_array().unwrap().len(), 1);
        validate_value(SEARCH_PLAN_V2, &plan).unwrap();
        assert_eq!(
            compiled.receipt.compiled_plan_hash,
            ContentHash::sha256(serde_jcs::to_vec(&plan).unwrap())
        );
        assert_eq!(compiled.receipt.clause_goal_ids.len(), 1);
        let (clause_id, goal_ids) = compiled.receipt.clause_goal_ids.iter().next().unwrap();
        assert!(clause_id.starts_with("clause-"));
        assert_eq!(goal_ids.len(), 1);
        assert!(goal_ids[0].starts_with("goal-"));
        assert_eq!(
            compiled.receipt.anchor_hash,
            ContentHash::sha256(question.as_bytes())
        );
    }

    #[test]
    fn each_required_objective_becomes_a_deterministic_independent_goal() {
        let question = "How durable is AAPL services growth?";
        let mut proposal = proposal();
        proposal["objectives"].as_array_mut().unwrap().push(json!({
            "priority":"required",
            "alternatives":[{"terms":["AAPL", "customer retention"]}],
            "directness":"direct_required",
            "object_types":[],
            "goal": {
                "kind":"qualitative_evidence",
                "concepts":["customer retention"],
                "predicates":[]
            }
        }));
        let compiled = compile_research_proposal(&proposal, company_scope(question)).unwrap();
        assert_eq!(compiled.search_plan["clauses"].as_array().unwrap().len(), 2);
        assert_eq!(compiled.receipt.clause_goal_ids.len(), 2);
        assert!(
            compiled
                .receipt
                .clause_goal_ids
                .iter()
                .all(|(clause_id, goal_ids)| {
                    clause_id.starts_with("clause-")
                        && goal_ids.len() == 1
                        && goal_ids[0].starts_with("goal-")
                })
        );
    }

    #[test]
    fn deferred_objective_does_not_become_an_initial_required_clause() {
        let question = "How durable is AAPL services growth?";
        let mut proposal = proposal();
        proposal["objectives"].as_array_mut().unwrap().push(json!({
            "priority":"deferred",
            "alternatives":[{"terms":["AAPL", "generic valuation downside"]}],
            "directness":"direct_required",
            "object_types":[],
            "goal": {
                "kind":"qualitative_evidence",
                "concepts":["generic valuation downside"],
                "predicates":[]
            }
        }));

        let compiled = compile_research_proposal(&proposal, company_scope(question)).unwrap();
        assert_eq!(compiled.search_plan["clauses"].as_array().unwrap().len(), 1);
        assert_eq!(compiled.receipt.clause_goal_ids.len(), 1);
        assert!(
            !compiled.search_plan["clauses"][0]["retrieval_query"]
                .as_str()
                .unwrap()
                .contains("generic valuation downside")
        );
    }

    #[test]
    fn equivalent_retrieval_alternatives_compile_to_one_minimum_clause() {
        let question = "How durable is AAPL services growth?";
        let mut proposal = proposal();
        proposal["objectives"][0]["alternatives"] = json!([
            {"terms":["AAPL", "services growth"]},
            {"terms":["AAPL", "services expansion"]}
        ]);

        let compiled = compile_research_proposal(&proposal, company_scope(question)).unwrap();
        let clauses = compiled.search_plan["clauses"].as_array().unwrap();
        assert_eq!(clauses.len(), 1);
        assert_eq!(compiled.selected_clause_ids.len(), 1);
        let query = clauses[0]["retrieval_query"].as_str().unwrap();
        assert!(query.contains("services growth") || query.contains("services expansion"));
        assert!(!(query.contains("services growth") && query.contains("services expansion")));
    }

    #[test]
    fn metric_objective_lowers_to_a_pure_dimensioned_metric_clause() {
        let question = "How durable is AAPL services growth?";
        let mut proposal = proposal();
        proposal["objectives"][0]["alternatives"][0]["terms"] = json!(["AAPL", "revenue"]);
        proposal["objectives"][0]["goal"] = json!({
            "kind":"metric_change",
            "metric":"revenue",
            "metric_dimensions":["geography"],
            "change":"growth_rate",
            "window":"year_over_year"
        });
        let plan = compile_research_proposal(&proposal, company_scope(question))
            .unwrap()
            .search_plan;
        // Metric clauses omit structured metric identity fields from the wire
        // payload; retrieval is driven by natural-language concepts.
        assert!(
            plan["clauses"][0].get("metrics").is_none()
                || plan["clauses"][0]["metrics"] == Value::Null
        );
        // The metric's natural filing phrase is injected into required_concepts
        // so the qualitative-prose retrieval path can match it in the 10-K.
        assert_eq!(
            plan["clauses"][0]["required_concepts"],
            json!(["revenue net sales"])
        );
        assert_eq!(plan["clauses"][0]["required_predicates"], json!([]));
        assert!(
            plan["clauses"][0]
                .get("retrieval_query")
                .is_some(),
            "metric clause still carries a natural-language retrieval query"
        );
    }

    #[test]
    fn metric_time_series_never_invents_a_comparison_window() {
        // Regression for the live Flash failure: "revenue trend" is a
        // request for observations across time, not implicitly a growth-rate
        // or period-over-period calculation. The tagged V4 goal lets the
        // compiler derive the only valid physical shape.
        let question = "Explain AAPL revenue trend from its latest 10-K.";
        let mut proposal = proposal();
        proposal["objectives"][0]["alternatives"][0]["terms"] = json!(["AAPL", "revenue trend"]);
        proposal["objectives"][0]["goal"] = json!({
            "kind":"metric_time_series",
            "metric":"revenue",
            "metric_dimensions":[]
        });

        let plan = compile_research_proposal(&proposal, company_scope(question))
            .unwrap()
            .search_plan;
        assert_eq!(plan["comparison_axes"], json!(["directness", "value"]));
        // Metric clauses omit structured metric identity fields from the wire
        // payload; the natural-language retrieval_query drives retrieval.
        assert!(
            plan["clauses"][0].get("metrics").is_none()
                || plan["clauses"][0]["metrics"] == Value::Null
        );
        assert!(
            plan["clauses"][0].get("calculation_window").is_none()
                || plan["clauses"][0]["calculation_window"] == Value::Null
        );
        assert!(
            !plan["comparison_axes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|axis| matches!(axis.as_str(), Some("growth_rate" | "absolute_change")))
        );
        validate_value(SEARCH_PLAN_V2, &plan).unwrap();
    }

    #[test]
    fn repeated_terms_remain_a_valid_kernel_owned_retrieval_query() {
        let question = "How durable is AAPL services growth?";
        let mut proposal = proposal();
        proposal["objectives"][0]["alternatives"][0]["terms"] = json!(["a", "a"]);
        let plan = compile_research_proposal(&proposal, company_scope(question))
            .unwrap()
            .search_plan;
        assert_eq!(
            plan["clauses"][0]["retrieval_query"],
            "AAPL a services growth"
        );
        validate_value(SEARCH_PLAN_V2, &plan).unwrap();
    }

    #[test]
    fn metric_and_qualitative_needs_compile_to_independently_verifiable_clauses() {
        let question = "Did AAPL revenue growth improve because of a services mix shift?";
        let mut proposal = proposal();
        proposal["objectives"][0] = json!({
            "priority":"required",
            "alternatives":[{"terms":["AAPL", "revenue growth"]}],
            "directness":"direct_required",
            "object_types":[],
            "goal": {
                "kind":"metric_change",
                "metric":"revenue",
                "metric_dimensions":[],
                "change":"growth_rate",
                "window":"year_over_year"
            }
        });
        proposal["objectives"].as_array_mut().unwrap().push(json!({
            "priority":"required",
            "alternatives":[{"terms":["AAPL", "services mix shift"]}],
            "directness":"direct_required",
            "object_types":[],
            "goal": {
                "kind":"qualitative_evidence",
                "concepts":["services mix", "revenue growth"],
                "predicates":["drives"]
            }
        }));

        let plan = compile_research_proposal(&proposal, company_scope(question))
            .unwrap()
            .search_plan;
        let clauses = plan["clauses"].as_array().unwrap();
        assert_eq!(clauses.len(), 2);
        // The metric clause omits `metrics` from the wire payload; identify it
        // by the absence of qualitative predicates instead.
        let metric_clause = clauses
            .iter()
            .find(|clause| {
                clause["required_predicates"] == json!([])
                    && clause.get("metrics").is_none()
            })
            .expect("one pure revenue metric clause without wire metric fields");
        let qualitative_clause = clauses
            .iter()
            .find(|clause| clause["required_predicates"] == json!(["drives"]))
            .expect("one qualitative relationship clause");
        // The metric's natural filing phrase is injected into required_concepts.
        assert_eq!(
            metric_clause["required_concepts"],
            json!(["revenue net sales"])
        );
        assert_eq!(metric_clause["required_predicates"], json!([]));
        assert!(
            metric_clause
                .get("retrieval_query")
                .is_some(),
            "metric clause carries a natural-language retrieval query"
        );
        assert_eq!(qualitative_clause["metrics"], json!([]));
        assert_eq!(
            qualitative_clause["required_concepts"],
            json!(["services mix", "revenue growth"])
        );
        assert_eq!(qualitative_clause["required_predicates"], json!(["drives"]));
        validate_value(SEARCH_PLAN_V2, &plan).unwrap();
    }

    #[test]
    fn covered_universe_is_derived_from_trusted_context() {
        let question = "Find durable growth ideas";
        let proposal = proposal();
        let context = RunContextV1::CoveredUniverse {
            universe: CoveredUniverseMarker::Covered,
        };
        let plan = compile_research_proposal(
            &proposal,
            InitialPlanScope {
                question,
                context: &context,
                derived_tickers: None,
                max_discovery_tickers: 7,
                prior_plan: None,
            },
        )
        .unwrap()
        .search_plan;
        assert_eq!(plan["tickers"], json!([]));
        assert_eq!(plan["universe"], "covered");
        assert_eq!(plan["limit_tickers"], 7);
    }

    #[test]
    fn committed_feed_ticker_projection_overrides_a_selected_item_context() {
        let question = "How durable is ACME services growth?";
        let mut proposal = proposal();
        proposal["objectives"][0]["alternatives"][0]["terms"] = json!(["ACME", "services growth"]);
        let context = RunContextV1::SelectedFeedItems {
            feed_item_ids: vec!["22222222-2222-4222-8222-222222222222".into()],
        };
        let derived = vec!["ACME".into()];
        let plan = compile_research_proposal(
            &proposal,
            InitialPlanScope {
                question,
                context: &context,
                derived_tickers: Some(&derived),
                max_discovery_tickers: 5,
                prior_plan: None,
            },
        )
        .unwrap()
        .search_plan;
        assert_eq!(plan["tickers"], json!(["ACME"]));
        assert_eq!(plan["universe"], Value::Null);
    }
}
