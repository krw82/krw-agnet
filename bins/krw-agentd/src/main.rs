use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::{ArgAction, Parser};
use krw_agent_artifact_store::{
    ArtifactStore, ArtifactStoreConfig, LocalArtifactStore, MasterKeyring, VersionedMasterKey,
};
use krw_agent_capability_runtime::{McpToolTransport, PooledMcpTransport};
use krw_agent_image::load_image_set;
use krw_agent_persistence::agent_v1::{AgentV1Client, AgentV1Procedure, JsonProcedureExecutor};
use krw_agent_persistence::daemon::{
    AgentV1Store, ClaimedRunExecutor, RecoveryArtifactStore, RunSupervisor, RunWorkerConfig,
};
use krw_agent_persistence::metrics;
use krw_agent_persistence::postgres::{
    PostgresJsonExecutor, PostgresPoolOptions, ProcessEnvironmentDatabaseSecrets,
};
use krw_agent_protocol::{DeploymentBinding, ModelRegistry, PublicReleaseDescriptor};
use krw_agent_release_authorization::{
    ReleaseAuthorizationError, VerificationContext, parse_canonical_authorization,
    parse_canonical_trust_registry, verify_for_descriptor,
};
use krw_agent_runtime_config::{
    BudgetRegistry, EndpointRegistry, MAX_RELEASE_IMAGES, ProcessEnvironment, ValidationMode,
    load_yaml, resolve_release_set,
};
use krw_agent_runtime_persistence::{
    ArtifactRepository, ArtifactTtlPolicy, DeepSeekProviderCatalog, DurableRunStore,
    FinalizationPolicy, ProductionClaimedRunExecutor, ProductionReleaseCatalog,
};
use krw_agent_tool_mcp::McpClientPool;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use zeroize::Zeroizing;

const ARTIFACT_MAX_PLAINTEXT_BYTES: u64 = 16 * 1024 * 1024;
const RECOVERY_MAX_ARTIFACT_BYTES: usize = 8 * 1024 * 1024;
const RECOVERY_MAX_TOTAL_BYTES: usize = 16 * 1024 * 1024;
const MAX_ACTIVE_RUNS: usize = 64;
const MASTER_KEY_BYTES: usize = 32;
const MAX_DESCRIPTOR_WRITE_ATTEMPTS: u64 = 16;
const MAX_RELEASE_AUTHORIZATION_ARTIFACT_BYTES: u64 = 64 * 1024;
static DESCRIPTOR_STAGING_NONCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Parser)]
#[command(name = "krw-agentd", version, about = "Machine-wide KRW Agent daemon")]
struct Args {
    /// Immutable compiled image directory. Repeat 1..=64 times to define the
    /// complete startup release set; no image can be added until restart.
    #[arg(long = "image-dir", required = true, num_args = 1, action = ArgAction::Append)]
    image_dirs: Vec<PathBuf>,
    #[arg(long)]
    deployment_binding: PathBuf,
    #[arg(long)]
    model_registry: PathBuf,
    #[arg(long)]
    budget_registry: PathBuf,
    #[arg(long)]
    endpoint_registry: PathBuf,
    /// Validate immutable startup inputs and exit without accepting claims.
    #[arg(long)]
    check: bool,
    /// Also establish TLS `PostgreSQL` connections and prepare the entire fixed
    /// `agent_v1` ABI, then exit without claiming work.
    #[arg(long)]
    database_check: bool,
    /// Environment variable name containing the `PostgreSQL` URL. The URL itself
    /// is intentionally not accepted as a command-line argument.
    #[arg(long, default_value = "KRW_AGENT_DATABASE_URL")]
    database_url_env: String,
    /// Optional environment variable name containing additional CA PEM data.
    #[arg(long)]
    database_ca_pem_env: Option<String>,
    #[arg(long, default_value_t = 8)]
    database_max_connections: usize,
    #[arg(long, default_value_t = 1)]
    database_min_idle: usize,
    /// Stable deployment runtime version accepted by queued immutable claims.
    #[arg(long, default_value = env!("CARGO_PKG_VERSION"))]
    runtime_version: String,
    /// Optional absolute output file for the canonical, secret-free public
    /// release descriptor. Its parent directory must already exist.
    #[arg(long)]
    public_release_descriptor_output: Option<PathBuf>,
    /// Canonical Ed25519-signed authorization for this exact resolved release.
    /// Required together with `--release-trust-registry` before live claims.
    #[arg(long)]
    release_authorization: Option<PathBuf>,
    /// Canonical public trust registry used to verify release authorization.
    /// Required together with `--release-authorization` before live claims.
    #[arg(long)]
    release_trust_registry: Option<PathBuf>,
    /// Unique non-secret worker identity. Required for live claim admission.
    #[arg(long)]
    worker_id: Option<String>,
    /// Absolute private directory for encrypted recovery artifacts. Required live.
    #[arg(long)]
    artifact_root: Option<PathBuf>,
    /// Version of the active artifact master key.
    #[arg(long, default_value_t = 1)]
    artifact_active_key_version: u32,
    /// Environment-variable name containing the active key as exactly 64 hex characters.
    #[arg(long, default_value = "KRW_AGENT_ARTIFACT_KEY_V1")]
    artifact_active_key_env: String,
    /// Historical read key as `VERSION:ENV_NAME`; repeat during bounded key rotation.
    #[arg(long = "artifact-read-key")]
    artifact_read_keys: Vec<String>,
    #[arg(long, default_value_t = 8)]
    artifact_blocking_operations: usize,
    #[arg(long, default_value_t = 64)]
    mcp_pool_entries: usize,
    #[arg(long, default_value_t = 16)]
    max_active_runs: usize,
    /// Minimum age, in days, before a terminal run becomes eligible for the
    /// hourly row-retention reaper. 0 disables cleanup.
    #[arg(long, default_value_t = 30)]
    retention_days: u32,
    /// Address (ip:port) for the Prometheus `/metrics` scrape endpoint.
    /// Disabled unless a bind address is supplied.
    #[arg(long)]
    metrics_bind: Option<String>,
    /// Number of tokio worker threads driving the multi-thread runtime.
    /// Defaults to `min(available_parallelism, 8)` so a default deployment
    /// doesn't pin the provider/MCP pools to the historical hard-coded 2.
    #[arg(long, default_value_t = default_worker_threads())]
    worker_threads: usize,
    /// Per-host idle connection limit handed to the provider HTTP pool. Tune
    /// together with `--max-active-runs` and the model `max_in_flight`.
    #[arg(long, default_value_t = 8, alias = "deepseek-max-idle-per-host")]
    provider_max_idle_per_host: usize,
}

/// Default tokio worker thread count: cap at 8 so very large hosts don't
/// oversubscribe, fall back to 8 if `available_parallelism` errors out.
fn default_worker_threads() -> usize {
    std::thread::available_parallelism()
        .map_or(8, std::num::NonZero::get)
        .min(8)
}

#[derive(Debug, Error)]
enum StartupError {
    #[error("live daemon setting is missing: {0}")]
    MissingLiveSetting(&'static str),
    #[error("artifact key environment reference is invalid")]
    InvalidKeyEnvironmentReference,
    #[error("artifact key value is absent or invalid")]
    InvalidKeyValue,
    #[error("active run limit exceeds the resident recovery-memory bound")]
    ActiveRunLimit,
    #[error("--worker-threads must be greater than 0")]
    WorkerThreadsZero,
    #[error("startup release set must contain 1..=64 image directories")]
    ReleaseImageCount,
    #[error("release authorization and trust registry must be supplied together")]
    ReleaseAuthorizationPair,
    #[error("live daemon requires a signed release authorization")]
    MissingReleaseAuthorization,
    #[error("release authorization artifact is unsafe or unreadable")]
    ReleaseAuthorizationArtifact(#[source] std::io::Error),
    #[error("release authorization verification failed: {0}")]
    ReleaseAuthorization(#[from] ReleaseAuthorizationError),
    #[error("system clock is before Unix epoch")]
    SystemClock,
}

#[derive(Debug, Error)]
enum DescriptorExportError {
    #[error(
        "public release descriptor output must be an absolute, normalized file path below a non-root directory"
    )]
    UnsafeOutputPath,
    #[error("public release descriptor parent directory is unavailable: {0}")]
    ParentUnavailable(#[source] std::io::Error),
    #[error("public release descriptor output target must be absent or a regular file")]
    InvalidOutputTarget,
    #[error("public release descriptor contains forbidden public field: {0}")]
    ForbiddenField(String),
    #[error("public release descriptor serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("public release descriptor atomic write failed: {0}")]
    Write(#[source] std::io::Error),
}

#[derive(Debug, PartialEq, Eq)]
struct KeyEnvironmentReference {
    version: u32,
    environment_name: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .without_time()
        .init();
    let args = Args::parse();
    if args.worker_threads == 0 {
        return Err(Box::new(StartupError::WorkerThreadsZero));
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(args.worker_threads)
        .enable_all()
        .build()?;
    runtime.block_on(async_main(args))
}

#[allow(clippy::too_many_lines)]
async fn async_main(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    validate_image_dir_count(&args.image_dirs)?;
    let images = load_image_set(&args.image_dirs)?;
    let binding: DeploymentBinding = load_yaml(&args.deployment_binding)?;
    let models: ModelRegistry = load_yaml(&args.model_registry)?;
    let budgets: BudgetRegistry = load_yaml(&args.budget_registry)?;
    let endpoints: EndpointRegistry = load_yaml(&args.endpoint_registry)?;
    let releases = resolve_release_set(
        images,
        &binding,
        &models,
        &budgets,
        &endpoints,
        &ProcessEnvironment,
        ValidationMode::Production,
    )?;
    let providers = DeepSeekProviderCatalog::compile_release_set_with_idle(
        &releases,
        args.provider_max_idle_per_host,
    )?;
    let release_catalog = ProductionReleaseCatalog::compile(&releases)?;
    let descriptor = releases.public_descriptor(&args.runtime_version)?;
    verify_release_authorization(&args, &descriptor)?;
    if let Some(output) = &args.public_release_descriptor_output {
        export_public_release_descriptor(output, &descriptor)?;
        info!(
            output = %output.display(),
            release_set_hash = %descriptor.release_set_hash,
            entries = descriptor.entries.len(),
            "canonical public release descriptor committed"
        );
    }
    info!(
        release_set_hash = %release_catalog.release_set_hash(),
        images = release_catalog.len(),
        "immutable agent release set loaded"
    );
    for release in releases.releases() {
        info!(
            image_hash = %release.image.content_hash,
            agent = %release.image.body.metadata.id,
            capabilities = release.runtime.capabilities.len(),
            "agent release compiled"
        );
    }
    if args.check && !args.database_check {
        return Ok(());
    }
    let database =
        PostgresJsonExecutor::connect(postgres_options(&args), &ProcessEnvironmentDatabaseSecrets)
            .await?;
    if args.database_check {
        let readiness = database.readiness();
        info!(
            abi_version = readiness.abi_version,
            prepared_procedures = readiness.prepared_procedures,
            ready_connections = readiness.ready_connections,
            "PostgreSQL agent ABI ready"
        );
        return Ok(());
    }

    if args.max_active_runs == 0 || args.max_active_runs > MAX_ACTIVE_RUNS {
        return Err(StartupError::ActiveRunLimit.into());
    }
    let worker_id = args
        .worker_id
        .clone()
        .ok_or(StartupError::MissingLiveSetting("worker_id"))?;
    let retention_worker_id = worker_id.clone();
    let artifact_root = args
        .artifact_root
        .as_deref()
        .ok_or(StartupError::MissingLiveSetting("artifact_root"))?;
    let artifact_repository = build_artifact_repository(artifact_root, &args)?;

    let mcp_pool = Arc::new(McpClientPool::new(
        args.mcp_pool_entries,
        Duration::from_secs(90),
    )?);
    let pooled_transport = PooledMcpTransport::new(Arc::clone(&mcp_pool));
    let capability_transport: Arc<dyn McpToolTransport> = pooled_transport;
    let client = Arc::new(AgentV1Client::new(database));
    let supervisor_store: Arc<dyn AgentV1Store> = client.clone();
    let durable_store: Arc<dyn DurableRunStore> = client.clone();
    let recovery_artifacts: Arc<dyn RecoveryArtifactStore> = Arc::new(artifact_repository.clone());
    let executor: Arc<dyn ClaimedRunExecutor> = Arc::new(ProductionClaimedRunExecutor::new(
        Arc::clone(&release_catalog),
        args.runtime_version.clone(),
        providers,
        capability_transport,
        durable_store,
        artifact_repository,
        FinalizationPolicy::default(),
    )?);
    let supervisor = RunSupervisor::new(
        supervisor_store,
        executor,
        recovery_artifacts,
        RunWorkerConfig {
            worker_id,
            runtime_version: args.runtime_version,
            accepted_agent_image_hashes: release_catalog.accepted_image_hashes(),
            max_in_flight: args.max_active_runs,
            lease_duration: Duration::from_secs(30),
            heartbeat_interval: Duration::from_secs(8),
            idle_backoff_min: Duration::from_millis(25),
            idle_backoff_max: Duration::from_millis(500),
            error_backoff_min: Duration::from_millis(100),
            error_backoff_max: Duration::from_secs(2),
            recovery_preflight_timeout: Duration::from_secs(10),
            max_recovery_artifact_bytes: RECOVERY_MAX_ARTIFACT_BYTES,
            max_recovery_total_bytes: RECOVERY_MAX_TOTAL_BYTES,
            drain_timeout: Duration::from_secs(30),
            forced_abort_timeout: Duration::from_secs(3),
        },
    )?;

    info!(
        max_active_runs = args.max_active_runs,
        database_max_connections = args.database_max_connections,
        mcp_pool_entries = args.mcp_pool_entries,
        "live claim admission ready"
    );
    info!("outbox delivery remains host-owned; krw-agentd will not acknowledge undelivered events");
    let shutdown = CancellationToken::new();
    let signal_shutdown = shutdown.clone();
    let signal_task = tokio::spawn(async move {
        if shutdown_signal().await.is_err() {
            warn!("signal listener failed; initiating bounded daemon drain");
        }
        signal_shutdown.cancel();
    });
    let cleanup_task = if args.retention_days > 0 {
        let cleanup_client = Arc::clone(&client);
        let cleanup_shutdown = shutdown.clone();
        let retention_days = args.retention_days;
        Some(tokio::spawn(async move {
            run_retention_reaper(
                cleanup_client,
                retention_worker_id,
                retention_days,
                cleanup_shutdown,
            )
            .await;
        }))
    } else {
        None
    };
    // Initialize the shared Prometheus registry so collectors exist even before
    // the first scrape, then optionally serve /metrics.
    let registry = metrics::registry();
    let metrics_task = match args.metrics_bind.as_deref().map(parse_bind_address) {
        Some(Ok(bind)) => {
            info!(%bind, "Prometheus /metrics endpoint enabled");
            Some(spawn_metrics_server(bind, registry))
        }
        Some(Err(error)) => {
            warn!(bind = ?args.metrics_bind, error = %error, "invalid --metrics-bind address; /metrics disabled");
            None
        }
        None => {
            info!("--metrics-bind not supplied; /metrics endpoint disabled");
            None
        }
    };
    let result = supervisor.run(shutdown.clone()).await;
    shutdown.cancel();
    signal_task.abort();
    if let Some(task) = cleanup_task {
        task.abort();
    }
    if let Some(task) = metrics_task {
        task.abort();
    }
    result?;
    let pool_stats = mcp_pool.stats().await;
    info!(
        mcp_pool_entries = pool_stats.entries,
        mcp_pool_in_use = pool_stats.in_use,
        "daemon drain complete"
    );
    Ok(())
}

fn validate_image_dir_count(image_dirs: &[PathBuf]) -> Result<(), StartupError> {
    if image_dirs.is_empty() || image_dirs.len() > MAX_RELEASE_IMAGES {
        return Err(StartupError::ReleaseImageCount);
    }
    Ok(())
}

/// Check a release authorization before opening database connections or
/// admitting claims. Check/database-check modes can omit both artifacts for
/// authoring diagnostics, but if either is supplied they must prove the exact
/// descriptor the daemon just resolved. Live mode cannot omit them.
fn verify_release_authorization(
    args: &Args,
    descriptor: &PublicReleaseDescriptor,
) -> Result<(), StartupError> {
    let (authorization_path, trust_registry_path) = match (
        args.release_authorization.as_deref(),
        args.release_trust_registry.as_deref(),
    ) {
        (None, None) if args.check || args.database_check => return Ok(()),
        (None, None) => return Err(StartupError::MissingReleaseAuthorization),
        (Some(authorization), Some(trust_registry)) => (authorization, trust_registry),
        _ => return Err(StartupError::ReleaseAuthorizationPair),
    };
    let authorization = parse_canonical_authorization(
        &read_release_authorization_artifact(authorization_path)
            .map_err(StartupError::ReleaseAuthorizationArtifact)?,
    )?;
    let trust = parse_canonical_trust_registry(
        &read_release_authorization_artifact(trust_registry_path)
            .map_err(StartupError::ReleaseAuthorizationArtifact)?,
    )?;
    let now_unix_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| StartupError::SystemClock)?
        .as_secs();
    verify_for_descriptor(
        &authorization,
        &trust,
        descriptor,
        VerificationContext {
            runtime_version: &args.runtime_version,
            kernel_version: env!("CARGO_PKG_VERSION"),
            now_unix_seconds,
        },
    )?;
    info!(
        key_id = %authorization.payload.key_id,
        sequence = authorization.payload.sequence,
        release_set_hash = %descriptor.release_set_hash,
        "signed release authorization verified"
    );
    Ok(())
}

fn read_release_authorization_artifact(path: &Path) -> Result<Vec<u8>, std::io::Error> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "artifact must be a regular non-symlink file",
        ));
    }
    if metadata.len() == 0 || metadata.len() > MAX_RELEASE_AUTHORIZATION_ARTIFACT_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "artifact is outside the allowed byte bound",
        ));
    }
    fs::read(path)
}

fn export_public_release_descriptor(
    output: &Path,
    descriptor: &PublicReleaseDescriptor,
) -> Result<(), DescriptorExportError> {
    let output = resolve_descriptor_output_path(output)?;
    let value = serde_json::to_value(descriptor)?;
    reject_forbidden_public_fields(&value)?;
    let bytes = serde_jcs::to_vec(&value).map_err(DescriptorExportError::Serialization)?;
    atomic_write_private_file(&output, &bytes).map_err(DescriptorExportError::Write)
}

fn resolve_descriptor_output_path(output: &Path) -> Result<PathBuf, DescriptorExportError> {
    if !output.is_absolute()
        || output
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(DescriptorExportError::UnsafeOutputPath);
    }
    let file_name = output
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or(DescriptorExportError::UnsafeOutputPath)?;
    let parent = output
        .parent()
        .filter(|parent| *parent != Path::new("/"))
        .ok_or(DescriptorExportError::UnsafeOutputPath)?;
    let canonical_parent = parent
        .canonicalize()
        .map_err(DescriptorExportError::ParentUnavailable)?;
    if canonical_parent == Path::new("/") {
        return Err(DescriptorExportError::UnsafeOutputPath);
    }
    let parent_metadata =
        fs::metadata(&canonical_parent).map_err(DescriptorExportError::ParentUnavailable)?;
    if !parent_metadata.is_dir() {
        return Err(DescriptorExportError::UnsafeOutputPath);
    }
    let resolved = canonical_parent.join(file_name);
    match fs::symlink_metadata(&resolved) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            Err(DescriptorExportError::InvalidOutputTarget)
        }
        Ok(_) => Ok(resolved),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(resolved),
        Err(error) => Err(DescriptorExportError::ParentUnavailable(error)),
    }
}

fn reject_forbidden_public_fields(value: &serde_json::Value) -> Result<(), DescriptorExportError> {
    match value {
        serde_json::Value::Object(object) => {
            for (key, nested) in object {
                let normalized = key.to_ascii_lowercase();
                if normalized.contains("secret")
                    || normalized.contains("credential")
                    || normalized.contains("endpoint")
                    || normalized.contains("prompt")
                    || normalized == "api_base"
                    || normalized == "api_key"
                    || normalized == "headers"
                    || normalized == "url"
                {
                    return Err(DescriptorExportError::ForbiddenField(key.clone()));
                }
                reject_forbidden_public_fields(nested)?;
            }
        }
        serde_json::Value::Array(array) => {
            for nested in array {
                reject_forbidden_public_fields(nested)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn atomic_write_private_file(output: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let parent = output.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "descriptor output has no parent directory",
        )
    })?;
    let file_name = output
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("public-release-descriptor.json");
    let mut last_collision = None;
    for _ in 0..MAX_DESCRIPTOR_WRITE_ATTEMPTS {
        let nonce = DESCRIPTOR_STAGING_NONCE.fetch_add(1, Ordering::Relaxed);
        let staging = parent.join(format!(".{file_name}.{}.{}.tmp", std::process::id(), nonce));
        match create_private_staging_file(&staging) {
            Ok(mut file) => {
                let result = (|| {
                    file.write_all(bytes)?;
                    file.sync_all()?;
                    drop(file);
                    fs::rename(&staging, output)?;
                    File::open(parent)?.sync_all()?;
                    Ok(())
                })();
                if result.is_err() {
                    let _ = fs::remove_file(&staging);
                }
                return result;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                last_collision = Some(error);
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_collision.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "descriptor staging name collision limit exceeded",
        )
    }))
}

fn create_private_staging_file(path: &Path) -> Result<File, std::io::Error> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn postgres_options(args: &Args) -> PostgresPoolOptions {
    PostgresPoolOptions {
        url_env: args.database_url_env.clone(),
        ca_pem_env: args.database_ca_pem_env.clone(),
        application_name: "krw-agentd".into(),
        max_connections: args.database_max_connections,
        min_idle: args.database_min_idle,
        connect_timeout: Duration::from_secs(5),
        checkout_timeout: Duration::from_secs(2),
        query_timeout: Duration::from_secs(15),
        statement_timeout: Duration::from_secs(14),
        lock_timeout: Duration::from_secs(2),
    }
}

fn build_artifact_repository(
    root: &Path,
    args: &Args,
) -> Result<ArtifactRepository, Box<dyn std::error::Error>> {
    let mut keys = Vec::with_capacity(1 + args.artifact_read_keys.len());
    keys.push(VersionedMasterKey::new(
        args.artifact_active_key_version,
        read_master_key(&args.artifact_active_key_env)?,
    )?);
    for encoded in &args.artifact_read_keys {
        let reference = parse_key_environment_reference(encoded)?;
        keys.push(VersionedMasterKey::new(
            reference.version,
            read_master_key(&reference.environment_name)?,
        )?);
    }
    let keyring = MasterKeyring::new(args.artifact_active_key_version, keys)?;
    let store: Arc<dyn ArtifactStore> = Arc::new(LocalArtifactStore::open(
        root,
        keyring,
        ArtifactStoreConfig {
            max_plaintext_bytes: ARTIFACT_MAX_PLAINTEXT_BYTES,
            max_ttl: Duration::from_hours(24),
            max_maintenance_examined: 2_048,
            max_maintenance_deleted: 256,
            max_blocking_operations: args.artifact_blocking_operations,
        },
    )?);
    Ok(ArtifactRepository::new(
        store,
        ArtifactTtlPolicy::default(),
    )?)
}

fn parse_key_environment_reference(encoded: &str) -> Result<KeyEnvironmentReference, StartupError> {
    let (version, environment_name) = encoded
        .split_once(':')
        .ok_or(StartupError::InvalidKeyEnvironmentReference)?;
    let version = version
        .parse::<u32>()
        .ok()
        .filter(|version| *version != 0)
        .ok_or(StartupError::InvalidKeyEnvironmentReference)?;
    if !valid_environment_name(environment_name) {
        return Err(StartupError::InvalidKeyEnvironmentReference);
    }
    Ok(KeyEnvironmentReference {
        version,
        environment_name: environment_name.into(),
    })
}

fn read_master_key(environment_name: &str) -> Result<[u8; MASTER_KEY_BYTES], StartupError> {
    if !valid_environment_name(environment_name) {
        return Err(StartupError::InvalidKeyEnvironmentReference);
    }
    let encoded =
        Zeroizing::new(std::env::var(environment_name).map_err(|_| StartupError::InvalidKeyValue)?);
    decode_master_key(&encoded)
}

fn decode_master_key(encoded: &str) -> Result<[u8; MASTER_KEY_BYTES], StartupError> {
    if encoded.len() != MASTER_KEY_BYTES * 2 || !encoded.is_ascii() {
        return Err(StartupError::InvalidKeyValue);
    }
    let mut material = [0_u8; MASTER_KEY_BYTES];
    hex::decode_to_slice(encoded.as_bytes(), &mut material)
        .map_err(|_| StartupError::InvalidKeyValue)?;
    Ok(material)
}

fn valid_environment_name(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_uppercase())
        && characters.all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
}

#[cfg(unix)]
async fn shutdown_signal() -> Result<(), std::io::Error> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        _ = terminate.recv() => Ok(()),
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() -> Result<(), std::io::Error> {
    tokio::signal::ctrl_c().await
}

/// Periodically reaps terminal runs older than the retention cutoff. Runs once
/// shortly after startup and then on an hourly cadence until the shutdown token
/// is cancelled. Failures are logged and never abort the daemon: the reaper is
/// best-effort background hygiene and the next tick will retry.
async fn run_retention_reaper(
    client: Arc<AgentV1Client<PostgresJsonExecutor>>,
    worker_id: String,
    retention_days: u32,
    shutdown: CancellationToken,
) {
    use tokio::time::{Duration, interval};
    #[allow(
        clippy::duration_suboptimal_units,
        reason = "std Duration has no from_hours"
    )]
    const REAPER_PERIOD: Duration = Duration::from_secs(60 * 60);
    const REAPER_MAX_RUNS: u64 = 1000;
    let request = serde_json::json!({
        "abi_version": krw_agent_persistence::agent_v1::ABI_VERSION,
        "worker_id": worker_id,
        "retention_days": retention_days,
        "max_runs": REAPER_MAX_RUNS,
    });
    let mut ticker = interval(REAPER_PERIOD);
    loop {
        tokio::select! {
            () = shutdown.cancelled() => break,
            _ = ticker.tick() => {}
        }
        if shutdown.is_cancelled() {
            break;
        }
        match client
            .executor()
            .execute_json(AgentV1Procedure::ReapRetainedRuns, request.clone())
            .await
        {
            Ok(response) => {
                let reaped = response.get("reaped").and_then(serde_json::Value::as_u64);
                info!(
                    reaped,
                    retention_days, "row retention reaper completed a sweep"
                );
            }
            Err(failure) => {
                let hash = failure.diagnostic_hash;
                warn!(
                    diagnostic_hash = %hash,
                    "row retention reaper sweep failed; will retry next tick"
                );
            }
        }
    }
}

/// Parse a `host:port` metrics bind string into a [`SocketAddr`].
fn parse_bind_address(value: &str) -> Result<SocketAddr, std::net::AddrParseError> {
    value.parse::<SocketAddr>()
}

/// Spawn a minimal HTTP server that serves the Prometheus text exposition
/// format at `/metrics`. Any other path receives a 404. The server is
/// intentionally tiny: no routing framework, no request body parsing.
fn spawn_metrics_server(
    bind: SocketAddr,
    registry: &'static prometheus::Registry,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let listener = match tokio::net::TcpListener::bind(bind).await {
            Ok(listener) => listener,
            Err(error) => {
                warn!(%bind, error = %error, "metrics TCP listener bind failed; /metrics disabled");
                return;
            }
        };
        info!(%bind, "Prometheus /metrics endpoint listening");
        loop {
            let accept = listener.accept().await;
            let (mut socket, peer) = match accept {
                Ok((socket, peer)) => (socket, peer),
                Err(error) => {
                    warn!(%error, "metrics listener accept failed");
                    continue;
                }
            };
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                let request_line = match read_http_request_line(&mut socket).await {
                    Ok(line) => line,
                    Err(error) => {
                        warn!(%peer, error = %error, "metrics request read failed");
                        return;
                    }
                };
                let body = if request_line.starts_with("GET /metrics") {
                    let metric_families = registry.gather();
                    let mut buffer = String::new();
                    let encoder = prometheus::TextEncoder::new();
                    match encoder.encode_utf8(&metric_families, &mut buffer) {
                        Ok(()) => buffer.into_bytes(),
                        Err(error) => {
                            warn!(%error, "metrics encoding failed");
                            return;
                        }
                    }
                } else {
                    Vec::new()
                };
                let (status, content_type) = if request_line.starts_with("GET /metrics") {
                    ("200 OK", "text/plain; version=0.0.4; charset=utf-8")
                } else {
                    ("404 Not Found", "text/plain; charset=utf-8")
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.write_all(&body).await;
                let _ = socket.shutdown().await;
            });
        }
    })
}

/// Read just enough of an HTTP/1.x request to obtain the request line. The
/// metrics endpoint never inspects headers or bodies, so we discard everything
/// after the first line up to a small cap.
async fn read_http_request_line(socket: &mut tokio::net::TcpStream) -> std::io::Result<String> {
    use tokio::io::AsyncReadExt;
    let mut buffer = [0_u8; 1024];
    let mut filled = 0_usize;
    loop {
        if filled >= buffer.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "metrics request line exceeded 1024 bytes",
            ));
        }
        let read = socket.read(&mut buffer[filled..]).await?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "metrics connection closed before request line",
            ));
        }
        filled += read;
        let window = &buffer[..filled];
        if let Some(newline) = window.iter().position(|byte| *byte == b'\n') {
            let mut end = newline;
            if end > 0 && buffer[end - 1] == b'\r' {
                end -= 1;
            }
            return Ok(String::from_utf8_lossy(&buffer[..end]).into_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use krw_agent_protocol::{ContentHash, GLM_MODEL_ID, PUBLIC_RELEASE_DESCRIPTOR_SCHEMA_VERSION};
    use krw_agent_release_authorization::{
        RELEASE_AUTHORIZATION_SCHEMA_VERSION, RELEASE_TRUST_REGISTRY_SCHEMA_VERSION,
        ReleaseAuthorizationPayloadV1, ReleaseTrustKeyV1, ReleaseTrustRegistryV1,
        canonical_authorization_bytes, canonical_trust_registry_bytes, generate_private_key_pkcs8,
        public_descriptor_hash, public_key_hex_from_private_key, sign,
    };
    use tempfile::TempDir;

    fn required_args() -> Vec<String> {
        [
            "krw-agentd",
            "--deployment-binding",
            "binding.yaml",
            "--model-registry",
            "models.yaml",
            "--budget-registry",
            "budgets.yaml",
            "--endpoint-registry",
            "endpoints.yaml",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }

    #[test]
    fn key_material_is_exactly_32_hex_bytes() {
        assert_eq!(decode_master_key(&"42".repeat(32)).unwrap(), [0x42; 32]);
        assert!(decode_master_key(&"42".repeat(31)).is_err());
        assert!(decode_master_key(&format!("{}gg", "42".repeat(31))).is_err());
    }

    #[test]
    fn historical_key_reference_contains_only_version_and_env_name() {
        assert_eq!(
            parse_key_environment_reference("7:KRW_AGENT_ARTIFACT_KEY_V7").unwrap(),
            KeyEnvironmentReference {
                version: 7,
                environment_name: "KRW_AGENT_ARTIFACT_KEY_V7".into(),
            }
        );
        for invalid in [
            "0:KRW_AGENT_ARTIFACT_KEY",
            "7:lowercase",
            "7:KRW-KEY",
            "7",
            "x:KRW_AGENT_ARTIFACT_KEY",
        ] {
            assert!(parse_key_environment_reference(invalid).is_err());
        }
    }

    #[test]
    fn image_dir_is_repeatable_and_has_a_closed_machine_bound() {
        let mut args = required_args();
        args.extend([
            "--image-dir".into(),
            "image-a".into(),
            "--image-dir".into(),
            "image-b".into(),
        ]);
        let parsed = Args::try_parse_from(args).unwrap();
        assert_eq!(
            parsed.image_dirs,
            vec![PathBuf::from("image-a"), PathBuf::from("image-b")]
        );
        validate_image_dir_count(&parsed.image_dirs).unwrap();
        assert!(matches!(
            validate_image_dir_count(&vec![PathBuf::from("image"); MAX_RELEASE_IMAGES + 1]),
            Err(StartupError::ReleaseImageCount)
        ));
        assert!(Args::try_parse_from(required_args()).is_err());
    }

    #[test]
    fn public_descriptor_argument_is_an_explicit_optional_path() {
        let mut args = required_args();
        args.extend([
            "--image-dir".into(),
            "image-a".into(),
            "--public-release-descriptor-output".into(),
            "/var/lib/krw-agent/public-release.json".into(),
        ]);
        let parsed = Args::try_parse_from(args).unwrap();
        assert_eq!(
            parsed.public_release_descriptor_output,
            Some(PathBuf::from("/var/lib/krw-agent/public-release.json"))
        );
    }

    #[test]
    fn signed_release_authorization_is_required_only_for_live_claims_and_paths_are_paired() {
        let mut command_line = required_args();
        command_line.extend(["--image-dir".into(), "image-a".into()]);
        let mut args = Args::try_parse_from(command_line).unwrap();
        let descriptor = PublicReleaseDescriptor {
            schema_version: PUBLIC_RELEASE_DESCRIPTOR_SCHEMA_VERSION,
            release_set_hash: ContentHash::sha256(b"release-set"),
            runtime_version: args.runtime_version.clone(),
            entries: Vec::new(),
        };
        assert!(matches!(
            verify_release_authorization(&args, &descriptor),
            Err(StartupError::MissingReleaseAuthorization)
        ));

        args.check = true;
        verify_release_authorization(&args, &descriptor).unwrap();
        args.release_authorization = Some(PathBuf::from("authorization.json"));
        assert!(matches!(
            verify_release_authorization(&args, &descriptor),
            Err(StartupError::ReleaseAuthorizationPair)
        ));
    }

    #[test]
    fn daemon_verifies_a_canonical_signed_release_before_live_claims() {
        let directory = TempDir::new().unwrap();
        let mut command_line = required_args();
        command_line.extend(["--image-dir".into(), "image-a".into()]);
        let mut args = Args::try_parse_from(command_line).unwrap();
        let descriptor = PublicReleaseDescriptor {
            schema_version: PUBLIC_RELEASE_DESCRIPTOR_SCHEMA_VERSION,
            release_set_hash: ContentHash::sha256(b"release-set"),
            runtime_version: args.runtime_version.clone(),
            entries: Vec::new(),
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock after Unix epoch")
            .as_secs();
        let issued_at = now.saturating_sub(60);
        let expires_at = now.checked_add(3_600).expect("test clock overflow");
        let private_key = generate_private_key_pkcs8().unwrap();
        let key_id = "release-2026-test";
        let authorization = sign(
            ReleaseAuthorizationPayloadV1 {
                schema_version: RELEASE_AUTHORIZATION_SCHEMA_VERSION,
                key_id: key_id.into(),
                sequence: 7,
                issued_at_unix_seconds: issued_at,
                expires_at_unix_seconds: expires_at,
                release_descriptor_hash: public_descriptor_hash(&descriptor).unwrap(),
                release_set_hash: descriptor.release_set_hash.clone(),
                runtime_version: args.runtime_version.clone(),
                kernel_version: env!("CARGO_PKG_VERSION").into(),
                model_id: GLM_MODEL_ID.into(),
            },
            &private_key,
        )
        .unwrap();
        let trust = ReleaseTrustRegistryV1 {
            schema_version: RELEASE_TRUST_REGISTRY_SCHEMA_VERSION,
            registry_id: "test-release-trust".into(),
            minimum_sequence: 7,
            keys: vec![ReleaseTrustKeyV1 {
                key_id: key_id.into(),
                ed25519_public_key_hex: public_key_hex_from_private_key(&private_key).unwrap(),
                not_before_unix_seconds: now.saturating_sub(3_600),
                not_after_unix_seconds: now.checked_add(7_200).expect("test clock overflow"),
                revoked: false,
            }],
        };
        let authorization_path = directory.path().join("authorization.json");
        let trust_path = directory.path().join("trust.json");
        fs::write(
            &authorization_path,
            canonical_authorization_bytes(&authorization).unwrap(),
        )
        .unwrap();
        fs::write(&trust_path, canonical_trust_registry_bytes(&trust).unwrap()).unwrap();
        args.release_authorization = Some(authorization_path);
        args.release_trust_registry = Some(trust_path);

        verify_release_authorization(&args, &descriptor).unwrap();
    }

    #[test]
    fn descriptor_export_is_canonical_private_and_replaceable() {
        let directory = TempDir::new().unwrap();
        let output = directory.path().join("public-release.json");
        fs::write(&output, b"stale descriptor").unwrap();
        let descriptor = PublicReleaseDescriptor {
            schema_version: 1,
            release_set_hash: ContentHash::sha256(b"release-set"),
            runtime_version: "krw-agentd-2026-08".into(),
            entries: Vec::new(),
        };

        export_public_release_descriptor(&output, &descriptor).unwrap();

        let bytes = fs::read(&output).unwrap();
        let value = serde_json::to_value(&descriptor).unwrap();
        assert_eq!(bytes, serde_jcs::to_vec(&value).unwrap());
        assert!(!bytes.ends_with(b"\n"));
        let names = fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, vec![output.file_name().unwrap()]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&output).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn failed_atomic_commit_leaves_no_partial_staging_file() {
        let directory = TempDir::new().unwrap();
        let invalid_target = directory.path().join("public-release.json");
        fs::create_dir(&invalid_target).unwrap();

        assert!(atomic_write_private_file(&invalid_target, b"complete").is_err());

        assert!(invalid_target.is_dir());
        let staging_files = fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().starts_with(".public-release.json."))
            .collect::<Vec<_>>();
        assert!(staging_files.is_empty());
    }

    #[test]
    fn descriptor_path_rejects_relative_root_broad_and_non_normal_paths() {
        let directory = TempDir::new().unwrap();
        for invalid in [
            PathBuf::from("public-release.json"),
            PathBuf::from("/"),
            PathBuf::from("/public-release.json"),
            PathBuf::from("/tmp/../tmp/public-release.json"),
        ] {
            assert!(matches!(
                resolve_descriptor_output_path(&invalid),
                Err(DescriptorExportError::UnsafeOutputPath)
            ));
        }
        assert!(matches!(
            resolve_descriptor_output_path(
                &directory.path().join("missing").join("public-release.json")
            ),
            Err(DescriptorExportError::ParentUnavailable(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_path_rejects_an_existing_symlink_target() {
        use std::os::unix::fs::symlink;

        let directory = TempDir::new().unwrap();
        let real_file = directory.path().join("real.json");
        let output = directory.path().join("public-release.json");
        fs::write(&real_file, b"do not replace").unwrap();
        symlink(&real_file, &output).unwrap();

        assert!(matches!(
            resolve_descriptor_output_path(&output),
            Err(DescriptorExportError::InvalidOutputTarget)
        ));
        assert_eq!(fs::read(&real_file).unwrap(), b"do not replace");
    }

    #[test]
    fn public_structure_guard_rejects_sensitive_field_names_recursively() {
        let unsafe_value = serde_json::json!({
            "entries": [{"safe": true, "nested": {"credential_ref": "not-public"}}]
        });
        assert!(matches!(
            reject_forbidden_public_fields(&unsafe_value),
            Err(DescriptorExportError::ForbiddenField(field)) if field == "credential_ref"
        ));
    }
}
