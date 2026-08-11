//! Fixture-backed, live-provider research-quality acceptance tests.
//!
//! This crate deliberately exercises the production `AgentImage`, canonical
//! contract guard, state interpreter, provider wire, evidence mapper,
//! direct-Markdown output boundary, immutable evidence-ledger receipt, and
//! final-commit boundary together. Only the retrieval endpoint
//! is replaced with a pinned fixture.  That keeps a model-quality regression
//! distinguishable from MCP availability, data freshness, or database issues.
//! It is not a substitute for a real-MCP factual-accuracy evaluation.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use krw_agent_evidence::EvidenceScope;
use krw_agent_image::{CapabilityResultIngest, LoadedImage, compile_agent_dir};
use krw_agent_persistence::{
    ActionDisposition, ActionFinalizationReceipt, ActionReceipt, ActionStage,
    FinalizeActionMutation,
};
use krw_agent_protocol::{
    AuthScope, BudgetLimits, BudgetUsage, CapabilityBinding, ContentHash, DeploymentBinding,
    GLM_MODEL_ID, McpToolSessionReuse, PROTOCOL_VERSION, ProviderWireCapabilities, ReasoningEffort,
    ResolvedExecutionSnapshot, RunRequest, ThinkingMode, TransportKind,
};
use krw_agent_provider_wire::{
    AssistantMessage, ContentBlock, EpisodeContext, MessagesRequest, ProviderEpisodeV1,
    ProviderMessage, ProviderToolDefinition, TokenUsage,
};
use krw_agent_research_planner::canonicalize_normalized_plan_exchange;
use krw_agent_run_engine::{
    ActionIntent, CapabilityInvocation, CapabilityResult, CapabilityRuntime, DeliveryCertainty,
    DependencyFailure, DurableActionObservation, DurableEpisode, DurableFinal, DurableRunState,
    EngineConfig, EngineError, FinalStatus, MarkActionAmbiguous, Persistence, Provider,
    RecoverySnapshot, RunControl, RunEngine, RunIdentity, RunInput, RunOutcome,
    durable_failure_diagnostic,
};
use krw_ontology_adapter::{
    MappingContext, map_company_context, map_market_snapshot, map_research_state,
    map_targeted_query, map_trace, parse_research_state,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use zeroize::Zeroize;

const SUITE_SCHEMA_VERSION: u16 = 4;
const MAX_SUITE_BYTES: usize = 512 * 1024;
const MAX_FIXTURE_BYTES: usize = 2 * 1024 * 1024;
const MAX_CASES: usize = 64;
const MAX_EVAL_PROVIDER_TURNS: u16 = 8;
const MAX_EVAL_CAPABILITY_CALLS: u16 = 4;
const MAX_EVAL_INPUT_TOKENS: u32 = 64_000;
const MAX_EVAL_OUTPUT_TOKENS: u32 = 8_000;
const MAX_EVAL_DEADLINE_MS: u64 = 120_000;
const QUALITY_FENCE: u64 = 1;

/// The content-addressed root of a research-quality suite.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchQualitySuite {
    pub schema_version: u16,
    pub suite_id: String,
    pub suite_version: String,
    pub cases_hash: ContentHash,
    /// Raw-byte hashes for every immutable JSON fixture consumed by a case.
    /// Agent source is intentionally not listed: it is the subject under
    /// evaluation and its immutable image hash is captured in each report.
    pub fixture_hashes: BTreeMap<String, ContentHash>,
    pub case_count: u16,
    #[serde(skip)]
    pub cases: Vec<ResearchQualityCase>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct QualityCasesDocument {
    schema_version: u16,
    cases: Vec<ResearchQualityCase>,
}

/// One bounded, fixture-backed acceptance case.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchQualityCase {
    pub id: String,
    pub agent_dir: String,
    pub run_request: String,
    /// Ordered raw responses for the exact capability sequence this case
    /// expects. The fixture adapter derives semantic decoding from the pinned
    /// `AgentImage` result-ingest declaration, never from capability IDs.
    pub capability_responses: Vec<QualityCapabilityResponse>,
    /// Recorded, typed assistant turns used by the offline full-kernel gate.
    /// The live quality command deliberately ignores these and calls
    /// `DeepSeek` Flash instead.
    pub recorded_provider_turns: Vec<String>,
    pub budget: BudgetLimits,
    pub expected: QualityExpectations,
}

/// One immutable raw capability response used by the quality runtime.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityCapabilityResponse {
    pub capability_id: String,
    pub payload: String,
    pub mode: FixtureResponseMode,
}

/// How a fixture response relates to the invocation. The only dynamic mode
/// is deliberately narrow: a canonical `ResearchState` template may bind its
/// plan and coverage to the accepted root `SearchPlan`, just as the real MCP
/// does. It is not an arbitrary fixture script.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureResponseMode {
    Static,
    MaterializeResearchStatePlanV2,
}

/// Explicit pass criteria.  There is intentionally no model self-score.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityExpectations {
    pub max_provider_turns: u16,
    /// Lower bound on planner re-evaluations. This makes re-search behavior a
    /// measurable outcome rather than a prompt-only expectation.
    pub min_replans: u8,
    pub exact_capability_sequence: Vec<String>,
    pub required_evidence_ids: Vec<String>,
    pub required_rendered_terms: Vec<String>,
    pub forbidden_rendered_terms: Vec<String>,
    pub plan: PlanExpectations,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanExpectations {
    pub ticker: String,
    pub required_document_type: String,
    /// The lower bound makes multi-claim fixtures prove that the initial
    /// set-cover did not silently drop an independently required clause.
    pub min_clauses: u8,
    pub max_clauses: u8,
    /// At least one of these terms must occur in the canonical plan's task,
    /// clause retrieval query, concepts, or predicates.
    pub relevant_terms: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QualityAssertion {
    pub id: String,
    pub passed: bool,
}

/// Redacted provider metadata.  It never retains the prompt, answer, tool
/// arguments, retrieved payload, or private provider reasoning.
#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderTurnReport {
    pub observed_model: String,
    pub requested_model: String,
    pub finish_reason: String,
    pub tool_call_count: usize,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub request_hash: ContentHash,
    pub replay_hash: ContentHash,
    pub tool_argument_shapes: Vec<ToolArgumentShapeReport>,
}

/// Structural report recorded before each provider request. It is safe to
/// retain even when a provider rejects the request: it contains no message text,
/// tool arguments, model output, or private reasoning.
#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderRequestReport {
    pub request_hash: ContentHash,
    pub message_count: usize,
    pub tool_definition_count: usize,
    pub all_tool_definitions_well_formed: bool,
    pub messages: Vec<ProviderMessageShapeReport>,
    /// Closed kernel recovery codes supplied after a model decision was
    /// rejected. These are static identifiers only; no prompt, tool argument,
    /// evidence, or model text is retained.
    pub recovery_reason_codes: Vec<String>,
    /// Safe JSON-pointer locations supplied by the kernel for model repairs.
    /// Only fixed ResearchProposal schema paths are retained.
    pub recovery_fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderMessageShapeReport {
    pub role: String,
    pub content_kind: String,
    pub has_tool_call_id: bool,
    pub tool_call_count: usize,
    pub has_reasoning_content: bool,
}

/// A privacy-preserving structural summary of one model-authored
/// `ResearchProposal`. It reports only known schema field names and collection
/// shapes, never values or unknown field names. The physical `SearchPlan` is
/// assessed separately after the deterministic planner lowers this proposal.
#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)]
pub struct ToolArgumentShapeReport {
    pub provider_tool_name: String,
    /// Whether the provider tool exposes a declared `proposal` envelope whose
    /// value is a `research-proposal/v4`. Non-proposal tools retain only
    /// generic JSON shape metadata; their semantic input validation is
    /// performed by the pinned canonical contract guard in the engine.
    pub model_input_is_research_proposal: bool,
    pub valid_json: bool,
    pub top_level_object: bool,
    pub provider_envelope_is_valid: bool,
    pub recognized_top_level_keys: Vec<String>,
    pub missing_research_proposal_keys: Vec<String>,
    pub unknown_top_level_key_count: usize,
    pub objectives_kind: String,
    pub objective_count: Option<usize>,
    pub first_objective_recognized_keys: Vec<String>,
    pub first_objective_missing_keys: Vec<String>,
    pub first_objective_unknown_key_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityCallReport {
    pub capability_id: String,
    pub action_key_hash: ContentHash,
    pub arguments_hash: ContentHash,
    pub fixture_plan_accepted: bool,
    /// Privacy-preserving outcome of the fixture's semantic plan gate. It
    /// contains only booleans/counts against this public synthetic case, never
    /// model-authored strings, plan values, or retrieved content.
    pub plan_quality: Option<FixturePlanQualityReport>,
    /// Structural comparison of the fixture response plan with the dispatched
    /// plan. It exposes only fixed `SearchPlan` field counts, never plan text.
    pub response_plan_shape: Option<FixtureResponsePlanShapeReport>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)] // Publicly report each independent fixture gate.
pub struct FixturePlanQualityReport {
    pub plan_is_object: bool,
    pub ticker_scope_matches: bool,
    pub required_document_type_present: bool,
    pub clause_count: Option<usize>,
    pub clause_count_within_limit: bool,
    pub has_required_clause: bool,
    pub clause_tickers_within_scope: bool,
    /// Every semantic term required by the fixture appears in the dispatched
    /// plan. This is intentionally an all-terms check so a multi-objective
    /// plan cannot satisfy a dual-claim case with only one side covered.
    pub relevant_term_present: bool,
    pub required_clauses_have_dispatch_fields: bool,
}

/// Content-free structural information about the server-normalized plan echoed
/// by a research-state fixture. This makes a protocol drift debuggable without
/// retaining model terms, the question, or evidence payloads.
#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureResponsePlanShapeReport {
    pub raw_plan_matches_dispatched: bool,
    pub top_level_key_count: Option<usize>,
    pub clause_key_counts: Vec<usize>,
    pub missing_canonical_clause_key_counts: Vec<usize>,
    pub unknown_clause_key_counts: Vec<usize>,
}

/// Machine-readable acceptance result.  The rendered answer is intentionally
/// excluded; callers must request it explicitly through `retain_answer`.
#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchQualityReport {
    pub schema_version: u16,
    pub harness: String,
    pub suite_id: String,
    pub suite_version: String,
    pub case_id: String,
    pub passed: bool,
    pub score: u8,
    /// Every submitted provider request, including requests rejected before a
    /// provider episode exists. This is structural metadata only.
    pub provider_requests: Vec<ProviderRequestReport>,
    pub provider_turns: Vec<ProviderTurnReport>,
    pub capability_calls: Vec<CapabilityCallReport>,
    pub usage: Option<BudgetUsage>,
    pub evidence_count: Option<usize>,
    pub final_status: Option<String>,
    pub final_output_hash: Option<ContentHash>,
    pub evidence_ledger_hash: Option<ContentHash>,
    pub rendered_answer_hash: Option<ContentHash>,
    pub assertions: Vec<QualityAssertion>,
    pub engine_error: Option<String>,
}

/// The answer is kept only when the caller explicitly opts in.  This avoids
/// accidentally persisting potentially private fixture output in CI logs.
pub struct ResearchQualityRun {
    pub report: ResearchQualityReport,
    pub rendered_answer: Option<String>,
}

impl fmt::Debug for ResearchQualityRun {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResearchQualityRun")
            .field("report", &self.report)
            .field(
                "rendered_answer",
                &self.rendered_answer.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

impl Drop for ResearchQualityRun {
    fn drop(&mut self) {
        if let Some(answer) = &mut self.rendered_answer {
            answer.zeroize();
        }
    }
}

#[derive(Debug, Error)]
pub enum QualityError {
    #[error("research-quality suite is invalid: {0}")]
    Suite(String),
    #[error("research-quality fixture is invalid: {0}")]
    Fixture(String),
    #[error(transparent)]
    Image(#[from] krw_agent_image::ImageError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// Load and fail-closed validate a content-addressed suite below `root`.
pub fn load_suite(root: &Path, suite_path: &Path) -> Result<ResearchQualitySuite, QualityError> {
    let suite_dir = resolve_relative(root, suite_path, "suite")?;
    let manifest_value = read_json_value(&suite_dir.join("manifest.json"), MAX_SUITE_BYTES)?;
    let mut suite: ResearchQualitySuite = serde_json::from_value(manifest_value)?;
    if suite.schema_version != SUITE_SCHEMA_VERSION
        || suite.suite_id.is_empty()
        || suite.suite_version.is_empty()
    {
        return Err(QualityError::Suite(
            "unsupported or incomplete manifest".into(),
        ));
    }
    let cases_value = read_json_value(&suite_dir.join("cases.json"), MAX_SUITE_BYTES)?;
    if ContentHash::sha256(serde_jcs::to_vec(&cases_value).map_err(json_fixture_error)?)
        != suite.cases_hash
    {
        return Err(QualityError::Suite("cases_hash mismatch".into()));
    }
    let document: QualityCasesDocument = serde_json::from_value(cases_value)?;
    if document.schema_version != SUITE_SCHEMA_VERSION
        || document.cases.is_empty()
        || document.cases.len() > MAX_CASES
        || usize::from(suite.case_count) != document.cases.len()
    {
        return Err(QualityError::Suite("case document bounds mismatch".into()));
    }
    validate_fixture_hashes(root, &suite.fixture_hashes)?;
    let mut ids = BTreeSet::new();
    for case in &document.cases {
        validate_case(case)?;
        for path in case_fixture_paths(case) {
            if !suite.fixture_hashes.contains_key(path) {
                return Err(QualityError::Suite(
                    "case fixture is not pinned by the suite manifest".into(),
                ));
            }
        }
        if !ids.insert(case.id.clone()) {
            return Err(QualityError::Suite("duplicate case id".into()));
        }
    }
    suite.cases = document.cases;
    Ok(suite)
}

fn validate_fixture_hashes(
    root: &Path,
    fixture_hashes: &BTreeMap<String, ContentHash>,
) -> Result<(), QualityError> {
    if fixture_hashes.is_empty() || fixture_hashes.len() > MAX_CASES.saturating_mul(8) {
        return Err(QualityError::Suite(
            "fixture hash inventory is outside bounds".into(),
        ));
    }
    for (relative, expected) in fixture_hashes {
        let path = resolve_case_path(root, relative, "fixture inventory")?;
        let bytes = read_regular_file(&path, MAX_FIXTURE_BYTES)?;
        if ContentHash::sha256(&bytes) != *expected {
            return Err(QualityError::Suite("pinned fixture hash mismatch".into()));
        }
    }
    Ok(())
}

fn case_fixture_paths(case: &ResearchQualityCase) -> impl Iterator<Item = &str> {
    std::iter::once(case.run_request.as_str())
        .chain(
            case.capability_responses
                .iter()
                .map(|response| response.payload.as_str()),
        )
        .chain(case.recorded_provider_turns.iter().map(String::as_str))
}

/// Execute one full kernel run with a real provider and a deterministic,
/// image-typed capability fixture sequence.
pub async fn run_fixture_case<P>(
    root: &Path,
    suite: &ResearchQualitySuite,
    case_id: &str,
    provider: Arc<P>,
    retain_answer: bool,
) -> Result<ResearchQualityRun, QualityError>
where
    P: Provider + 'static,
{
    run_fixture_case_with_model(
        root,
        suite,
        case_id,
        provider,
        retain_answer,
        GLM_MODEL_ID,
        "glm_high",
    )
    .await
}

/// Execute the same fixture flow against the configured GLM provider. Keeping
/// the model fixed makes the recorded quality suite exercise the same wire
/// contract as local and production releases.
pub async fn run_fixture_case_with_model<P>(
    root: &Path,
    suite: &ResearchQualitySuite,
    case_id: &str,
    provider: Arc<P>,
    retain_answer: bool,
    model_id: &str,
    model_profile: &str,
) -> Result<ResearchQualityRun, QualityError>
where
    P: Provider + 'static,
{
    let case = suite
        .cases
        .iter()
        .find(|case| case.id == case_id)
        .ok_or_else(|| QualityError::Suite("requested case id is absent".into()))?;
    let image = compile_agent_dir(resolve_case_path(root, &case.agent_dir, "agent_dir")?)?
        .into_loaded()
        .map_err(QualityError::Image)?;
    let mut request: RunRequest = serde_json::from_value(read_json_value(
        &resolve_case_path(root, &case.run_request, "run_request")?,
        MAX_FIXTURE_BYTES,
    )?)?;
    validate_request_for_case(&request, case)?;
    if model_id != GLM_MODEL_ID {
        return Err(QualityError::Fixture(
            "quality case model must be the configured GLM provider".into(),
        ));
    }
    request.requested_model = model_id.into();
    request.model_profile = model_profile.into();
    request.budget = case.budget.clone();
    let capability = Arc::new(load_fixture_capability(root, &image, case)?);
    let deployment = fixture_deployment(case, &image, &capability)?;
    let snapshot = fixture_snapshot(&request, &image, &deployment, model_id);
    let recording_provider = Arc::new(RecordingProvider::new(provider));
    let persistence = Arc::new(FixturePersistence::default());
    let config = EngineConfig::production(&image.manifest)
        .map_err(|error| QualityError::Fixture(format!("canonical guard: {error}")))?;
    let engine = RunEngine::new(
        Arc::clone(&recording_provider),
        Arc::clone(&capability),
        persistence,
        config,
    );
    let deadline = Instant::now()
        .checked_add(Duration::from_millis(request.budget.deadline_ms))
        .ok_or_else(|| QualityError::Fixture("deadline overflow".into()))?;
    let outcome = engine
        .run(RunInput {
            image: &image,
            deployment: &deployment,
            resolved_deployment_binding_hash: &snapshot.deployment_binding_hash,
            request: &request,
            snapshot: &snapshot,
            market_snapshot_context: None,
            runtime_timings: None,
            execution_plan: None,
            hard_deadline: deadline,
        })
        .await;
    let provider_requests = recording_provider.requests();
    let provider_turns = recording_provider.turns();
    let capability_calls = capability.calls();
    let mut run = report_from_outcome(
        suite,
        case,
        outcome,
        provider_requests,
        provider_turns,
        capability_calls,
        retain_answer,
        model_id,
    );
    run.report.assertions.push(assertion(
        "fixture_capability_script_consumed",
        capability.is_exhausted(),
    ));
    finalize_report(&mut run.report);
    Ok(run)
}

/// Run a recorded, pinned provider script through the same production kernel
/// path used by a live quality case. This is the hermetic regression gate: it
/// verifies the typed provider wire, `ResearchProposal → SearchPlan` lowering,
/// direct-root capability call, evidence ingestion, the Markdown output
/// boundary, `EvidenceLedger` receipt binding, and final commit without a
/// credential or a network request.
pub async fn run_recorded_fixture_case(
    root: &Path,
    suite: &ResearchQualitySuite,
    case_id: &str,
    retain_answer: bool,
) -> Result<ResearchQualityRun, QualityError> {
    let case = suite
        .cases
        .iter()
        .find(|case| case.id == case_id)
        .ok_or_else(|| QualityError::Suite("requested case id is absent".into()))?;
    let script = case
        .recorded_provider_turns
        .iter()
        .map(|relative| {
            let value = read_json_value(
                &resolve_case_path(root, relative, "recorded provider turn")?,
                MAX_FIXTURE_BYTES,
            )?;
            serde_json::from_value(value).map_err(QualityError::Json)
        })
        .collect::<Result<Vec<AssistantMessage>, QualityError>>()?;
    let provider = Arc::new(RecordedFixtureProvider::new(script));
    let mut run = run_fixture_case_with_model(
        root,
        suite,
        case_id,
        Arc::clone(&provider),
        retain_answer,
        GLM_MODEL_ID,
        "glm_high",
    )
    .await?;
    run.report.assertions.push(assertion(
        "recorded_provider_script_consumed",
        provider.is_exhausted(),
    ));
    finalize_report(&mut run.report);
    Ok(run)
}

fn validate_case(case: &ResearchQualityCase) -> Result<(), QualityError> {
    if !valid_id(&case.id)
        || case.agent_dir.is_empty()
        || case.run_request.is_empty()
        || case.capability_responses.is_empty()
        || case.capability_responses.len() > usize::from(MAX_EVAL_CAPABILITY_CALLS)
        || case.recorded_provider_turns.is_empty()
        || usize::from(case.expected.max_provider_turns) < case.recorded_provider_turns.len()
        || case.expected.exact_capability_sequence.is_empty()
        || case.expected.required_evidence_ids.is_empty()
        || case.expected.required_rendered_terms.is_empty()
        || case.expected.plan.ticker.is_empty()
        || case.expected.plan.required_document_type.is_empty()
        || case.expected.plan.relevant_terms.is_empty()
        || case.expected.plan.min_clauses == 0
        || case.expected.plan.min_clauses > case.expected.plan.max_clauses
        || case.expected.plan.max_clauses > 12
        || case.expected.max_provider_turns == 0
        || case.expected.max_provider_turns > MAX_EVAL_PROVIDER_TURNS
    {
        return Err(QualityError::Suite(
            "case has invalid required fields".into(),
        ));
    }
    if case.budget.max_provider_turns == 0
        || case.budget.max_provider_turns > case.expected.max_provider_turns
        || case.budget.max_provider_turns > MAX_EVAL_PROVIDER_TURNS
        || case.budget.max_capability_calls == 0
        || case.budget.max_capability_calls > MAX_EVAL_CAPABILITY_CALLS
        || case.expected.min_replans > case.budget.max_replans
        || case.budget.max_input_tokens == 0
        || case.budget.max_input_tokens > MAX_EVAL_INPUT_TOKENS
        || case.budget.max_output_tokens == 0
        || case.budget.max_output_tokens > MAX_EVAL_OUTPUT_TOKENS
        || case.budget.deadline_ms == 0
        || case.budget.deadline_ms > MAX_EVAL_DEADLINE_MS
    {
        return Err(QualityError::Suite(
            "case budget exceeds quality safety ceiling".into(),
        ));
    }
    if !case
        .expected
        .exact_capability_sequence
        .iter()
        .map(String::as_str)
        .eq(case
            .capability_responses
            .iter()
            .map(|response| response.capability_id.as_str()))
    {
        return Err(QualityError::Suite(
            "expected capability sequence differs from fixture responses".into(),
        ));
    }
    for relative in [&case.agent_dir, &case.run_request] {
        validate_relative_string(relative)?;
    }
    for response in &case.capability_responses {
        if !valid_id(&response.capability_id) {
            return Err(QualityError::Suite(
                "fixture response capability id is invalid".into(),
            ));
        }
        validate_relative_string(&response.payload)?;
    }
    for turn in &case.recorded_provider_turns {
        validate_relative_string(turn)?;
    }
    Ok(())
}

fn validate_request_for_case(
    request: &RunRequest,
    case: &ResearchQualityCase,
) -> Result<(), QualityError> {
    if request.requested_model != GLM_MODEL_ID || request.question.is_empty() {
        return Err(QualityError::Fixture(
            "quality case must use GLM and a non-empty question".into(),
        ));
    }
    let expected_ticker = &case.expected.plan.ticker;
    if !matches!(&request.context, krw_agent_protocol::RunContextV1::CompanyTickerSet { tickers } if tickers.len() == 1 && tickers[0].as_str() == expected_ticker.as_str())
    {
        return Err(QualityError::Fixture(
            "run request ticker scope differs from case expectation".into(),
        ));
    }
    Ok(())
}

fn validate_research_state_template(template: &Value) -> Result<(), QualityError> {
    let bytes = serde_jcs::to_vec(template).map_err(json_fixture_error)?;
    parse_research_state(&bytes)
        .map_err(|_| QualityError::Fixture("research-state template is not valid v2".into()))?;
    Ok(())
}

fn fixture_release_hash(case_id: &str, capability_id: &str) -> ContentHash {
    ContentHash::sha256(format!(
        "quality-fixture-data-release-v2:{case_id}:{capability_id}"
    ))
}

fn fixture_deployment(
    case: &ResearchQualityCase,
    image: &LoadedImage,
    capability: &FixtureCapabilityRuntime,
) -> Result<DeploymentBinding, QualityError> {
    let capabilities = fixture_bound_capability_ids(case, capability)
        .iter()
        .enumerate()
        .map(|(index, capability_id)| {
            let specification = image
                .manifest
                .body
                .capabilities
                .iter()
                .find(|candidate| candidate.id == *capability_id)
                .ok_or_else(|| {
                    QualityError::Fixture("fixture capability is absent from AgentImage".into())
                })?;
            Ok(CapabilityBinding {
                binding_key: specification.binding_key.clone(),
                mcp_tool_name: format!("quality_fixture_{index}"),
                transport: TransportKind::Native,
                endpoint_ref: "quality-fixture".into(),
                credential_ref: None,
                auth_scope: AuthScope::Tenant,
                tool_session_reuse: McpToolSessionReuse::RunScoped,
                server_schema_bundle_hash: ContentHash::sha256("quality-fixture-schema-v2"),
                server_build: "quality-fixture-v2".into(),
                data_release_hash: fixture_release_hash(&case.id, capability_id),
                max_connections: 1,
                request_timeout_ms: 10_000,
            })
        })
        .collect::<Result<Vec<_>, QualityError>>()?;
    Ok(DeploymentBinding {
        schema_version: 3,
        deployment_id: format!("quality-fixture-{}", case.id),
        capabilities,
    })
}

fn fixture_bound_capability_ids(
    case: &ResearchQualityCase,
    capability: &FixtureCapabilityRuntime,
) -> Vec<String> {
    let mut ids = capability.capability_ids().to_vec();
    let mut seen = ids.iter().cloned().collect::<BTreeSet<_>>();
    for capability_id in case.budget.capability_call_limits.keys() {
        if seen.insert(capability_id.clone()) {
            ids.push(capability_id.clone());
        }
    }
    ids
}

fn fixture_snapshot(
    request: &RunRequest,
    image: &LoadedImage,
    deployment: &DeploymentBinding,
    model_id: &str,
) -> ResolvedExecutionSnapshot {
    let release_hashes = image
        .manifest
        .body
        .capabilities
        .iter()
        .filter_map(|capability| {
            deployment
                .capabilities
                .iter()
                .find(|binding| binding.binding_key == capability.binding_key)
                .map(|binding| (capability.id.clone(), binding.data_release_hash.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    ResolvedExecutionSnapshot {
        protocol_version: PROTOCOL_VERSION,
        run_id: request.run_id.clone(),
        fencing_token: QUALITY_FENCE,
        cancel_generation: 0,
        agent_image_hash: image.content_hash.clone(),
        deployment_binding_hash: ContentHash::sha256(
            serde_jcs::to_vec(deployment).expect("fixture deployment is serializable"),
        ),
        model_registry_hash: ContentHash::sha256("quality-fixture-glm-registry-v1"),
        budget_registry_hash: ContentHash::sha256("quality-fixture-budget-registry-v1"),
        model_profile: request.model_profile.clone(),
        requested_model: model_id.into(),
        resolved_model: model_id.into(),
        provider_api_version: "anthropic-messages-v1".into(),
        provider_max_context_tokens: 204_800,
        provider_wire_capabilities: ProviderWireCapabilities::glm_5_2(),
        thinking: ThinkingMode::Enabled,
        reasoning_effort: Some(ReasoningEffort::High),
        capability_release_hashes: release_hashes,
        budget: request.budget.clone(),
    }
}

#[derive(Debug)]
struct RecordingProvider<P> {
    inner: Arc<P>,
    requests: Mutex<Vec<ProviderRequestReport>>,
    turns: Mutex<Vec<ProviderTurnReport>>,
}

/// A keyless provider implementation for replaying immutable quality turns.
/// It deliberately builds a fresh `ProviderEpisodeV1` from the production
/// request and episode context rather than deserializing a previously formed
/// episode, so request hash, tool-schema hash, model identity, and replay
/// lineage are all verified by the normal engine path.
#[derive(Debug)]
struct RecordedFixtureProvider {
    script: Mutex<VecDeque<AssistantMessage>>,
}

impl RecordedFixtureProvider {
    fn new(script: Vec<AssistantMessage>) -> Self {
        Self {
            script: Mutex::new(script.into()),
        }
    }

    fn is_exhausted(&self) -> bool {
        self.script.lock().is_ok_and(|script| script.is_empty())
    }
}

#[async_trait]
impl Provider for RecordedFixtureProvider {
    async fn complete(
        &self,
        request: &MessagesRequest,
        context: &EpisodeContext,
    ) -> Result<ProviderEpisodeV1, DependencyFailure> {
        let assistant = self
            .script
            .lock()
            .map_err(|_| fixture_dependency("quality_recorded_provider_poisoned"))?
            .pop_front()
            .ok_or_else(|| fixture_dependency("quality_recorded_provider_script_exhausted"))?;
        let finish_reason = if assistant.tool_calls.is_empty() {
            "stop"
        } else {
            "tool_calls"
        };
        let request_hash = ContentHash::sha256(
            serde_jcs::to_vec(request)
                .map_err(|_| fixture_dependency("quality_recorded_request_canonicalization"))?,
        );
        let mut episode = ProviderEpisodeV1 {
            schema_version: 1,
            request_hash,
            requested_model: request.model.clone(),
            observed_model: request.model.clone(),
            api_version: context.api_version.clone(),
            assistant,
            tool_results: Vec::new(),
            tool_schema_hash: context.tool_schema_hash.clone(),
            agent_image_hash: context.agent_image_hash.clone(),
            finish_reason: finish_reason.into(),
            usage: TokenUsage {
                prompt_tokens: 20,
                completion_tokens: 10,
                total_tokens: 30,
                prompt_cache_hit_tokens: 0,
                prompt_cache_miss_tokens: 20,
            },
            replay_hash: ContentHash::sha256("pending"),
        };
        episode.replay_hash = episode
            .calculate_replay_hash()
            .map_err(|_| fixture_dependency("quality_recorded_episode_hash"))?;
        Ok(episode)
    }
}

impl<P> RecordingProvider<P> {
    fn new(inner: Arc<P>) -> Self {
        Self {
            inner,
            requests: Mutex::new(Vec::new()),
            turns: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<ProviderRequestReport> {
        self.requests
            .lock()
            .map_or_else(|_| Vec::new(), |requests| requests.clone())
    }

    fn turns(&self) -> Vec<ProviderTurnReport> {
        self.turns
            .lock()
            .map_or_else(|_| Vec::new(), |turns| turns.clone())
    }
}

#[async_trait]
impl<P> Provider for RecordingProvider<P>
where
    P: Provider,
{
    async fn complete(
        &self,
        request: &MessagesRequest,
        context: &EpisodeContext,
    ) -> Result<ProviderEpisodeV1, DependencyFailure> {
        let request_report = provider_request_report(request)?;
        self.requests
            .lock()
            .map_err(|_| fixture_dependency("quality_request_recorder_poisoned"))?
            .push(request_report);
        let episode = self.inner.complete(request, context).await?;
        let report = ProviderTurnReport {
            observed_model: episode.observed_model.clone(),
            requested_model: episode.requested_model.clone(),
            finish_reason: episode.finish_reason.clone(),
            tool_call_count: episode.assistant.tool_calls.len(),
            prompt_tokens: episode.usage.prompt_tokens,
            completion_tokens: episode.usage.completion_tokens,
            total_tokens: episode.usage.total_tokens,
            request_hash: episode.request_hash.clone(),
            replay_hash: episode.replay_hash.clone(),
            tool_argument_shapes: episode
                .assistant
                .tool_calls
                .iter()
                .map(|call| {
                    tool_argument_shape(
                        call.function.name.as_str(),
                        &call.function.arguments,
                        request
                            .tools
                            .iter()
                            .find(|tool| tool.name() == call.function.name.as_str())
                            .is_some_and(provider_tool_uses_research_proposal),
                    )
                })
                .collect(),
        };
        self.turns
            .lock()
            .map_err(|_| fixture_dependency("quality_turn_recorder_poisoned"))?
            .push(report);
        Ok(episode)
    }
}

fn provider_request_report(
    request: &MessagesRequest,
) -> Result<ProviderRequestReport, DependencyFailure> {
    let request_hash = ContentHash::sha256(
        serde_jcs::to_vec(request)
            .map_err(|_| fixture_dependency("quality_request_canonicalization_failed"))?,
    );
    Ok(ProviderRequestReport {
        request_hash,
        message_count: request.messages.len(),
        tool_definition_count: request.tools.len(),
        all_tool_definitions_well_formed: request
            .tools
            .iter()
            .all(provider_tool_definition_is_well_formed),
        messages: request
            .messages
            .iter()
            .map(provider_message_shape)
            .collect(),
        recovery_reason_codes: request
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|block| match block {
                ContentBlock::ToolResult { content, .. } => recovery_reason_code(content),
                ContentBlock::Text { .. }
                | ContentBlock::ToolUse { .. }
                | ContentBlock::Thinking { .. } => None,
            })
            .collect(),
        recovery_fields: request
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|block| match block {
                ContentBlock::ToolResult { content, .. } => recovery_field(content),
                ContentBlock::Text { .. }
                | ContentBlock::ToolUse { .. }
                | ContentBlock::Thinking { .. } => None,
            })
            .collect(),
    })
}

fn recovery_reason_code(content: &str) -> Option<String> {
    let payload: Value = serde_json::from_str(content).ok()?;
    let object = payload.as_object()?;
    if object.get("status").and_then(Value::as_str) != Some("recovery_required")
        || object.get("class").and_then(Value::as_str) != Some("model_correctable")
    {
        return None;
    }
    let reason_code = object.get("reason_code").and_then(Value::as_str)?;
    (reason_code.len() <= 96
        && reason_code
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'))
    .then(|| reason_code.to_owned())
}

fn recovery_field(content: &str) -> Option<String> {
    let payload: Value = serde_json::from_str(content).ok()?;
    let object = payload.as_object()?;
    if object.get("status").and_then(Value::as_str) != Some("recovery_required")
        || object.get("class").and_then(Value::as_str) != Some("model_correctable")
    {
        return None;
    }
    let field = object
        .get("detail")
        .and_then(Value::as_object)?
        .get("field")
        .and_then(Value::as_str)?;
    safe_research_proposal_field(field).then(|| field.to_owned())
}

fn safe_research_proposal_field(field: &str) -> bool {
    if matches!(
        field,
        "/" | "/answer_scope" | "/uncertainty" | "/objectives"
    ) {
        return true;
    }
    let Some(rest) = field.strip_prefix("/objectives/") else {
        return false;
    };
    let Some((index, suffix)) = rest.split_once('/') else {
        return false;
    };
    if index.is_empty() || !index.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    matches!(
        suffix,
        "priority"
            | "directness"
            | "goal/kind"
            | "goal/metric"
            | "goal/change"
            | "goal/window"
            | "goal/concepts"
            | "goal/predicates"
    )
}

fn provider_message_shape(message: &ProviderMessage) -> ProviderMessageShapeReport {
    // Under the Anthropic Messages API every turn is `{role, content:
    // Vec<ContentBlock>}`. We collapse the block list to a single shape report
    // keyed off the first non-trivial block, which is sufficient for the
    // provider-shape quality gates this helper feeds.
    let role = match message.role {
        krw_agent_provider_wire::MessageRole::User => "user",
        krw_agent_provider_wire::MessageRole::Assistant => "assistant",
    };
    let mut content_kind = "empty".to_string();
    let mut has_tool_call_id = false;
    let mut tool_call_count = 0usize;
    let mut has_reasoning_content = false;
    for block in &message.content {
        match block {
            ContentBlock::Text { .. } => {
                if content_kind == "empty" {
                    content_kind = "string".into();
                }
            }
            ContentBlock::ToolUse { .. } => {
                tool_call_count += 1;
                if content_kind == "empty" {
                    content_kind = "tool_use".into();
                }
            }
            ContentBlock::ToolResult { tool_use_id, .. } => {
                has_tool_call_id = !tool_use_id.is_empty();
                if content_kind == "empty" {
                    content_kind = "canonical_json_text".into();
                }
            }
            ContentBlock::Thinking { thinking, .. } => {
                has_reasoning_content = !thinking.is_empty();
                if content_kind == "empty" {
                    content_kind = "thinking".into();
                }
            }
        }
    }
    if message.role == krw_agent_provider_wire::MessageRole::Assistant && content_kind == "string" {
        content_kind = "string_or_null".into();
    }
    ProviderMessageShapeReport {
        role: role.into(),
        content_kind,
        has_tool_call_id,
        tool_call_count,
        has_reasoning_content,
    }
}

fn provider_tool_definition_is_well_formed(tool: &ProviderToolDefinition) -> bool {
    let name = tool.name();
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && tool.input_schema.as_value().is_object()
}

const RESEARCH_PROPOSAL_KEYS: &[&str] = &[
    "answer_scope",
    "document_types",
    "intent",
    "periods",
    "objectives",
    "uncertainty",
];

const RESEARCH_PROPOSAL_OBJECTIVE_KEYS: &[&str] = &[
    "alternatives",
    "directness",
    "goal",
    "object_types",
    "priority",
];

fn provider_tool_uses_research_proposal(tool: &ProviderToolDefinition) -> bool {
    let parameters = tool.input_schema.as_value();
    let proposal_required = parameters["properties"]["proposal"]["required"].as_array();
    let proposal_has_exact_required_fields = proposal_required.is_some_and(|actual| {
        actual.len() == RESEARCH_PROPOSAL_KEYS.len()
            && RESEARCH_PROPOSAL_KEYS.iter().all(|expected| {
                actual
                    .iter()
                    .any(|actual| actual.as_str() == Some(expected))
            })
    });
    parameters["type"].as_str() == Some("object")
        && parameters["additionalProperties"].as_bool() == Some(false)
        && parameters["required"] == serde_json::json!(["proposal"])
        && proposal_has_exact_required_fields
        && parameters["$defs"].is_object()
}

fn tool_argument_shape(
    provider_tool_name: &str,
    arguments: &str,
    model_input_is_research_proposal: bool,
) -> ToolArgumentShapeReport {
    let parsed: Value = match serde_json::from_str(arguments) {
        Ok(value) => value,
        Err(_) => {
            return empty_tool_shape(
                provider_tool_name,
                model_input_is_research_proposal,
                false,
                false,
            );
        }
    };
    let Some(object) = parsed.as_object() else {
        return empty_tool_shape(
            provider_tool_name,
            model_input_is_research_proposal,
            true,
            false,
        );
    };
    if !model_input_is_research_proposal {
        return ToolArgumentShapeReport {
            provider_tool_name: provider_tool_name.into(),
            model_input_is_research_proposal: false,
            valid_json: true,
            top_level_object: true,
            provider_envelope_is_valid: false,
            recognized_top_level_keys: Vec::new(),
            missing_research_proposal_keys: Vec::new(),
            unknown_top_level_key_count: 0,
            objectives_kind: "not_applicable".into(),
            objective_count: None,
            first_objective_recognized_keys: Vec::new(),
            first_objective_missing_keys: Vec::new(),
            first_objective_unknown_key_count: 0,
        };
    }
    let provider_envelope_is_valid = object.len() == 1 && object.contains_key("proposal");
    let proposal = object.get("proposal").and_then(Value::as_object);
    let (recognized_top_level_keys, missing_research_proposal_keys, unknown_top_level_key_count) =
        proposal.map_or_else(
            || {
                (
                    Vec::new(),
                    RESEARCH_PROPOSAL_KEYS
                        .iter()
                        .map(|key| (*key).into())
                        .collect(),
                    0,
                )
            },
            |proposal| shape_keys(proposal, RESEARCH_PROPOSAL_KEYS),
        );
    let (
        objectives_kind,
        objective_count,
        first_objective_recognized_keys,
        first_objective_missing_keys,
        first_objective_unknown_key_count,
    ) = match proposal.and_then(|proposal| proposal.get("objectives")) {
        Some(Value::Array(objectives)) => {
            let first = objectives.first().and_then(Value::as_object);
            let (known, missing, unknown) = first.map_or_else(
                || {
                    (
                        Vec::new(),
                        RESEARCH_PROPOSAL_OBJECTIVE_KEYS
                            .iter()
                            .map(|key| (*key).into())
                            .collect(),
                        0,
                    )
                },
                |objective| shape_keys(objective, RESEARCH_PROPOSAL_OBJECTIVE_KEYS),
            );
            (
                "array".into(),
                Some(objectives.len()),
                known,
                missing,
                unknown,
            )
        }
        Some(Value::Object(_)) => ("object".into(), None, Vec::new(), Vec::new(), 0),
        Some(Value::Null) => ("null".into(), None, Vec::new(), Vec::new(), 0),
        Some(_) => ("other".into(), None, Vec::new(), Vec::new(), 0),
        None => ("missing".into(), None, Vec::new(), Vec::new(), 0),
    };
    ToolArgumentShapeReport {
        provider_tool_name: provider_tool_name.into(),
        model_input_is_research_proposal: true,
        valid_json: true,
        top_level_object: true,
        provider_envelope_is_valid,
        recognized_top_level_keys,
        missing_research_proposal_keys,
        unknown_top_level_key_count,
        objectives_kind,
        objective_count,
        first_objective_recognized_keys,
        first_objective_missing_keys,
        first_objective_unknown_key_count,
    }
}

fn empty_tool_shape(
    provider_tool_name: &str,
    model_input_is_research_proposal: bool,
    valid_json: bool,
    top_level_object: bool,
) -> ToolArgumentShapeReport {
    ToolArgumentShapeReport {
        provider_tool_name: provider_tool_name.into(),
        model_input_is_research_proposal,
        valid_json,
        top_level_object,
        provider_envelope_is_valid: false,
        recognized_top_level_keys: Vec::new(),
        missing_research_proposal_keys: RESEARCH_PROPOSAL_KEYS
            .iter()
            .map(|key| (*key).into())
            .collect(),
        unknown_top_level_key_count: 0,
        objectives_kind: "unavailable".into(),
        objective_count: None,
        first_objective_recognized_keys: Vec::new(),
        first_objective_missing_keys: RESEARCH_PROPOSAL_OBJECTIVE_KEYS
            .iter()
            .map(|key| (*key).into())
            .collect(),
        first_objective_unknown_key_count: 0,
    }
}

fn shape_keys(
    object: &serde_json::Map<String, Value>,
    expected: &[&str],
) -> (Vec<String>, Vec<String>, usize) {
    let expected = expected.iter().copied().collect::<BTreeSet<_>>();
    let recognized = object
        .keys()
        .filter(|key| expected.contains(key.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let missing = expected
        .iter()
        .filter(|key| !object.contains_key(**key))
        .map(|key| (*key).to_owned())
        .collect::<Vec<_>>();
    let unknown_count = object.len().saturating_sub(recognized.len());
    (recognized, missing, unknown_count)
}

#[derive(Debug, Clone)]
struct FixtureCapabilityResponse {
    capability_id: String,
    result_ingest: CapabilityResultIngest,
    payload: Value,
    mode: FixtureResponseMode,
}

#[derive(Debug)]
struct FixtureCapabilityRuntime {
    responses: Mutex<VecDeque<FixtureCapabilityResponse>>,
    bound_capability_ids: Vec<String>,
    expected: QualityExpectations,
    calls: Mutex<Vec<CapabilityCallReport>>,
}

fn load_fixture_capability(
    root: &Path,
    image: &LoadedImage,
    case: &ResearchQualityCase,
) -> Result<FixtureCapabilityRuntime, QualityError> {
    let mut responses = VecDeque::new();
    let mut bound_capability_ids = Vec::new();
    let mut bound_ids = BTreeSet::new();
    for response in &case.capability_responses {
        let specification = image
            .manifest
            .body
            .capabilities
            .iter()
            .find(|candidate| candidate.id == response.capability_id)
            .ok_or_else(|| {
                QualityError::Fixture("fixture capability is absent from AgentImage".into())
            })?;
        let payload = match response.mode {
            FixtureResponseMode::MaterializeResearchStatePlanV2
                if specification.result_ingest == CapabilityResultIngest::ResearchStateV2 =>
            {
                let template = read_json_value(
                    &resolve_case_path(root, &response.payload, "research-state payload")?,
                    MAX_FIXTURE_BYTES,
                )?;
                validate_research_state_template(&template)?;
                template
            }
            FixtureResponseMode::Static => match specification.result_ingest {
                CapabilityResultIngest::ResearchStateV2 => {
                    let template = read_json_value(
                        &resolve_case_path(root, &response.payload, "research-state payload")?,
                        MAX_FIXTURE_BYTES,
                    )?;
                    validate_research_state_template(&template)?;
                    template
                }
                CapabilityResultIngest::CompanyContextV1
                | CapabilityResultIngest::MarketSnapshotV1
                | CapabilityResultIngest::TargetedEvidenceV1
                | CapabilityResultIngest::TraceLineageV1 => read_json_value(
                    &resolve_case_path(root, &response.payload, "supplemental payload")?,
                    MAX_FIXTURE_BYTES,
                )?,
                _ => {
                    return Err(QualityError::Fixture(
                        "fixture response result ingest/mode is not supported by research quality"
                            .into(),
                    ));
                }
            },
            FixtureResponseMode::MaterializeResearchStatePlanV2 => {
                return Err(QualityError::Fixture(
                    "fixture response result ingest/mode is not supported by research quality"
                        .into(),
                ));
            }
        };
        responses.push_back(FixtureCapabilityResponse {
            capability_id: response.capability_id.clone(),
            result_ingest: specification.result_ingest,
            payload,
            mode: response.mode,
        });
        if bound_ids.insert(response.capability_id.clone()) {
            bound_capability_ids.push(response.capability_id.clone());
        }
    }
    Ok(FixtureCapabilityRuntime {
        responses: Mutex::new(responses),
        bound_capability_ids,
        expected: case.expected.clone(),
        calls: Mutex::new(Vec::new()),
    })
}

impl FixtureCapabilityRuntime {
    fn capability_ids(&self) -> &[String] {
        &self.bound_capability_ids
    }

    fn is_exhausted(&self) -> bool {
        self.responses
            .lock()
            .is_ok_and(|responses| responses.is_empty())
    }

    fn calls(&self) -> Vec<CapabilityCallReport> {
        self.calls
            .lock()
            .map_or_else(|_| Vec::new(), |calls| calls.clone())
    }

    fn record_call(
        &self,
        invocation: &CapabilityInvocation,
        fixture_plan_accepted: bool,
        plan_quality: Option<FixturePlanQualityReport>,
        response_plan_shape: Option<FixtureResponsePlanShapeReport>,
    ) -> Result<(), DependencyFailure> {
        self.calls
            .lock()
            .map_err(|_| fixture_dependency("quality_capability_recorder_poisoned"))?
            .push(CapabilityCallReport {
                capability_id: invocation.capability_id.clone(),
                action_key_hash: ContentHash::sha256(&invocation.action_key),
                arguments_hash: ContentHash::sha256(
                    serde_jcs::to_vec(&invocation.arguments)
                        .map_err(|_| fixture_dependency("quality_plan_canonicalization"))?,
                ),
                fixture_plan_accepted,
                plan_quality,
                response_plan_shape,
            });
        Ok(())
    }

    fn mapping_context(
        invocation: &CapabilityInvocation,
        payload_ref: ContentHash,
    ) -> MappingContext {
        MappingContext {
            capability_id: invocation.capability_id.clone(),
            action_key: invocation.action_key.clone(),
            server_build: invocation.binding.server_build.clone(),
            normalized_contract_hash: invocation.normalized_output_contract_hash.clone(),
            server_schema_bundle_hash: invocation.binding.server_schema_bundle_hash.clone(),
            data_release_hash: invocation.binding.data_release_hash.clone(),
            scope: EvidenceScope {
                auth_scope: invocation.binding.auth_scope.clone(),
                scope_hash: ContentHash::sha256("quality-fixture-tenant-scope-v1"),
            },
            payload_ref,
        }
    }

    fn map_response(
        response: &FixtureCapabilityResponse,
        invocation: &CapabilityInvocation,
        payload: Value,
    ) -> Result<CapabilityResult, DependencyFailure> {
        let payload_bytes = serde_jcs::to_vec(&payload)
            .map_err(|_| fixture_dependency("quality_fixture_payload_canonicalization"))?;
        let context = Self::mapping_context(invocation, ContentHash::sha256(&payload_bytes));
        match response.result_ingest {
            CapabilityResultIngest::ResearchStateV2 => {
                let state = parse_research_state(&payload_bytes)
                    .map_err(|_| fixture_dependency("quality_fixture_payload_invalid"))?;
                let delta = map_research_state(&state, &context)
                    .map_err(|_| fixture_dependency("quality_fixture_evidence_mapping"))?;
                Ok(CapabilityResult {
                    provider_content: payload,
                    evidence: delta.records,
                    answerability: Some(delta.answerability),
                    calculations: delta.calculations,
                })
            }
            CapabilityResultIngest::CompanyContextV1 => {
                let ticker = invocation
                    .arguments
                    .get("ticker")
                    .and_then(Value::as_str)
                    .ok_or_else(|| fixture_dependency("quality_fixture_company_context_ticker"))?;
                let delta = map_company_context(&payload, ticker, &context)
                    .map_err(|_| fixture_dependency("quality_fixture_company_context_mapping"))?;
                Ok(CapabilityResult {
                    provider_content: delta.provider_content,
                    evidence: delta.records,
                    answerability: None,
                    calculations: Vec::new(),
                })
            }
            CapabilityResultIngest::MarketSnapshotV1 => {
                let ticker = invocation
                    .arguments
                    .get("ticker")
                    .and_then(Value::as_str)
                    .ok_or_else(|| fixture_dependency("quality_fixture_market_snapshot_ticker"))?;
                let delta = map_market_snapshot(&payload, ticker, &context)
                    .map_err(|_| fixture_dependency("quality_fixture_market_snapshot_mapping"))?;
                Ok(CapabilityResult {
                    provider_content: delta.provider_content,
                    evidence: delta.records,
                    answerability: None,
                    calculations: Vec::new(),
                })
            }
            CapabilityResultIngest::TargetedEvidenceV1 => {
                let delta = map_targeted_query(&payload, &context)
                    .map_err(|_| fixture_dependency("quality_fixture_targeted_mapping"))?;
                Ok(CapabilityResult {
                    provider_content: payload,
                    evidence: delta.records,
                    answerability: None,
                    calculations: delta.calculations,
                })
            }
            CapabilityResultIngest::TraceLineageV1 => {
                let delta = map_trace(&payload, &context)
                    .map_err(|_| fixture_dependency("quality_fixture_trace_mapping"))?;
                Ok(CapabilityResult {
                    provider_content: payload,
                    evidence: delta.records,
                    answerability: None,
                    calculations: delta.calculations,
                })
            }
            CapabilityResultIngest::FrontFeedListItemsV1
            | CapabilityResultIngest::FrontFeedGetItemsV1
            | CapabilityResultIngest::FrontFeedContextV2
            | CapabilityResultIngest::FrontFilingSearchV1
            | CapabilityResultIngest::FrontFilingMetadataV1
            | CapabilityResultIngest::FrontFilingBriefV1
            | CapabilityResultIngest::FrontFilingSectionsV1
            | CapabilityResultIngest::FrontFilingSectionTextV1
            | CapabilityResultIngest::FrontFilingDocumentsV1
            | CapabilityResultIngest::FrontFilingDocumentTextV1
            | CapabilityResultIngest::FrontForm4TransactionsV1
            | CapabilityResultIngest::GuruQueryContextV1
            | CapabilityResultIngest::GuruCompanyBriefV1
            | CapabilityResultIngest::GuruEvidenceReviewV1
            | CapabilityResultIngest::SkillContentV1 => Err(fixture_dependency(
                "quality_fixture_result_ingest_unavailable",
            )),
        }
    }
}

#[async_trait]
impl CapabilityRuntime for FixtureCapabilityRuntime {
    async fn invoke(
        &self,
        invocation: &CapabilityInvocation,
    ) -> Result<CapabilityResult, DependencyFailure> {
        let response = self
            .responses
            .lock()
            .map_err(|_| fixture_dependency("quality_capability_script_poisoned"))?
            .pop_front()
            .ok_or_else(|| fixture_dependency("quality_fixture_capability_script_exhausted"))?;
        if response.capability_id != invocation.capability_id {
            self.record_call(invocation, false, None, None)?;
            return Err(fixture_dependency(
                "quality_fixture_capability_sequence_mismatch",
            ));
        }
        let (payload, plan_quality, plan_valid) = match response.mode {
            FixtureResponseMode::Static => (response.payload.clone(), None, true),
            FixtureResponseMode::MaterializeResearchStatePlanV2 => {
                let quality = assess_fixture_plan(&invocation.arguments, &self.expected);
                let valid = quality.accepted();
                let payload = valid
                    .then(|| {
                        materialize_research_state(
                            &response.payload,
                            &invocation.arguments,
                            &self.expected,
                        )
                    })
                    .transpose()?;
                (
                    payload.unwrap_or_else(|| response.payload.clone()),
                    Some(quality),
                    valid,
                )
            }
        };
        let response_plan_shape = fixture_response_plan_shape(&payload, &invocation.arguments);
        self.record_call(invocation, plan_valid, plan_quality, response_plan_shape)?;
        if !plan_valid {
            return Err(fixture_dependency("quality_fixture_plan_rejected"));
        }
        Self::map_response(&response, invocation, payload)
    }
}

impl FixturePlanQualityReport {
    fn accepted(&self) -> bool {
        self.plan_is_object
            && self.ticker_scope_matches
            && self.required_document_type_present
            && self.clause_count_within_limit
            && self.has_required_clause
            && self.clause_tickers_within_scope
            && self.relevant_term_present
            && self.required_clauses_have_dispatch_fields
    }
}

fn assess_fixture_plan(plan: &Value, expected: &QualityExpectations) -> FixturePlanQualityReport {
    let Some(object) = plan.as_object() else {
        return FixturePlanQualityReport {
            plan_is_object: false,
            ticker_scope_matches: false,
            required_document_type_present: false,
            clause_count: None,
            clause_count_within_limit: false,
            has_required_clause: false,
            clause_tickers_within_scope: false,
            relevant_term_present: false,
            required_clauses_have_dispatch_fields: false,
        };
    };
    let ticker_scope_matches = string_array(object.get("tickers"))
        .is_ok_and(|tickers| tickers.as_slice() == [expected.plan.ticker.as_str()]);
    let required_document_type_present =
        string_array(object.get("document_types")).is_ok_and(|document_types| {
            document_types.iter().any(|document_type| {
                document_type.eq_ignore_ascii_case(&expected.plan.required_document_type)
            })
        });
    let clauses = object.get("clauses").and_then(Value::as_array);
    let clause_count = clauses.map(Vec::len);
    let clause_count_within_limit = clause_count.is_some_and(|count| {
        (usize::from(expected.plan.min_clauses)..=usize::from(expected.plan.max_clauses))
            .contains(&count)
    });
    let has_required_clause = clauses.is_some_and(|clauses| {
        clauses
            .iter()
            .any(|clause| clause.get("required").and_then(Value::as_bool) == Some(true))
    });
    let clause_tickers_within_scope = clauses.is_some_and(|clauses| {
        clauses.iter().all(|clause| {
            clause
                .get("tickers")
                .and_then(Value::as_array)
                .is_none_or(|tickers| {
                    tickers
                        .iter()
                        .all(|ticker| ticker.as_str() == Some(&expected.plan.ticker))
                })
        })
    });
    let required_clauses_have_dispatch_fields = clauses.is_some_and(|clauses| {
        clauses.iter().all(|clause| {
            clause.get("required").and_then(Value::as_bool) != Some(true)
                || (clause
                    .get("clause_id")
                    .and_then(Value::as_str)
                    .is_some_and(|value| !value.is_empty())
                    && matches!(
                        clause.get("directness").and_then(Value::as_str),
                        Some("any" | "direct_preferred" | "direct_required")
                    ))
        })
    });
    let mut haystack = Vec::new();
    collect_plan_strings(plan, &mut haystack);
    let haystack = haystack.join(" ").to_lowercase();
    let relevant_term_present = expected
        .plan
        .relevant_terms
        .iter()
        .all(|term| haystack.contains(&term.to_lowercase()));
    FixturePlanQualityReport {
        plan_is_object: true,
        ticker_scope_matches,
        required_document_type_present,
        clause_count,
        clause_count_within_limit,
        has_required_clause,
        clause_tickers_within_scope,
        relevant_term_present,
        required_clauses_have_dispatch_fields,
    }
}

fn fixture_response_plan_shape(
    payload: &Value,
    dispatched_plan: &Value,
) -> Option<FixtureResponsePlanShapeReport> {
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
    let plan = payload.get("plan")?;
    let object = plan.as_object()?;
    let clauses = object.get("clauses")?.as_array()?;
    let mut clause_key_counts = Vec::with_capacity(clauses.len());
    let mut missing_canonical_clause_key_counts = Vec::with_capacity(clauses.len());
    let mut unknown_clause_key_counts = Vec::with_capacity(clauses.len());
    for clause in clauses {
        let Some(clause) = clause.as_object() else {
            clause_key_counts.push(0);
            missing_canonical_clause_key_counts.push(CLAUSE_KEYS.len());
            unknown_clause_key_counts.push(0);
            continue;
        };
        clause_key_counts.push(clause.len());
        missing_canonical_clause_key_counts.push(
            CLAUSE_KEYS
                .iter()
                .filter(|key| !clause.contains_key(**key))
                .count(),
        );
        unknown_clause_key_counts.push(
            clause
                .keys()
                .filter(|key| !CLAUSE_KEYS.contains(&key.as_str()))
                .count(),
        );
    }
    Some(FixtureResponsePlanShapeReport {
        raw_plan_matches_dispatched: plan == dispatched_plan,
        top_level_key_count: Some(object.len()),
        clause_key_counts,
        missing_canonical_clause_key_counts,
        unknown_clause_key_counts,
    })
}

#[cfg(test)]
fn validate_fixture_plan(plan: &Value, expected: &QualityExpectations) -> Result<(), ()> {
    assess_fixture_plan(plan, expected)
        .accepted()
        .then_some(())
        .ok_or(())
}

fn string_array(value: Option<&Value>) -> Result<Vec<&str>, ()> {
    value
        .and_then(Value::as_array)
        .ok_or(())?
        .iter()
        .map(|value| value.as_str().ok_or(()))
        .collect()
}

fn collect_plan_strings(value: &Value, output: &mut Vec<String>) {
    match value {
        Value::String(value) => output.push(value.clone()),
        Value::Array(values) => {
            for value in values {
                collect_plan_strings(value, output);
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                collect_plan_strings(value, output);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

#[derive(Debug, Clone)]
struct FixtureClauseEvidence {
    evidence_ids: Vec<String>,
    covered_tickers: Vec<String>,
    best_directness: Option<String>,
    best_evidence_grade: Option<String>,
    directness_satisfied: bool,
    strong_claim_ready: bool,
}

fn materialize_research_state(
    template: &Value,
    plan: &Value,
    expected: &QualityExpectations,
) -> Result<Value, DependencyFailure> {
    // The real ontology server parses `SearchPlan` through Pydantic and
    // serializes its documented defaults in the returned `ResearchState`.
    // Mirror that boundary here: the fixture must exercise the same response
    // shape as production, while the engine still independently verifies the
    // normalized plan exchange.
    let canonical_plan = canonicalize_normalized_plan_exchange(plan, plan)
        .map_err(|_| fixture_dependency("quality_fixture_plan_normalization"))?;
    let mut payload = template.clone();
    // Fixture evidence is authored against readable semantic template clauses,
    // while the provider may expand one user question into several clauses.
    // Align every dispatched clause with a semantically related template
    // clause, then copy the fixture evidence linkage to that dispatched ID.
    // Unrelated extra clauses receive no linkage and therefore remain
    // uncovered instead of being silently accepted.
    align_fixture_clause_references(&mut payload, &canonical_plan, expected)?;
    let required = canonical_plan
        .get("clauses")
        .and_then(Value::as_array)
        .ok_or_else(|| fixture_dependency("quality_fixture_plan_shape"))?
        .iter()
        .filter(|clause| clause.get("required").and_then(Value::as_bool) == Some(true))
        .map(|clause| {
            let id = clause
                .get("clause_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| fixture_dependency("quality_fixture_clause_id"))?;
            let directness = clause
                .get("directness")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| fixture_dependency("quality_fixture_clause_directness"))?;
            Ok((id.to_owned(), directness.to_owned()))
        })
        .collect::<Result<Vec<_>, DependencyFailure>>()?;
    if required.is_empty() {
        return Err(fixture_dependency("quality_fixture_no_required_clause"));
    }
    let object = payload
        .as_object_mut()
        .ok_or_else(|| fixture_dependency("quality_fixture_template_shape"))?;
    let evidence_units = object
        .get("evidence_units")
        .and_then(Value::as_array)
        .ok_or_else(|| fixture_dependency("quality_fixture_evidence_shape"))?;
    let clause_evidence = required
        .iter()
        .map(|(clause_id, directness)| {
            (
                clause_id.clone(),
                directness.clone(),
                fixture_evidence_for_clause(evidence_units, clause_id, directness),
            )
        })
        .collect::<Vec<_>>();
    let covered_required = clause_evidence
        .iter()
        .filter(|(_, _, evidence)| evidence.directness_satisfied)
        .count();
    let all_required_covered = covered_required == required.len();
    let all_required_strong = clause_evidence
        .iter()
        .all(|(_, _, evidence)| evidence.strong_claim_ready);
    object.insert("plan".into(), canonical_plan);
    object.insert(
        "answerability".into(),
        serde_json::json!({
            "status": if all_required_covered { "answerable" } else if covered_required == 0 { "not_answerable" } else { "partial" },
            "strong_claim_allowed": all_required_covered && all_required_strong,
            "required_clause_count": required.len(),
            "covered_required_clause_count": covered_required,
            "requires_direct_evidence": required.iter().any(|(_, directness)| directness == "direct_required"),
            "reason_codes": if all_required_covered { Vec::<String>::new() } else { vec!["fixture_missing_clause_evidence".to_owned()] }
        }),
    );
    object.insert(
        "clause_coverage".into(),
        Value::Array(
            clause_evidence
                .iter()
                .map(|(clause_id, directness, evidence)| {
                    let status = if evidence.directness_satisfied {
                        "covered"
                    } else if evidence.evidence_ids.is_empty() {
                        "missing"
                    } else {
                        "partial"
                    };
                    serde_json::json!({
                        "clause_id": clause_id,
                        "required": true,
                        "directness_required": directness,
                        "status": status,
                        "evidence_ids": evidence.evidence_ids,
                        "covered_tickers": evidence.covered_tickers,
                        "missing_tickers": [],
                        "best_directness": evidence.best_directness,
                        "best_evidence_grade": evidence.best_evidence_grade,
                        "strong_claim_ready": evidence.strong_claim_ready,
                        "reason": if evidence.directness_satisfied { Value::Null } else { Value::String("fixture evidence does not meet clause directness".into()) }
                    })
                })
                .collect(),
        ),
    );
    Ok(payload)
}

fn align_fixture_clause_references(
    payload: &mut Value,
    plan: &Value,
    expected: &QualityExpectations,
) -> Result<(), DependencyFailure> {
    let template_clauses = payload
        .get("plan")
        .and_then(|value| value.get("clauses"))
        .and_then(Value::as_array)
        .ok_or_else(|| fixture_dependency("quality_fixture_template_plan_shape"))?
        .clone();
    let dispatched_clauses = plan
        .get("clauses")
        .and_then(Value::as_array)
        .ok_or_else(|| fixture_dependency("quality_fixture_plan_shape"))?;
    if template_clauses.is_empty() || dispatched_clauses.is_empty() {
        return Err(fixture_dependency("quality_fixture_clause_cardinality"));
    }

    let mut aligned = BTreeMap::<String, Vec<String>>::new();
    for dispatched in dispatched_clauses {
        let Some(dispatched_id) = dispatched.get("clause_id").and_then(Value::as_str) else {
            continue;
        };
        let Some((template_id, _score)) = template_clauses
            .iter()
            .filter_map(|template| {
                let template_id = template.get("clause_id").and_then(Value::as_str)?;
                let score = fixture_clause_similarity(template, dispatched, expected);
                (score > 0).then_some((template_id, score))
            })
            .max_by_key(|(_, score)| *score)
        else {
            continue;
        };
        aligned
            .entry(template_id.to_owned())
            .or_default()
            .push(dispatched_id.to_owned());
    }

    let primary_replacements = aligned
        .iter()
        .filter_map(|(source, targets)| {
            targets
                .first()
                .map(|target| (source.clone(), target.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    replace_fixture_clause_references(payload, &primary_replacements);

    let evidence_units = payload
        .get_mut("evidence_units")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| fixture_dependency("quality_fixture_evidence_shape"))?;
    for unit in evidence_units {
        let supports = unit
            .get("supports_clause_ids")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut aligned_supports = Vec::new();
        for source in supports.iter().filter_map(Value::as_str) {
            if let Some(targets) = aligned
                .values()
                .find(|targets| targets.iter().any(|target| target == source))
            {
                aligned_supports.extend(targets.iter().cloned().map(Value::String));
            }
        }
        deduplicate_json_strings(&mut aligned_supports);
        unit["supports_clause_ids"] = Value::Array(aligned_supports);

        let matches = unit
            .get("clause_matches")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut aligned_matches = Vec::new();
        for entry in matches {
            let Some(source) = entry.get("clause_id").and_then(Value::as_str) else {
                continue;
            };
            let Some(targets) = aligned
                .values()
                .find(|targets| targets.iter().any(|target| target == source))
            else {
                continue;
            };
            for target in targets {
                let mut copy = entry.clone();
                copy["clause_id"] = Value::String(target.clone());
                aligned_matches.push(copy);
            }
        }
        unit["clause_matches"] = Value::Array(aligned_matches);
    }
    Ok(())
}

fn replace_fixture_clause_references(value: &mut Value, replacements: &BTreeMap<String, String>) {
    match value {
        Value::String(current) => {
            if let Some(replacement) = replacements.get(current) {
                *current = replacement.clone();
            }
        }
        Value::Array(values) => values
            .iter_mut()
            .for_each(|value| replace_fixture_clause_references(value, replacements)),
        Value::Object(values) => values
            .values_mut()
            .for_each(|value| replace_fixture_clause_references(value, replacements)),
        _ => {}
    }
}

fn fixture_clause_similarity(
    template: &Value,
    dispatched: &Value,
    expected: &QualityExpectations,
) -> u8 {
    let template_text = fixture_value_text(template).to_lowercase();
    let dispatched_text = fixture_value_text(dispatched).to_lowercase();
    let relevant_overlap = expected
        .plan
        .relevant_terms
        .iter()
        .filter(|term| {
            let term = term.to_lowercase();
            template_text.contains(&term) && dispatched_text.contains(&term)
        })
        .count();
    if relevant_overlap > 0 {
        return 2;
    }
    0
}

fn fixture_value_text(value: &Value) -> String {
    let mut values = Vec::new();
    collect_plan_strings(value, &mut values);
    values.join(" ")
}

fn deduplicate_json_strings(values: &mut Vec<Value>) {
    let mut seen = BTreeSet::new();
    values.retain(|value| {
        value
            .as_str()
            .is_some_and(|text| seen.insert(text.to_owned()))
    });
}

fn fixture_evidence_for_clause(
    evidence_units: &[Value],
    clause_id: &str,
    directness_required: &str,
) -> FixtureClauseEvidence {
    let matches = evidence_units
        .iter()
        .filter(|unit| fixture_unit_matches_clause(unit, clause_id))
        .collect::<Vec<_>>();
    let mut evidence_ids = matches
        .iter()
        .filter_map(|unit| unit.get("evidence_id").and_then(Value::as_str))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    evidence_ids.sort();
    evidence_ids.dedup();
    let covered_tickers = matches
        .iter()
        .filter_map(|unit| unit.get("ticker").and_then(Value::as_str))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let best_directness = best_fixture_value(&matches, "directness", directness_rank);
    let best_evidence_grade = best_fixture_value(&matches, "evidence_grade", evidence_grade_rank);
    let has_direct = matches
        .iter()
        .any(|unit| unit.get("directness").and_then(Value::as_str) == Some("direct"));
    let directness_satisfied = match directness_required {
        "direct_required" => has_direct,
        "any" | "direct_preferred" => !evidence_ids.is_empty(),
        _ => false,
    };
    let strong_claim_ready = matches.iter().any(|unit| {
        unit.get("directness").and_then(Value::as_str) == Some("direct")
            && unit.get("evidence_grade").and_then(Value::as_str) == Some("strong")
    });
    FixtureClauseEvidence {
        evidence_ids,
        covered_tickers,
        best_directness,
        best_evidence_grade,
        directness_satisfied,
        strong_claim_ready,
    }
}

fn fixture_unit_matches_clause(unit: &Value, clause_id: &str) -> bool {
    let supports = unit
        .get("supports_clause_ids")
        .and_then(Value::as_array)
        .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(clause_id)));
    let matched = unit
        .get("clause_matches")
        .and_then(Value::as_array)
        .is_some_and(|matches| {
            matches
                .iter()
                .any(|entry| entry.get("clause_id").and_then(Value::as_str) == Some(clause_id))
        });
    supports && matched
}

fn best_fixture_value(units: &[&Value], field: &str, rank: impl Fn(&str) -> u8) -> Option<String> {
    units
        .iter()
        .filter_map(|unit| unit.get(field).and_then(Value::as_str))
        .max_by_key(|value| rank(value))
        .map(str::to_owned)
}

fn directness_rank(value: &str) -> u8 {
    match value {
        "direct" => 4,
        "metric_lineage" => 3,
        "related" => 2,
        "unverified" => 1,
        _ => 0,
    }
}

fn evidence_grade_rank(value: &str) -> u8 {
    match value {
        "strong" => 4,
        "medium" => 3,
        "weak" => 2,
        "unverified" => 1,
        _ => 0,
    }
}

fn fixture_dependency(code: &str) -> DependencyFailure {
    DependencyFailure::redacted(
        code,
        "quality fixture rejected the typed action",
        false,
        DeliveryCertainty::NotDispatched,
    )
}

#[derive(Debug, Default)]
struct FixturePersistence {
    state: Mutex<FixturePersistenceState>,
}

#[derive(Debug, Default)]
struct FixturePersistenceState {
    actions: BTreeMap<String, ActionReceipt>,
    finalizations: BTreeMap<String, (ActionDisposition, ContentHash, ContentHash)>,
    results: BTreeMap<String, Vec<u8>>,
    final_hash: Option<ContentHash>,
}

#[async_trait]
impl Persistence for FixturePersistence {
    async fn load_recovery(
        &self,
        _run: &RunIdentity,
    ) -> Result<RecoverySnapshot, DependencyFailure> {
        Ok(RecoverySnapshot::Fresh)
    }

    async fn inspect_run(&self, run: &RunIdentity) -> Result<RunControl, DependencyFailure> {
        if run.fencing_token != QUALITY_FENCE || run.expected_cancel_generation != 0 {
            return Ok(RunControl::Finalized);
        }
        Ok(RunControl::Active {
            fencing_token: QUALITY_FENCE,
            cancel_generation: 0,
        })
    }

    async fn checkpoint_episode(&self, _episode: &DurableEpisode) -> Result<(), DependencyFailure> {
        Ok(())
    }

    async fn checkpoint_run_state(
        &self,
        _state: &DurableRunState,
    ) -> Result<(), DependencyFailure> {
        Ok(())
    }

    async fn begin_action(
        &self,
        intent: &ActionIntent,
    ) -> Result<ActionReceipt, DependencyFailure> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| fixture_dependency("quality_persistence_poisoned"))?;
        if let Some(receipt) = state.actions.get(&intent.mutation.action_key) {
            return Ok(receipt.clone());
        }
        let receipt = ActionReceipt {
            action_key: intent.mutation.action_key.clone(),
            mutation_id: intent.mutation.mutation_id.clone(),
            request_hash: intent.mutation.request_hash.clone(),
            result_hash: None,
            stage: ActionStage::Begun,
            retryable_read: intent.mutation.retryable_read,
        };
        state
            .actions
            .insert(receipt.action_key.clone(), receipt.clone());
        Ok(receipt)
    }

    async fn observe_action(
        &self,
        observation: &DurableActionObservation,
    ) -> Result<ActionReceipt, DependencyFailure> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| fixture_dependency("quality_persistence_poisoned"))?;
        let observed = {
            let action = state
                .actions
                .get_mut(&observation.mutation.action_key)
                .ok_or_else(|| fixture_dependency("quality_action_missing"))?;
            if action.stage == ActionStage::Begun {
                action.result_hash = Some(observation.mutation.result_hash.clone());
                action.stage = ActionStage::Observed;
            }
            action.clone()
        };
        if observed.stage == ActionStage::Observed {
            state.results.insert(
                observation.mutation.action_key.clone(),
                observation.result_bytes.clone(),
            );
        }
        Ok(observed)
    }

    async fn finalize_action(
        &self,
        mutation: FinalizeActionMutation,
    ) -> Result<ActionFinalizationReceipt, DependencyFailure> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| fixture_dependency("quality_persistence_poisoned"))?;
        if let Some((disposition, validation, policy)) =
            state.finalizations.get(&mutation.action_key)
        {
            if *disposition != mutation.disposition
                || *validation != mutation.validation_receipt_hash
                || *policy != mutation.policy_receipt_hash
            {
                return Err(fixture_dependency("quality_action_finalization_conflict"));
            }
        } else {
            state.finalizations.insert(
                mutation.action_key.clone(),
                (
                    mutation.disposition,
                    mutation.validation_receipt_hash.clone(),
                    mutation.policy_receipt_hash.clone(),
                ),
            );
        }
        let action = state
            .actions
            .get_mut(&mutation.action_key)
            .ok_or_else(|| fixture_dependency("quality_action_missing"))?;
        if action.result_hash.as_ref() != Some(&mutation.result_hash) {
            return Err(fixture_dependency("quality_action_result_mismatch"));
        }
        action.stage = mutation.disposition.stage();
        Ok(ActionFinalizationReceipt {
            action: action.clone(),
            disposition: mutation.disposition,
            validation_receipt_hash: mutation.validation_receipt_hash,
            policy_receipt_hash: mutation.policy_receipt_hash,
        })
    }

    async fn mark_action_ambiguous(
        &self,
        mutation: MarkActionAmbiguous,
    ) -> Result<(), DependencyFailure> {
        if let Some(action) = self
            .state
            .lock()
            .map_err(|_| fixture_dependency("quality_persistence_poisoned"))?
            .actions
            .get_mut(&mutation.action_key)
        {
            action.stage = ActionStage::Ambiguous;
        }
        Ok(())
    }

    async fn load_action_result(
        &self,
        _run: &RunIdentity,
        action_key: &str,
        _expected_hash: &ContentHash,
    ) -> Result<Option<Vec<u8>>, DependencyFailure> {
        Ok(self
            .state
            .lock()
            .map_err(|_| fixture_dependency("quality_persistence_poisoned"))?
            .results
            .get(action_key)
            .cloned())
    }

    async fn commit_final(
        &self,
        final_value: &DurableFinal,
    ) -> Result<FinalStatus, DependencyFailure> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| fixture_dependency("quality_persistence_poisoned"))?;
        if state.final_hash.as_ref() == Some(&final_value.mutation.answer_bundle_hash) {
            return Ok(FinalStatus::AlreadyCommitted);
        }
        if state.final_hash.is_some() {
            return Err(fixture_dependency("quality_final_conflict"));
        }
        state.final_hash = Some(final_value.mutation.answer_bundle_hash.clone());
        Ok(FinalStatus::Committed)
    }
}

fn report_from_outcome(
    suite: &ResearchQualitySuite,
    case: &ResearchQualityCase,
    outcome: Result<RunOutcome, EngineError>,
    provider_requests: Vec<ProviderRequestReport>,
    provider_turns: Vec<ProviderTurnReport>,
    capability_calls: Vec<CapabilityCallReport>,
    retain_answer: bool,
    model_id: &str,
) -> ResearchQualityRun {
    let mut assertions = Vec::new();
    let mut usage = None;
    let mut evidence_count = None;
    let mut final_status = None;
    let mut final_output_hash = None;
    let mut evidence_ledger_hash = None;
    let mut rendered_answer_hash = None;
    let mut answer = None;
    let engine_error;

    match outcome {
        Ok(outcome) => {
            assertions.push(assertion("engine_completed", true));
            usage = Some(outcome.answer_bundle.usage.clone());
            evidence_count = Some(outcome.evidence_count);
            final_status = Some(
                match outcome.final_status {
                    FinalStatus::Committed => "committed",
                    FinalStatus::AlreadyCommitted => "already_committed",
                    FinalStatus::Cancelled => "cancelled",
                }
                .into(),
            );
            final_output_hash = serde_jcs::to_vec(&outcome.answer_bundle.output)
                .ok()
                .map(ContentHash::sha256);
            evidence_ledger_hash = Some(outcome.answer_bundle.evidence_ledger_hash.clone());
            rendered_answer_hash =
                Some(ContentHash::sha256(&outcome.answer_bundle.rendered_content));
            answer = retain_answer.then_some(outcome.answer_bundle.rendered_content.clone());
            assertions.push(assertion(
                "final_commit",
                !matches!(outcome.final_status, FinalStatus::Cancelled),
            ));
            assertions.push(assertion(
                "minimum_replans",
                outcome.answer_bundle.usage.replans >= case.expected.min_replans,
            ));
            assertions.push(assertion(
                "minimum_evidence",
                outcome.evidence_count >= case.expected.required_evidence_ids.len(),
            ));
            let evidence_is_committed =
                case.expected.required_evidence_ids.iter().all(|required| {
                    outcome
                        .answer_bundle
                        .evidence_ids
                        .iter()
                        .any(|id| id == required)
                });
            assertions.push(assertion(
                "required_evidence_is_committed",
                evidence_is_committed,
            ));
            assertions.push(assertion(
                "direct_markdown_output",
                outcome.answer_bundle.answer_ir.is_none()
                    && outcome.answer_bundle.output_contract.id == "final-markdown/v1",
            ));
            let answer_text = &outcome.answer_bundle.rendered_content;
            assertions.push(assertion(
                "required_rendered_terms",
                case.expected
                    .required_rendered_terms
                    .iter()
                    .all(|term| answer_text.contains(term)),
            ));
            assertions.push(assertion(
                "internal_terms_not_exposed",
                case.expected
                    .forbidden_rendered_terms
                    .iter()
                    .all(|term| !answer_text.to_lowercase().contains(&term.to_lowercase())),
            ));
            engine_error = None;
        }
        Err(error) => {
            assertions.push(assertion("engine_completed", false));
            engine_error = Some(safe_engine_error_code(&error));
        }
    }

    assertions.push(assertion(
        "exact_provider_identity",
        !provider_turns.is_empty()
            && provider_turns
                .iter()
                .all(|turn| turn.requested_model == model_id && turn.observed_model == model_id),
    ));
    assertions.push(assertion("model_research_proposal_shape", {
        let proposal_shapes = provider_turns
            .iter()
            .flat_map(|turn| &turn.tool_argument_shapes)
            .filter(|shape| shape.model_input_is_research_proposal)
            .collect::<Vec<_>>();
        !proposal_shapes.is_empty()
            && proposal_shapes
                .into_iter()
                .all(research_proposal_shape_is_complete)
    }));
    assertions.push(assertion(
        "provider_turn_budget",
        provider_turns.len() <= usize::from(case.expected.max_provider_turns),
    ));
    assertions.push(assertion(
        "exact_capability_sequence",
        capability_calls
            .iter()
            .map(|call| call.capability_id.as_str())
            .eq(case
                .expected
                .exact_capability_sequence
                .iter()
                .map(String::as_str)),
    ));
    assertions.push(assertion(
        "fixture_plan_accepted",
        !capability_calls.is_empty()
            && capability_calls
                .iter()
                .all(|call| call.fixture_plan_accepted),
    ));

    let mut report = ResearchQualityReport {
        schema_version: 7,
        harness: "fixture_backed_real_provider_v4".into(),
        suite_id: suite.suite_id.clone(),
        suite_version: suite.suite_version.clone(),
        case_id: case.id.clone(),
        passed: false,
        score: 0,
        provider_requests,
        provider_turns,
        capability_calls,
        usage,
        evidence_count,
        final_status,
        final_output_hash,
        evidence_ledger_hash,
        rendered_answer_hash,
        assertions,
        engine_error,
    };
    finalize_report(&mut report);
    ResearchQualityRun {
        report,
        rendered_answer: answer,
    }
}

fn finalize_report(report: &mut ResearchQualityReport) {
    let passed_count = report
        .assertions
        .iter()
        .filter(|assertion| assertion.passed)
        .count();
    report.score = if report.assertions.is_empty() {
        0
    } else {
        u8::try_from((passed_count * 100) / report.assertions.len()).unwrap_or(0)
    };
    report.passed = report.assertions.iter().all(|assertion| assertion.passed);
}

fn research_proposal_shape_is_complete(shape: &ToolArgumentShapeReport) -> bool {
    shape.model_input_is_research_proposal
        && shape.valid_json
        && shape.top_level_object
        && shape.provider_envelope_is_valid
        && shape.missing_research_proposal_keys.is_empty()
        && shape.unknown_top_level_key_count == 0
        && shape.objectives_kind == "array"
        && shape.objective_count.is_some_and(|count| count > 0)
        && shape.first_objective_missing_keys.is_empty()
        && shape.first_objective_unknown_key_count == 0
}

fn assertion(id: &str, passed: bool) -> QualityAssertion {
    QualityAssertion {
        id: id.into(),
        passed,
    }
}

fn safe_engine_error_code(error: &EngineError) -> String {
    match error {
        EngineError::Dependency { component, failure } => {
            format!("dependency:{component}:{}", failure.code)
        }
        EngineError::AnswerValidation(codes) => format!("answer_validation:{}", codes.join(",")),
        EngineError::RuleViolations(codes) => format!("answer_policy:{}", codes.join(",")),
        EngineError::InvalidWorkflowControl => "invalid_workflow_control".into(),
        EngineError::InvalidWorkflowTransitionShape => "invalid_transition_shape".into(),
        EngineError::ModelProposalRejected(code) => format!("model_proposal_rejected:{code}"),
        // A planner failure is internally classified into a closed,
        // content-free diagnostic. Keeping that subtype in the quality
        // report makes a live-model failure actionable without recording the
        // model proposal, question, or retrieval payload.
        EngineError::ResearchPlanner(_) => durable_failure_diagnostic(error)
            .map(|diagnostic| diagnostic.kind.into())
            .unwrap_or_else(|| "research_plan_rejected".into()),
        EngineError::WorkflowResolution { .. } => "workflow_resolution".into(),
        EngineError::DeadlineExceeded(_) => "deadline_exceeded".into(),
        EngineError::NoRemainingOutputBudget => "output_budget_exhausted".into(),
        EngineError::CapabilityPrerequisiteMissing(_) => "capability_prerequisite_missing".into(),
        EngineError::CapabilityBudgetExceeded { .. } => "capability_budget_exceeded".into(),
        EngineError::ActionRejected(_) => "capability_action_rejected".into(),
        EngineError::InvalidCapabilityResult(_) => "capability_result_invalid".into(),
        EngineError::Invariant(_) => "engine_invariant".into(),
        EngineError::InvalidProviderEpisode(_) => "invalid_provider_episode".into(),
        EngineError::PhaseRuleViolations { .. } => "phase_policy".into(),
        EngineError::Contract(_) => "contract".into(),
        EngineError::Kernel(_) => "kernel".into(),
        EngineError::Wire(_) => "provider_wire".into(),
        EngineError::Image(_) => "agent_image".into(),
        EngineError::Evidence(_) => "evidence".into(),
        EngineError::StateArtifact(_) => "state_artifact".into(),
        EngineError::ContextPlan(_) => "context_plan".into(),
        EngineError::ContextCompaction(_) => "context_compaction".into(),
        EngineError::BoundedChild(_) => "bounded_child".into(),
        EngineError::Policy(_) => "policy".into(),
        EngineError::Json(_) => "json".into(),
        _ => "engine_rejected".into(),
    }
}

fn resolve_case_path(root: &Path, relative: &str, label: &str) -> Result<PathBuf, QualityError> {
    validate_relative_string(relative)?;
    resolve_relative(root, Path::new(relative), label)
}

fn resolve_relative(root: &Path, relative: &Path, label: &str) -> Result<PathBuf, QualityError> {
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(QualityError::Suite(format!(
            "{label} escapes repository root"
        )));
    }
    Ok(root.join(relative))
}

fn validate_relative_string(relative: &str) -> Result<(), QualityError> {
    if relative.len() > 512 || relative.contains('\0') {
        return Err(QualityError::Suite("fixture path is invalid".into()));
    }
    resolve_relative(Path::new("."), Path::new(relative), "fixture path").map(|_| ())
}

fn read_json_value(path: &Path, maximum: usize) -> Result<Value, QualityError> {
    let bytes = read_regular_file(path, maximum)?;
    serde_json::from_slice(&bytes).map_err(QualityError::Json)
}

fn read_regular_file(path: &Path, maximum: usize) -> Result<Vec<u8>, QualityError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| QualityError::Fixture("fixture file is missing".into()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() == 0 {
        return Err(QualityError::Fixture(
            "fixture input must be a non-empty regular file".into(),
        ));
    }
    let length = usize::try_from(metadata.len())
        .map_err(|_| QualityError::Fixture("fixture size overflows platform".into()))?;
    if length > maximum {
        return Err(QualityError::Fixture("fixture exceeds byte limit".into()));
    }
    fs::read(path).map_err(|_| QualityError::Fixture("fixture cannot be read".into()))
}

fn valid_id(id: &str) -> bool {
    (3..=96).contains(&id.len())
        && id.bytes().enumerate().all(|(index, byte)| {
            if index == 0 {
                byte.is_ascii_lowercase() || byte.is_ascii_digit()
            } else {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'_' | b'.' | b'-')
            }
        })
}

fn json_fixture_error(error: serde_json::Error) -> QualityError {
    QualityError::Json(error)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    #[test]
    fn checked_in_quality_suite_is_valid() {
        let suite = load_suite(&root(), Path::new("evals/krw-research-quality/v4"));
        assert!(suite.is_ok(), "{suite:?}");
    }

    #[tokio::test]
    async fn recorded_quality_cases_exercise_the_real_intent_to_search_plan_boundary() {
        let root = root();
        let suite = load_suite(&root, Path::new("evals/krw-research-quality/v4")).unwrap();
        for case in &suite.cases {
            let run = run_recorded_fixture_case(&root, &suite, &case.id, true)
                .await
                .unwrap();
            assert!(run.report.passed, "{:#?}", run.report);
            assert!(
                run.report
                    .capability_calls
                    .iter()
                    .all(|call| call.fixture_plan_accepted)
            );
        }
        let run = run_recorded_fixture_case(
            &root,
            &suite,
            "company-research-independent-cash-and-debt",
            true,
        )
        .await
        .unwrap();
        assert!(
            run.report.assertions.iter().any(|assertion| assertion.id
                == "model_research_proposal_shape"
                && assertion.passed)
        );
        assert_eq!(
            run.report.capability_calls[0]
                .plan_quality
                .as_ref()
                .and_then(|quality| quality.clause_count),
            Some(2)
        );
        assert!(
            run.rendered_answer
                .as_deref()
                .is_some_and(|answer| answer.contains("VG"))
        );
    }

    #[test]
    fn fixture_plan_requires_same_ticker_filing_and_relevant_term() {
        let expected = QualityExpectations {
            max_provider_turns: 4,
            min_replans: 0,
            exact_capability_sequence: vec!["ontology.query_context".into()],
            required_evidence_ids: vec!["evidence".into()],
            required_rendered_terms: vec!["VG".into()],
            forbidden_rendered_terms: Vec::new(),
            plan: PlanExpectations {
                ticker: "VG".into(),
                required_document_type: "10-K".into(),
                min_clauses: 1,
                max_clauses: 2,
                relevant_terms: vec!["cash".into()],
            },
        };
        let valid = serde_json::json!({
            "tickers": ["VG"],
            "document_types": ["10-K"],
            "question": "cash generation",
            "clauses": [{
                "clause_id": "cash_generation",
                "directness": "direct_required",
                "required": true,
                "tickers": ["VG"]
            }]
        });
        assert!(validate_fixture_plan(&valid, &expected).is_ok());
        let wrong_ticker = serde_json::json!({
            "tickers": ["AAPL"],
            "document_types": ["10-K"],
            "question": "cash generation",
            "clauses": [{"required": true, "tickers": ["AAPL"]}]
        });
        assert!(validate_fixture_plan(&wrong_ticker, &expected).is_err());
    }
}
