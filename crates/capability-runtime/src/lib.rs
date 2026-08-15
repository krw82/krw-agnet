//! Run-scoped production adapters for pooled MCP capabilities.
//!
//! Agent meaning is compiled once into [`CapabilityCatalog`]. A live run owns
//! only two `Arc`s and a compact [`RunScope`]; endpoints, credentials, schema
//! pins, and MCP clients remain machine-wide. The adapter deliberately
//! supports a closed set of read-only KRW ontology mappings.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use krw_agent_contracts::{
    MARKET_SNAPSHOT_REQUEST_V1, NORMALIZED_CAPABILITY_RESULT_V1, ONTOLOGY_COMPANY_CONTEXT_V1,
    ONTOLOGY_TARGETED_QUERY_V1, ONTOLOGY_TRACE_INPUT_V1, QUERY_CONTEXT_INPUT_CORRECTION_V1,
    RESEARCH_STATE_V2, SEARCH_PLAN_V2, SKILL_CONTENT_V1, SKILL_LOAD_V1, validate_value, verify_pin,
};
use krw_agent_evidence::EvidenceScope;
use krw_agent_image::{
    AgentImageManifest, CapabilityResultIngest, CapabilitySpec, IdempotencyPolicy, InputDerivation,
    Permission, ResolvedCapabilityContracts,
};
use krw_agent_protocol::{ContentHash, TransportKind, is_canonical_ticker};
use krw_agent_run_engine::{
    CapabilityInvocation, CapabilityResult, CapabilityRuntime, DeliveryCertainty,
    DependencyFailure, deterministic_action_key,
};
use krw_agent_runtime_config::{ResolvedCapability, ResolvedRuntime};
use krw_agent_tool_mcp::{
    McpClientPool, McpError, McpHttpConfig, PoolKey, PoolScope, ToolCallOutcome,
};
use krw_ontology_adapter::{
    MappingContext, map_company_context, map_market_snapshot, map_research_state,
    map_targeted_query, map_trace, parse_research_state, sanitize_research_state_scope,
};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::{Mutex as AsyncMutex, Semaphore};
use zeroize::{Zeroize, Zeroizing};

mod front_mapping;
use front_mapping::{FrontMapping, FrontRunState};
mod guru_mapping;
use guru_mapping::{
    GuruMapping, GuruRunState, is_correction as is_guru_correction, validate_correction_for_mapping,
};

const MAX_MCP_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
const MAX_SCOPE_ID_BYTES: usize = 128;
const CLIENT_NAME: &str = "krw-agent-kernel";
const MAX_CPU_WORK_PARALLELISM: usize = 4;

#[derive(Debug)]
struct CpuWorkLimiter {
    permits: Arc<Semaphore>,
    max_parallelism: usize,
}

impl CpuWorkLimiter {
    fn new(max_parallelism: NonZeroUsize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(max_parallelism.get())),
            max_parallelism: max_parallelism.get(),
        }
    }

    async fn execute<T, F>(
        &self,
        delivery: DeliveryCertainty,
        work: F,
    ) -> Result<T, DependencyFailure>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, DependencyFailure> + Send + 'static,
    {
        // Acquire before spawn_blocking, so this subsystem can create at most
        // `max_parallelism` blocking jobs/threads even when many runs wait.
        let permit = Arc::clone(&self.permits)
            .acquire_owned()
            .await
            .map_err(|_| {
                reject(
                    "cpu_work_limiter_closed",
                    "bounded CPU work admission is unavailable",
                )
            })?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work()
        })
        .await
        .map_err(|error| {
            DependencyFailure::redacted("cpu_work_join", format!("{error:?}"), false, delivery)
        })?
    }
}

fn machine_cpu_work_limiter() -> Arc<CpuWorkLimiter> {
    static LIMITER: OnceLock<Arc<CpuWorkLimiter>> = OnceLock::new();
    Arc::clone(LIMITER.get_or_init(|| {
        // Keep one logical processor available when possible, while bounding
        // high-core machines so memory-heavy 8 MiB mappings cannot fan out.
        let logical = std::thread::available_parallelism().map_or(1, NonZeroUsize::get);
        let parallelism = logical.saturating_sub(1).clamp(1, MAX_CPU_WORK_PARALLELISM);
        Arc::new(CpuWorkLimiter::new(
            NonZeroUsize::new(parallelism).expect("parallelism is clamped to at least one"),
        ))
    }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvidenceMapping {
    ResearchStateV2,
    CompanyContextV1,
    MarketSnapshotV1,
    TargetedEvidenceV1,
    TraceLineageV1,
    Front(FrontMapping),
    Guru(GuruMapping),
    /// Local skill body passthrough. Produces no evidence ledger entries; the
    /// raw Markdown is forwarded as the provider-visible content.
    SkillContent,
}

impl EvidenceMapping {
    const fn from_result_ingest(value: CapabilityResultIngest) -> Self {
        match value {
            CapabilityResultIngest::ResearchStateV2 => Self::ResearchStateV2,
            CapabilityResultIngest::CompanyContextV1 => Self::CompanyContextV1,
            CapabilityResultIngest::MarketSnapshotV1 => Self::MarketSnapshotV1,
            CapabilityResultIngest::TargetedEvidenceV1 => Self::TargetedEvidenceV1,
            CapabilityResultIngest::TraceLineageV1 => Self::TraceLineageV1,
            CapabilityResultIngest::FrontFeedListItemsV1 => {
                Self::Front(FrontMapping::FeedListItems)
            }
            CapabilityResultIngest::FrontFeedGetItemsV1 => Self::Front(FrontMapping::FeedGetItems),
            CapabilityResultIngest::FrontFeedContextV2 => Self::Front(FrontMapping::FeedContext),
            CapabilityResultIngest::FrontFilingSearchV1 => Self::Front(FrontMapping::FilingSearch),
            CapabilityResultIngest::FrontFilingMetadataV1 => {
                Self::Front(FrontMapping::FilingMetadata)
            }
            CapabilityResultIngest::FrontFilingBriefV1 => Self::Front(FrontMapping::FilingBrief),
            CapabilityResultIngest::FrontFilingSectionsV1 => {
                Self::Front(FrontMapping::FilingSections)
            }
            CapabilityResultIngest::FrontFilingSectionTextV1 => {
                Self::Front(FrontMapping::FilingSectionText)
            }
            CapabilityResultIngest::FrontFilingDocumentsV1 => {
                Self::Front(FrontMapping::FilingDocuments)
            }
            CapabilityResultIngest::FrontFilingDocumentTextV1 => {
                Self::Front(FrontMapping::FilingDocumentText)
            }
            CapabilityResultIngest::FrontForm4TransactionsV1 => {
                Self::Front(FrontMapping::Form4Transactions)
            }
            CapabilityResultIngest::GuruQueryContextV1 => Self::Guru(GuruMapping::QueryContext),
            CapabilityResultIngest::GuruCompanyBriefV1 => Self::Guru(GuruMapping::CompanyBrief),
            CapabilityResultIngest::GuruEvidenceReviewV1 => Self::Guru(GuruMapping::EvidenceReview),
            CapabilityResultIngest::SkillContentV1 => Self::SkillContent,
        }
    }

    const fn requires_run_order(self) -> bool {
        matches!(self, Self::Front(_) | Self::Guru(_))
    }

    const fn input_contract(self) -> &'static str {
        match self {
            Self::ResearchStateV2 => SEARCH_PLAN_V2,
            Self::CompanyContextV1 => ONTOLOGY_COMPANY_CONTEXT_V1,
            Self::MarketSnapshotV1 => MARKET_SNAPSHOT_REQUEST_V1,
            Self::TargetedEvidenceV1 => ONTOLOGY_TARGETED_QUERY_V1,
            Self::TraceLineageV1 => ONTOLOGY_TRACE_INPUT_V1,
            Self::SkillContent => SKILL_LOAD_V1,
            Self::Front(mapping) => mapping.input_contract(),
            Self::Guru(mapping) => mapping.input_contract(),
        }
    }

    const fn output_contracts(self) -> &'static [&'static str] {
        match self {
            Self::ResearchStateV2 => &[
                RESEARCH_STATE_V2,
                QUERY_CONTEXT_INPUT_CORRECTION_V1,
                NORMALIZED_CAPABILITY_RESULT_V1,
            ],
            Self::CompanyContextV1
            | Self::MarketSnapshotV1
            | Self::TargetedEvidenceV1
            | Self::TraceLineageV1 => &[NORMALIZED_CAPABILITY_RESULT_V1],
            Self::SkillContent => &[SKILL_CONTENT_V1, NORMALIZED_CAPABILITY_RESULT_V1],
            Self::Front(mapping) => mapping.output_contracts(),
            Self::Guru(mapping) => mapping.output_contracts(),
        }
    }
}

#[derive(Clone)]
struct CapabilityDescriptor {
    specification: CapabilitySpec,
    contracts: ResolvedCapabilityContracts,
    normalized_output_contract_hash: ContentHash,
    mapping: EvidenceMapping,
}

/// Image-declared, sealed evidence boundary for the Guru workflow. Capability
/// IDs remain data owned by the image; the runtime only understands the closed
/// result-mapping classes required to preserve the sealed workflow.
#[derive(Debug, Clone)]
struct GuruRuntimePolicy {
    evidence_capability_ids: BTreeSet<String>,
}

impl GuruRuntimePolicy {
    fn contains_evidence_capability(&self, capability_id: &str) -> bool {
        self.evidence_capability_ids.contains(capability_id)
    }
}

impl CapabilityDescriptor {
    fn is_guru_evidence_capability(&self, policy: Option<&GuruRuntimePolicy>) -> bool {
        policy.is_some_and(|policy| policy.contains_evidence_capability(&self.specification.id))
    }

    fn requires_run_order(&self, policy: Option<&GuruRuntimePolicy>) -> bool {
        self.mapping.requires_run_order() || self.is_guru_evidence_capability(policy)
    }
}

impl fmt::Debug for CapabilityDescriptor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CapabilityDescriptor")
            .field("capability_id", &self.specification.id)
            .field("binding_key", &self.specification.binding_key)
            .field("input_schema_hash", &self.contracts.input.content_hash)
            .field(
                "output_schema_hash",
                &self.contracts.output_contract_set_hash,
            )
            .field(
                "normalized_output_contract_hash",
                &self.normalized_output_contract_hash,
            )
            .field("mapping", &self.mapping)
            .finish()
    }
}

/// Startup-validated immutable link between an `AgentImage` and deployment
/// capabilities. It owns no per-run state and can be shared by every session.
pub struct CapabilityCatalog {
    image_hash: ContentHash,
    descriptors: BTreeMap<String, CapabilityDescriptor>,
    guru_policy: Option<Arc<GuruRuntimePolicy>>,
    runtime: Arc<ResolvedRuntime>,
}

impl fmt::Debug for CapabilityCatalog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CapabilityCatalog")
            .field("image_hash", &self.image_hash)
            .field("descriptors", &self.descriptors)
            .field(
                "guru_evidence_capability_count",
                &self
                    .guru_policy
                    .as_ref()
                    .map_or(0, |policy| policy.evidence_capability_ids.len()),
            )
            .field("runtime", &"[REDACTED_DEPLOYMENT]")
            .finish()
    }
}

/// Derive only the already-authorized company set embedded in the compiled
/// SearchPlan.  This is intentionally separate from natural-language ticker
/// extraction: a plan with no ticker scope is a wide discovery request and
/// must retain its discovered companies.
fn expected_research_tickers(arguments: &Value) -> Vec<String> {
    let mut tickers = BTreeSet::new();
    let mut collect = |values: Option<&Vec<Value>>| {
        for ticker in values.into_iter().flatten().filter_map(Value::as_str) {
            if is_canonical_ticker(ticker) {
                tickers.insert(ticker.to_owned());
            }
        }
    };
    collect(arguments.get("tickers").and_then(Value::as_array));
    for clause in arguments
        .get("clauses")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        collect(clause.get("tickers").and_then(Value::as_array));
    }
    tickers.into_iter().take(50).collect()
}

impl CapabilityCatalog {
    pub fn compile(
        image: &AgentImageManifest,
        runtime: Arc<ResolvedRuntime>,
    ) -> Result<Arc<Self>, CatalogError> {
        let observed_image_hash = ContentHash::sha256(serde_jcs::to_vec(&image.body)?);
        if observed_image_hash != image.content_hash {
            return Err(CatalogError::ImageHashMismatch);
        }

        let mut descriptors = BTreeMap::new();
        for capability in &image.body.capabilities {
            let mapping = EvidenceMapping::from_result_ingest(capability.result_ingest);
            validate_capability_semantics(capability, mapping)?;
            let contracts = image.resolve_capability_contracts(capability)?;
            verify_contracts(&contracts, mapping)?;
            let normalized_output_contract_hash = contracts
                .outputs
                .iter()
                .find(|contract| contract.id == NORMALIZED_CAPABILITY_RESULT_V1)
                .ok_or_else(|| CatalogError::MissingNormalizedOutput(capability.id.clone()))?
                .content_hash
                .clone();
            let resolved = runtime
                .capabilities
                .get(&capability.id)
                .ok_or_else(|| CatalogError::MissingResolvedBinding(capability.id.clone()))?;
            validate_resolved_binding(capability, resolved)?;
            let descriptor = CapabilityDescriptor {
                specification: capability.clone(),
                contracts,
                normalized_output_contract_hash,
                mapping,
            };
            if descriptors
                .insert(capability.id.clone(), descriptor)
                .is_some()
            {
                return Err(CatalogError::DuplicateCapability(capability.id.clone()));
            }
        }
        let runtime_ids = runtime.capabilities.keys().collect::<BTreeSet<_>>();
        let image_ids = descriptors.keys().collect::<BTreeSet<_>>();
        if runtime_ids != image_ids {
            return Err(CatalogError::ResolvedCapabilitySetMismatch);
        }
        let guru_policy = compile_guru_runtime_policy(image, &descriptors)?;
        Ok(Arc::new(Self {
            image_hash: image.content_hash.clone(),
            descriptors,
            guru_policy,
            runtime,
        }))
    }

    pub fn image_hash(&self) -> &ContentHash {
        &self.image_hash
    }

    pub fn capability_count(&self) -> usize {
        self.descriptors.len()
    }

    /// Construct the one sealed, best-effort market preflight invocation. This
    /// is intentionally discovered by its closed result mapping, rather than
    /// by a configurable capability name or arbitrary plugin instruction.
    /// The normal workflow may still call the same capability later when a
    /// fresh snapshot can materially improve the answer.
    pub fn market_snapshot_preflight_invocation(
        &self,
        run_id: &str,
        ticker: &str,
    ) -> Result<Option<CapabilityInvocation>, CatalogError> {
        let mut candidates = self.descriptors.iter().filter(|(_, descriptor)| {
            matches!(descriptor.mapping, EvidenceMapping::MarketSnapshotV1)
        });
        let Some((capability_id, descriptor)) = candidates.next() else {
            return Ok(None);
        };
        if candidates.next().is_some() {
            return Err(CatalogError::AmbiguousMarketSnapshotPreflight);
        }
        let arguments = serde_json::json!({"ticker": ticker});
        validate_value(MARKET_SNAPSHOT_REQUEST_V1, &arguments)
            .map_err(|_| CatalogError::InvalidMarketSnapshotPreflight)?;
        let resolved = self
            .runtime
            .capabilities
            .get(capability_id.as_str())
            .ok_or_else(|| CatalogError::MissingResolvedBinding(capability_id.clone()))?;
        let request_hash = ContentHash::sha256(
            serde_jcs::to_vec(&arguments)
                .map_err(|_| CatalogError::InvalidMarketSnapshotPreflight)?,
        );
        let action_key = deterministic_action_key(
            run_id,
            &self.image_hash,
            &descriptor.specification,
            &descriptor.contracts,
            &resolved.binding,
            &arguments,
        )
        .map_err(|_| CatalogError::InvalidMarketSnapshotPreflight)?;
        Ok(Some(CapabilityInvocation {
            run_id: run_id.to_owned(),
            action_key,
            capability_id: capability_id.clone(),
            request_hash,
            input_schema_hash: descriptor.contracts.input.content_hash.clone(),
            output_schema_hash: descriptor.contracts.output_contract_set_hash.clone(),
            normalized_output_contract_hash: descriptor.normalized_output_contract_hash.clone(),
            arguments,
            binding: resolved.binding.clone(),
        }))
    }
}

/// Compile the sealed Guru evidence boundary from `AgentImage` declarations.
/// The review derivation is the source of truth for the evidence tool set.
/// Guru may run this policy in the ordinary parent workflow; a bounded child
/// is optional and, when present, is checked as an additional allowlist.
fn compile_guru_runtime_policy(
    image: &AgentImageManifest,
    descriptors: &BTreeMap<String, CapabilityDescriptor>,
) -> Result<Option<Arc<GuruRuntimePolicy>>, CatalogError> {
    let guru_enabled = descriptors
        .values()
        .any(|descriptor| matches!(descriptor.mapping, EvidenceMapping::Guru(_)));
    let review_derivations = descriptors
        .values()
        .filter_map(
            |descriptor| match &descriptor.specification.input_derivation {
                InputDerivation::SealedGuruEvidenceReviewV1 {
                    evidence_capabilities,
                    ..
                } => Some(evidence_capabilities),
                InputDerivation::Identity
                | InputDerivation::CompanyContextRequestV1
                | InputDerivation::SealedGuruQueryContextV1
                | InputDerivation::ResearchProposalToSearchPlanV4
                | InputDerivation::SealedGuruCompanyBriefV1 { .. } => None,
            },
        )
        .collect::<Vec<_>>();
    if !guru_enabled {
        return if review_derivations.is_empty() {
            Ok(None)
        } else {
            Err(CatalogError::InvalidGuruChildPolicy)
        };
    }

    let [evidence_capabilities] = review_derivations.as_slice() else {
        return Err(CatalogError::InvalidGuruChildPolicy);
    };

    let mut child_roles = image
        .body
        .roles
        .iter()
        .filter(|role| role.bounded_child.is_some());
    let evidence_capability_ids = evidence_capabilities.iter().collect::<BTreeSet<_>>();
    if let Some(role) = child_roles.next() {
        if child_roles.next().is_some() || role.deterministic {
            return Err(CatalogError::InvalidGuruChildPolicy);
        }
        // A bounded researcher may intentionally expose a strict subset of
        // the evidence capabilities. It must never gain a capability outside
        // the sealed review set, while the review derivation may still retain
        // the complete evidence union for parent-side follow-up.
        let child = role
            .bounded_child
            .as_ref()
            .ok_or(CatalogError::InvalidGuruChildPolicy)?;
        if child
            .allowed_capabilities
            .iter()
            .any(|capability_id| !evidence_capability_ids.contains(capability_id))
        {
            return Err(CatalogError::InvalidGuruChildPolicy);
        }
    }
    for capability_id in *evidence_capabilities {
        let Some(descriptor) = descriptors.get(capability_id) else {
            return Err(CatalogError::InvalidGuruChildPolicy);
        };
        if descriptor.specification.research_action.is_none()
            || !matches!(
                descriptor.mapping,
                EvidenceMapping::ResearchStateV2
                    | EvidenceMapping::TargetedEvidenceV1
                    | EvidenceMapping::TraceLineageV1
            )
        {
            return Err(CatalogError::InvalidGuruChildPolicy);
        }
    }
    Ok(Some(Arc::new(GuruRuntimePolicy {
        evidence_capability_ids: evidence_capabilities.iter().cloned().collect(),
    })))
}

fn validate_capability_semantics(
    capability: &CapabilitySpec,
    mapping: EvidenceMapping,
) -> Result<(), CatalogError> {
    if capability.permission != Permission::Read
        || capability.idempotency != IdempotencyPolicy::CanonicalArgs
        || capability.input_contract != mapping.input_contract()
        || capability
            .output_contracts
            .iter()
            .map(String::as_str)
            .ne(mapping.output_contracts().iter().copied())
    {
        return Err(CatalogError::IncompatibleCapability(capability.id.clone()));
    }
    Ok(())
}

fn verify_contracts(
    contracts: &ResolvedCapabilityContracts,
    mapping: EvidenceMapping,
) -> Result<(), CatalogError> {
    verify_pin(mapping.input_contract(), &contracts.input.content_hash)?;
    for output in &contracts.outputs {
        verify_pin(&output.id, &output.content_hash)?;
    }
    Ok(())
}

fn validate_resolved_binding(
    capability: &CapabilitySpec,
    resolved: &ResolvedCapability,
) -> Result<(), CatalogError> {
    if resolved.binding.binding_key != capability.binding_key
        || resolved.binding.transport != TransportKind::McpHttp
        || resolved.binding.max_connections == 0
        || resolved.binding.request_timeout_ms == 0
    {
        return Err(CatalogError::IncompatibleResolvedBinding(
            capability.id.clone(),
        ));
    }
    Ok(())
}

#[derive(Clone, PartialEq, Eq)]
pub struct RunScope {
    pub tenant_id: String,
    pub principal_id: String,
    pub run_id: String,
}

impl fmt::Debug for RunScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RunScope")
            .field("tenant_id_hash", &ContentHash::sha256(&self.tenant_id))
            .field(
                "principal_id_hash",
                &ContentHash::sha256(&self.principal_id),
            )
            .field("run_id_hash", &ContentHash::sha256(&self.run_id))
            .finish()
    }
}

impl RunScope {
    fn validate(&self) -> Result<(), CatalogError> {
        for value in [&self.tenant_id, &self.principal_id, &self.run_id] {
            if value.is_empty()
                || value.len() > MAX_SCOPE_ID_BYTES
                || value.chars().any(char::is_control)
            {
                return Err(CatalogError::InvalidRunScope);
            }
        }
        Ok(())
    }

    fn into_pool_scope(self) -> PoolScope {
        PoolScope {
            tenant_id: self.tenant_id,
            principal_id: self.principal_id,
            run_id: self.run_id,
        }
    }
}

#[async_trait]
pub trait McpToolTransport: fmt::Debug + Send + Sync {
    async fn call_tool(
        &self,
        resolved: &ResolvedCapability,
        scope: &PoolScope,
        tool_name: &str,
        arguments: Value,
    ) -> Result<ToolCallOutcome, DependencyFailure>;
}

/// Production transport backed by the machine-wide, bounded, single-flight
/// MCP pool. Shareable bindings are valid only for initialization-only,
/// stateless servers; stateful/personal adapters must use principal/run scope.
#[derive(Debug)]
pub struct PooledMcpTransport {
    pool: Arc<McpClientPool>,
    max_response_bytes: usize,
}

impl PooledMcpTransport {
    pub fn new(pool: Arc<McpClientPool>) -> Arc<Self> {
        Arc::new(Self {
            pool,
            max_response_bytes: MAX_MCP_PAYLOAD_BYTES,
        })
    }

    pub fn pool(&self) -> &Arc<McpClientPool> {
        &self.pool
    }
}

#[async_trait]
impl McpToolTransport for PooledMcpTransport {
    async fn call_tool(
        &self,
        resolved: &ResolvedCapability,
        scope: &PoolScope,
        tool_name: &str,
        arguments: Value,
    ) -> Result<ToolCallOutcome, DependencyFailure> {
        let binding = &resolved.binding;
        let key = PoolKey::from_binding(
            binding.endpoint_ref.clone(),
            binding,
            &resolved.endpoint,
            &resolved.readiness_endpoint,
            &resolved.origin,
            resolved.protocol_version.clone(),
            scope,
            resolved.credential_version.clone(),
            resolved.tls_profile.clone(),
            resolved.tls_ca_pem_hash.clone(),
        )
        .map_err(|error| {
            mcp_failure(
                "mcp_pool_key",
                &error,
                false,
                DeliveryCertainty::NotDispatched,
            )
        })?;
        // `get_or_connect_with` takes ownership of the pool key. Keep the
        // identity needed to evict this exact client if the single tool call
        // later loses its HTTP/MCP session.
        let pool_key = key.clone();
        let client = self
            .pool
            .get_or_connect_with(key, || {
                Ok(McpHttpConfig {
                    endpoint: resolved.endpoint.clone(),
                    readiness_endpoint: resolved.readiness_endpoint.clone(),
                    origin: resolved.origin.clone(),
                    bearer_token: resolved.bearer_token.clone(),
                    protocol_version: resolved.protocol_version.clone(),
                    client_name: CLIENT_NAME.into(),
                    client_version: env!("CARGO_PKG_VERSION").into(),
                    tls_profile: resolved.tls_profile.clone(),
                    tls_ca_pem: resolved
                        .tls_ca_pem
                        .as_ref()
                        .map(|pem| zeroize::Zeroizing::new(pem.as_str().to_owned())),
                    max_concurrency: usize::from(binding.max_connections),
                    request_timeout: Duration::from_millis(binding.request_timeout_ms),
                    max_response_bytes: self.max_response_bytes,
                })
            })
            .await
            .map_err(|error| {
                let retryable = retryable_mcp_error(&error);
                mcp_failure(
                    "mcp_connect",
                    &error,
                    retryable,
                    DeliveryCertainty::NotDispatched,
                )
            })?;
        // Single dispatch attempt. The bounded transport retry lives inside
        // `McpHttpClient::request` (one layer only) to avoid nested retry
        // storms. Capability calls carry `DeliveryCertainty::MayHaveDispatched`
        // because a stream-level failure can occur after the server has acted;
        // blindly re-POSTing here would risk duplicate dispatch against the
        // evidence ledger. The run-engine's capability recovery directive
        // (see `model_recovery_directive`) routes retryable MayHaveDispatched
        // failures to recovery instead of re-executing the capability.
        match client.call_tool(tool_name, arguments).await {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                let retryable = retryable_mcp_error(&error);
                // Do not reuse a client after a stream/session failure. The
                // failed call remains at-most-once, but future calls should
                // establish a fresh session instead of inheriting a broken
                // HTTP/MCP connection.
                self.pool
                    .invalidate_if_current(&pool_key, &client)
                    .await;
                Err(mcp_failure(
                    "mcp_call",
                    &error,
                    retryable,
                    DeliveryCertainty::MayHaveDispatched,
                ))
            }
        }
    }
}

fn retryable_mcp_error(error: &McpError) -> bool {
    error.is_retryable()
}

fn mcp_failure(
    code: &str,
    error: &McpError,
    retryable: bool,
    delivery: DeliveryCertainty,
) -> DependencyFailure {
    DependencyFailure::redacted(code, format!("{error:?}"), retryable, delivery)
}

/// Cheap per-active-run adapter. No prompt, schema, endpoint, token, or client
/// is duplicated here.
pub struct PooledMcpCapabilityRuntime {
    catalog: Arc<CapabilityCatalog>,
    transport: Arc<dyn McpToolTransport>,
    pool_scope: PoolScope,
    front_state: Arc<Mutex<FrontRunState>>,
    pending_front_result: Arc<Mutex<Option<(String, ContentHash)>>>,
    guru_state: Arc<Mutex<GuruRunState>>,
    /// Private per-run presentation channel captured from tools/call `_meta`.
    /// Never normalized into evidence and never visible to the provider; the
    /// engine drains it at commit time for deterministic chart compilation.
    presentation: Arc<Mutex<Vec<Value>>>,
    stateful_order: AsyncMutex<()>,
    cpu_work: Arc<CpuWorkLimiter>,
    guru_policy: Option<Arc<GuruRuntimePolicy>>,
}

/// All inputs needed by the CPU-bound recovery projection check. Grouping them
/// makes the recovery boundary explicit and keeps it difficult to accidentally
/// omit an isolation or state binding when this path evolves.
struct RestoreCommittedInput<'a> {
    image_hash: &'a ContentHash,
    pool_scope: &'a PoolScope,
    invocation: &'a CapabilityInvocation,
    result: &'a CapabilityResult,
    descriptor: &'a CapabilityDescriptor,
    resolved: &'a ResolvedCapability,
    front_state: &'a Mutex<FrontRunState>,
    pending_front_result: &'a Mutex<Option<(String, ContentHash)>>,
    guru_state: &'a Mutex<GuruRunState>,
    guru_policy: Option<&'a GuruRuntimePolicy>,
}

impl fmt::Debug for PooledMcpCapabilityRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PooledMcpCapabilityRuntime")
            .field("image_hash", &self.catalog.image_hash)
            .field(
                "tenant_id_hash",
                &ContentHash::sha256(&self.pool_scope.tenant_id),
            )
            .field(
                "principal_id_hash",
                &ContentHash::sha256(&self.pool_scope.principal_id),
            )
            .field("run_id_hash", &ContentHash::sha256(&self.pool_scope.run_id))
            .field(
                "front_state",
                &self
                    .front_state
                    .lock()
                    .map_or("[POISONED]".into(), |state| format!("{state:?}")),
            )
            .field(
                "guru_evidence_capability_count",
                &self
                    .guru_policy
                    .as_ref()
                    .map_or(0, |policy| policy.evidence_capability_ids.len()),
            )
            .field("cpu_work_parallelism", &self.cpu_work.max_parallelism)
            .field("transport", &"[REDACTED_TRANSPORT]")
            .finish_non_exhaustive()
    }
}

impl PooledMcpCapabilityRuntime {
    pub fn for_run(
        catalog: Arc<CapabilityCatalog>,
        transport: Arc<dyn McpToolTransport>,
        scope: RunScope,
    ) -> Result<Self, CatalogError> {
        scope.validate()?;
        let pool_scope = scope.into_pool_scope();
        let guru_policy = catalog.guru_policy.clone();
        Ok(Self {
            catalog,
            transport,
            pool_scope,
            front_state: Arc::new(Mutex::new(FrontRunState::default())),
            pending_front_result: Arc::new(Mutex::new(None)),
            guru_state: Arc::new(Mutex::new(GuruRunState::default())),
            presentation: Arc::new(Mutex::new(Vec::new())),
            stateful_order: AsyncMutex::new(()),
            cpu_work: machine_cpu_work_limiter(),
            guru_policy,
        })
    }

    fn lookup_invocation<'a>(
        &'a self,
        invocation: &CapabilityInvocation,
    ) -> Result<(&'a CapabilityDescriptor, &'a Arc<ResolvedCapability>), DependencyFailure> {
        if invocation.run_id != self.pool_scope.run_id {
            return Err(reject(
                "run_scope_mismatch",
                "invocation run differs from fixed run scope",
            ));
        }
        let descriptor = self
            .catalog
            .descriptors
            .get(&invocation.capability_id)
            .ok_or_else(|| reject("unknown_capability", "capability is absent from AgentImage"))?;
        let resolved = self
            .catalog
            .runtime
            .capabilities
            .get(&invocation.capability_id)
            .ok_or_else(|| reject("missing_resolved_binding", "deployment binding is absent"))?;
        if invocation.binding != resolved.binding
            || invocation.binding.binding_key != descriptor.specification.binding_key
            || invocation.input_schema_hash != descriptor.contracts.input.content_hash
            || invocation.output_schema_hash != descriptor.contracts.output_contract_set_hash
            || invocation.normalized_output_contract_hash
                != descriptor.normalized_output_contract_hash
        {
            return Err(reject(
                "invocation_contract_mismatch",
                "invocation does not match the frozen image/deployment contract",
            ));
        }
        Ok((descriptor, resolved))
    }

    fn validate_invocation_cpu(
        image_hash: &ContentHash,
        invocation: &CapabilityInvocation,
        descriptor: &CapabilityDescriptor,
        binding: &krw_agent_protocol::CapabilityBinding,
    ) -> Result<(), DependencyFailure> {
        let canonical_arguments = Zeroizing::new(
            serde_jcs::to_vec(&invocation.arguments)
                .map_err(|error| reject("arguments_canonicalization", format!("{error:?}")))?,
        );
        if ContentHash::sha256(canonical_arguments.as_slice()) != invocation.request_hash {
            return Err(reject(
                "request_hash_mismatch",
                "canonical invocation arguments differ from the durable request hash",
            ));
        }
        validate_value(
            &descriptor.specification.input_contract,
            &invocation.arguments,
        )
        .map_err(|error| reject("canonical_input_invalid", format!("{error:?}")))?;
        let expected_action_key = deterministic_action_key(
            &invocation.run_id,
            image_hash,
            &descriptor.specification,
            &descriptor.contracts,
            binding,
            &invocation.arguments,
        )
        .map_err(|error| reject("action_key_derivation", format!("{error:?}")))?;
        if invocation.action_key != expected_action_key {
            return Err(reject(
                "action_key_mismatch",
                "action key differs from the frozen semantic/deployment fingerprint",
            ));
        }
        Ok(())
    }

    fn prepare_arguments_cpu(
        descriptor: &CapabilityDescriptor,
        invocation: &CapabilityInvocation,
        front_state: &Mutex<FrontRunState>,
        guru_state: &Mutex<GuruRunState>,
        guru_policy: Option<&GuruRuntimePolicy>,
    ) -> Result<Value, DependencyFailure> {
        if let EvidenceMapping::Front(mapping) = descriptor.mapping {
            front_state
                .lock()
                .map_err(|_| {
                    reject(
                        "front_state_poisoned",
                        "front authorization state is unavailable",
                    )
                })?
                .authorize(mapping, &invocation.arguments)?;
        }
        let is_guru_evidence = descriptor.is_guru_evidence_capability(guru_policy);
        match descriptor.mapping {
            EvidenceMapping::Guru(mapping) => guru_state
                .lock()
                .map_err(|_| {
                    reject(
                        "guru_state_poisoned",
                        "Guru authorization state is unavailable",
                    )
                })?
                .prepare_invocation(mapping, &invocation.arguments),
            EvidenceMapping::ResearchStateV2 if is_guru_evidence => {
                guru_state
                    .lock()
                    .map_err(|_| {
                        reject(
                            "guru_state_poisoned",
                            "Guru authorization state is unavailable",
                        )
                    })?
                    .authorize_company_search_plan(&invocation.arguments)?;
                Ok(invocation.arguments.clone())
            }
            EvidenceMapping::TargetedEvidenceV1 | EvidenceMapping::TraceLineageV1
                if is_guru_evidence =>
            {
                guru_state
                    .lock()
                    .map_err(|_| {
                        reject(
                            "guru_state_poisoned",
                            "Guru authorization state is unavailable",
                        )
                    })?
                    .authorize_company_evidence()?;
                Ok(invocation.arguments.clone())
            }
            _ => Ok(invocation.arguments.clone()),
        }
    }

    async fn prepare_invocation_async(
        &self,
        invocation: &CapabilityInvocation,
    ) -> Result<
        (
            CapabilityDescriptor,
            Arc<ResolvedCapability>,
            Value,
            Arc<CapabilityInvocation>,
        ),
        DependencyFailure,
    > {
        let (descriptor, resolved) = self.lookup_invocation(invocation)?;
        let descriptor = descriptor.clone();
        let resolved = Arc::clone(resolved);
        let image_hash = self.catalog.image_hash.clone();
        let invocation = Arc::new(invocation.clone());
        let cpu_invocation = Arc::clone(&invocation);
        let cpu_descriptor = descriptor.clone();
        let binding = resolved.binding.clone();
        let front_state = Arc::clone(&self.front_state);
        let guru_state = Arc::clone(&self.guru_state);
        let guru_policy = self.guru_policy.clone();
        let arguments = self
            .cpu_work
            .execute(DeliveryCertainty::NotDispatched, move || {
                Self::validate_invocation_cpu(
                    &image_hash,
                    &cpu_invocation,
                    &cpu_descriptor,
                    &binding,
                )?;
                Self::prepare_arguments_cpu(
                    &cpu_descriptor,
                    &cpu_invocation,
                    &front_state,
                    &guru_state,
                    guru_policy.as_deref(),
                )
            })
            .await?;
        Ok((descriptor, resolved, arguments, invocation))
    }

    fn map_success_cpu(
        pool_scope: &PoolScope,
        front_state: Option<&FrontRunState>,
        guru_state: Option<&GuruRunState>,
        invocation: &CapabilityInvocation,
        descriptor: &CapabilityDescriptor,
        resolved: &ResolvedCapability,
        payload: Value,
    ) -> Result<(CapabilityResult, Option<ContentHash>), DependencyFailure> {
        let payload_bytes = Zeroizing::new(
            serde_jcs::to_vec(&payload)
                .map_err(|error| reject("payload_canonicalization", format!("{error:?}")))?,
        );
        let payload_ref = ContentHash::sha256(payload_bytes.as_slice());
        let scope_hash = pool_scope
            .partition_hash(&resolved.binding.auth_scope)
            .map_err(|error| {
                mcp_failure(
                    "evidence_scope",
                    &error,
                    false,
                    DeliveryCertainty::NotDispatched,
                )
            })?;
        let context = MappingContext {
            capability_id: invocation.capability_id.clone(),
            action_key: invocation.action_key.clone(),
            server_build: resolved.binding.server_build.clone(),
            normalized_contract_hash: descriptor.normalized_output_contract_hash.clone(),
            server_schema_bundle_hash: resolved.binding.server_schema_bundle_hash.clone(),
            data_release_hash: resolved.binding.data_release_hash.clone(),
            scope: EvidenceScope {
                auth_scope: resolved.binding.auth_scope.clone(),
                scope_hash,
            },
            payload_ref,
        };
        let result = match descriptor.mapping {
            EvidenceMapping::ResearchStateV2 => {
                validate_value(RESEARCH_STATE_V2, &payload).map_err(|error| {
                    reject("research_state_contract_invalid", format!("{error:?}"))
                })?;
                let state = parse_research_state(payload_bytes.as_slice()).map_err(|error| {
                    reject("research_state_typed_invalid", format!("{error:?}"))
                })?;
                // A fixed-company plan is a trusted scope boundary.  Keep a
                // malformed cross-company unit from reaching either the
                // evidence ledger or the next model turn, but preserve the
                // rest of the successful response as a qualified result.
                // Discovery plans carry no fixed ticker set and remain
                // intentionally unfiltered here.
                let state = sanitize_research_state_scope(
                    &state,
                    &expected_research_tickers(&invocation.arguments),
                );
                let provider_content = serde_json::to_value(&state).map_err(|error| {
                    reject("research_state_scope_projection", format!("{error:?}"))
                })?;
                validate_value(RESEARCH_STATE_V2, &provider_content).map_err(|error| {
                    reject(
                        "research_state_scope_projection_invalid",
                        format!("{error:?}"),
                    )
                })?;
                let delta = map_research_state(&state, &context)
                    .map_err(|error| reject("research_state_mapping", format!("{error:?}")))?;
                CapabilityResult {
                    provider_content,
                    evidence: delta.records,
                    answerability: Some(delta.answerability),
                    calculations: delta.calculations,
                    presentation: None,
                }
            }
            EvidenceMapping::CompanyContextV1 => {
                let ticker = invocation
                    .arguments
                    .get("ticker")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        reject(
                            "company_context_input_identity",
                            "company context input did not retain a ticker",
                        )
                    })?;
                let delta = map_company_context(&payload, ticker, &context)
                    .map_err(|error| reject("company_context_mapping", format!("{error:?}")))?;
                CapabilityResult {
                    // Keep only the adapter's explicit, bounded orientation
                    // projection. The raw provider payload can include
                    // internal router metadata and never reaches the model.
                    provider_content: delta.provider_content,
                    evidence: delta.records,
                    answerability: None,
                    calculations: Vec::new(),
                    presentation: None,
                }
            }
            EvidenceMapping::MarketSnapshotV1 => {
                let ticker = invocation
                    .arguments
                    .get("ticker")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        reject(
                            "market_snapshot_input_identity",
                            "market snapshot input did not retain a ticker",
                        )
                    })?;
                let delta = map_market_snapshot(&payload, ticker, &context)
                    .map_err(|error| reject("market_snapshot_mapping", format!("{error:?}")))?;
                CapabilityResult {
                    provider_content: delta.provider_content,
                    evidence: delta.records,
                    answerability: None,
                    calculations: Vec::new(),
                    presentation: None,
                }
            }
            EvidenceMapping::TargetedEvidenceV1 => {
                let delta = map_targeted_query(&payload, &context)
                    .map_err(|error| reject("targeted_evidence_mapping", format!("{error:?}")))?;
                CapabilityResult {
                    provider_content: payload,
                    evidence: delta.records,
                    answerability: None,
                    calculations: delta.calculations,
                    presentation: None,
                }
            }
            EvidenceMapping::TraceLineageV1 => {
                let delta = map_trace(&payload, &context)
                    .map_err(|error| reject("trace_lineage_mapping", format!("{error:?}")))?;
                CapabilityResult {
                    provider_content: payload,
                    evidence: delta.records,
                    answerability: None,
                    calculations: delta.calculations,
                    presentation: None,
                }
            }
            EvidenceMapping::Front(mapping) => {
                let front_state = front_state.ok_or_else(|| {
                    reject(
                        "front_state_unavailable",
                        "front validation state was not provided",
                    )
                })?;
                validate_value(mapping.output_contract(), &payload).map_err(|error| {
                    reject("front_success_contract_invalid", format!("{error:?}"))
                })?;
                front_mapping::validate_exchange(mapping, &invocation.arguments, &payload)?;
                front_state.authorize(mapping, &invocation.arguments)?;
                front_state.validate_identity(mapping, &payload)?;
                let (evidence, answerability) =
                    front_mapping::map_evidence(mapping, &payload, &context)?;
                CapabilityResult {
                    provider_content: payload,
                    evidence,
                    answerability,
                    calculations: Vec::new(),
                    presentation: None,
                }
            }
            EvidenceMapping::Guru(mapping) => {
                let guru_state = guru_state.ok_or_else(|| {
                    reject(
                        "guru_state_unavailable",
                        "Guru validation state was not provided",
                    )
                })?;
                guru_state.validate_success(mapping, &invocation.arguments, &payload)?;
                CapabilityResult {
                    // Guru philosophy, briefs, and reviews are private control
                    // artifacts. They are never promoted to company evidence.
                    provider_content: payload,
                    evidence: Vec::new(),
                    answerability: None,
                    calculations: Vec::new(),
                    presentation: None,
                }
            }
            EvidenceMapping::SkillContent => {
                // The skill body is a local prompt artifact, not evidence. It
                // is forwarded verbatim as provider-visible content; no ledger
                // entries, no answerability, no calculations.
                validate_value(SKILL_CONTENT_V1, &payload).map_err(|error| {
                    reject("skill_content_contract_invalid", format!("{error:?}"))
                })?;
                CapabilityResult {
                    provider_content: payload,
                    evidence: Vec::new(),
                    answerability: None,
                    calculations: Vec::new(),
                    presentation: None,
                }
            }
        };
        validate_normalized_result(&result)?;
        let pending_hash = if matches!(descriptor.mapping, EvidenceMapping::Front(_)) {
            let bytes =
                Zeroizing::new(serde_jcs::to_vec(&result).map_err(|error| {
                    reject("front_result_canonicalization", format!("{error:?}"))
                })?);
            Some(ContentHash::sha256(bytes.as_slice()))
        } else {
            None
        };
        Ok((result, pending_hash))
    }

    fn map_tool_error(
        descriptor: &CapabilityDescriptor,
        resolved: &ResolvedCapability,
        payload: Value,
    ) -> Result<CapabilityResult, DependencyFailure> {
        match descriptor.mapping {
            EvidenceMapping::ResearchStateV2 => {
                validate_value(QUERY_CONTEXT_INPUT_CORRECTION_V1, &payload)
                    .map_err(|error| reject("correction_contract_invalid", format!("{error:?}")))?;
            }
            EvidenceMapping::Guru(GuruMapping::CompanyBrief | GuruMapping::EvidenceReview) => {
                let EvidenceMapping::Guru(mapping) = descriptor.mapping else {
                    unreachable!("match arm establishes Guru mapping")
                };
                validate_correction_for_mapping(
                    mapping,
                    resolved.binding.mcp_tool_name.as_str(),
                    &payload,
                )?;
            }
            EvidenceMapping::Guru(GuruMapping::QueryContext)
            | EvidenceMapping::CompanyContextV1
            | EvidenceMapping::MarketSnapshotV1
            | EvidenceMapping::TargetedEvidenceV1
            | EvidenceMapping::TraceLineageV1
            | EvidenceMapping::Front(_)
            | EvidenceMapping::SkillContent => {
                return Err(reject(
                    "untyped_tool_error",
                    "the capability has no declared typed error result",
                ));
            }
        }
        let result = CapabilityResult {
            provider_content: payload,
            evidence: Vec::new(),
            answerability: None,
            calculations: Vec::new(),
            presentation: None,
        };
        validate_normalized_result(&result)?;
        Ok(result)
    }

    fn set_pending_front_result(
        &self,
        invocation: &CapabilityInvocation,
        pending_hash: Option<ContentHash>,
    ) -> Result<(), DependencyFailure> {
        if let Some(pending_hash) = pending_hash {
            let mut pending = self.pending_front_result.lock().map_err(|_| {
                reject("front_state_poisoned", "front pending state is unavailable")
            })?;
            *pending = Some((invocation.action_key.clone(), pending_hash));
        }
        Ok(())
    }

    fn restore_committed_result_cpu(
        input: &RestoreCommittedInput<'_>,
    ) -> Result<(), DependencyFailure> {
        let RestoreCommittedInput {
            image_hash,
            pool_scope,
            invocation,
            result,
            descriptor,
            resolved,
            front_state,
            pending_front_result,
            guru_state,
            guru_policy,
        } = *input;
        Self::validate_invocation_cpu(image_hash, invocation, descriptor, &resolved.binding)?;
        let is_guru_evidence = descriptor.is_guru_evidence_capability(guru_policy);
        match descriptor.mapping {
            EvidenceMapping::Front(mapping) => {
                let observed_bytes =
                    Zeroizing::new(serde_jcs::to_vec(result).map_err(|error| {
                        reject("front_result_canonicalization", format!("{error:?}"))
                    })?);
                let observed_hash = ContentHash::sha256(observed_bytes.as_slice());
                let pending = pending_front_result
                    .lock()
                    .map_err(|_| {
                        reject("front_state_poisoned", "front pending state is unavailable")
                    })?
                    .take();
                match pending {
                    Some((action_key, expected_hash))
                        if action_key == invocation.action_key
                            && expected_hash == observed_hash => {}
                    Some(_) => {
                        return Err(reject(
                            "front_committed_projection_mismatch",
                            "committed normalized projection differs from the invoked result",
                        ));
                    }
                    None => {
                        let expected = {
                            let state = front_state.lock().map_err(|_| {
                                reject(
                                    "front_state_poisoned",
                                    "front authorization state is unavailable",
                                )
                            })?;
                            Self::map_success_cpu(
                                pool_scope,
                                Some(&state),
                                None,
                                invocation,
                                descriptor,
                                resolved,
                                result.provider_content.clone(),
                            )?
                            .0
                        };
                        if expected != *result {
                            return Err(reject(
                                "front_recovery_projection_mismatch",
                                "persisted normalized result is not the canonical raw-output projection",
                            ));
                        }
                    }
                }
                front_state
                    .lock()
                    .map_err(|_| {
                        reject(
                            "front_state_poisoned",
                            "front authorization state is unavailable",
                        )
                    })?
                    .apply_committed(mapping, &invocation.arguments, &result.provider_content)
            }
            EvidenceMapping::Guru(mapping) => {
                let expected = if is_guru_correction(&result.provider_content) {
                    Self::map_tool_error(descriptor, resolved, result.provider_content.clone())?
                } else {
                    let state = guru_state.lock().map_err(|_| {
                        reject(
                            "guru_state_poisoned",
                            "Guru authorization state is unavailable",
                        )
                    })?;
                    Self::map_success_cpu(
                        pool_scope,
                        None,
                        Some(&state),
                        invocation,
                        descriptor,
                        resolved,
                        result.provider_content.clone(),
                    )?
                    .0
                };
                if expected != *result {
                    return Err(reject(
                        "guru_committed_projection_mismatch",
                        "persisted Guru result is not the canonical raw-output projection",
                    ));
                }
                guru_state
                    .lock()
                    .map_err(|_| {
                        reject(
                            "guru_state_poisoned",
                            "Guru authorization state is unavailable",
                        )
                    })?
                    .apply_committed(
                        mapping,
                        resolved.binding.mcp_tool_name.as_str(),
                        &invocation.arguments,
                        &result.provider_content,
                    )
            }
            EvidenceMapping::ResearchStateV2 if is_guru_evidence => {
                let mut state = guru_state.lock().map_err(|_| {
                    reject(
                        "guru_state_poisoned",
                        "Guru authorization state is unavailable",
                    )
                })?;
                state.authorize_company_search_plan(&invocation.arguments)?;
                state.observe_company_evidence(&result.provider_content)
            }
            EvidenceMapping::TargetedEvidenceV1 | EvidenceMapping::TraceLineageV1
                if is_guru_evidence =>
            {
                guru_state
                    .lock()
                    .map_err(|_| {
                        reject(
                            "guru_state_poisoned",
                            "Guru authorization state is unavailable",
                        )
                    })?
                    .observe_company_evidence(&result.provider_content)
            }
            EvidenceMapping::ResearchStateV2
            | EvidenceMapping::CompanyContextV1
            | EvidenceMapping::MarketSnapshotV1
            | EvidenceMapping::TargetedEvidenceV1
            | EvidenceMapping::TraceLineageV1
            | EvidenceMapping::SkillContent => Ok(()),
        }
    }

    async fn map_outcome_async(
        &self,
        invocation: Arc<CapabilityInvocation>,
        descriptor: CapabilityDescriptor,
        resolved: Arc<ResolvedCapability>,
        outcome: ToolCallOutcome,
    ) -> Result<CapabilityResult, DependencyFailure> {
        let (envelope, is_error) = outcome.into_payload();
        let cpu_invocation = Arc::clone(&invocation);
        let pool_scope = self.pool_scope.clone();
        let front_state = Arc::clone(&self.front_state);
        let guru_state = Arc::clone(&self.guru_state);
        let captures_presentation = matches!(descriptor.mapping, EvidenceMapping::ResearchStateV2);
        let (mut result, pending_hash, presentation) = self
            .cpu_work
            .execute(DeliveryCertainty::MayHaveDispatched, move || {
                let extracted = extract_json_tool_payload(envelope, is_error)?;
                let presentation = extracted.presentation;
                let payload = extracted.payload;
                if is_error {
                    return Self::map_tool_error(&descriptor, &resolved, payload)
                        .map(|result| (result, None, presentation));
                }
                // Live run state is read-only in this non-cancelable blocking
                // task. Per-run ordering plus these locks keeps validation
                // stable without cloning potentially multi-megabyte state.
                let front_state = if matches!(descriptor.mapping, EvidenceMapping::Front(_)) {
                    Some(front_state.lock().map_err(|_| {
                        reject(
                            "front_state_poisoned",
                            "front authorization state is unavailable",
                        )
                    })?)
                } else {
                    None
                };
                let guru_state = if matches!(descriptor.mapping, EvidenceMapping::Guru(_)) {
                    Some(guru_state.lock().map_err(|_| {
                        reject(
                            "guru_state_poisoned",
                            "Guru authorization state is unavailable",
                        )
                    })?)
                } else {
                    None
                };
                Self::map_success_cpu(
                    &pool_scope,
                    front_state.as_deref(),
                    guru_state.as_deref(),
                    &cpu_invocation,
                    &descriptor,
                    &resolved,
                    payload,
                )
                .map(|(result, pending_hash)| (result, pending_hash, presentation))
            })
            .await?;
        if captures_presentation {
            result.presentation = presentation.clone();
        }
        self.set_pending_front_result(&invocation, pending_hash)?;
        if let Some(pack) = presentation
            && captures_presentation
        {
            // Presentation is optional. A poisoned best-effort ledger must not
            // turn a successfully retrieved research result into a dependency
            // failure or trigger a model recovery turn.
            if let Ok(mut ledger) = self.presentation.lock() {
                retain_presentation_diagnostic(&mut ledger, pack);
            }
        }
        Ok(result)
    }
}

#[async_trait]
impl CapabilityRuntime for PooledMcpCapabilityRuntime {
    fn presentation_packs(&self) -> Vec<Value> {
        self.presentation
            .lock()
            .map(|ledger| ledger.clone())
            .unwrap_or_default()
    }

    async fn invoke(
        &self,
        invocation: &CapabilityInvocation,
    ) -> Result<CapabilityResult, DependencyFailure> {
        let descriptor = self
            .catalog
            .descriptors
            .get(&invocation.capability_id)
            .ok_or_else(|| reject("unknown_capability", "capability is absent from AgentImage"))?;
        let _stateful_order = if descriptor.requires_run_order(self.guru_policy.as_deref()) {
            Some(self.stateful_order.lock().await)
        } else {
            None
        };
        let (descriptor, resolved, arguments, canonical_invocation) =
            self.prepare_invocation_async(invocation).await?;
        let outcome = self
            .transport
            .call_tool(
                &resolved,
                &self.pool_scope,
                resolved.binding.mcp_tool_name.as_str(),
                arguments,
            )
            .await?;
        self.map_outcome_async(canonical_invocation, descriptor, resolved, outcome)
            .await
    }

    async fn restore_committed_result(
        &self,
        invocation: &CapabilityInvocation,
        result: &CapabilityResult,
    ) -> Result<(), DependencyFailure> {
        let (descriptor, resolved) = self.lookup_invocation(invocation)?;
        let descriptor = descriptor.clone();
        let resolved = Arc::clone(resolved);
        let _stateful_order = if descriptor.requires_run_order(self.guru_policy.as_deref()) {
            Some(self.stateful_order.lock().await)
        } else {
            None
        };
        let invocation = Arc::new(invocation.clone());
        let result = Arc::new(result.clone());
        let image_hash = self.catalog.image_hash.clone();
        let pool_scope = self.pool_scope.clone();
        let front_state = Arc::clone(&self.front_state);
        let pending_front_result = Arc::clone(&self.pending_front_result);
        let guru_state = Arc::clone(&self.guru_state);
        let guru_policy = self.guru_policy.clone();
        self.cpu_work
            .execute(DeliveryCertainty::NotDispatched, move || {
                Self::restore_committed_result_cpu(&RestoreCommittedInput {
                    image_hash: &image_hash,
                    pool_scope: &pool_scope,
                    invocation: &invocation,
                    result: &result,
                    descriptor: &descriptor,
                    resolved: &resolved,
                    front_state: &front_state,
                    pending_front_result: &pending_front_result,
                    guru_state: &guru_state,
                    guru_policy: guru_policy.as_deref(),
                })
            })
            .await
    }
}

/// Private vendor channel for presentation data on the tools/call envelope.
/// Values under this key are chart input for the deterministic presentation
/// compiler and are never normalized into evidence or provider content.
const PRESENTATION_META_KEY: &str = "com.krwontology/presentationSeries";
const MAX_PRESENTATION_PACKS: usize = 16;
const MAX_PRESENTATION_PACK_BYTES: usize = 64 * 1024;
const MAX_PRESENTATION_PACK_SERIES: usize = 8;
const MAX_PRESENTATION_PACK_POINTS: usize = 12;

struct ExtractedToolPayload {
    payload: Value,
    presentation: Option<Value>,
}

fn extract_json_tool_payload(
    mut envelope: Value,
    expected_is_error: bool,
) -> Result<ExtractedToolPayload, DependencyFailure> {
    let result = (|| {
        let object = envelope
            .as_object_mut()
            .ok_or_else(|| reject("mcp_envelope_shape", "tools/call result must be an object"))?;
        if object.keys().any(|key| {
            !matches!(
                key.as_str(),
                "content" | "structuredContent" | "isError" | "_meta"
            )
        }) {
            return Err(reject(
                "mcp_envelope_unknown_field",
                "tools/call result has an unsupported field",
            ));
        }
        let observed_is_error = match object.remove("isError") {
            None => false,
            Some(Value::Bool(value)) => value,
            Some(_) => {
                return Err(reject("mcp_envelope_error_flag", "isError must be boolean"));
            }
        };
        if observed_is_error != expected_is_error {
            return Err(reject(
                "mcp_envelope_error_mismatch",
                "transport classification differs from envelope",
            ));
        }
        // The presentation pack must be cloned out before the envelope (and
        // every string inside `_meta`) is scrubbed below.
        let presentation = object
            .remove("_meta")
            .and_then(|meta| meta.as_object().cloned())
            .and_then(|meta| extract_presentation_pack(&meta));
        let Some(Value::Array(mut content)) = object.remove("content") else {
            return Err(reject(
                "mcp_envelope_content",
                "content must be one text item",
            ));
        };
        if content.len() != 1 {
            content.iter_mut().for_each(scrub_json);
            return Err(reject(
                "mcp_envelope_content_count",
                "content must contain exactly one item",
            ));
        }
        let mut item = content.remove(0);
        let Some(item_object) = item.as_object_mut() else {
            scrub_json(&mut item);
            return Err(reject(
                "mcp_content_shape",
                "content item must be an object",
            ));
        };
        if item_object
            .keys()
            .any(|key| !matches!(key.as_str(), "type" | "text" | "annotations" | "_meta"))
            || item_object.get("type").and_then(Value::as_str) != Some("text")
        {
            scrub_json(&mut item);
            return Err(reject(
                "mcp_content_type",
                "only a standard text content item is supported",
            ));
        }
        let Some(Value::String(mut text)) = item_object.remove("text") else {
            scrub_json(&mut item);
            return Err(reject("mcp_content_text", "text content is missing"));
        };
        if text.len() > MAX_MCP_PAYLOAD_BYTES {
            text.zeroize();
            scrub_json(&mut item);
            return Err(reject(
                "mcp_payload_limit",
                "text content exceeds the fixed payload bound",
            ));
        }
        let parsed: Result<Value, _> = serde_json::from_str(&text);
        text.zeroize();
        scrub_json(&mut item);
        let mut parsed = parsed.map_err(|error| reject("mcp_text_json", format!("{error:?}")))?;
        let structured = object.remove("structuredContent");
        let payload = if let Some(mut structured) = structured {
            // Some Streamable HTTP MCP implementations serialize an otherwise
            // standard structured result as {"result":"<json>"}, while
            // retaining the canonical JSON in the sole text item.  Treat that
            // exact one-field wrapper as a transport representation, not a
            // second semantic result.  The decoded value still has to agree
            // byte-for-byte after RFC 8785 canonicalization below; any other
            // wrapper or any disagreement remains fail-closed.
            if let Some(mut wrapped) = structured
                .as_object()
                .filter(|object| object.len() == 1)
                .and_then(|object| object.get("result"))
                .and_then(Value::as_str)
                .map(str::to_owned)
            {
                if wrapped.len() > MAX_MCP_PAYLOAD_BYTES {
                    wrapped.zeroize();
                    scrub_json(&mut parsed);
                    scrub_json(&mut structured);
                    return Err(reject(
                        "mcp_structured_result_limit",
                        "wrapped structured result exceeds the fixed payload bound",
                    ));
                }
                let decoded: Result<Value, _> = serde_json::from_str(&wrapped);
                wrapped.zeroize();
                structured = decoded.map_err(|error| {
                    scrub_json(&mut parsed);
                    reject("mcp_structured_result_json", format!("{error:?}"))
                })?;
            }
            let text_jcs = match serde_jcs::to_vec(&parsed) {
                Ok(bytes) => Zeroizing::new(bytes),
                Err(error) => {
                    scrub_json(&mut parsed);
                    scrub_json(&mut structured);
                    return Err(reject("mcp_text_canonicalization", format!("{error:?}")));
                }
            };
            let structured_jcs = match serde_jcs::to_vec(&structured) {
                Ok(bytes) => Zeroizing::new(bytes),
                Err(error) => {
                    scrub_json(&mut parsed);
                    scrub_json(&mut structured);
                    return Err(reject(
                        "mcp_structured_canonicalization",
                        format!("{error:?}"),
                    ));
                }
            };
            if text_jcs.as_slice() != structured_jcs.as_slice() {
                scrub_json(&mut parsed);
                scrub_json(&mut structured);
                return Err(reject(
                    "mcp_dual_payload_mismatch",
                    "text and structuredContent differ after RFC 8785 canonicalization",
                ));
            }
            scrub_json(&mut parsed);
            structured
        } else {
            parsed
        };
        let mut payload = payload;
        let canonical_len = match serde_jcs::to_vec(&payload) {
            Ok(bytes) => Zeroizing::new(bytes).len(),
            Err(error) => {
                scrub_json(&mut payload);
                return Err(reject("mcp_payload_canonicalization", format!("{error:?}")));
            }
        };
        if canonical_len > MAX_MCP_PAYLOAD_BYTES {
            scrub_json(&mut payload);
            return Err(reject(
                "mcp_payload_limit",
                "canonical payload exceeds the fixed payload bound",
            ));
        }
        Ok(ExtractedToolPayload {
            payload,
            presentation,
        })
    })();
    scrub_json(&mut envelope);
    result
}

/// Validate and return the vendor presentation pack from a tools/call `_meta`
/// object. Unknown vendor keys are ignored; malformed or oversized packs are
/// omitted. The primary MCP result is still validated strictly: presentation
/// is optional and must never prevent the research answer from completing.
fn extract_presentation_pack(meta: &serde_json::Map<String, Value>) -> Option<Value> {
    let Some(pack) = meta.get(PRESENTATION_META_KEY) else {
        return None;
    };
    let pack_object = pack.as_object()?;
    if pack_object.get("schema_version").and_then(Value::as_i64) != Some(2) {
        return None;
    }
    if pack_object.keys().any(|key| {
        matches!(
            key.as_str(),
            "html" | "markup" | "svg" | "jsx" | "rendered_html" | "renderer_code"
        )
    }) {
        // Presentation is a typed data channel.  Renderer instructions or
        // markup must never cross the MCP boundary, even as an ignored field.
        return None;
    }
    let series = pack_object.get("series").and_then(Value::as_array)?;
    if series.len() > MAX_PRESENTATION_PACK_SERIES {
        return None;
    }
    for item in series {
        let points = item.as_object()?.get("points").and_then(Value::as_array)?;
        if points.len() > MAX_PRESENTATION_PACK_POINTS {
            return None;
        }
    }
    let canonical = serde_jcs::to_vec(pack).ok()?;
    if canonical.len() > MAX_PRESENTATION_PACK_BYTES {
        return None;
    }
    Some(pack.clone())
}

/// Keep the diagnostic accessor bounded as well. The authoritative path is
/// `CapabilityResult.presentation`; this copy exists only for isolated
/// runtime diagnostics and must not become an unbounded per-run heap.
fn retain_presentation_diagnostic(ledger: &mut Vec<Value>, pack: Value) {
    if ledger.len() >= MAX_PRESENTATION_PACKS {
        return;
    }
    let Ok(bytes) = serde_jcs::to_vec(&pack) else {
        return;
    };
    if bytes.len() > MAX_PRESENTATION_PACK_BYTES {
        return;
    }
    let hash = ContentHash::sha256(&bytes);
    if ledger
        .iter()
        .filter_map(|existing| serde_jcs::to_vec(existing).ok())
        .all(|existing| ContentHash::sha256(existing) != hash)
    {
        ledger.push(pack);
    }
}

fn validate_normalized_result(result: &CapabilityResult) -> Result<(), DependencyFailure> {
    if result.evidence.len() > 256 || result.calculations.len() > 64 {
        return Err(reject(
            "normalized_result_limit",
            "normalized evidence/calculation count exceeded",
        ));
    }
    let mut normalized = result.clone();
    normalized.presentation = None;
    let mut value = serde_json::to_value(&normalized)
        .map_err(|error| reject("normalized_result_serialization", format!("{error:?}")))?;
    let validation = validate_value(NORMALIZED_CAPABILITY_RESULT_V1, &value)
        .map_err(|error| reject("normalized_result_invalid", format!("{error:?}")));
    scrub_json(&mut value);
    validation?;
    validate_evidence_lineage(result)
}

fn validate_evidence_lineage(result: &CapabilityResult) -> Result<(), DependencyFailure> {
    let mut evidence_ids = BTreeSet::new();
    let mut identities = BTreeSet::new();
    for evidence in &result.evidence {
        if !evidence_ids.insert(evidence.evidence_id.as_str())
            || !identities.insert((
                evidence.content_hash.as_str(),
                evidence.source.data_release_hash.as_str(),
                evidence.scope.scope_hash.as_str(),
            ))
        {
            return Err(reject(
                "duplicate_evidence_identity",
                "normalized evidence contains duplicate identity",
            ));
        }
    }
    let mut calculation_ids = BTreeSet::new();
    for calculation in &result.calculations {
        if calculation.input_evidence_ids.is_empty()
            || !calculation_ids.insert(calculation.calculation_id.as_str())
            || calculation
                .input_evidence_ids
                .iter()
                .any(|evidence_id| !evidence_ids.contains(evidence_id.as_str()))
        {
            return Err(reject(
                "invalid_calculation_lineage",
                "calculation inputs do not resolve to unique normalized evidence",
            ));
        }
    }
    Ok(())
}

fn reject(code: &str, diagnostic: impl AsRef<[u8]>) -> DependencyFailure {
    DependencyFailure::redacted(code, diagnostic, false, DeliveryCertainty::NotDispatched)
}

fn scrub_json(value: &mut Value) {
    match value {
        Value::String(text) => text.zeroize(),
        Value::Array(values) => values.iter_mut().for_each(scrub_json),
        Value::Object(values) => values.values_mut().for_each(scrub_json),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("canonical JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("AgentImage validation failed: {0}")]
    Image(#[from] krw_agent_image::ImageError),
    #[error("canonical contract pin failed: {0}")]
    Contract(#[from] krw_agent_contracts::ContractArtifactError),
    #[error("AgentImage body does not match its content hash")]
    ImageHashMismatch,
    #[error("capability is incompatible with its closed adapter: {0}")]
    IncompatibleCapability(String),
    #[error("capability has no normalized output contract: {0}")]
    MissingNormalizedOutput(String),
    #[error("deployment has no resolved capability binding: {0}")]
    MissingResolvedBinding(String),
    #[error("resolved binding is incompatible with capability: {0}")]
    IncompatibleResolvedBinding(String),
    #[error("duplicate capability: {0}")]
    DuplicateCapability(String),
    #[error("resolved capability set differs from AgentImage")]
    ResolvedCapabilitySetMismatch,
    #[error("Guru bounded child policy does not match the closed runtime ABI")]
    InvalidGuruChildPolicy,
    #[error("tenant/principal/run scope is invalid")]
    InvalidRunScope,
    #[error("more than one capability declares the sealed market preflight mapping")]
    AmbiguousMarketSnapshotPreflight,
    #[error("sealed market preflight invocation is invalid")]
    InvalidMarketSnapshotPreflight,
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use krw_agent_contracts::{
        KRW_FEED_LIST_ITEMS_INPUT_V1, KRW_FEED_LIST_ITEMS_RESULT_V1, KRW_FILING_GET_INPUT_V1,
        KRW_FILING_METADATA_V1, KRW_FILING_READ_SECTION_INPUT_V1,
        KRW_FILING_READ_SECTION_RESULT_V1, KRW_FILING_SECTIONS_INPUT_V1,
        KRW_FILING_SECTIONS_RESULT_V1, KRW_GURU_COMPANY_BRIEF_INPUT_V1,
        KRW_GURU_COMPANY_BRIEF_RESULT_V1, KRW_GURU_QUERY_CONTEXT_INPUT_V1,
        KRW_GURU_QUERY_CONTEXT_RESULT_V1, validate_value,
    };
    use krw_agent_evidence::{Answerability, Directness, EvidenceGrade, EvidenceLedger};
    use krw_agent_image::compile_agent_dir;
    use krw_agent_protocol::{
        AuthScope, CapabilityBinding, DeploymentBinding, McpToolSessionReuse, ModelRegistry,
    };
    use krw_agent_runtime_config::{
        BudgetProfile, BudgetRegistry, ConfigError, EndpointDescriptor, EndpointRegistry,
        SecretSource, ValidationMode, load_yaml, resolve_runtime,
    };
    use krw_agent_tool_mcp::classify_tool_result;
    use zeroize::Zeroizing;

    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn saturated_cpu_limiter_keeps_async_heartbeat_live_and_never_exceeds_cap() {
        let limiter = Arc::new(CpuWorkLimiter::new(NonZeroUsize::new(1).unwrap()));
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();

        let first_limiter = Arc::clone(&limiter);
        let first_active = Arc::clone(&active);
        let first_maximum = Arc::clone(&maximum);
        let first = tokio::spawn(async move {
            first_limiter
                .execute(DeliveryCertainty::NotDispatched, move || {
                    let now = first_active.fetch_add(1, Ordering::SeqCst) + 1;
                    first_maximum.fetch_max(now, Ordering::SeqCst);
                    let _ = started_tx.send(());
                    release_rx.recv().expect("release first CPU job");
                    first_active.fetch_sub(1, Ordering::SeqCst);
                    Ok::<_, DependencyFailure>(1_u8)
                })
                .await
        });
        started_rx.await.expect("first CPU job started");

        let second_limiter = Arc::clone(&limiter);
        let second_active = Arc::clone(&active);
        let second_maximum = Arc::clone(&maximum);
        let second = tokio::spawn(async move {
            second_limiter
                .execute(DeliveryCertainty::NotDispatched, move || {
                    let now = second_active.fetch_add(1, Ordering::SeqCst) + 1;
                    second_maximum.fetch_max(now, Ordering::SeqCst);
                    second_active.fetch_sub(1, Ordering::SeqCst);
                    Ok::<_, DependencyFailure>(2_u8)
                })
                .await
        });

        let heartbeat = tokio::spawn(async {
            for _ in 0..64 {
                tokio::task::yield_now().await;
            }
            64_u8
        });
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(100), heartbeat)
                .await
                .expect("I/O runtime heartbeat must not be blocked")
                .expect("heartbeat task"),
            64
        );
        assert!(
            !second.is_finished(),
            "second CPU job must wait for its permit"
        );
        assert_eq!(maximum.load(Ordering::SeqCst), 1);

        release_tx.send(()).expect("release first CPU job");
        assert_eq!(first.await.expect("first join").expect("first result"), 1);
        assert_eq!(
            second.await.expect("second join").expect("second result"),
            2
        );
        assert_eq!(maximum.load(Ordering::SeqCst), 1);
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }

    #[derive(Debug, Default)]
    struct FixtureSecrets(BTreeMap<String, String>);

    impl SecretSource for FixtureSecrets {
        fn read_secret(&self, name: &str) -> Result<Zeroizing<String>, ConfigError> {
            self.0
                .get(name)
                .cloned()
                .map(Zeroizing::new)
                .ok_or_else(|| ConfigError::MissingSecret(name.into()))
        }
    }

    struct FakeTransport {
        outcomes: Mutex<VecDeque<Value>>,
        calls: AtomicUsize,
        last_tool: Mutex<Option<String>>,
        last_arguments: Mutex<Option<Value>>,
    }

    impl fmt::Debug for FakeTransport {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("FakeTransport")
                .field("calls", &self.calls.load(Ordering::Relaxed))
                .field("payloads", &"[REDACTED]")
                .finish_non_exhaustive()
        }
    }

    impl FakeTransport {
        fn new(outcomes: impl IntoIterator<Item = Value>) -> Arc<Self> {
            Arc::new(Self {
                outcomes: Mutex::new(outcomes.into_iter().collect()),
                calls: AtomicUsize::new(0),
                last_tool: Mutex::new(None),
                last_arguments: Mutex::new(None),
            })
        }
    }

    #[async_trait]
    impl McpToolTransport for FakeTransport {
        async fn call_tool(
            &self,
            _resolved: &ResolvedCapability,
            _scope: &PoolScope,
            tool_name: &str,
            arguments: Value,
        ) -> Result<ToolCallOutcome, DependencyFailure> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            *self.last_tool.lock().expect("last tool mutex") = Some(tool_name.into());
            *self.last_arguments.lock().expect("last arguments mutex") = Some(arguments);
            let value = self
                .outcomes
                .lock()
                .expect("outcome mutex")
                .pop_front()
                .expect("fixture outcome");
            classify_tool_result(value)
                .map_err(|error| reject("fixture_transport", format!("{error:?}")))
        }
    }

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn fixture_image_and_runtime() -> (AgentImageManifest, Arc<ResolvedRuntime>) {
        let root = root();
        let image = compile_agent_dir(root.join("agents/krw-ontology"))
            .expect("compile fixture AgentImage")
            .manifest;
        let runtime = fixture_runtime(&image);
        (image, runtime)
    }

    fn fixture_runtime(image: &AgentImageManifest) -> Arc<ResolvedRuntime> {
        let root = root();
        let release = ContentHash::sha256("fixture-release");
        let schema = ContentHash::sha256("fixture-server-schema");
        let deployment = DeploymentBinding {
            schema_version: 3,
            deployment_id: "fixture".into(),
            capabilities: image
                .body
                .capabilities
                .iter()
                .map(|capability| capability.binding_key.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .map(|binding_key| CapabilityBinding {
                    // Keep the fixture's physical symbol intentionally
                    // distinct from every AgentImage binding key.  The
                    // runtime must always consume the resolved deployment
                    // symbol, never infer a tool name from a capability.
                    mcp_tool_name: format!("fixture_{binding_key}_v3"),
                    binding_key,
                    transport: TransportKind::McpHttp,
                    endpoint_ref: "krw-ontology-test".into(),
                    credential_ref: Some("KRW_TEST_MCP_TOKEN".into()),
                    auth_scope: AuthScope::Tenant,
                    tool_session_reuse: McpToolSessionReuse::RunScoped,
                    server_schema_bundle_hash: schema.clone(),
                    server_build: "fixture-build".into(),
                    data_release_hash: release.clone(),
                    max_connections: 4,
                    request_timeout_ms: 30_000,
                })
                .collect(),
        };
        let registry: ModelRegistry =
            load_yaml(root.join("deployments/local/model-registry.yaml")).expect("model registry");
        let base_budget: BudgetRegistry =
            load_yaml(root.join("deployments/local/budget-registry.yaml"))
                .expect("budget registry");
        let template_limits = base_budget.profiles[0].limits.clone();
        let budget = BudgetRegistry {
            schema_version: 1,
            registry_id: "fixture".into(),
            profiles: image
                .body
                .entrypoints
                .values()
                .map(|entrypoint| entrypoint.required_budget_profile.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .map(|profile_id| BudgetProfile {
                    profile_id,
                    limits: template_limits.clone(),
                })
                .collect(),
        };
        let endpoints = EndpointRegistry {
            schema_version: 1,
            registry_id: "fixture".into(),
            endpoints: vec![EndpointDescriptor {
                endpoint_ref: "krw-ontology-test".into(),
                url_env: "KRW_TEST_MCP_URL".into(),
                readiness_url_env: "KRW_TEST_MCP_READY_URL".into(),
                protocol_version: "2025-06-18".into(),
                origin: "https://agent.test".into(),
                credential_version: "test-v1".into(),
                tls_profile: "system-roots-v1".into(),
                tls_ca_pem_env: None,
            }],
        };
        let secrets = FixtureSecrets(BTreeMap::from([
            ("GLM_API_KEY".into(), "glm-secret".into()),
            (
                "KRW_TEST_MCP_URL".into(),
                "https://ontology.test/mcp".into(),
            ),
            (
                "KRW_TEST_MCP_READY_URL".into(),
                "https://ontology.test/readyz".into(),
            ),
            ("KRW_TEST_MCP_TOKEN".into(), "mcp-secret".into()),
        ]));
        let runtime = resolve_runtime(
            image,
            &deployment,
            &registry,
            &budget,
            &endpoints,
            &secrets,
            ValidationMode::Fixture,
        )
        .expect("resolved fixture runtime");
        Arc::new(runtime)
    }

    fn front_vector(contract_id: &str) -> Value {
        let vectors: Value = serde_json::from_slice(
            &fs::read(root().join("contracts/krw-front/v1/conformance-vectors.json"))
                .expect("front conformance vectors"),
        )
        .expect("front conformance JSON");
        vectors["vectors"]
            .as_array()
            .expect("front vector array")
            .iter()
            .find(|vector| {
                vector["contract_id"] == contract_id
                    && vector["name"] == "minimal_valid"
                    && vector["valid"] == true
            })
            .expect("minimal valid front vector")["value"]
            .clone()
    }

    fn guru_vector(contract_id: &str) -> Value {
        let vectors: Value = serde_json::from_slice(
            &fs::read(root().join("contracts/krw-guru/v1/conformance-vectors.json"))
                .expect("Guru conformance vectors"),
        )
        .expect("Guru conformance JSON");
        vectors["vectors"]
            .as_array()
            .expect("Guru vector array")
            .iter()
            .find(|vector| {
                vector["contract_id"] == contract_id
                    && vector["name"] == "minimal_valid"
                    && vector["valid"] == true
            })
            .expect("minimal valid Guru vector")["value"]
            .clone()
    }

    fn scope() -> RunScope {
        RunScope {
            tenant_id: "tenant-a".into(),
            principal_id: "principal-a".into(),
            run_id: "run-a".into(),
        }
    }

    fn fixture_plan_and_state() -> (Value, Value) {
        let state: Value = serde_json::from_slice(
            &fs::read(root().join("fixtures/vertical-slice/v1/mcp/research-state-answerable.json"))
                .expect("ResearchState fixture"),
        )
        .expect("ResearchState JSON");
        (state["plan"].clone(), state)
    }

    fn fixture_correction() -> Value {
        let vectors: Value = serde_json::from_slice(
            &fs::read(root().join("contracts/krw-ontology/v2/conformance-vectors.json"))
                .expect("conformance vectors"),
        )
        .expect("conformance JSON");
        vectors["vectors"]
            .as_array()
            .expect("vector array")
            .iter()
            .find(|vector| {
                vector["contract_id"] == QUERY_CONTEXT_INPUT_CORRECTION_V1
                    && vector["expected"]["accepted"] == true
            })
            .expect("accepted correction vector")["expected"]["normalized"]
            .clone()
    }

    fn invocation(
        catalog: &CapabilityCatalog,
        capability_id: &str,
        arguments: Value,
    ) -> CapabilityInvocation {
        let descriptor = catalog
            .descriptors
            .get(capability_id)
            .expect("capability descriptor");
        let resolved = catalog
            .runtime
            .capabilities
            .get(capability_id)
            .expect("resolved capability");
        let request_hash = ContentHash::sha256(
            serde_jcs::to_vec(&arguments).expect("canonical invocation arguments"),
        );
        let action_key = deterministic_action_key(
            "run-a",
            &catalog.image_hash,
            &descriptor.specification,
            &descriptor.contracts,
            &resolved.binding,
            &arguments,
        )
        .expect("deterministic action key");
        CapabilityInvocation {
            run_id: "run-a".into(),
            action_key,
            capability_id: capability_id.into(),
            request_hash,
            input_schema_hash: descriptor.contracts.input.content_hash.clone(),
            output_schema_hash: descriptor.contracts.output_contract_set_hash.clone(),
            normalized_output_contract_hash: descriptor.normalized_output_contract_hash.clone(),
            arguments,
            binding: resolved.binding.clone(),
        }
    }

    fn envelope(payload: &Value, is_error: bool) -> Value {
        serde_json::json!({
            "content": [{
                "type": "text",
                "text": serde_jcs::to_string(payload).expect("canonical payload")
            }],
            "structuredContent": payload,
            "isError": is_error
        })
    }

    fn presentation_pack() -> Value {
        serde_json::json!({
            "schema_version": 2,
            "mode": "chart_series_sidecar",
            "series": [{
                "series_key": "AAPL:revenue",
                "label": "Revenue",
                "ticker": "AAPL",
                "canonical_metric": "revenue",
                "unit": "USD_millions",
                "scope": {"kind": "company_total", "key": "AAPL", "label": "Apple"},
                "basis": "consolidated",
                "duration": "fy",
                "source_class": "income_statement",
                "statement_family": "income_statement",
                "period_type": "annual",
                "points": [
                    {"period": "FY2024", "value": 391.0, "formatted_value": "391.0", "object_id": "obj-a"},
                    {"period": "FY2025", "value": 416.2, "formatted_value": "416.2", "object_id": "obj-b"}
                ]
            }]
        })
    }

    fn envelope_with_meta(payload: &Value, meta: Value) -> Value {
        let mut result = envelope(payload, false);
        result["_meta"] = meta;
        result
    }

    #[tokio::test]
    async fn presentation_meta_pack_is_captured_into_the_run_ledger() {
        let (image, resolved) = fixture_image_and_runtime();
        let catalog = CapabilityCatalog::compile(&image, resolved).expect("catalog");
        let (plan, state) = fixture_plan_and_state();
        let pack = presentation_pack();
        let transport = FakeTransport::new([
            envelope_with_meta(&state, serde_json::json!({
                "com.krwontology/presentationSeries": pack
            })),
            envelope(&state, false),
        ]);
        let runtime =
            PooledMcpCapabilityRuntime::for_run(Arc::clone(&catalog), transport, scope())
                .expect("run runtime");

        let first = runtime
            .invoke(&invocation(&catalog, "ontology.query_context", plan.clone()))
            .await
            .expect("query with presentation pack");
        assert!(!first.evidence.is_empty(), "evidence mapping is unchanged");
        let second = runtime
            .invoke(&invocation(&catalog, "ontology.query_context", plan))
            .await
            .expect("query without presentation pack");
        assert!(!second.evidence.is_empty());

        let packs = runtime.presentation_packs();
        assert_eq!(packs.len(), 1, "only the _meta pack is retained");
        assert_eq!(packs[0]["schema_version"], 2);
        assert_eq!(packs[0]["series"][0]["basis"], "consolidated");
    }

    #[tokio::test]
    async fn malformed_presentation_meta_is_omitted_without_failing_capability() {
        let (image, resolved) = fixture_image_and_runtime();
        let catalog = CapabilityCatalog::compile(&image, resolved).expect("catalog");
        let (plan, state) = fixture_plan_and_state();
        let mut bad_version = presentation_pack();
        bad_version["schema_version"] = serde_json::json!(1);
        let transport = FakeTransport::new([envelope_with_meta(
            &state,
            serde_json::json!({"com.krwontology/presentationSeries": bad_version}),
        )]);
        let runtime =
            PooledMcpCapabilityRuntime::for_run(Arc::clone(&catalog), transport, scope())
                .expect("run runtime");

        let result = runtime
            .invoke(&invocation(&catalog, "ontology.query_context", plan))
            .await
            .expect("primary research result remains valid");
        assert!(!result.evidence.is_empty());
        assert!(runtime.presentation_packs().is_empty());
    }

    #[test]
    fn unknown_meta_vendor_keys_are_ignored() {
        let payload = serde_json::json!({"ok": true});
        let envelope = envelope_with_meta(
            &payload,
            serde_json::json!({"com.other/vendor": {"anything": true}}),
        );
        let extracted = extract_json_tool_payload(envelope, false).expect("unknown keys ignored");
        assert_eq!(extracted.payload, payload);
        assert!(extracted.presentation.is_none());
    }

    #[test]
    fn oversized_presentation_pack_is_omitted_at_extraction() {
        let payload = serde_json::json!({"ok": true});
        let mut oversized = presentation_pack();
        oversized["series"] = serde_json::json!(
            (0..9).map(|index| serde_json::json!({"series_key": index})).collect::<Vec<_>>()
        );
        let extracted = extract_json_tool_payload(
            envelope_with_meta(
                &payload,
                serde_json::json!({"com.krwontology/presentationSeries": oversized}),
            ),
            false,
        )
        .expect("primary MCP payload remains valid");
        assert_eq!(extracted.payload, payload);
        assert!(extracted.presentation.is_none());
    }

    #[test]
    fn markup_shaped_presentation_pack_is_omitted_without_affecting_payload() {
        let payload = serde_json::json!({"ok": true});
        let mut markup = presentation_pack();
        markup["html"] = serde_json::json!("<html><script>bad()</script></html>");
        let extracted = extract_json_tool_payload(
            envelope_with_meta(
                &payload,
                serde_json::json!({"com.krwontology/presentationSeries": markup}),
            ),
            false,
        )
        .expect("primary MCP payload remains valid");
        assert_eq!(extracted.payload, payload);
        assert!(extracted.presentation.is_none());
    }


    #[test]
    fn front_and_guru_agents_compile_into_the_closed_production_catalog() {
        for directory in [
            "agents/krw-feed",
            "agents/krw-source-filing",
            "agents/krw-guru-advisor",
        ] {
            let image = compile_agent_dir(root().join(directory))
                .expect("compile closed-mapping AgentImage")
                .manifest;
            let runtime = fixture_runtime(&image);
            let catalog = CapabilityCatalog::compile(&image, runtime)
                .expect("image must use only closed mappings");
            assert_eq!(catalog.capability_count(), image.body.capabilities.len());
        }
    }

    #[test]
    fn market_snapshot_preflight_is_closed_to_the_pinned_market_mapping() {
        let (image, runtime) = fixture_image_and_runtime();
        let catalog = CapabilityCatalog::compile(&image, runtime).expect("ontology catalog");

        let first = catalog
            .market_snapshot_preflight_invocation("run-a", "AAPL")
            .expect("closed preflight invocation")
            .expect("ontology image declares one market snapshot capability");
        let second = catalog
            .market_snapshot_preflight_invocation("run-a", "AAPL")
            .expect("deterministic preflight invocation")
            .expect("market preflight invocation");

        assert_eq!(first.capability_id, "market.snapshot");
        assert_eq!(first.arguments, serde_json::json!({"ticker": "AAPL"}));
        validate_value(MARKET_SNAPSHOT_REQUEST_V1, &first.arguments)
            .expect("preflight arguments use the canonical market contract");
        assert_eq!(first.action_key, second.action_key);
        assert_eq!(first.request_hash, second.request_hash);
        assert_eq!(
            first.binding, catalog.runtime.capabilities["market.snapshot"].binding,
            "the preflight must use the image-pinned resolved binding"
        );
        assert!(
            catalog
                .market_snapshot_preflight_invocation("run-a", "not-a-canonical-ticker")
                .is_err()
        );
    }

    #[test]
    fn guru_catalog_accepts_an_ordinary_research_role_without_child_abi() {
        let image = compile_agent_dir(root().join("agents/krw-guru-advisor"))
            .expect("compile Guru AgentImage")
            .manifest;
        let role = image
            .body
            .roles
            .iter()
            .find(|role| role.id == "company_evidence_researcher")
            .expect("company evidence role");
        assert!(role.bounded_child.is_none());
        let runtime = fixture_runtime(&image);
        CapabilityCatalog::compile(&image, runtime)
            .expect("ordinary Guru role must compile without a child ABI");
    }

    #[tokio::test]
    async fn guru_runtime_requires_a_committed_seal_and_rechecks_committed_projection() {
        let image = compile_agent_dir(root().join("agents/krw-guru-advisor"))
            .expect("compile Guru AgentImage")
            .manifest;
        let resolved = fixture_runtime(&image);
        let catalog = CapabilityCatalog::compile(&image, resolved).expect("Guru catalog");
        let query_input = guru_vector(KRW_GURU_QUERY_CONTEXT_INPUT_V1);
        let query_result = guru_vector(KRW_GURU_QUERY_CONTEXT_RESULT_V1);
        let transport = FakeTransport::new([envelope(&query_result, false)]);
        let runtime =
            PooledMcpCapabilityRuntime::for_run(Arc::clone(&catalog), transport.clone(), scope())
                .expect("Guru run runtime");
        let expected_guru_tool = catalog
            .runtime
            .capabilities
            .get("guru.query_context")
            .expect("resolved Guru binding")
            .binding
            .mcp_tool_name
            .clone();

        let (plan, _) = fixture_plan_and_state();
        assert_eq!(
            runtime
                .invoke(&invocation(&catalog, "ontology.query_context", plan))
                .await
                .expect_err("filing research before sealed brief")
                .code,
            "guru_sealed_brief_required"
        );
        assert_eq!(transport.calls.load(Ordering::Relaxed), 0);

        let guru_invocation = invocation(&catalog, "guru.query_context", query_input);
        let result = runtime
            .invoke(&guru_invocation)
            .await
            .expect("validated Guru retrieval");
        assert!(result.evidence.is_empty());
        assert_eq!(result.answerability, None);
        assert_eq!(
            transport.last_tool.lock().expect("last tool").as_deref(),
            Some(expected_guru_tool.as_str())
        );

        let mut forged = result.clone();
        forged.answerability = Some(Answerability::StrongAllowed);
        assert_eq!(
            runtime
                .restore_committed_result(&guru_invocation, &forged)
                .await
                .expect_err("forged normalized Guru projection")
                .code,
            "guru_committed_projection_mismatch"
        );
        runtime
            .restore_committed_result(&guru_invocation, &result)
            .await
            .expect("commit canonical Guru projection");
        assert_eq!(
            runtime
                .invoke(&guru_invocation)
                .await
                .expect_err("Guru retrieval may commit only once")
                .code,
            "guru_query_already_committed"
        );
        assert_eq!(transport.calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn guru_brief_transport_receives_the_exact_kernel_sealed_envelope() {
        let image = compile_agent_dir(root().join("agents/krw-guru-advisor"))
            .expect("compile Guru AgentImage")
            .manifest;
        let resolved = fixture_runtime(&image);
        let catalog = CapabilityCatalog::compile(&image, resolved).expect("Guru catalog");
        let query_input = guru_vector(KRW_GURU_QUERY_CONTEXT_INPUT_V1);
        let query_payload = guru_vector(KRW_GURU_QUERY_CONTEXT_RESULT_V1);
        let brief_payload = guru_vector(KRW_GURU_COMPANY_BRIEF_RESULT_V1);
        let brief_input = guru_vector(KRW_GURU_COMPANY_BRIEF_INPUT_V1);
        let transport = FakeTransport::new([
            envelope(&query_payload, false),
            envelope(&brief_payload, false),
        ]);
        let runtime =
            PooledMcpCapabilityRuntime::for_run(Arc::clone(&catalog), transport.clone(), scope())
                .expect("Guru run runtime");

        let query_invocation = invocation(&catalog, "guru.query_context", query_input);
        let query_result = runtime.invoke(&query_invocation).await.expect("Guru query");
        runtime
            .restore_committed_result(&query_invocation, &query_result)
            .await
            .expect("commit Guru query");

        let brief_invocation = invocation(&catalog, "guru.company_brief", brief_input.clone());
        let brief_result = runtime
            .invoke(&brief_invocation)
            .await
            .expect("sealed brief");
        let physical = transport
            .last_arguments
            .lock()
            .expect("last arguments")
            .clone()
            .expect("physical brief arguments");
        validate_value(
            krw_agent_contracts::KRW_GURU_COMPANY_BRIEF_INPUT_V1,
            &physical,
        )
        .expect("physical brief ABI");
        assert_eq!(
            physical["investigation_questions"],
            brief_input["investigation_questions"]
        );
        assert_eq!(physical["author_keys"], serde_json::json!(["buffett"]));
        assert_eq!(physical["ticker"], "AAPL");
        assert!(physical.get("research_pack").is_none());
        assert_eq!(physical, brief_input);
        runtime
            .restore_committed_result(&brief_invocation, &brief_result)
            .await
            .expect("commit sealed brief");
        assert_eq!(transport.calls.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn feed_is_only_qualified_related_evidence_and_error_envelopes_fail_closed() {
        let image = compile_agent_dir(root().join("agents/krw-feed"))
            .expect("compile feed AgentImage")
            .manifest;
        let resolved = fixture_runtime(&image);
        let catalog = CapabilityCatalog::compile(&image, resolved).expect("feed catalog");
        let mut input = front_vector(KRW_FEED_LIST_ITEMS_INPUT_V1);
        input
            .as_object_mut()
            .expect("feed input object")
            .remove("tickers");
        let output = front_vector(KRW_FEED_LIST_ITEMS_RESULT_V1);
        let transport = FakeTransport::new([envelope(&output, false), envelope(&output, true)]);
        let runtime =
            PooledMcpCapabilityRuntime::for_run(Arc::clone(&catalog), transport.clone(), scope())
                .expect("run runtime");
        let feed_invocation = invocation(&catalog, "feed.list_items", input.clone());
        let result = runtime
            .invoke(&feed_invocation)
            .await
            .expect("canonical feed mapping");
        assert_eq!(result.answerability, Some(Answerability::QualifiedOnly));
        assert!(!result.evidence.is_empty());
        assert!(result.evidence.iter().all(|record| {
            !record.strong_claim_allowed
                && record.directness <= Directness::Related
                && record.grade <= EvidenceGrade::Medium
        }));
        runtime
            .restore_committed_result(&feed_invocation, &result)
            .await
            .expect("committed feed projection");

        let error = runtime
            .invoke(&invocation(&catalog, "feed.list_items", input))
            .await
            .expect_err("MCP error envelope must never be a front success");
        assert_eq!(error.code, "untyped_tool_error");
        assert_eq!(transport.calls.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn filing_direct_evidence_requires_committed_identity_and_list_membership() {
        let image = compile_agent_dir(root().join("agents/krw-source-filing"))
            .expect("compile source-filing AgentImage")
            .manifest;
        let resolved = fixture_runtime(&image);
        let catalog = CapabilityCatalog::compile(&image, resolved).expect("filing catalog");
        let metadata = front_vector(KRW_FILING_METADATA_V1);
        let sections = front_vector(KRW_FILING_SECTIONS_RESULT_V1);
        let read = front_vector(KRW_FILING_READ_SECTION_RESULT_V1);
        let transport = FakeTransport::new([
            envelope(&metadata, false),
            envelope(&sections, false),
            envelope(&read, false),
        ]);
        let runtime =
            PooledMcpCapabilityRuntime::for_run(Arc::clone(&catalog), transport.clone(), scope())
                .expect("run runtime");

        let get_invocation = invocation(
            &catalog,
            "filing.get",
            front_vector(KRW_FILING_GET_INPUT_V1),
        );
        let get_result = runtime
            .invoke(&get_invocation)
            .await
            .expect("verified metadata");
        assert!(get_result.evidence.is_empty());
        assert_eq!(get_result.answerability, None);
        runtime
            .restore_committed_result(&get_invocation, &get_result)
            .await
            .expect("commit metadata identity");

        let read_invocation = invocation(
            &catalog,
            "filing.read_section",
            front_vector(KRW_FILING_READ_SECTION_INPUT_V1),
        );
        assert_eq!(
            runtime
                .invoke(&read_invocation)
                .await
                .expect_err("unlisted section must fail before dispatch")
                .code,
            "filing_section_not_authorized"
        );
        assert_eq!(transport.calls.load(Ordering::Relaxed), 1);

        let list_invocation = invocation(
            &catalog,
            "filing.list_sections",
            front_vector(KRW_FILING_SECTIONS_INPUT_V1),
        );
        let list_result = runtime
            .invoke(&list_invocation)
            .await
            .expect("canonical section list");
        assert!(list_result.evidence.is_empty());
        runtime
            .restore_committed_result(&list_invocation, &list_result)
            .await
            .expect("commit section membership");

        let read_result = runtime
            .invoke(&read_invocation)
            .await
            .expect("authorized direct section read");
        assert_eq!(
            read_result.answerability,
            Some(Answerability::StrongAllowed)
        );
        assert!(!read_result.evidence.is_empty());
        assert!(read_result.evidence.iter().all(|record| {
            record.directness == Directness::Direct
                && record.grade == EvidenceGrade::Strong
                && record.strong_claim_allowed
        }));

        let mut forged = read_result.clone();
        forged.evidence[0].directness = Directness::Related;
        assert_eq!(
            runtime
                .restore_committed_result(&read_invocation, &forged)
                .await
                .expect_err("committed projection must exactly match invoked projection")
                .code,
            "front_committed_projection_mismatch"
        );
        assert_eq!(transport.calls.load(Ordering::Relaxed), 3);
    }

    #[tokio::test]
    async fn research_state_runs_through_frozen_binding_and_evidence_mapping() {
        let (image, resolved) = fixture_image_and_runtime();
        let catalog = CapabilityCatalog::compile(&image, resolved).expect("catalog");
        let (plan, state) = fixture_plan_and_state();
        let transport = FakeTransport::new([envelope(&state, false)]);
        let runtime =
            PooledMcpCapabilityRuntime::for_run(Arc::clone(&catalog), transport.clone(), scope())
                .expect("run runtime");
        let expected_context_tool = catalog
            .runtime
            .capabilities
            .get("ontology.query_context")
            .expect("resolved context binding")
            .binding
            .mcp_tool_name
            .clone();
        let result = runtime
            .invoke(&invocation(
                &catalog,
                "ontology.query_context",
                plan.clone(),
            ))
            .await
            .expect("mapped capability result");

        assert_eq!(transport.calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            transport.last_tool.lock().expect("last tool").as_deref(),
            Some(expected_context_tool.as_str())
        );
        assert_eq!(
            transport
                .last_arguments
                .lock()
                .expect("last arguments")
                .as_ref(),
            Some(&plan)
        );
        assert_eq!(result.provider_content, state);
        assert!(!result.evidence.is_empty());
        assert_eq!(result.answerability, Some(Answerability::StrongAllowed));
        assert!(result.evidence.iter().all(|record| {
            record.source.capability_id == "ontology.query_context"
                && record.scope.auth_scope == AuthScope::Tenant
        }));
        let mut ledger = EvidenceLedger::from_records(result.evidence.clone())
            .expect("normalized evidence must satisfy ledger ABI");
        ledger
            .extend_calculations(result.calculations.clone())
            .expect("normalized calculations must satisfy ledger ABI");
    }

    #[tokio::test]
    async fn foreign_company_research_state_is_withheld_but_the_run_can_continue() {
        let (image, resolved) = fixture_image_and_runtime();
        let catalog = CapabilityCatalog::compile(&image, resolved).expect("catalog");
        let (plan, mut state) = fixture_plan_and_state();
        state["evidence_units"][0]["ticker"] = Value::String("MSFT".into());
        state["clause_coverage"][0]["covered_tickers"] = serde_json::json!(["MSFT"]);
        state["clause_coverage"][0]["missing_tickers"] = serde_json::json!([]);
        let transport = FakeTransport::new([envelope(&state, false)]);
        let runtime = PooledMcpCapabilityRuntime::for_run(Arc::clone(&catalog), transport, scope())
            .expect("run runtime");

        let result = runtime
            .invoke(&invocation(&catalog, "ontology.query_context", plan))
            .await
            .expect("foreign evidence is a partial result, not a terminal failure");

        assert!(result.evidence.is_empty());
        assert_eq!(result.answerability, Some(Answerability::NotAnswerable));
        assert!(
            result
                .provider_content
                .get("warnings")
                .and_then(Value::as_array)
                .is_some_and(|warnings| warnings
                    .iter()
                    .any(|warning| { warning.as_str() == Some("out_of_scope_evidence_withheld") }))
        );
    }

    #[tokio::test]
    async fn mismatched_dual_payload_fails_closed_without_evidence() {
        let (image, resolved) = fixture_image_and_runtime();
        let catalog = CapabilityCatalog::compile(&image, resolved).expect("catalog");
        let (plan, state) = fixture_plan_and_state();
        let mut result = envelope(&state, false);
        result["structuredContent"]["release_id"] = Value::String("tampered".into());
        let transport = FakeTransport::new([result]);
        let runtime = PooledMcpCapabilityRuntime::for_run(Arc::clone(&catalog), transport, scope())
            .expect("run runtime");
        let failure = runtime
            .invoke(&invocation(&catalog, "ontology.query_context", plan))
            .await
            .expect_err("dual payload mismatch must fail");
        assert_eq!(failure.code, "mcp_dual_payload_mismatch");
        assert_eq!(failure.delivery, DeliveryCertainty::NotDispatched);
    }

    #[tokio::test]
    async fn wrapped_structured_result_is_accepted_only_when_semantically_identical() {
        let (image, resolved) = fixture_image_and_runtime();
        let catalog = CapabilityCatalog::compile(&image, resolved).expect("catalog");
        let (plan, state) = fixture_plan_and_state();
        let mut result = envelope(&state, false);
        result["structuredContent"] = serde_json::json!({
            "result": serde_jcs::to_string(&state).expect("canonical payload")
        });
        let transport = FakeTransport::new([result]);
        let runtime = PooledMcpCapabilityRuntime::for_run(Arc::clone(&catalog), transport, scope())
            .expect("run runtime");

        let result = runtime
            .invoke(&invocation(&catalog, "ontology.query_context", plan))
            .await
            .expect("equivalent wrapped structured payload");
        assert_eq!(result.provider_content, state);
    }

    #[tokio::test]
    async fn typed_tool_error_is_provider_control_data_and_never_evidence() {
        let (image, resolved) = fixture_image_and_runtime();
        let catalog = CapabilityCatalog::compile(&image, resolved).expect("catalog");
        let (plan, _) = fixture_plan_and_state();
        let correction = fixture_correction();
        let transport = FakeTransport::new([envelope(&correction, true)]);
        let runtime =
            PooledMcpCapabilityRuntime::for_run(Arc::clone(&catalog), transport.clone(), scope())
                .expect("run runtime");

        let result = runtime
            .invoke(&invocation(&catalog, "ontology.query_context", plan))
            .await
            .expect("typed correction");
        assert_eq!(result.provider_content, correction);
        assert!(result.evidence.is_empty());
        assert!(result.calculations.is_empty());
        assert_eq!(result.answerability, None);
        assert_eq!(transport.calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn targeted_query_passes_exact_arguments_but_never_grants_strong_claims() {
        let (image, resolved) = fixture_image_and_runtime();
        let catalog = CapabilityCatalog::compile(&image, resolved).expect("catalog");
        let arguments = serde_json::json!({
            "ticker": "VG",
            "topic": "cash generation",
            "limit": 1,
            "response_format": "json",
            "response_detail": "compact"
        });
        let payload = serde_json::json!({
            "results": [{
                "id": "claim:VG:1",
                "type": "ResearchClaim",
                "ticker": "VG",
                "document_type": "10-K",
                "period": "CY2025",
                "section": "MD&A",
                "text": "Cash generation improved.",
                "quality": {"evidence_grade": "strong"},
                "evidence": {
                    "quotes": [{"text": "Operating cash flow increased."}],
                    "spans": [],
                    "metric_lineage": null
                }
            }]
        });
        let text_only = serde_json::json!({
            "content": [{
                "type": "text",
                "text": serde_jcs::to_string(&payload).expect("canonical payload")
            }],
            "isError": false
        });
        let transport = FakeTransport::new([text_only]);
        let runtime =
            PooledMcpCapabilityRuntime::for_run(Arc::clone(&catalog), transport.clone(), scope())
                .expect("run runtime");
        let result = runtime
            .invoke(&invocation(&catalog, "ontology.query", arguments.clone()))
            .await
            .expect("targeted query result");

        assert_eq!(
            transport
                .last_arguments
                .lock()
                .expect("last arguments")
                .as_ref(),
            Some(&arguments)
        );
        assert_eq!(result.evidence.len(), 1);
        assert!(
            result
                .evidence
                .iter()
                .all(|record| !record.strong_claim_allowed)
        );
        assert_eq!(result.answerability, None);
    }

    #[tokio::test]
    async fn invocation_hash_binding_and_run_scope_are_rechecked_before_transport() {
        let (image, resolved) = fixture_image_and_runtime();
        let catalog = CapabilityCatalog::compile(&image, resolved).expect("catalog");
        let (plan, state) = fixture_plan_and_state();
        let transport = FakeTransport::new([
            envelope(&state, false),
            envelope(&state, false),
            envelope(&state, false),
        ]);
        let runtime =
            PooledMcpCapabilityRuntime::for_run(Arc::clone(&catalog), transport.clone(), scope())
                .expect("run runtime");

        let mut wrong_hash = invocation(&catalog, "ontology.query_context", plan.clone());
        wrong_hash.request_hash = ContentHash::sha256("wrong");
        assert_eq!(
            runtime
                .invoke(&wrong_hash)
                .await
                .expect_err("wrong hash")
                .code,
            "request_hash_mismatch"
        );

        let mut wrong_binding = invocation(&catalog, "ontology.query_context", plan.clone());
        wrong_binding.binding.server_build = "wrong".into();
        assert_eq!(
            runtime
                .invoke(&wrong_binding)
                .await
                .expect_err("wrong binding")
                .code,
            "invocation_contract_mismatch"
        );

        let mut wrong_run = invocation(&catalog, "ontology.query_context", plan);
        wrong_run.run_id = "run-b".into();
        assert_eq!(
            runtime
                .invoke(&wrong_run)
                .await
                .expect_err("wrong run")
                .code,
            "run_scope_mismatch"
        );
        assert_eq!(transport.calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn catalog_rejects_typed_result_ingest_that_conflicts_with_its_contract() {
        let (mut image, runtime) = fixture_image_and_runtime();
        image.body.capabilities[0].result_ingest = CapabilityResultIngest::TraceLineageV1;
        image.content_hash =
            ContentHash::sha256(serde_jcs::to_vec(&image.body).expect("canonical modified image"));
        assert!(matches!(
            CapabilityCatalog::compile(&image, runtime),
            Err(CatalogError::IncompatibleCapability(_))
        ));
    }

    #[test]
    fn envelope_requires_one_json_text_item_and_exact_known_fields() {
        let payload = serde_json::json!({"ok": true});
        let unknown = serde_json::json!({
            "content": [{"type": "text", "text": "{\"ok\":true}"}],
            "structuredContent": payload,
            "isError": false,
            "extension": "not-pinned"
        });
        assert_eq!(
            extract_json_tool_payload(unknown, false)
                .map(|_| ())
                .expect_err("unknown field")
                .code,
            "mcp_envelope_unknown_field"
        );
        let two_items = serde_json::json!({
            "content": [
                {"type": "text", "text": "{\"ok\":true}"},
                {"type": "text", "text": "{\"ok\":true}"}
            ],
            "isError": false
        });
        assert_eq!(
            extract_json_tool_payload(two_items, false)
                .map(|_| ())
                .expect_err("multiple items")
                .code,
            "mcp_envelope_content_count"
        );
    }

    #[test]
    fn runtime_debug_never_contains_scope_or_deployment_secrets() {
        let (image, resolved) = fixture_image_and_runtime();
        let catalog = CapabilityCatalog::compile(&image, resolved).expect("catalog");
        let transport = FakeTransport::new([]);
        let runtime = PooledMcpCapabilityRuntime::for_run(
            catalog,
            transport,
            RunScope {
                tenant_id: "tenant-secret".into(),
                principal_id: "principal-secret".into(),
                run_id: "run-secret".into(),
            },
        )
        .expect("run runtime");
        let debug = format!("{runtime:?}");
        for secret in [
            "tenant-secret",
            "principal-secret",
            "run-secret",
            "mcp-secret",
            "ontology.test",
        ] {
            assert!(!debug.contains(secret));
        }
    }
}
