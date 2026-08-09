//! Redacted offline audit for a retained live-acceptance provider episode.
//!
//! This test exists so a failed live run can be diagnosed from its encrypted
//! receipt without printing the provider reasoning, prompt, user question, or
//! tool arguments.  It is opt-in and reads only the exact artifact reference
//! and scope supplied by the operator.

use std::env;
use std::path::PathBuf;

use krw_agent_artifact_store::{
    ArtifactRef, ArtifactScope, ArtifactStore, ArtifactStoreConfig, LocalArtifactStore,
    MasterKeyring, VersionedMasterKey,
};
use krw_agent_image::compile_agent_dir;
use krw_agent_protocol::{RunContextV1, provider_tool_name};
use krw_agent_provider_wire::ProviderEpisodeV1;
use krw_agent_research_planner::{InitialPlanError, InitialPlanScope, compile_research_proposal};
use serde_json::Value;

const ENABLE_ENV: &str = "KRW_LIVE_E2E_EPISODE_AUDIT";
const RECOVERY_STATE_AUDIT_ENV: &str = "KRW_LIVE_E2E_RECOVERY_STATE_AUDIT";
const QUESTION: &str = "AAPL의 매출 추이를 최근 10-K 공시 근거로 간단히 설명해줘";

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn required(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("{name} is required when {ENABLE_ENV}=1"))
}

fn initial_plan_code(error: &InitialPlanError) -> &'static str {
    match error {
        InitialPlanError::Contract => "proposal_contract_invalid",
        InitialPlanError::Decode => "proposal_decode_invalid",
        InitialPlanError::UnsupportedScope => "proposal_scope_invalid",
        InitialPlanError::PriorPlan => "proposal_prior_plan_invalid",
        InitialPlanError::DuplicateClause => "proposal_duplicate_clause",
        InitialPlanError::UnlinkedUserGoal => "proposal_anchor_invalid",
        InitialPlanError::UnlinkedDependency | InitialPlanError::GoalGraph => {
            "proposal_graph_invalid"
        }
        InitialPlanError::CandidateCoverage | InitialPlanError::UncoverableGoal => {
            "proposal_lowering_invalid"
        }
        InitialPlanError::PlanTooLarge => "proposal_plan_too_large",
        InitialPlanError::SearchPlanContract => "compiled_search_plan_invalid",
        InitialPlanError::Receipt => "proposal_receipt_invalid",
        InitialPlanError::Canonicalization => "proposal_canonicalization_failed",
    }
}

fn contract_code(
    error: &krw_agent_contracts::ContractValueError,
    arguments: &Value,
) -> &'static str {
    match error {
        krw_agent_contracts::ContractValueError::UnknownContract(_) => "unknown_contract",
        krw_agent_contracts::ContractValueError::Shape(_) => "shape_invalid",
        krw_agent_contracts::ContractValueError::Semantic(_) => {
            krw_agent_contracts::research_proposal_v4_validation_code(arguments)
        }
        krw_agent_contracts::ContractValueError::Limit(_) => "limit_exceeded",
        krw_agent_contracts::ContractValueError::Json(_) => "serialization_invalid",
    }
}

fn final_output_contract_code(error: &krw_agent_contracts::ContractValueError) -> &'static str {
    match error {
        krw_agent_contracts::ContractValueError::UnknownContract(_) => "unknown_contract",
        krw_agent_contracts::ContractValueError::Shape(_) => "shape_invalid",
        krw_agent_contracts::ContractValueError::Semantic(_) => "semantic_invalid",
        krw_agent_contracts::ContractValueError::Limit(_) => "limit_exceeded",
        krw_agent_contracts::ContractValueError::Json(_) => "serialization_invalid",
    }
}

#[tokio::test]
async fn audit_retained_live_provider_episode_without_exposing_content() {
    if env::var(ENABLE_ENV).as_deref() != Ok("1") {
        return;
    }

    let reference: ArtifactRef =
        serde_json::from_str(&required("KRW_LIVE_E2E_PROVIDER_EPISODE_REF"))
            .expect("canonical provider-episode artifact reference");
    let root = PathBuf::from(required("KRW_LIVE_E2E_ARTIFACT_ROOT"));
    let store = LocalArtifactStore::open(
        root,
        MasterKeyring::new(
            1,
            [VersionedMasterKey::new(1, [0x5A; 32]).expect("ephemeral audit key")],
        )
        .expect("ephemeral audit keyring"),
        ArtifactStoreConfig {
            max_plaintext_bytes: 16 * 1024 * 1024,
            ..ArtifactStoreConfig::default()
        },
    )
    .expect("live artifact store");
    let scope = ArtifactScope::new(
        required("KRW_LIVE_E2E_TENANT_ID"),
        required("KRW_LIVE_E2E_PRINCIPAL_ID"),
        required("KRW_LIVE_E2E_RUN_ID"),
    )
    .expect("live artifact scope");
    let plaintext = store
        .get(&scope, &reference)
        .await
        .expect("authenticated provider episode read");
    if env::var(RECOVERY_STATE_AUDIT_ENV).as_deref() == Ok("1") {
        let checkpoint: Value =
            serde_json::from_slice(plaintext.expose()).expect("recovery state checkpoint JSON");
        let state_id = checkpoint
            .pointer("/interpreter/current_state")
            .and_then(Value::as_str)
            .unwrap_or("invalid");
        let provider_turns = checkpoint
            .pointer("/usage/provider_turns")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let output_tokens = checkpoint
            .pointer("/usage/output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let repairs = checkpoint
            .pointer("/usage/repairs")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let trace_len = checkpoint
            .get("state_trace")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        println!(
            "live_episode_audit=recovery_state current_state={state_id} provider_turns={provider_turns} output_tokens={output_tokens} repairs={repairs} state_trace_len={trace_len}",
        );
        return;
    }
    let episode: ProviderEpisodeV1 =
        serde_json::from_slice(plaintext.expose()).expect("provider episode schema");
    println!(
        "live_episode_audit=episode finish_reason={} tool_calls={} content_present={} reasoning_present={} prompt_tokens={} completion_tokens={} total_tokens={}",
        episode.finish_reason,
        episode.assistant.tool_calls.len(),
        episode.assistant.content.is_some(),
        episode.assistant.reasoning_content.is_some(),
        episode.usage.prompt_tokens,
        episode.usage.completion_tokens,
        episode.usage.total_tokens,
    );
    if episode.assistant.tool_calls.is_empty() {
        let Some(content) = episode.assistant.content.as_deref() else {
            return;
        };
        let output = Value::String(content.to_owned());
        let contract =
            krw_agent_contracts::validate_value(krw_agent_contracts::FINAL_MARKDOWN_V1, &output)
                .map_or_else(|error| final_output_contract_code(&error), |()| "valid");
        let looks_like_json = content.trim_start().starts_with(['{', '[']);
        println!(
            "live_episode_audit=final_content kind=markdown byte_len={} looks_like_json={looks_like_json} final_markdown_contract={contract}",
            content.len(),
        );
        return;
    }
    let [tool] = episode.assistant.tool_calls.as_slice() else {
        return;
    };
    let Ok(arguments) = serde_json::from_str::<Value>(&tool.function.arguments) else {
        println!(
            "live_episode_audit=tool_arguments_json_invalid tool={}",
            tool.function.name.as_str()
        );
        return;
    };
    let arguments_are_object = arguments.is_object();
    println!(
        "live_episode_audit=tool name={} kind={:?} arguments_object={arguments_are_object}",
        tool.function.name.as_str(),
        tool.kind,
    );
    if tool.function.name.as_str() != provider_tool_name("ontology.query_context") {
        return;
    }
    let image = compile_agent_dir(repository_root().join("agents/krw-ontology"))
        .expect("compile current agent image");
    let capability = image
        .manifest
        .body
        .capabilities
        .iter()
        .find(|capability| capability.id == "ontology.query_context")
        .expect("query-context capability");
    let Ok(arguments) = capability.provider_input_codec.decode(arguments) else {
        println!("live_episode_audit=provider_input_envelope_invalid");
        return;
    };
    let context = RunContextV1::CompanyTickerSet {
        tickers: vec!["AAPL".into()],
    };
    let contract =
        krw_agent_contracts::validate_value(krw_agent_contracts::RESEARCH_PROPOSAL_V4, &arguments);
    let compilation = compile_research_proposal(
        &arguments,
        InitialPlanScope {
            question: QUESTION,
            context: &context,
            derived_tickers: None,
            max_discovery_tickers: 1,
            prior_plan: None,
        },
    );
    let compile_code = match compilation {
        Ok(compiled) => {
            let selected = compiled.selected_clause_ids.len();
            println!("live_episode_audit=proposal_compiles selected_clauses={selected}");
            return;
        }
        Err(error) => initial_plan_code(&error),
    };
    // Only stable structural metadata leaves this audit.  The tool name is an
    // internal ABI identifier; provider content and argument values never do.
    println!(
        "live_episode_audit=proposal_rejected tool={} contract={} reason={compile_code}",
        tool.function.name.as_str(),
        contract.map_or_else(|error| contract_code(&error, &arguments), |()| "valid")
    );
}
