use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use krw_agent_capability_runtime::{
    CapabilityCatalog, McpToolTransport, PooledMcpCapabilityRuntime, RunScope,
};
use krw_agent_image::LoadedImage;
use krw_agent_persistence::agent_v1::{ClaimReceipt, SessionMemoryReadMode};
use krw_agent_persistence::daemon::{
    ClaimedRunContext, ClaimedRunExecutor, RunExecutionFailure, SuccessfulRunOutcome,
};
use krw_agent_protocol::{ALLOWED_MODEL_IDS, ContentHash, DeploymentBinding, RunRequest};
use krw_agent_provider_wire::{ProviderClient, ProviderClientConfig};
use krw_agent_run_engine::{
    CapabilityRuntime, DeliveryCertainty, EngineConfig, EngineError, FinalStatus, RunEngine,
    RunInput, TrustedMarketSnapshot, durable_failure_diagnostic, state_artifact_failure_code,
};
use krw_agent_runtime_config::{ResolvedReleaseSet, ResolvedRuntime};
use krw_context_planner::ContextPlanner;
use serde_json::json;
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::timeout;

use crate::{
    ArtifactRepository, ClaimValidationError, DurableRunPersistence, DurableRunStore,
    FinalizationPolicy, MemoryResolutionError, SessionMemoryPageAccumulator, ValidatedClaim,
    validate_claim,
};

const CANCEL_DRAIN_GRACE: Duration = Duration::from_secs(2);
const SESSION_MEMORY_PAGE_LIMIT: u16 = 8;
/// The market seed is useful only when it arrives immediately. It must never
/// add a recovery branch or make a company-research run fail.
const MARKET_SNAPSHOT_PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(3);

/// Stable, content-free claim admission diagnostics.  The persisted terminal
/// reason must let operators distinguish a host-contract drift from a corrupt
/// receipt without retaining requests, model output, or configuration values.
fn claim_failure_code(error: &ClaimValidationError) -> &'static str {
    match error {
        ClaimValidationError::InvalidEncoding => "invalid_claim_encoding",
        ClaimValidationError::PayloadSize => "claim_payload_size_invalid",
        ClaimValidationError::PayloadHashMismatch => "claim_payload_hash_mismatch",
        ClaimValidationError::IdentityMismatch => "claim_identity_mismatch",
        ClaimValidationError::DeploymentMismatch => "claim_deployment_mismatch",
        ClaimValidationError::ResourceMismatch => "claim_resource_profile_mismatch",
        ClaimValidationError::SnapshotMismatch => "claim_snapshot_mismatch",
        ClaimValidationError::SnapshotContractMismatch(field) => match *field {
            "protocol_version" => "claim_execution_protocol_version_mismatch",
            "agent_image_hash" => "claim_execution_image_hash_mismatch",
            "deployment_binding_hash" => "claim_execution_binding_hash_mismatch",
            "model_registry_hash" => "claim_execution_model_registry_mismatch",
            "budget_registry_hash" => "claim_execution_budget_registry_mismatch",
            "model_profile" => "claim_execution_model_profile_mismatch",
            "requested_model" | "resolved_model" => "claim_execution_model_mismatch",
            "provider_api_version" => "claim_execution_provider_api_mismatch",
            "provider_max_context_tokens" => "claim_execution_provider_context_mismatch",
            "provider_wire_capabilities" => "claim_execution_provider_wire_mismatch",
            "thinking" | "reasoning_effort" => "claim_execution_thinking_profile_mismatch",
            "capability_release_hashes" => "claim_execution_capability_release_mismatch",
            "budget" => "claim_execution_budget_mismatch",
            _ => "claim_execution_contract_mismatch",
        },
        ClaimValidationError::ModelContextExceeded => "claim_model_context_exceeded",
        ClaimValidationError::Runtime(_) => "claim_runtime_resolution_failed",
    }
}

#[derive(Debug, Error)]
pub enum ProviderCatalogError {
    #[error("resolved runtime contains no provider model")]
    Empty,
    #[error("resolved runtime contains a duplicate or inconsistent model")]
    Duplicate,
    #[error("resolved runtime must contain only protocol-allowed models")]
    ModelInventory,
    #[error("resolved runtime model API version is unsupported")]
    ApiVersion,
    #[error("provider client construction failed")]
    Client,
    #[error("provider API key is missing for model {0}")]
    MissingApiKey(String),
}

/// Machine-wide `DeepSeek` clients grouped by exact HTTPS API base. Models on
/// the same base share one HTTP/2 connection pool and authorization header.
pub struct DeepSeekProviderCatalog {
    by_model: BTreeMap<String, Arc<ProviderClient>>,
    permits_by_model: BTreeMap<String, Arc<Semaphore>>,
}

/// Maps each distinct `api_base` to the API key for the model(s) hosted there.
/// `DeepSeek` models get the `DeepSeek` key, GLM models get the GLM key. Two models
/// on the same base must share one key (they share one HTTP pool); if a base
/// hosts mixed providers that would be a deployment misconfiguration caught
/// downstream as a `MissingApiKey` error.
fn build_api_keys_by_base<'a>(
    models: impl IntoIterator<Item = &'a krw_agent_protocol::ModelDescriptor>,
    deepseek_api_key: &str,
    glm_api_key: &str,
) -> BTreeMap<String, String> {
    let mut by_base: BTreeMap<String, String> = BTreeMap::new();
    for model in models {
        let key = if model.model_id == krw_agent_protocol::GLM_MODEL_ID {
            glm_api_key.to_owned()
        } else {
            deepseek_api_key.to_owned()
        };
        by_base.entry(model.api_base.clone()).or_insert(key);
    }
    by_base
}

impl DeepSeekProviderCatalog {
    pub fn compile(runtime: &ResolvedRuntime) -> Result<Arc<Self>, ProviderCatalogError> {
        let api_keys_by_base = build_api_keys_by_base(
            runtime.models(),
            runtime.deepseek_api_key(),
            runtime.glm_api_key(),
        );
        Self::compile_from(runtime.models(), &api_keys_by_base, 8)
    }

    /// Backwards-compatible default entry point: keeps the pre-existing
    /// `max_idle_per_host = 8` semantics for callers that don't have a CLI
    /// override handy.
    pub fn compile_release_set(
        releases: &ResolvedReleaseSet,
    ) -> Result<Arc<Self>, ProviderCatalogError> {
        let api_keys_by_base = build_api_keys_by_base(
            releases.models(),
            releases.deepseek_api_key(),
            releases.glm_api_key(),
        );
        Self::compile_from(releases.models(), &api_keys_by_base, 8)
    }

    /// Same as `compile_release_set` but lets the daemon thread a CLI-supplied
    /// `--deepseek-max-idle-per-host` value into the underlying HTTP pool.
    pub fn compile_release_set_with_idle(
        releases: &ResolvedReleaseSet,
        max_idle_per_host: usize,
    ) -> Result<Arc<Self>, ProviderCatalogError> {
        let api_keys_by_base = build_api_keys_by_base(
            releases.models(),
            releases.deepseek_api_key(),
            releases.glm_api_key(),
        );
        Self::compile_from(releases.models(), &api_keys_by_base, max_idle_per_host)
    }

    fn compile_from<'a>(
        models: impl IntoIterator<Item = &'a krw_agent_protocol::ModelDescriptor>,
        api_keys_by_base: &BTreeMap<String, String>,
        max_idle_per_host: usize,
    ) -> Result<Arc<Self>, ProviderCatalogError> {
        let mut by_base = BTreeMap::<String, BTreeSet<String>>::new();
        let mut seen = BTreeSet::new();
        let mut max_in_flight_by_model = BTreeMap::<String, u16>::new();
        for model in models {
            if model.api_version != "anthropic-messages-v1" {
                return Err(ProviderCatalogError::ApiVersion);
            }
            if !seen.insert(model.model_id.clone()) {
                return Err(ProviderCatalogError::Duplicate);
            }
            max_in_flight_by_model.insert(model.model_id.clone(), model.max_in_flight);
            by_base
                .entry(model.api_base.clone())
                .or_default()
                .insert(model.model_id.clone());
        }
        if seen.is_empty() {
            return Err(ProviderCatalogError::Empty);
        }
        // The catalog admits any non-empty subset of the protocol's allowed
        // model inventory.  Pre-multi-provider this was a strict equality
        // check against the single DeepSeek model; relaxing it to a subset
        // keeps the original single-model deployment behaving identically
        // while permitting additional provider models (e.g. GLM) to coexist
        // in one release set.
        if !seen
            .iter()
            .all(|model| ALLOWED_MODEL_IDS.contains(&model.as_str()))
        {
            return Err(ProviderCatalogError::ModelInventory);
        }
        let mut by_model = BTreeMap::new();
        let mut permits_by_model = BTreeMap::new();
        for (api_base, models) in by_base {
            let api_key = api_keys_by_base.get(&api_base).ok_or_else(|| {
                ProviderCatalogError::MissingApiKey(
                    models.iter().next().cloned().unwrap_or_default(),
                )
            })?;
            if api_key.is_empty() {
                return Err(ProviderCatalogError::MissingApiKey(
                    models.iter().next().cloned().unwrap_or_default(),
                ));
            }
            let client = Arc::new(
                ProviderClient::new(
                    ProviderClientConfig::production(
                        api_base,
                        models.iter().cloned(),
                        max_idle_per_host,
                    ),
                    api_key,
                )
                .map_err(|_| ProviderCatalogError::Client)?,
            );
            for model in models {
                if by_model
                    .insert(model.clone(), Arc::clone(&client))
                    .is_some()
                {
                    return Err(ProviderCatalogError::Duplicate);
                }
                let permits = max_in_flight_by_model
                    .get(&model)
                    .copied()
                    .unwrap_or(1)
                    .max(1) as usize;
                permits_by_model.insert(model, Arc::new(Semaphore::new(permits)));
            }
        }
        Ok(Arc::new(Self {
            by_model,
            permits_by_model,
        }))
    }

    fn exact(&self, model: &str) -> Option<Arc<ProviderClient>> {
        self.by_model.get(model).cloned()
    }

    /// Acquires an in-flight permit for `model_id`, bounding concurrent
    /// provider episodes per model to the descriptor's `max_in_flight`. The
    /// returned RAII permit releases the slot when dropped.
    pub async fn acquire_permit(
        &self,
        model_id: &str,
    ) -> Result<OwnedSemaphorePermit, RunExecutionFailure> {
        let semaphore = self
            .permits_by_model
            .get(model_id)
            .ok_or_else(|| RunExecutionFailure::failed("provider_model_not_loaded"))?;
        semaphore.clone().acquire_owned().await.map_err(|_| {
            RunExecutionFailure::deferred("provider_in_flight_saturated", Duration::from_secs(1))
        })
    }
}

impl fmt::Debug for DeepSeekProviderCatalog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeepSeekProviderCatalog")
            .field("models", &self.by_model.keys().collect::<Vec<_>>())
            .field(
                "permits",
                &self
                    .permits_by_model
                    .iter()
                    .map(|(k, v)| (k, v.available_permits()))
                    .collect::<BTreeMap<_, _>>(),
            )
            .field("authorization", &"[REDACTED]")
            .finish()
    }
}

/// Fully precompiled immutable execution entry for one exact image hash.
pub struct ProductionReleaseEntry {
    pub image: Arc<LoadedImage>,
    pub deployment: Arc<DeploymentBinding>,
    pub runtime: Arc<ResolvedRuntime>,
    pub capability_catalog: Arc<CapabilityCatalog>,
    pub context_planner: Arc<ContextPlanner>,
    pub engine_config: EngineConfig,
}

impl fmt::Debug for ProductionReleaseEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProductionReleaseEntry")
            .field("image_hash", &self.image.content_hash)
            .field("agent_id", &self.image.body.metadata.id)
            .field("capability_catalog", &self.capability_catalog)
            .field("context_states", &self.context_planner.state_count())
            .finish_non_exhaustive()
    }
}

/// Static hash router compiled once before claim admission. There is no
/// default image and no mutation API.
pub struct ProductionReleaseCatalog {
    release_set_hash: krw_agent_protocol::ContentHash,
    by_image_hash: BTreeMap<krw_agent_protocol::ContentHash, Arc<ProductionReleaseEntry>>,
}

impl ProductionReleaseCatalog {
    pub fn compile(releases: &ResolvedReleaseSet) -> Result<Arc<Self>, ExecutorBuildError> {
        let mut by_image_hash = BTreeMap::new();
        for release in releases.releases() {
            let capability_catalog =
                CapabilityCatalog::compile(&release.image.manifest, Arc::clone(&release.runtime))
                    .map_err(|_| ExecutorBuildError::InvalidConfiguration)?;
            let engine_config = EngineConfig::production(&release.image.manifest)
                .map_err(|_| ExecutorBuildError::InvalidConfiguration)?;
            let context_planner = Arc::new(
                ContextPlanner::compile(&release.image)
                    .map_err(|_| ExecutorBuildError::InvalidConfiguration)?,
            );
            if release.image.content_hash != *capability_catalog.image_hash()
                || release.image.content_hash != *context_planner.image_hash()
            {
                return Err(ExecutorBuildError::InvalidConfiguration);
            }
            let entry = Arc::new(ProductionReleaseEntry {
                image: Arc::clone(&release.image),
                deployment: Arc::clone(&release.deployment),
                runtime: Arc::clone(&release.runtime),
                capability_catalog,
                context_planner,
                engine_config,
            });
            if by_image_hash
                .insert(release.image.content_hash.clone(), entry)
                .is_some()
            {
                return Err(ExecutorBuildError::InvalidConfiguration);
            }
        }
        if by_image_hash.is_empty() {
            return Err(ExecutorBuildError::InvalidConfiguration);
        }
        Ok(Arc::new(Self {
            release_set_hash: releases.release_set_hash().clone(),
            by_image_hash,
        }))
    }

    pub fn release_set_hash(&self) -> &krw_agent_protocol::ContentHash {
        &self.release_set_hash
    }

    pub fn len(&self) -> usize {
        self.by_image_hash.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_image_hash.is_empty()
    }

    pub fn accepted_image_hashes(&self) -> Vec<krw_agent_protocol::ContentHash> {
        self.by_image_hash.keys().cloned().collect()
    }

    pub fn release(
        &self,
        image_hash: &krw_agent_protocol::ContentHash,
    ) -> Option<&Arc<ProductionReleaseEntry>> {
        self.by_image_hash.get(image_hash)
    }

    /// Route strictly by the durable receipt hash before parsing or validating
    /// its immutable payload, then rederive every pin against that release.
    pub fn route_and_validate_claim(
        &self,
        receipt: &ClaimReceipt,
        runtime_version: &str,
    ) -> Result<(Arc<ProductionReleaseEntry>, ValidatedClaim), RoutedClaimError> {
        let release = self
            .by_image_hash
            .get(&receipt.agent_image_hash)
            .cloned()
            .ok_or(RoutedClaimError::UnknownImageHash)?;
        let validated = validate_claim(receipt, &release.image, &release.runtime, runtime_version)?;
        Ok((release, validated))
    }
}

impl fmt::Debug for ProductionReleaseCatalog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProductionReleaseCatalog")
            .field("release_set_hash", &self.release_set_hash)
            .field("releases", &self.by_image_hash)
            .finish()
    }
}

#[derive(Debug, Error)]
pub enum RoutedClaimError {
    #[error("claim references an image outside the immutable release set")]
    UnknownImageHash,
    #[error("claim does not match its selected immutable release")]
    InvalidClaim(#[from] ClaimValidationError),
}

pub struct ProductionClaimedRunExecutor {
    releases: Arc<ProductionReleaseCatalog>,
    runtime_version: String,
    providers: Arc<DeepSeekProviderCatalog>,
    capability_transport: Arc<dyn McpToolTransport>,
    store: Arc<dyn DurableRunStore>,
    artifacts: ArtifactRepository,
    finalization: FinalizationPolicy,
}

impl ProductionClaimedRunExecutor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        releases: Arc<ProductionReleaseCatalog>,
        runtime_version: String,
        providers: Arc<DeepSeekProviderCatalog>,
        capability_transport: Arc<dyn McpToolTransport>,
        store: Arc<dyn DurableRunStore>,
        artifacts: ArtifactRepository,
        finalization: FinalizationPolicy,
    ) -> Result<Self, ExecutorBuildError> {
        if runtime_version.is_empty() || runtime_version.len() > 64 || releases.is_empty() {
            return Err(ExecutorBuildError::InvalidConfiguration);
        }
        Ok(Self {
            releases,
            runtime_version,
            providers,
            capability_transport,
            store,
            artifacts,
            finalization,
        })
    }
}

/// Fetch a single current market snapshot before the provider sees the task.
/// This is a sealed runtime seed, not a model-selected capability action: it
/// receives only the authenticated singleton ticker, shares the normal MCP
/// capability validation path, and is omitted on every error or timeout.
async fn preflight_market_snapshot(
    catalog: &CapabilityCatalog,
    capabilities: &PooledMcpCapabilityRuntime,
    request: &RunRequest,
    hard_deadline: Instant,
) -> Option<TrustedMarketSnapshot> {
    if request.run_kind != "company_research" {
        return None;
    }
    let [ticker] = request.context.trusted_tickers() else {
        return None;
    };
    // This seed is optional. If the remaining lease/request time is too short
    // to give the provider a fair first turn, skip it rather than competing
    // with research work or extending the run past its hard deadline.
    if hard_deadline.saturating_duration_since(Instant::now()) <= MARKET_SNAPSHOT_PREFLIGHT_TIMEOUT
    {
        return None;
    }
    let invocation = catalog
        .market_snapshot_preflight_invocation(&request.run_id, ticker)
        .ok()??;
    let result = timeout(
        MARKET_SNAPSHOT_PREFLIGHT_TIMEOUT,
        capabilities.invoke(&invocation),
    )
    .await
    .ok()?
    .ok()?;
    TrustedMarketSnapshot::from_provider_content(ticker, &result.provider_content).ok()
}

impl fmt::Debug for ProductionClaimedRunExecutor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProductionClaimedRunExecutor")
            .field("release_set", &self.releases)
            .field("runtime_version", &self.runtime_version)
            .field("providers", &self.providers)
            .field("capability_transport", &"[REDACTED]")
            .field("store", &self.store)
            .field("artifacts", &self.artifacts)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Error)]
pub enum ExecutorBuildError {
    #[error("live executor configuration is invalid")]
    InvalidConfiguration,
}

#[async_trait]
impl ClaimedRunExecutor for ProductionClaimedRunExecutor {
    async fn execute(
        &self,
        context: ClaimedRunContext,
    ) -> Result<SuccessfulRunOutcome, RunExecutionFailure> {
        let (release, validated) = self
            .releases
            .route_and_validate_claim(context.receipt(), &self.runtime_version)
            .map_err(|error| match error {
                RoutedClaimError::UnknownImageHash => {
                    RunExecutionFailure::failed("unknown_agent_image_hash")
                }
                RoutedClaimError::InvalidClaim(error) => {
                    RunExecutionFailure::failed(claim_failure_code(&error))
                }
            })?;
        let provider = self
            .providers
            .exact(&validated.snapshot().resolved_model)
            .ok_or_else(|| RunExecutionFailure::failed("provider_model_not_loaded"))?;
        // Bound concurrent provider episodes for this model to its declared
        // `max_in_flight`. The permit is released when it drops at scope exit,
        // which is always after `execution` resolves or is cancelled.
        let _provider_permit = self
            .providers
            .acquire_permit(&validated.snapshot().resolved_model)
            .await?;
        let persistence = Arc::new(
            DurableRunPersistence::new(
                Arc::clone(&self.store),
                self.artifacts.clone(),
                Arc::new(context.receipt().clone()),
                context.lease.clone(),
                Arc::clone(&context.recovery),
                self.finalization.clone(),
            )
            .map_err(|_| RunExecutionFailure::failed("persistence_bridge_invalid"))?,
        );
        let mut memory = SessionMemoryPageAccumulator::new(
            validated.request().run_id.clone(),
            &validated.request().session_id,
            validated.snapshot().fencing_token,
        )
        .map_err(|_| RunExecutionFailure::failed("session_memory_integrity_failure"))?;
        let mut memory_read_mode = SessionMemoryReadMode::SnapshotTail;
        loop {
            let page = match persistence
                .read_session_memory(
                    memory.next_after_revision(),
                    SESSION_MEMORY_PAGE_LIMIT,
                    memory_read_mode,
                )
                .await
            {
                Ok(page) => page,
                Err(failure)
                    if failure.code == "session_memory_rebuild_required"
                        && memory_read_mode == SessionMemoryReadMode::SnapshotTail
                        && memory.next_after_revision() == 0 =>
                {
                    memory_read_mode = SessionMemoryReadMode::AuditRebuild;
                    memory = SessionMemoryPageAccumulator::new(
                        validated.request().run_id.clone(),
                        &validated.request().session_id,
                        validated.snapshot().fencing_token,
                    )
                    .map_err(|_| RunExecutionFailure::failed("session_memory_integrity_failure"))?;
                    continue;
                }
                Err(failure) => {
                    return Err(execution_failure_from_dependency(
                        "session_memory_read",
                        &failure,
                    ));
                }
            };
            let has_more = page.has_more;
            memory
                .push_page(&page)
                .map_err(session_memory_execution_failure)?;
            if !has_more {
                break;
            }
        }
        let resolved_memory = memory
            .finish(&validated.request().question)
            .map_err(session_memory_execution_failure)?;
        // Hashing the receipt here makes the exact memory resolution auditable
        // without retaining its plaintext view in generic logs. The carrier's
        // hash, source revision and frontier are independently bound into the
        // first run checkpoint by the engine.
        resolved_memory
            .receipt
            .content_hash()
            .map_err(session_memory_execution_failure)?;
        if let Some(snapshot) = resolved_memory.checkpoint_snapshot.as_ref() {
            persistence
                .checkpoint_session_memory_snapshot(snapshot)
                .await
                .map_err(|failure| {
                    execution_failure_from_dependency("session_memory_snapshot", &failure)
                })?;
        }
        let mut effective_request = validated.request().clone();
        effective_request.session_memory = resolved_memory.carrier;
        let capabilities = Arc::new(
            PooledMcpCapabilityRuntime::for_run(
                Arc::clone(&release.capability_catalog),
                Arc::clone(&self.capability_transport),
                RunScope {
                    tenant_id: effective_request.tenant_id.clone(),
                    principal_id: effective_request.principal_id.clone(),
                    run_id: effective_request.run_id.clone(),
                },
            )
            .map_err(|_| RunExecutionFailure::failed("invalid_capability_scope"))?,
        );
        let hard_deadline = Instant::now()
            .checked_add(Duration::from_millis(effective_request.budget.deadline_ms))
            .ok_or_else(|| RunExecutionFailure::failed("deadline_invalid"))?;
        let market_snapshot_context = tokio::select! {
            biased;
            () = context.cancellation.cancelled() => {
                return Ok(SuccessfulRunOutcome::Cancelled);
            }
            snapshot = preflight_market_snapshot(
                release.capability_catalog.as_ref(),
                capabilities.as_ref(),
                &effective_request,
                hard_deadline,
            ) => snapshot,
        };
        let engine = RunEngine::new(
            provider,
            capabilities,
            Arc::clone(&persistence),
            release.engine_config.clone(),
        );
        let execution = engine.run(RunInput {
            image: &release.image,
            deployment: &release.deployment,
            resolved_deployment_binding_hash: release.runtime.deployment_binding_hash(),
            request: &effective_request,
            snapshot: validated.snapshot(),
            market_snapshot_context: market_snapshot_context.as_ref(),
            hard_deadline,
        });
        tokio::pin!(execution);
        let result = tokio::select! {
            biased;
            () = context.cancellation.cancelled() => {
                match timeout(CANCEL_DRAIN_GRACE, &mut execution).await {
                    Ok(result) => result,
                    Err(_) => return Err(RunExecutionFailure::deferred(
                        "execution_cancelled",
                        Duration::from_secs(5),
                    )),
                }
            }
            result = &mut execution => result,
        };
        match result {
            Ok(outcome) if outcome.final_status == FinalStatus::Cancelled => {
                Ok(SuccessfulRunOutcome::Cancelled)
            }
            Ok(_) | Err(EngineError::AlreadyFinalized) => Ok(SuccessfulRunOutcome::Committed),
            Err(EngineError::Cancelled) => Ok(SuccessfulRunOutcome::Cancelled),
            Err(EngineError::FinalCommitAmbiguous(hash)) => match persistence
                .resolve_final_ambiguity(&hash)
                .await
                .map_err(|failure| {
                    execution_failure_from_dependency("final_ambiguity_read", &failure)
                })? {
                Some(FinalStatus::Committed | FinalStatus::AlreadyCommitted) => {
                    Ok(SuccessfulRunOutcome::Committed)
                }
                Some(FinalStatus::Cancelled) => Ok(SuccessfulRunOutcome::Cancelled),
                None => Err(RunExecutionFailure::deferred(
                    "final_outcome_unresolved",
                    Duration::from_secs(2),
                )),
            },
            Err(EngineError::Dependency { component, failure }) => {
                tracing::warn!(component, code = %failure.code, retryable = failure.retryable, "dependency_failure");
                Err(execution_failure_from_dependency(component, &failure))
            }
            Err(EngineError::StaleFence { .. }) => Err(RunExecutionFailure::deferred(
                "stale_fence",
                Duration::from_secs(1),
            )),
            Err(EngineError::DeadlineExceeded(_)) => {
                Err(RunExecutionFailure::failed("deadline_exceeded"))
            }
            Err(error) => {
                // Observability: engine_execution_failure deliberately discards
                // content-bearing detail when writing the terminal outcome, which
                // makes research_planner_failure / context_plan_failure etc.
                // opaque. Log the Debug form here so a local stack can diagnose
                // the exact planner/contract failure without changing the durable
                // ABI reason code.
                tracing::warn!(error = ?error, "run_engine_terminal_failure");
                Err(engine_execution_failure(&error))
            }
        }
    }
}

fn session_memory_execution_failure(_: MemoryResolutionError) -> RunExecutionFailure {
    RunExecutionFailure::failed("session_memory_integrity_failure")
}

fn execution_failure_from_dependency(
    origin: &'static str,
    failure: &krw_agent_run_engine::DependencyFailure,
) -> RunExecutionFailure {
    if failure.retryable {
        RunExecutionFailure::deferred("dependency_unavailable", Duration::from_secs(2))
    } else {
        let mut diagnostic = json!({
            "kind": "krw.agent/dependency-failure-diagnostic-v1",
            "origin": origin,
            "dependency_code_hash": ContentHash::sha256(failure.code.as_bytes()),
            "diagnostic_hash": failure.diagnostic_hash,
            "delivery": match failure.delivery {
                DeliveryCertainty::NotDispatched => "not_dispatched",
                DeliveryCertainty::MayHaveDispatched => "may_have_dispatched",
            },
        });
        if let Some(code) = retained_provider_dependency_code(origin, &failure.code) {
            diagnostic
                .as_object_mut()
                .expect("dependency diagnostic is an object")
                .insert("provider_code".into(), json!(code));
        }
        RunExecutionFailure::failed("dependency_contract_failure").with_release(diagnostic)
    }
}

/// Provider failures already enter the engine through a closed, redacted
/// vocabulary (`glm_*` in the active release, plus the retained compatibility
/// `deepseek_*` vocabulary). Retaining only those bounded categories makes a
/// live compatibility drift diagnosable without retaining the provider body,
/// request, prompt, tool arguments, or account material. Other dependency
/// origins remain hash-only because their codes may be authored by arbitrary
/// adapters.
fn retained_provider_dependency_code<'a>(origin: &str, code: &'a str) -> Option<&'a str> {
    (origin == "provider").then_some(code).filter(|code| {
        (1..=128).contains(&code.len())
            && (code.starts_with("glm_") || code.starts_with("deepseek_"))
            && code
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    })
}

/// Convert an engine failure to the bounded worker ABI while preserving only
/// a release-safe fingerprint when it materially improves root-cause
/// diagnosis.  This is deliberately narrower than normal tracing: raw state
/// artifacts, provider episodes, tool results, prompts, and state identifiers
/// must never cross the durable failure boundary.
///
/// A closed set of model-correctable failure categories (protocol, proposal,
/// workflow state, planner, capability selection) is routed to `deferred`
/// instead of `failed` so the daemon re-queues the run for another attempt.
/// These failures are transient in the sense that a fresh provider turn may
/// produce a valid decision; the repair budget already bounded the in-process
/// recovery, so re-queuing gives the model a clean slate. Failures that are
/// structural (budget exhaustion, immutable scope, recovery integrity) remain
/// terminal.
fn engine_execution_failure(error: &EngineError) -> RunExecutionFailure {
    let reason_code = engine_reason_code(error);
    let is_requeueable = matches!(
        reason_code,
        "provider_protocol_failure"
            | "model_proposal_rejected"
            | "workflow_state_failure"
            | "capability_unknown"
            | "research_planner_failure"
            | "context_plan_failure"
            | "capability_result_invalid"
    );
    let failure = if is_requeueable {
        RunExecutionFailure::deferred(reason_code, REQUEUE_DELAY)
    } else {
        RunExecutionFailure::failed(reason_code)
    };
    match durable_failure_diagnostic(error) {
        Some(diagnostic) => failure.with_release(json!({
            "kind": "krw.agent/failure-diagnostic-v1",
            "failure_kind": diagnostic.kind,
            "identifier_hash": diagnostic.identifier_hash,
        })),
        None => failure,
    }
}

/// Delay before a re-queued run becomes claimable again. Short enough to keep
/// interactive latency reasonable, long enough to let a transient provider
/// hiccup clear.
const REQUEUE_DELAY: Duration = Duration::from_secs(5);

fn engine_reason_code(error: &EngineError) -> &'static str {
    match error {
        // Failure reasons cross the durable worker ABI and may be visible to
        // operations.  Keep them stable, bounded, and free of provider text,
        // prompts, tool payloads, and identifiers.  A single
        // `engine_contract_failure` made an early live-admission failure
        // indistinguishable from a malformed provider response, which in turn
        // encouraged unsafe ad-hoc diagnostics.
        EngineError::InvalidInput(reason) => match *reason {
            "run request exceeds fixed bounds" => "engine_input_bounds_invalid",
            "run_id mismatch" => "engine_input_run_id_mismatch",
            "protocol version mismatch" => "engine_input_protocol_mismatch",
            "agent image hash mismatch" => "engine_input_image_mismatch",
            "model identity mismatch" => "engine_input_model_mismatch",
            "provider execution profile mismatch" => "engine_input_provider_profile_mismatch",
            "budget snapshot mismatch" => "engine_input_budget_mismatch",
            "deployment binding hash mismatch" => "engine_input_binding_mismatch",
            "deadline must be non-zero" => "engine_input_deadline_invalid",
            "engine bounds must be non-zero" => "engine_input_bounds_config_invalid",
            "no matching image entrypoint" => "engine_input_entrypoint_missing",
            "ambiguous image entrypoint" => "engine_input_entrypoint_ambiguous",
            "workflow capability is missing from image" => "engine_input_workflow_mismatch",
            "budget deadline overflow" => "engine_input_deadline_overflow",
            "session memory schema mismatch"
            | "session memory validation failed"
            | "session memory ownership or hash mismatch"
            | "session memory was not UTF-8" => "engine_input_memory_invalid",
            _ => "engine_input_invalid",
        },
        EngineError::InvalidRecoverySnapshot(_)
        | EngineError::RecoveryArtifactMismatch(_)
        | EngineError::RecoveryStateMismatch
        | EngineError::DurableResultHashMismatch
        | EngineError::MissingDurableActionResult => "recovery_integrity_failure",
        EngineError::InvalidProviderEpisode(_)
        | EngineError::InvalidToolCallId
        | EngineError::TooManyToolCalls { .. }
        | EngineError::ToolArgumentsMustBeObject(_) => "provider_protocol_failure",
        EngineError::UnknownCapability(_) => "capability_unknown",
        EngineError::UnsafeCapability(_) => "capability_unsafe",
        EngineError::CapabilityPrerequisiteMissing(_) => "capability_prerequisite_missing",
        EngineError::MissingCapabilityBinding(_) => "capability_binding_missing",
        EngineError::MissingNormalizedOutputContract(_) => "capability_output_contract_missing",
        EngineError::MissingPinnedRelease(_) => "capability_release_pin_missing",
        EngineError::CapabilityReleaseMismatch(_) => "capability_release_pin_mismatch",
        EngineError::ModelProposalRejected(_) => "model_proposal_rejected",
        EngineError::CapabilityInputDerivation(_) => "capability_input_derivation_failed",
        EngineError::RunScopeViolation(_)
        | EngineError::DerivedFeedScopeUnavailable
        | EngineError::GuruAuthorMismatch
        | EngineError::ProductContextMismatch(_)
        | EngineError::ProductContract(_) => "immutable_scope_failure",
        EngineError::CapabilityBudgetExceeded { .. }
        | EngineError::NoRemainingOutputBudget
        | EngineError::FinalOutputReserveReached
        | EngineError::CounterOverflow(_) => "budget_exhausted",
        EngineError::CanonicalContractGuardRequired | EngineError::CanonicalRegistry(_) => {
            "canonical_contract_failure"
        }
        EngineError::SizeLimit { .. } => "engine_size_limit",
        EngineError::InvalidActionReceipt(_)
        | EngineError::ActionRejected(_)
        | EngineError::CalculationConflict(_)
        | EngineError::UncommittedCalculation(_) => "action_contract_failure",
        EngineError::InvalidCapabilityResult(_) => "capability_result_invalid",
        EngineError::AnswerValidation(_)
        | EngineError::RuleViolations(_)
        | EngineError::PhaseRuleViolations { .. } => "answer_verification_failed",
        EngineError::AmbiguousAction(_) => "ambiguous_read_action",
        EngineError::ReservedRuleInputField | EngineError::Policy(_) => "policy_runtime_failure",
        EngineError::InvalidWorkflowTransitionShape => "invalid_transition_shape",
        EngineError::CapabilityStateMappingUnavailable
        | EngineError::InvalidStateProgram
        | EngineError::WorkflowResolution { .. }
        | EngineError::InvalidWorkflowControl
        | EngineError::ResearchPlannerDecisionMismatch
        | EngineError::WorkflowTerminated(_)
        | EngineError::Invariant(_) => "workflow_state_failure",
        EngineError::Contract(error) => protocol_contract_reason_code(error),
        EngineError::Image(error) => agent_image_reason_code(error),
        EngineError::StateArtifact(error) => state_artifact_failure_code(error),
        EngineError::Kernel(_) => "kernel_contract_failure",
        EngineError::Wire(_) => "provider_wire_failure",
        EngineError::Evidence(_) => "evidence_contract_failure",
        EngineError::ResearchPlanner(_) => "research_planner_failure",
        EngineError::ContextPlan(_) | EngineError::ContextCompaction(_) => "context_plan_failure",
        EngineError::BoundedChild(_) => "bounded_child_failure",
        EngineError::Json(_) => "engine_serialization_failure",
        _ => "engine_contract_failure",
    }
}

/// Convert a protocol contract failure into a stable, non-content-bearing
/// worker ABI code.  Details such as a model name or user-supplied context are
/// deliberately discarded here; this is an observability boundary, not a
/// second validation layer.
fn protocol_contract_reason_code(error: &krw_agent_protocol::ContractError) -> &'static str {
    match error {
        krw_agent_protocol::ContractError::InvalidHash(_) => "protocol_hash_invalid",
        krw_agent_protocol::ContractError::BudgetExceeded { .. } => "protocol_budget_exceeded",
        krw_agent_protocol::ContractError::UnknownModelProfile(_) => {
            "protocol_model_profile_unknown"
        }
        krw_agent_protocol::ContractError::UnknownModel(_) => "protocol_model_unknown",
        krw_agent_protocol::ContractError::ModelMismatch { .. } => "protocol_model_mismatch",
        krw_agent_protocol::ContractError::InvalidRunContext(_) => "protocol_run_context_invalid",
        krw_agent_protocol::ContractError::InvalidSessionMemory(_) => {
            "protocol_session_memory_invalid"
        }
    }
}

/// Keep immutable image/load failures distinguishable from runtime data
/// failures without leaking file paths, prompt text, or identifiers.
fn agent_image_reason_code(error: &krw_agent_image::ImageError) -> &'static str {
    match error {
        krw_agent_image::ImageError::Io(_) => "agent_image_io",
        krw_agent_image::ImageError::Yaml(_) => "agent_image_yaml_invalid",
        krw_agent_image::ImageError::Json(_) => "agent_image_json_invalid",
        krw_agent_image::ImageError::InvalidSpec(_) => "agent_image_spec_invalid",
        krw_agent_image::ImageError::InvalidCompiledWorkflow => "agent_image_workflow_invalid",
        krw_agent_image::ImageError::DuplicateId { .. } => "agent_image_duplicate_id",
        krw_agent_image::ImageError::UnknownReference { .. } => "agent_image_reference_unknown",
        krw_agent_image::ImageError::PathEscape(_) => "agent_image_path_escape",
        krw_agent_image::ImageError::OutputExists(_) => "agent_image_output_exists",
        krw_agent_image::ImageError::InvalidPromptUtf8(_) => "agent_image_prompt_utf8_invalid",
        krw_agent_image::ImageError::InvalidPromptText(_) => "agent_image_prompt_text_invalid",
        krw_agent_image::ImageError::MissingBlob(_) => "agent_image_blob_missing",
        krw_agent_image::ImageError::InternedBlobConflict(_) => "agent_image_blob_conflict",
        krw_agent_image::ImageError::BlobLengthMismatch { .. } => {
            "agent_image_blob_length_mismatch"
        }
        krw_agent_image::ImageError::BlobHashMismatch { .. } => "agent_image_blob_hash_mismatch",
        krw_agent_image::ImageError::Limit(_) => "agent_image_limit_exceeded",
        krw_agent_image::ImageError::UnsupportedImageFormat(_) => "agent_image_format_unsupported",
        krw_agent_image::ImageError::UnsupportedRuleIsa(_) => "agent_image_rule_isa_unsupported",
        krw_agent_image::ImageError::ImageHashMismatch { .. } => "agent_image_hash_mismatch",
        krw_agent_image::ImageError::RuleFuelExhausted => "agent_image_rule_fuel_exhausted",
        krw_agent_image::ImageError::RuleInput(_) => "agent_image_rule_input_invalid",
        krw_agent_image::ImageError::ContractRegistry(_) => "agent_image_registry_invalid",
        krw_agent_image::ImageError::StateArtifact(_) => "agent_image_state_artifact_invalid",
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use krw_agent_image::{PromptBlobInterner, compile_agent_dir};
    use krw_agent_persistence::agent_v1::RecoveryReceipt;
    use krw_agent_protocol::{
        ContentHash, DEEPSEEK_MODEL_ID, GLM_MODEL_ID, ModelRegistry, RunRequest,
    };
    use krw_agent_runtime_config::{
        BudgetRegistry, ConfigError, EndpointRegistry, SecretSource, ValidationMode, load_yaml,
        resolve_release_set,
    };
    use zeroize::Zeroizing;

    use crate::{ImmutableRunClaimV1, RunResourceProfileV1};

    use super::*;

    #[test]
    fn durable_failure_codes_preserve_contract_owner_without_content() {
        let protocol = EngineError::Contract(krw_agent_protocol::ContractError::InvalidRunContext(
            "private run context",
        ));
        let image = EngineError::Image(krw_agent_image::ImageError::InvalidSpec(
            "private image detail".into(),
        ));
        assert_eq!(
            engine_reason_code(&protocol),
            "protocol_run_context_invalid"
        );
        assert_eq!(engine_reason_code(&image), "agent_image_spec_invalid");
        for code in [engine_reason_code(&protocol), engine_reason_code(&image)] {
            assert!(!code.contains("private"));
        }
    }

    #[test]
    fn ordinary_engine_failure_does_not_create_a_diagnostic_payload() {
        let failure = engine_execution_failure(&EngineError::InvalidWorkflowControl);

        assert_eq!(failure.reason_code, "workflow_state_failure");
        assert_eq!(failure.release, json!({}));
    }

    #[test]
    fn dependency_failure_retains_hashes_but_not_dependency_content() {
        let dependency = krw_agent_run_engine::DependencyFailure::redacted(
            "private-dependency-code",
            "private dependency detail",
            false,
            DeliveryCertainty::NotDispatched,
        );

        let failure = execution_failure_from_dependency("checkpoint_state", &dependency);

        assert_eq!(failure.reason_code, "dependency_contract_failure");
        assert_eq!(
            failure.release,
            json!({
                "kind": "krw.agent/dependency-failure-diagnostic-v1",
                "origin": "checkpoint_state",
                "dependency_code_hash": ContentHash::sha256("private-dependency-code"),
                "diagnostic_hash": ContentHash::sha256("private dependency detail"),
                "delivery": "not_dispatched",
            })
        );
        let serialized = serde_json::to_string(&failure.release).unwrap();
        assert!(!serialized.contains("private-dependency-code"));
        assert!(!serialized.contains("private dependency detail"));
    }

    #[test]
    fn provider_dependency_failure_retains_only_the_closed_provider_code() {
        let dependency = krw_agent_run_engine::DependencyFailure::redacted(
            "deepseek_http_400_invalid_param",
            "provider body that must remain private",
            false,
            DeliveryCertainty::MayHaveDispatched,
        );

        let failure = execution_failure_from_dependency("provider", &dependency);
        assert_eq!(
            failure.release["provider_code"],
            json!("deepseek_http_400_invalid_param")
        );
        let serialized = serde_json::to_string(&failure.release).unwrap();
        assert!(!serialized.contains("provider body"));
    }

    #[test]
    fn provider_dependency_failure_retains_the_active_glm_code() {
        let dependency = krw_agent_run_engine::DependencyFailure::redacted(
            "glm_http_400_invalid_param",
            "provider body that must remain private",
            false,
            DeliveryCertainty::MayHaveDispatched,
        );

        let failure = execution_failure_from_dependency("provider", &dependency);
        assert_eq!(
            failure.release["provider_code"],
            json!("glm_http_400_invalid_param")
        );
        let serialized = serde_json::to_string(&failure.release).unwrap();
        assert!(!serialized.contains("provider body"));
    }

    #[test]
    fn provider_dependency_failure_rejects_unbounded_or_foreign_code() {
        assert!(retained_provider_dependency_code("provider", "private-detail").is_none());
        assert!(retained_provider_dependency_code("mcp", "deepseek_http_400").is_none());
    }

    #[derive(Debug)]
    struct FixtureSecrets {
        ontology_mcp_url: &'static str,
    }

    impl FixtureSecrets {
        const fn baseline() -> Self {
            Self {
                ontology_mcp_url: "https://ontology.invalid/mcp",
            }
        }
    }

    impl SecretSource for FixtureSecrets {
        fn read_secret(&self, name: &str) -> Result<Zeroizing<String>, ConfigError> {
            match name {
                "DEEPSEEK_API_KEY" => Ok(Zeroizing::new("fixture-key".into())),
                "GLM_API_KEY" => Ok(Zeroizing::new("fixture-glm-key".into())),
                "KRW_ONTOLOGY_MCP_URL" => Ok(Zeroizing::new(self.ontology_mcp_url.into())),
                "KRW_ONTOLOGY_READY_URL" => {
                    Ok(Zeroizing::new("https://ontology.invalid/readyz".into()))
                }
                "KRW_GURU_MCP_URL" => Ok(Zeroizing::new("https://guru.invalid/mcp".into())),
                "KRW_GURU_MCP_READY_URL" => {
                    Ok(Zeroizing::new("https://guru.invalid/readyz".into()))
                }
                "KRW_GURU_MCP_TOKEN" => Ok(Zeroizing::new("guru-token".into())),
                "KRW_FEED_MCP_URL" => Ok(Zeroizing::new("https://feed.invalid/mcp".into())),
                "KRW_FEED_MCP_READY_URL" => {
                    Ok(Zeroizing::new("https://feed.invalid/readyz".into()))
                }
                "KRW_FEED_MCP_TOKEN" => Ok(Zeroizing::new("feed-token".into())),
                "KRW_FILINGS_MCP_URL" => Ok(Zeroizing::new("https://filings.invalid/mcp".into())),
                "KRW_FILINGS_MCP_READY_URL" => {
                    Ok(Zeroizing::new("https://filings.invalid/readyz".into()))
                }
                "KRW_FILINGS_MCP_TOKEN" => Ok(Zeroizing::new("filings-token".into())),
                _ => Err(ConfigError::MissingSecret(name.into())),
            }
        }
    }

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn fixture() -> (
        Arc<ProductionReleaseCatalog>,
        Arc<DeepSeekProviderCatalog>,
        ContentHash,
        ContentHash,
        RunRequest,
        RunRequest,
    ) {
        fixture_with_ontology_endpoint("https://ontology.invalid/mcp", "2025-06-18")
    }

    fn fixture_with_ontology_endpoint(
        ontology_mcp_url: &'static str,
        protocol_version: &'static str,
    ) -> (
        Arc<ProductionReleaseCatalog>,
        Arc<DeepSeekProviderCatalog>,
        ContentHash,
        ContentHash,
        RunRequest,
        RunRequest,
    ) {
        let root = root();
        let mut interner = PromptBlobInterner::default();
        let ko = compile_agent_dir(root.join("agents/krw-ontology"))
            .unwrap()
            .into_loaded_with_interner(&mut interner)
            .unwrap();
        let en = compile_agent_dir(root.join("agents/krw-ontology-en"))
            .unwrap()
            .into_loaded_with_interner(&mut interner)
            .unwrap();
        let binding: DeploymentBinding =
            load_yaml(root.join("deployments/local/deployment-binding.example.yaml")).unwrap();
        let models: ModelRegistry =
            load_yaml(root.join("deployments/local/model-registry.yaml")).unwrap();
        let budgets: BudgetRegistry =
            load_yaml(root.join("deployments/local/budget-registry.yaml")).unwrap();
        let mut endpoints: EndpointRegistry =
            load_yaml(root.join("deployments/local/endpoint-registry.example.yaml")).unwrap();
        endpoints
            .endpoints
            .iter_mut()
            .find(|endpoint| endpoint.endpoint_ref == "krw-ontology-local")
            .unwrap()
            .protocol_version = protocol_version.into();
        let secrets = FixtureSecrets { ontology_mcp_url };
        let releases = resolve_release_set(
            vec![ko, en],
            &binding,
            &models,
            &budgets,
            &endpoints,
            &secrets,
            ValidationMode::Fixture,
        )
        .unwrap();
        let ko_hash = releases.owner("company_research", "ko-KR").unwrap().clone();
        let en_hash = releases
            .owner("company_research_en", "en-US")
            .unwrap()
            .clone();
        let providers = DeepSeekProviderCatalog::compile_release_set(&releases).unwrap();
        let catalog = ProductionReleaseCatalog::compile(&releases).unwrap();
        let ko_request: RunRequest = serde_json::from_slice(
            &fs::read(root.join("fixtures/vertical-slice/v1/run-request.json")).unwrap(),
        )
        .unwrap();
        let mut en_request = ko_request.clone();
        en_request.run_id = "run-release-en".into();
        en_request.run_kind = "company_research_en".into();
        en_request.locale = "en-US".into();
        (catalog, providers, ko_hash, en_hash, ko_request, en_request)
    }

    fn receipt(
        catalog: &ProductionReleaseCatalog,
        image_hash: &ContentHash,
        request: &RunRequest,
    ) -> ClaimReceipt {
        let release = catalog.release(image_hash).unwrap();
        let snapshot = release
            .runtime
            .resolve_run(image_hash, request, 7, 0)
            .unwrap();
        let payload =
            ImmutableRunClaimV1::new(request.clone(), &snapshot, RunResourceProfileV1::default())
                .unwrap();
        ClaimReceipt {
            run_id: request.run_id.clone(),
            tenant_id: request.tenant_id.clone(),
            principal_id: request.principal_id.clone(),
            session_id: request.session_id.clone(),
            fencing_token: 7,
            run_version: 2,
            cancel_generation: 0,
            checkpoint_seq: 0,
            action_frontier_seq: 0,
            action_frontier_hash: ContentHash::sha256(b"[]"),
            lease_deadline: "fixture".into(),
            agent_image_hash: image_hash.clone(),
            runtime_version: "runtime-test".into(),
            priority: 0,
            immutable_snapshot_hash: payload.canonical_hash().unwrap(),
            immutable_snapshot: serde_json::to_value(&payload).unwrap(),
            resource_profile: serde_json::to_value(RunResourceProfileV1::default()).unwrap(),
            budgets: serde_json::to_value(&request.budget).unwrap(),
            reclaimed: false,
            recovery: RecoveryReceipt {
                state_checkpoint: None,
                episodes: Vec::new(),
                actions: Vec::new(),
            },
        }
    }

    #[test]
    fn two_image_claims_route_to_exact_precompiled_releases() {
        let (catalog, providers, ko_hash, en_hash, ko_request, en_request) = fixture();
        let ko_receipt = receipt(&catalog, &ko_hash, &ko_request);
        let en_receipt = receipt(&catalog, &en_hash, &en_request);

        let (ko, ko_claim) = catalog
            .route_and_validate_claim(&ko_receipt, "runtime-test")
            .unwrap();
        let (en, en_claim) = catalog
            .route_and_validate_claim(&en_receipt, "runtime-test")
            .unwrap();
        assert_eq!(ko.image.content_hash, ko_hash);
        assert_eq!(en.image.content_hash, en_hash);
        assert_eq!(ko_claim.request().run_kind, "company_research");
        assert_eq!(en_claim.request().run_kind, "company_research_en");
        assert_eq!(catalog.len(), 2);
        assert_eq!(providers.by_model.len(), 1);
        assert!(providers.exact(GLM_MODEL_ID).is_some());
    }

    #[test]
    fn unknown_hash_and_cross_image_replay_fail_before_execution() {
        let (catalog, _providers, ko_hash, en_hash, ko_request, _en_request) = fixture();
        let original = receipt(&catalog, &ko_hash, &ko_request);

        let mut unknown = original.clone();
        unknown.agent_image_hash = ContentHash::sha256("unknown-image");
        unknown.immutable_snapshot = serde_json::json!({"malformed": true});
        unknown.immutable_snapshot_hash = ContentHash::sha256("also-wrong");
        assert!(matches!(
            catalog.route_and_validate_claim(&unknown, "runtime-test"),
            Err(RoutedClaimError::UnknownImageHash)
        ));

        let mut replay = original;
        replay.agent_image_hash = en_hash;
        assert!(matches!(
            catalog.route_and_validate_claim(&replay, "runtime-test"),
            Err(RoutedClaimError::InvalidClaim(
                ClaimValidationError::DeploymentMismatch
            ))
        ));
    }

    #[test]
    fn queued_claim_from_before_endpoint_registry_drift_is_rejected_after_restart() {
        let (old_catalog, _providers, ko_hash, _en_hash, ko_request, _en_request) = fixture();
        let queued_claim = receipt(&old_catalog, &ko_hash, &ko_request);
        let (restarted_catalog, _providers, restarted_ko_hash, _, _, _) =
            fixture_with_ontology_endpoint("https://ontology.invalid/mcp", "2026-08-02");

        assert_eq!(ko_hash, restarted_ko_hash);
        assert_ne!(
            old_catalog.release_set_hash(),
            restarted_catalog.release_set_hash()
        );
        assert!(matches!(
            restarted_catalog.route_and_validate_claim(&queued_claim, "runtime-test"),
            Err(RoutedClaimError::InvalidClaim(
                ClaimValidationError::SnapshotMismatch
                    | ClaimValidationError::SnapshotContractMismatch(_)
            ))
        ));
    }

    #[test]
    fn routing_does_not_accumulate_session_entries() {
        let (catalog, _providers, ko_hash, _en_hash, ko_request, _en_request) = fixture();
        let receipt = receipt(&catalog, &ko_hash, &ko_request);
        let baseline = catalog.len();
        for _ in 0..128 {
            let routed = catalog
                .route_and_validate_claim(&receipt, "runtime-test")
                .unwrap();
            drop(routed);
        }
        assert_eq!(catalog.len(), baseline);
    }

    #[test]
    fn all_eight_checked_in_images_precompile_into_one_production_catalog() {
        let root = root();
        let mut interner = PromptBlobInterner::default();
        let images = [
            "krw-display",
            "krw-feed",
            "krw-guru-advisor",
            "krw-notebook",
            "krw-ontology-en",
            "krw-ontology",
            "krw-router",
            "krw-source-filing",
        ]
        .into_iter()
        .map(|package| {
            compile_agent_dir(root.join("agents").join(package))
                .unwrap()
                .into_loaded_with_interner(&mut interner)
                .unwrap()
        })
        .collect();
        let binding =
            load_yaml(root.join("deployments/local/deployment-binding.example.yaml")).unwrap();
        let models = load_yaml(root.join("deployments/local/model-registry.yaml")).unwrap();
        let budgets = load_yaml(root.join("deployments/local/budget-registry.yaml")).unwrap();
        let endpoints =
            load_yaml(root.join("deployments/local/endpoint-registry.example.yaml")).unwrap();
        let releases = resolve_release_set(
            images,
            &binding,
            &models,
            &budgets,
            &endpoints,
            &FixtureSecrets::baseline(),
            ValidationMode::Fixture,
        )
        .unwrap();

        let providers = DeepSeekProviderCatalog::compile_release_set(&releases).unwrap();
        for release in releases.releases() {
            CapabilityCatalog::compile(&release.image.manifest, Arc::clone(&release.runtime))
                .unwrap_or_else(|error| {
                    panic!(
                        "capability catalog failed for {}: {error:?}",
                        release.image.body.metadata.id
                    )
                });
            EngineConfig::production(&release.image.manifest).unwrap_or_else(|error| {
                panic!(
                    "engine config failed for {}: {error:?}",
                    release.image.body.metadata.id
                )
            });
        }
        let catalog = ProductionReleaseCatalog::compile(&releases).unwrap();
        assert_eq!(catalog.len(), 8);
        assert_eq!(catalog.accepted_image_hashes().len(), 8);
        assert!(
            catalog
                .by_image_hash
                .values()
                .all(|release| release.context_planner.state_count() > 0)
        );
        assert_eq!(providers.by_model.len(), 1);
    }

    #[test]
    fn provider_catalog_rejects_any_unpinned_inventory() {
        let root = root();
        let models: ModelRegistry =
            load_yaml(root.join("deployments/local/model-registry.yaml")).unwrap();
        let mut forbidden = models.models[0].clone();
        forbidden.model_id = "forbidden-provider-model".into();
        let descriptors = [models.models[0].clone(), forbidden];
        let api_keys_by_base =
            build_api_keys_by_base(descriptors.iter(), "fixture-key", "fixture-key");
        assert!(matches!(
            DeepSeekProviderCatalog::compile_from(descriptors.iter(), &api_keys_by_base, 8),
            Err(ProviderCatalogError::ModelInventory)
        ));
    }

    #[test]
    fn provider_catalog_accepts_multiple_allowed_models() {
        let root = root();
        let models: ModelRegistry =
            load_yaml(root.join("deployments/local/model-registry.yaml")).unwrap();
        // Build a second allowed descriptor on a distinct api_base so the
        // catalog has to materialise two independent ProviderClient instances
        // and two independent permit semaphores.
        let mut second = models.models[0].clone();
        second.model_id = DEEPSEEK_MODEL_ID.to_string();
        second.api_base = "https://deepseek-provider.invalid/anthropic".to_string();
        let descriptors = [models.models[0].clone(), second];
        let api_keys_by_base = build_api_keys_by_base(
            descriptors.iter(),
            "deepseek-fixture-key",
            "glm-fixture-key",
        );
        let catalog =
            DeepSeekProviderCatalog::compile_from(descriptors.iter(), &api_keys_by_base, 8)
                .expect("multi-model catalog compiles when every model is allowed");
        assert_eq!(catalog.by_model.len(), 2);
        assert!(catalog.exact(DEEPSEEK_MODEL_ID).is_some());
        assert!(catalog.exact(GLM_MODEL_ID).is_some());
        assert_eq!(catalog.permits_by_model.len(), 2);
        // Two distinct api bases must yield two distinct client arcs.
        let deepseek_client = catalog.exact(DEEPSEEK_MODEL_ID).unwrap();
        let glm_client = catalog.exact(GLM_MODEL_ID).unwrap();
        assert!(
            !Arc::ptr_eq(&deepseek_client, &glm_client),
            "models on different api bases must not share a client"
        );
    }
}
