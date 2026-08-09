//! Opt-in live acceptance for the complete read-only company-research path.
//!
//! The enclosing script supplies an ephemeral, certificate-verified `PostgreSQL`
//! instance and TLS ingress for a separately started `krw-capabilityd`. This
//! test then exercises the actual production `RunSupervisor` path:
//!
//! `DeepSeek Flash` -> real immutable ontology release -> immutable evidence
//! ledger + direct Korean Markdown -> `PostgreSQL` `commit_final` and outbox.
//!
//! No prompt, answer, provider reasoning, capability payload, URL, or secret
//! is emitted by this test. It is skipped unless an operator sets the explicit
//! `KRW_LIVE_E2E_ACCEPTANCE=1` gate.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use krw_agent_artifact_store::{
    ArtifactStoreConfig, LocalArtifactStore, MasterKeyring, VersionedMasterKey,
};
use krw_agent_capability_runtime::{McpToolTransport, PooledMcpTransport};
use krw_agent_image::compile_agent_dir;
use krw_agent_persistence::agent_v1::{
    AgentV1Client, EnqueueRunRequest, ReadCommittedOutcomeRequest,
};
use krw_agent_persistence::daemon::{
    AgentV1Store, RecoveryArtifactStore, RunSupervisor, RunWorkerConfig,
};
use krw_agent_persistence::postgres::{
    PostgresJsonExecutor, PostgresPoolOptions, ProcessEnvironmentDatabaseSecrets,
};
use krw_agent_protocol::{
    ContentHash, DeploymentBinding, ModelRegistry, RunRequest, is_canonical_ticker,
};
use krw_agent_runtime_config::{
    BudgetRegistry, EndpointDescriptor, EndpointRegistry, ProcessEnvironment, ValidationMode,
    load_yaml, resolve_release_set,
};
use krw_agent_runtime_persistence::{
    ArtifactRepository, ArtifactTtlPolicy, DeepSeekProviderCatalog, DurableRunStore,
    FinalizationPolicy, ImmutableRunClaimV1, ProductionClaimedRunExecutor,
    ProductionReleaseCatalog, RunResourceProfileV1,
};
use krw_agent_tool_mcp::McpClientPool;
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;

const ENABLE_ENV: &str = "KRW_LIVE_E2E_ACCEPTANCE";
const RUNTIME_VERSION: &str = "live-acceptance-v1";
const DEFAULT_RUN_ID: &str = "live-e2e-aapl-v1";
const TENANT_ID: &str = "live-e2e-tenant";
const PRINCIPAL_ID: &str = "live-e2e-principal";
const SESSION_ID: &str = "live-e2e-session";
const WORKER_ID: &str = "live-e2e-worker";
const DEFAULT_QUESTION: &str = "AAPL의 매출 추이를 최근 10-K 공시 근거로 간단히 설명해줘";
const DEFAULT_TICKER: &str = "AAPL";
const RUN_ID_ENV: &str = "KRW_LIVE_E2E_RUN_ID";
const QUESTION_ENV: &str = "KRW_LIVE_E2E_QUESTION";
const TICKER_ENV: &str = "KRW_LIVE_E2E_TICKER";

fn required(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("{name} is required when {ENABLE_ENV}=1"))
}

fn required_hash(name: &str) -> ContentHash {
    ContentHash::parse(required(name)).unwrap_or_else(|_| panic!("{name} must be a content hash"))
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").clone()
}

fn live_endpoint_registry() -> EndpointRegistry {
    EndpointRegistry {
        schema_version: 1,
        registry_id: "live-ontology-acceptance-v1".into(),
        endpoints: vec![EndpointDescriptor {
            endpoint_ref: "krw-ontology-local".into(),
            url_env: "KRW_ONTOLOGY_MCP_URL".into(),
            readiness_url_env: "KRW_ONTOLOGY_READY_URL".into(),
            protocol_version: "2025-06-18".into(),
            origin: "https://krw-agent.local".into(),
            credential_version: "public-live-acceptance-v1".into(),
            tls_profile: "system-plus-pinned-ca-v1".into(),
            tls_ca_pem_env: Some("KRW_ONTOLOGY_CA_PEM".into()),
        }],
    }
}

fn live_ontology_binding(root: &Path) -> DeploymentBinding {
    let mut binding: DeploymentBinding =
        load_yaml(root.join("deployments/local/deployment-binding.krw-ontology.example.yaml"))
            .expect("load narrow ontology binding template");
    let schema_hash = required_hash("KRW_LIVE_MCP_TOOL_SCHEMA_SHA256");
    let release_hash = required_hash("KRW_LIVE_MCP_RELEASE_MANIFEST_SHA256");
    let server_build = required("KRW_LIVE_MCP_SERVER_BUILD");
    assert_eq!(binding.capabilities.len(), 4);
    for capability in &mut binding.capabilities {
        assert!(matches!(
            capability.binding_key.as_str(),
            "krw_ontology_query_context"
                | "krw_ontology_company_context"
                | "krw_ontology_query"
                | "krw_ontology_trace"
        ));
        capability.server_schema_bundle_hash = schema_hash.clone();
        capability.data_release_hash = release_hash.clone();
        capability.server_build.clone_from(&server_build);
    }
    binding.deployment_id = "live-ontology-acceptance-v1".into();
    binding
}

fn live_request(root: &Path) -> RunRequest {
    let mut request: RunRequest = serde_json::from_slice(
        &fs::read(root.join("fixtures/vertical-slice/v1/run-request.json"))
            .expect("read company research request fixture"),
    )
    .expect("parse company research request fixture");
    request.run_id = configured_run_id();
    request.tenant_id = TENANT_ID.into();
    request.principal_id = PRINCIPAL_ID.into();
    request.session_id = SESSION_ID.into();
    request.question = configured_question();
    let ticker = configured_ticker();
    request.context = serde_json::from_value(serde_json::json!({
        "kind": "company_ticker_set",
        "tickers": [ticker]
    }))
    .expect("configured company context");
    request
}

fn configured_run_id() -> String {
    let value = env::var(RUN_ID_ENV).unwrap_or_else(|_| DEFAULT_RUN_ID.into());
    assert!(
        is_safe_operator_id(&value),
        "{RUN_ID_ENV} must be 1..=128 ASCII letters, digits, `_`, or `-`"
    );
    value
}

fn configured_question() -> String {
    let value = env::var(QUESTION_ENV).unwrap_or_else(|_| DEFAULT_QUESTION.into());
    assert!(
        !value.is_empty() && !value.contains('\0') && value.len() <= 64 * 1024,
        "{QUESTION_ENV} must be a non-empty, NUL-free question no larger than 64 KiB"
    );
    value
}

fn configured_ticker() -> String {
    let value = env::var(TICKER_ENV).unwrap_or_else(|_| DEFAULT_TICKER.into());
    assert!(
        is_canonical_ticker(&value),
        "{TICKER_ENV} must be a canonical uppercase ticker"
    );
    value
}

fn is_safe_operator_id(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn postgres_options() -> PostgresPoolOptions {
    PostgresPoolOptions {
        url_env: "KRW_LIVE_E2E_DATABASE_URL".into(),
        ca_pem_env: Some("KRW_LIVE_E2E_DATABASE_CA_PEM".into()),
        application_name: "krw-agent-live-e2e".into(),
        max_connections: 4,
        min_idle: 1,
        connect_timeout: Duration::from_secs(5),
        checkout_timeout: Duration::from_secs(2),
        query_timeout: Duration::from_secs(15),
        statement_timeout: Duration::from_secs(14),
        lock_timeout: Duration::from_secs(2),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flash_real_ontology_and_postgres_commit_final() {
    if env::var(ENABLE_ENV).as_deref() != Ok("1") {
        return;
    }

    let root = repository_root();
    let image = compile_agent_dir(root.join("agents/krw-ontology"))
        .expect("compile direct-authored company agent")
        .into_loaded()
        .expect("load verified company agent image");
    let models: ModelRegistry =
        load_yaml(root.join("deployments/local/model-registry.yaml")).expect("model registry");
    let budgets: BudgetRegistry =
        load_yaml(root.join("deployments/local/budget-registry.yaml")).expect("budget registry");
    let releases = resolve_release_set(
        vec![image],
        &live_ontology_binding(&root),
        &models,
        &budgets,
        &live_endpoint_registry(),
        &ProcessEnvironment,
        ValidationMode::Production,
    )
    .expect("resolve exact production-shaped live release");
    let image_hash = releases
        .owner("company_research", "ko-KR")
        .expect("company research owner")
        .clone();
    let release = releases
        .release(&image_hash)
        .expect("company research release");
    let request = live_request(&root);
    let snapshot = release
        .runtime
        .resolve_run(&image_hash, &request, 1, 0)
        .expect("resolve immutable execution snapshot");
    let immutable_claim =
        ImmutableRunClaimV1::new(request.clone(), &snapshot, RunResourceProfileV1::default())
            .expect("build immutable claim");
    let immutable_snapshot_hash = immutable_claim.canonical_hash().expect("claim hash");
    let immutable_snapshot = serde_json::to_value(&immutable_claim).expect("claim JSON");

    let database =
        PostgresJsonExecutor::connect(postgres_options(), &ProcessEnvironmentDatabaseSecrets)
            .await
            .expect("certificate-verified PostgreSQL agent ABI readiness");
    let client = Arc::new(AgentV1Client::new(database));
    let enqueue = client
        .execute(&EnqueueRunRequest {
            mutation_id: "live-e2e-enqueue-v1".into(),
            run_id: request.run_id.clone(),
            tenant_id: request.tenant_id.clone(),
            principal_id: request.principal_id.clone(),
            session_id: request.session_id.clone(),
            agent_image_hash: image_hash.clone(),
            runtime_version: RUNTIME_VERSION.into(),
            priority: 0,
            immutable_snapshot_hash,
            immutable_snapshot,
            resource_profile: serde_json::to_value(RunResourceProfileV1::default())
                .expect("resource profile JSON"),
            budgets: serde_json::to_value(&request.budget).expect("budget JSON"),
        })
        .await
        .expect("enqueue exact immutable live claim");
    assert_eq!(enqueue.outcome, "enqueued");

    let artifact_root = PathBuf::from(required("KRW_LIVE_E2E_ARTIFACT_ROOT"));
    let artifact_key = VersionedMasterKey::new(1, [0x5A; 32]).expect("ephemeral artifact key");
    let artifact_store = LocalArtifactStore::open(
        artifact_root,
        MasterKeyring::new(1, [artifact_key]).expect("artifact keyring"),
        ArtifactStoreConfig {
            max_plaintext_bytes: 16 * 1024 * 1024,
            ..ArtifactStoreConfig::default()
        },
    )
    .expect("private artifact store");
    let artifacts = ArtifactRepository::new(Arc::new(artifact_store), ArtifactTtlPolicy::default())
        .expect("artifact repository");
    let providers = DeepSeekProviderCatalog::compile_release_set(&releases)
        .expect("Flash-only provider catalog");
    let release_catalog = ProductionReleaseCatalog::compile(&releases).expect("release catalog");
    let mcp_pool =
        Arc::new(McpClientPool::new(4, Duration::from_secs(90)).expect("bounded MCP pool"));
    let transport: Arc<dyn McpToolTransport> = PooledMcpTransport::new(Arc::clone(&mcp_pool));
    let store: Arc<dyn DurableRunStore> = client.clone();
    let executor: Arc<dyn krw_agent_persistence::daemon::ClaimedRunExecutor> = Arc::new(
        ProductionClaimedRunExecutor::new(
            release_catalog.clone(),
            RUNTIME_VERSION.into(),
            providers,
            transport,
            store,
            artifacts.clone(),
            FinalizationPolicy::default(),
        )
        .expect("production claimed-run executor"),
    );
    let supervisor_store: Arc<dyn AgentV1Store> = client.clone();
    let recovery_artifacts: Arc<dyn RecoveryArtifactStore> = Arc::new(artifacts);
    let supervisor = Arc::new(
        RunSupervisor::new(
            supervisor_store,
            executor,
            recovery_artifacts,
            RunWorkerConfig {
                worker_id: WORKER_ID.into(),
                runtime_version: RUNTIME_VERSION.into(),
                accepted_agent_image_hashes: release_catalog.accepted_image_hashes(),
                max_in_flight: 1,
                lease_duration: Duration::from_secs(30),
                heartbeat_interval: Duration::from_secs(8),
                idle_backoff_min: Duration::from_millis(25),
                idle_backoff_max: Duration::from_millis(250),
                error_backoff_min: Duration::from_millis(100),
                error_backoff_max: Duration::from_secs(1),
                recovery_preflight_timeout: Duration::from_secs(10),
                max_recovery_artifact_bytes: 8 * 1024 * 1024,
                max_recovery_total_bytes: 16 * 1024 * 1024,
                drain_timeout: Duration::from_secs(30),
                forced_abort_timeout: Duration::from_secs(3),
            },
        )
        .expect("run supervisor"),
    );
    let shutdown = CancellationToken::new();
    let supervisor_task = {
        let supervisor = Arc::clone(&supervisor);
        let shutdown = shutdown.clone();
        tokio::spawn(async move { supervisor.run(shutdown).await })
    };

    // The immutable request budget is the authoritative live-run deadline.
    // Keep a small supervisor/terminal-observation allowance outside it so a
    // deliberately broad quality profile cannot be cut short by this test's
    // former, unrelated three-minute wall clock.
    let terminal_wait = Duration::from_millis(request.budget.deadline_ms)
        .checked_add(Duration::from_secs(45))
        .expect("live execution wait is representable");
    let terminal = timeout(terminal_wait, async {
        loop {
            let outcome = client
                .execute(&ReadCommittedOutcomeRequest {
                    run_id: request.run_id.clone(),
                    tenant_id: request.tenant_id.clone(),
                })
                .await
                .expect("read live terminal outcome");
            if matches!(outcome.state.as_str(), "final" | "cancelled" | "failed") {
                break outcome;
            }
            sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .expect("live execution deadline");
    shutdown.cancel();
    timeout(Duration::from_secs(35), supervisor_task)
        .await
        .expect("supervisor drain deadline")
        .expect("supervisor task join")
        .expect("supervisor result");

    assert_eq!(terminal.state, "final", "live run must commit, not fail");
    let terminal_outcome = terminal.terminal_outcome.expect("final terminal receipt");
    assert_eq!(
        terminal_outcome
            .get("kind")
            .and_then(serde_json::Value::as_str),
        Some("final")
    );
    assert!(
        terminal_outcome
            .get("answer_bundle_hash")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|hash| hash.starts_with("sha256:"))
    );
    // The script performs a read-only database assertion of the committed
    // bundle shape. Keep this test free of answer text so the live acceptance
    // path never emits a model answer or evidence payload into test output.
    let pool_stats = mcp_pool.stats().await;
    assert!(pool_stats.entries <= 4);
}
