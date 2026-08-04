//! Deterministic keyless vertical-slice harness. This crate is never linked into the daemon.

use std::fs;
use std::path::Path;

use krw_agent_deepseek_wire::{AssistantMessage, ProviderEpisodeV1, TokenUsage};
use krw_agent_evidence::{EvidenceLedger, EvidenceScope};
use krw_agent_image::compile_agent_dir;
use krw_agent_kernel::{AuthorizedAction, ExecutionEvent, ExecutionState};
use krw_agent_persistence::{
    ActionDisposition, ActionStage, BeginActionMutation, CheckpointEpisodeMutation, CommitOutcome,
    FinalCommitMutation, FinalizeActionMutation, InMemoryPersistence, ObserveActionMutation,
    RunReceipt,
};
use krw_agent_protocol::{AuthScope, ContentHash, RunRequest, provider_tool_name};
use krw_agent_research_planner::{InitialPlanScope, compile_research_proposal};
use krw_ontology_adapter::{MappingContext, map_research_state, parse_research_state};
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

const PRIVATE_CANARY: &str = "PRIVATE_REASONING_CANARY_7F4C";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QuickstartReport {
    pub status: String,
    pub image_hash: ContentHash,
    pub requested_model: String,
    pub observed_model: String,
    pub action_key: String,
    pub evidence_count: usize,
    pub evidence_ledger_hash: ContentHash,
    pub answer_bundle_hash: ContentHash,
    pub durable_trace: Vec<String>,
    pub public_markdown: String,
}

pub fn run_vertical_slice(
    agent_dir: impl AsRef<Path>,
    fixture_dir: impl AsRef<Path>,
) -> Result<QuickstartReport, QuickstartError> {
    let agent_dir = agent_dir.as_ref();
    let fixture_dir = fixture_dir.as_ref();
    let image = compile_agent_dir(agent_dir)?;
    let request: RunRequest = read_json(&fixture_dir.join("run-request.json"))?;
    let mut trace = Vec::new();
    let store = InMemoryPersistence::default();
    store.insert_run(RunReceipt {
        run_id: request.run_id.clone(),
        fencing_token: 1,
        run_version: 1,
        cancel_generation: 0,
    })?;
    trace.push("claim_committed".into());

    let mut execution = ExecutionState::Queued;
    execution = execution.transition(ExecutionEvent::Claim)?;
    execution = execution.transition(ExecutionEvent::Admit)?;
    execution = execution.transition(ExecutionEvent::Start)?;

    let mut matching_entrypoints = image
        .manifest
        .body
        .entrypoints
        .values()
        .filter(|entrypoint| {
            entrypoint.run_kind == request.run_kind && entrypoint.locale == request.locale
        });
    let entrypoint =
        matching_entrypoints
            .next()
            .ok_or_else(|| QuickstartError::MissingEntrypoint {
                run_kind: request.run_kind.clone(),
                locale: request.locale.clone(),
            })?;
    if matching_entrypoints.next().is_some() {
        return Err(QuickstartError::AmbiguousEntrypoint {
            run_kind: request.run_kind.clone(),
            locale: request.locale.clone(),
        });
    }
    entrypoint.validate_run_context(&request.context)?;
    // Compile the exact typed state program used by the production engine.
    // The quickstart intentionally does not maintain a second workflow cursor;
    // StateInterpreter is the only runtime transition authority.
    image.manifest.state_program(&entrypoint.workflow)?;

    let assistant: AssistantMessage =
        read_json(&fixture_dir.join("provider/turn-1-assistant.json"))?;
    let tool_call = assistant
        .tool_calls
        .first()
        .filter(|_| assistant.tool_calls.len() == 1)
        .ok_or(QuickstartError::ExpectedOneToolCall)?;
    if tool_call.function.name.as_str() != provider_tool_name("ontology.query_context") {
        return Err(QuickstartError::Invariant(
            "fixture must call ontology.query_context",
        ));
    }
    let provider_arguments: Value = serde_json::from_str(&tool_call.function.arguments)?;
    let capability = image
        .manifest
        .body
        .capabilities
        .iter()
        .find(|capability| capability.id == "ontology.query_context")
        .ok_or(QuickstartError::Invariant(
            "fixture capability is absent from the compiled image",
        ))?;
    let research_proposal = capability
        .provider_input_codec
        .decode(provider_arguments)
        .map_err(|_| {
            QuickstartError::Invariant(
                "fixture provider arguments do not match the image input codec",
            )
        })?;
    let parsed_arguments = compile_research_proposal(
        &research_proposal,
        InitialPlanScope {
            question: &request.question,
            context: &request.context,
            derived_tickers: None,
            max_discovery_tickers: entrypoint.scope.cardinality.value(),
            prior_plan: None,
        },
    )
    .map_err(QuickstartError::ResearchPlanner)?
    .search_plan;
    let canonical_arguments = serde_jcs::to_vec(&parsed_arguments)?;
    let request_hash = ContentHash::sha256(&canonical_arguments);
    let action_key = ContentHash::sha256(
        [
            b"ontology.query_context\0".as_slice(),
            canonical_arguments.as_slice(),
        ]
        .concat(),
    )
    .to_string();
    let mut episode = ProviderEpisodeV1 {
        schema_version: 1,
        request_hash: ContentHash::sha256("fixture-provider-request"),
        requested_model: request.requested_model.clone(),
        observed_model: "deepseek-v4-flash".into(),
        api_version: "chat-completions-v1".into(),
        assistant,
        tool_results: Vec::new(),
        tool_schema_hash: ContentHash::sha256("fixture-tool-schema"),
        agent_image_hash: image.manifest.content_hash.clone(),
        finish_reason: "tool_calls".into(),
        usage: TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 20,
            total_tokens: 120,
            prompt_cache_hit_tokens: 80,
            prompt_cache_miss_tokens: 20,
        },
        replay_hash: ContentHash::sha256("pending"),
    };
    episode.verify_model_identity()?;
    episode.replay_hash = episode.calculate_replay_hash()?;
    let episode_hash = episode.replay_hash.clone();
    store.checkpoint_episode(CheckpointEpisodeMutation {
        run_id: request.run_id.clone(),
        fencing_token: 1,
        mutation_id: "checkpoint-episode-001".into(),
        episode_hash: episode_hash.clone(),
    })?;
    if !store.has_episode(&request.run_id, &episode_hash)? {
        return Err(QuickstartError::Invariant(
            "provider episode checkpoint was not durable",
        ));
    }
    trace.push("provider_episode_committed".into());

    let mut action =
        AuthorizedAction::proposed(action_key.clone(), request_hash.clone(), episode_hash);
    action.episode_committed()?;
    let action_receipt = store.begin_action(BeginActionMutation {
        run_id: request.run_id.clone(),
        fencing_token: 1,
        mutation_id: "begin-action-001".into(),
        action_key: action_key.clone(),
        request_hash,
        retryable_read: true,
    })?;
    action.bind_receipt(&action_receipt)?;
    trace.push("action_begun".into());
    action.mark_dispatched()?;
    trace.push("mcp_dispatched".into());

    let mut research_state_value: Value =
        read_json(&fixture_dir.join("mcp/research-state-answerable.json"))?;
    research_state_value["plan"] = parsed_arguments.clone();
    let research_state_bytes = serde_jcs::to_vec(&research_state_value)?;
    let result_hash = ContentHash::sha256(&research_state_bytes);
    action.observe(result_hash.clone())?;
    store.observe_action(ObserveActionMutation {
        run_id: request.run_id.clone(),
        fencing_token: 1,
        mutation_id: "observe-action-001".into(),
        action_key: action_key.clone(),
        result_hash: result_hash.clone(),
    })?;
    trace.push("action_observed".into());

    let research_state = parse_research_state(&research_state_bytes)?;
    let delta = map_research_state(
        &research_state,
        &MappingContext {
            capability_id: "ontology.query_context".into(),
            action_key: action_key.clone(),
            server_build: "fixture".into(),
            normalized_contract_hash: ContentHash::parse(
                "sha256:8c44e23d6a2e0b565b7eed9e31cfd702dc5cbd5f139a99f9b55aa903f28cc151",
            )
            .expect("static normalized contract hash"),
            server_schema_bundle_hash: ContentHash::sha256("fixture-research-state-schema"),
            data_release_hash: ContentHash::sha256("fixture-release-v1"),
            scope: EvidenceScope {
                auth_scope: AuthScope::Tenant,
                scope_hash: ContentHash::sha256("fixture-tenant"),
            },
            payload_ref: result_hash.clone(),
        },
    )?;
    let mut ledger = EvidenceLedger::from_records(delta.records)?;
    ledger.extend_calculations(delta.calculations)?;
    ledger.set_answerability(delta.answerability);
    let finalization = store.finalize_action(FinalizeActionMutation {
        run_id: request.run_id.clone(),
        fencing_token: 1,
        mutation_id: "finalize-action-001".into(),
        action_key: action_key.clone(),
        result_hash: result_hash.clone(),
        disposition: ActionDisposition::Accepted,
        validation_receipt_hash: ContentHash::sha256("fixture-validation:research-state-v2"),
        policy_receipt_hash: ContentHash::sha256("fixture-policy:after-action-allow"),
    })?;
    if finalization.action.stage != ActionStage::Accepted {
        return Err(QuickstartError::Invariant(
            "validated action must finalize as accepted",
        ));
    }
    action.accept(&result_hash)?;
    trace.push("action_finalized_accepted".into());
    trace.push("evidence_ingested".into());

    let final_assistant: AssistantMessage =
        read_json(&fixture_dir.join("provider/turn-3-answer.json"))?;
    if !final_assistant.tool_calls.is_empty() {
        return Err(QuickstartError::Invariant(
            "Markdown completion must not request a tool",
        ));
    }
    let public_markdown = final_assistant
        .content
        .as_deref()
        .filter(|content| !content.trim().is_empty())
        .map(str::to_owned)
        .ok_or(QuickstartError::Invariant(
            "Markdown completion must contain prose",
        ))?;
    if image
        .manifest
        .body
        .answer_policy
        .forbidden_user_terms
        .iter()
        .any(|term| {
            public_markdown
                .to_lowercase()
                .contains(&term.to_lowercase())
        })
    {
        return Err(QuickstartError::InternalTermLeak);
    }
    trace.push("markdown_completed".into());
    execution = execution.transition(ExecutionEvent::BeginVerification)?;

    if public_markdown.contains(PRIVATE_CANARY) {
        return Err(QuickstartError::PrivateCanaryLeak);
    }
    let evidence_ledger_hash = ContentHash::sha256(serde_jcs::to_vec(&ledger)?);
    trace.push("evidence_ledger_receipt_bound".into());
    execution = execution.transition(ExecutionEvent::BeginCommit)?;
    let answer_bundle_hash = ContentHash::sha256(format!(
        "quickstart-final-markdown/v1\0{}\0{}",
        ContentHash::sha256(&public_markdown),
        evidence_ledger_hash,
    ));
    let outcome = store.commit_final(FinalCommitMutation {
        run_id: request.run_id.clone(),
        fencing_token: 1,
        expected_cancel_generation: 0,
        mutation_id: "commit-final-001".into(),
        answer_bundle_hash: answer_bundle_hash.clone(),
        session_memory_delta_hash: None,
    })?;
    if !matches!(outcome, CommitOutcome::Committed(_)) {
        return Err(QuickstartError::Invariant(
            "first final commit must create the outcome",
        ));
    }
    execution = execution.transition(ExecutionEvent::CommitSucceeded)?;
    trace.push("final_committed".into());
    if !execution.is_terminal() {
        return Err(QuickstartError::Invariant(
            "execution state must be terminal",
        ));
    }

    Ok(QuickstartReport {
        status: "ok".into(),
        image_hash: image.manifest.content_hash,
        requested_model: episode.requested_model,
        observed_model: episode.observed_model,
        action_key,
        evidence_count: ledger.len(),
        evidence_ledger_hash,
        answer_bundle_hash,
        durable_trace: trace,
        public_markdown,
    })
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, QuickstartError> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

#[derive(Debug, Error)]
pub enum QuickstartError {
    #[error("I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Image(#[from] krw_agent_image::ImageError),
    #[error(transparent)]
    ContextPolicy(#[from] krw_agent_image::ContextPolicyError),
    #[error(transparent)]
    ResearchPlanner(#[from] krw_agent_research_planner::InitialPlanError),
    #[error(transparent)]
    Kernel(#[from] krw_agent_kernel::KernelError),
    #[error(transparent)]
    Persistence(#[from] krw_agent_persistence::PersistenceError),
    #[error(transparent)]
    Wire(#[from] krw_agent_deepseek_wire::WireError),
    #[error(transparent)]
    Evidence(#[from] krw_agent_evidence::EvidenceError),
    #[error(transparent)]
    Ontology(#[from] krw_ontology_adapter::AdapterError),
    #[error("expected exactly one tool call")]
    ExpectedOneToolCall,
    #[error("no entrypoint matches run_kind={run_kind} and locale={locale}")]
    MissingEntrypoint { run_kind: String, locale: String },
    #[error("multiple entrypoints match run_kind={run_kind} and locale={locale}")]
    AmbiguousEntrypoint { run_kind: String, locale: String },
    #[error("internal runtime term reached public Markdown")]
    InternalTermLeak,
    #[error("private reasoning canary reached public output")]
    PrivateCanaryLeak,
    #[error("vertical-slice invariant failed: {0}")]
    Invariant(&'static str),
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    #[test]
    fn vertical_slice_matches_durable_order_and_hides_reasoning() {
        let root = repo_root();
        let report = run_vertical_slice(
            root.join("agents/krw-ontology"),
            root.join("fixtures/vertical-slice/v1"),
        )
        .unwrap();
        assert_eq!(
            report.durable_trace,
            vec![
                "claim_committed",
                "provider_episode_committed",
                "action_begun",
                "mcp_dispatched",
                "action_observed",
                "action_finalized_accepted",
                "evidence_ingested",
                "markdown_completed",
                "evidence_ledger_receipt_bound",
                "final_committed",
            ]
        );
        assert_eq!(report.requested_model, report.observed_model);
        assert!(!report.public_markdown.contains(PRIVATE_CANARY));
        assert_eq!(report.evidence_count, 1);
    }
}
