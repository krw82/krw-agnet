use std::fmt;

use krw_agent_image::LoadedImage;
use krw_agent_persistence::agent_v1::ClaimReceipt;
use krw_agent_protocol::{
    ALLOWED_MODEL_IDS, BudgetLimits, CLAIM_PAYLOAD_SCHEMA_VERSION, ContentHash, PROTOCOL_VERSION,
    PinnedExecutionContract, ResolvedExecutionSnapshot, RunRequest,
};
use krw_agent_runtime_config::{ConfigError, ResolvedRuntime};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_CLAIM_BYTES: usize = 1024 * 1024;

fn is_allowed_model(model_id: &str) -> bool {
    ALLOWED_MODEL_IDS.contains(&model_id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkloadClass {
    ReadOnlyInteractive,
}

/// The scheduler profile is intentionally a closed enum. Arbitrary per-run
/// CPU, memory, or concurrency knobs would let an untrusted host bypass the
/// daemon's machine-wide admission bounds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunResourceProfileV1 {
    pub schema_version: u16,
    pub workload_class: WorkloadClass,
}

impl Default for RunResourceProfileV1 {
    fn default() -> Self {
        Self {
            schema_version: CLAIM_PAYLOAD_SCHEMA_VERSION,
            workload_class: WorkloadClass::ReadOnlyInteractive,
        }
    }
}

/// Canonical enqueue payload. This is the only host-owned JSON that can become
/// a [`RunRequest`]; provider messages and tool definitions are not fields.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImmutableRunClaimV1 {
    pub schema_version: u16,
    pub request: RunRequest,
    pub execution: PinnedExecutionContract,
    pub resource_profile: RunResourceProfileV1,
}

impl fmt::Debug for ImmutableRunClaimV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImmutableRunClaimV1")
            .field("schema_version", &self.schema_version)
            .field("run_id_hash", &ContentHash::sha256(&self.request.run_id))
            .field(
                "tenant_id_hash",
                &ContentHash::sha256(&self.request.tenant_id),
            )
            .field(
                "principal_id_hash",
                &ContentHash::sha256(&self.request.principal_id),
            )
            .field("question", &"[REDACTED]")
            .field("execution", &self.execution)
            .field("resource_profile", &self.resource_profile)
            .finish()
    }
}

impl ImmutableRunClaimV1 {
    pub fn new(
        request: RunRequest,
        snapshot: &ResolvedExecutionSnapshot,
        resource_profile: RunResourceProfileV1,
    ) -> Result<Self, ClaimValidationError> {
        if snapshot.run_id != request.run_id || snapshot.budget != request.budget {
            return Err(ClaimValidationError::SnapshotMismatch);
        }
        let value = Self {
            schema_version: CLAIM_PAYLOAD_SCHEMA_VERSION,
            request,
            execution: snapshot.into(),
            resource_profile,
        };
        value.validate_shape()?;
        Ok(value)
    }

    pub fn canonical_hash(&self) -> Result<ContentHash, ClaimValidationError> {
        let bytes = serde_jcs::to_vec(self).map_err(|_| ClaimValidationError::InvalidEncoding)?;
        if bytes.is_empty() || bytes.len() > MAX_CLAIM_BYTES {
            return Err(ClaimValidationError::PayloadSize);
        }
        Ok(ContentHash::sha256(bytes))
    }

    fn validate_shape(&self) -> Result<(), ClaimValidationError> {
        if self.schema_version != CLAIM_PAYLOAD_SCHEMA_VERSION
            || self.resource_profile.schema_version != CLAIM_PAYLOAD_SCHEMA_VERSION
            || self.execution.protocol_version != PROTOCOL_VERSION
            || !is_allowed_model(&self.request.requested_model)
            || !is_allowed_model(&self.execution.requested_model)
            || !is_allowed_model(&self.execution.resolved_model)
            || self.execution.model_profile != self.request.model_profile
            || self.execution.requested_model != self.request.requested_model
            || self.execution.resolved_model != self.request.requested_model
            || self.execution.budget != self.request.budget
            // Session memory is resolved by the fenced worker from the
            // append-only durable log. Accepting a host-supplied carrier here
            // would let an enqueue caller inject unverified prior context.
            || self.request.session_memory.is_some()
        {
            return Err(ClaimValidationError::SnapshotMismatch);
        }
        Ok(())
    }
}

impl Drop for ImmutableRunClaimV1 {
    fn drop(&mut self) {
        self.request.zeroize_sensitive();
    }
}

pub struct ValidatedClaim {
    request: RunRequest,
    snapshot: ResolvedExecutionSnapshot,
    resource_profile: RunResourceProfileV1,
}

impl ValidatedClaim {
    pub fn request(&self) -> &RunRequest {
        &self.request
    }

    pub fn snapshot(&self) -> &ResolvedExecutionSnapshot {
        &self.snapshot
    }

    pub const fn resource_profile(&self) -> &RunResourceProfileV1 {
        &self.resource_profile
    }
}

impl fmt::Debug for ValidatedClaim {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ValidatedClaim")
            .field("run_id_hash", &ContentHash::sha256(&self.request.run_id))
            .field("question", &"[REDACTED]")
            .field("snapshot", &self.snapshot)
            .field("resource_profile", &self.resource_profile)
            .finish()
    }
}

impl Drop for ValidatedClaim {
    fn drop(&mut self) {
        self.request.zeroize_sensitive();
    }
}

#[derive(Debug, Error)]
pub enum ClaimValidationError {
    #[error("claim payload encoding is invalid")]
    InvalidEncoding,
    #[error("claim payload exceeds its fixed byte bound")]
    PayloadSize,
    #[error("claim payload hash does not match its durable receipt")]
    PayloadHashMismatch,
    #[error("claim identity does not match its immutable request")]
    IdentityMismatch,
    #[error("claim agent image or runtime version is not admitted")]
    DeploymentMismatch,
    #[error("claim budget or resource profile is not the pinned value")]
    ResourceMismatch,
    #[error("claim execution snapshot does not match startup configuration")]
    SnapshotMismatch,
    #[error("claim execution pin differs from startup configuration: {0}")]
    SnapshotContractMismatch(&'static str),
    #[error("claim token budget exceeds the exact model context limit")]
    ModelContextExceeded,
    #[error("runtime snapshot resolution failed: {0}")]
    Runtime(#[from] ConfigError),
}

pub fn validate_claim(
    receipt: &ClaimReceipt,
    image: &LoadedImage,
    runtime: &ResolvedRuntime,
    expected_runtime_version: &str,
) -> Result<ValidatedClaim, ClaimValidationError> {
    let raw = serde_jcs::to_vec(&receipt.immutable_snapshot)
        .map_err(|_| ClaimValidationError::InvalidEncoding)?;
    if raw.is_empty() || raw.len() > MAX_CLAIM_BYTES {
        return Err(ClaimValidationError::PayloadSize);
    }
    if ContentHash::sha256(&raw) != receipt.immutable_snapshot_hash {
        return Err(ClaimValidationError::PayloadHashMismatch);
    }
    let payload: ImmutableRunClaimV1 =
        serde_json::from_slice(&raw).map_err(|_| ClaimValidationError::InvalidEncoding)?;
    payload.validate_shape()?;
    if payload.canonical_hash()? != receipt.immutable_snapshot_hash {
        return Err(ClaimValidationError::PayloadHashMismatch);
    }

    if payload.request.run_id != receipt.run_id
        || payload.request.tenant_id != receipt.tenant_id
        || payload.request.principal_id != receipt.principal_id
        || payload.request.session_id != receipt.session_id
    {
        return Err(ClaimValidationError::IdentityMismatch);
    }
    if receipt.agent_image_hash != image.content_hash
        || payload.execution.agent_image_hash != image.content_hash
        || receipt.runtime_version != expected_runtime_version
    {
        return Err(ClaimValidationError::DeploymentMismatch);
    }

    let durable_budget: BudgetLimits = serde_json::from_value(receipt.budgets.clone())
        .map_err(|_| ClaimValidationError::ResourceMismatch)?;
    let durable_profile: RunResourceProfileV1 =
        serde_json::from_value(receipt.resource_profile.clone())
            .map_err(|_| ClaimValidationError::ResourceMismatch)?;
    if durable_budget != payload.request.budget
        || durable_budget != payload.execution.budget
        || durable_profile != payload.resource_profile
    {
        return Err(ClaimValidationError::ResourceMismatch);
    }

    let snapshot = runtime.resolve_run(
        &image.content_hash,
        &payload.request,
        receipt.fencing_token,
        receipt.cancel_generation,
    )?;
    let expected_execution = PinnedExecutionContract::from(&snapshot);
    if expected_execution != payload.execution {
        return Err(ClaimValidationError::SnapshotContractMismatch(
            pinned_execution_mismatch_field(&expected_execution, &payload.execution),
        ));
    }
    let model = runtime
        .model(&payload.request.requested_model)
        .ok_or(ClaimValidationError::SnapshotMismatch)?;
    let total_token_budget = u64::from(payload.request.budget.max_input_tokens)
        .checked_add(u64::from(payload.request.budget.max_output_tokens))
        .ok_or(ClaimValidationError::ModelContextExceeded)?;
    if !is_allowed_model(&model.model_id)
        || !is_allowed_model(&payload.request.requested_model)
        || model.model_id != payload.request.requested_model
        || payload.request.budget.max_output_tokens > model.max_output_tokens
        || total_token_budget > u64::from(model.max_context_tokens)
    {
        return Err(ClaimValidationError::ModelContextExceeded);
    }

    Ok(ValidatedClaim {
        request: payload.request.clone(),
        snapshot,
        resource_profile: payload.resource_profile.clone(),
    })
}

/// This is deliberately field-name-only: it is useful for diagnosing a
/// cross-language release contract drift without logging user input, secrets,
/// URLs, model transcripts, or the pinned hash values themselves.
fn pinned_execution_mismatch_field(
    expected: &PinnedExecutionContract,
    observed: &PinnedExecutionContract,
) -> &'static str {
    if expected.protocol_version != observed.protocol_version {
        "protocol_version"
    } else if expected.agent_image_hash != observed.agent_image_hash {
        "agent_image_hash"
    } else if expected.deployment_binding_hash != observed.deployment_binding_hash {
        "deployment_binding_hash"
    } else if expected.model_registry_hash != observed.model_registry_hash {
        "model_registry_hash"
    } else if expected.budget_registry_hash != observed.budget_registry_hash {
        "budget_registry_hash"
    } else if expected.model_profile != observed.model_profile {
        "model_profile"
    } else if expected.requested_model != observed.requested_model {
        "requested_model"
    } else if expected.resolved_model != observed.resolved_model {
        "resolved_model"
    } else if expected.provider_api_version != observed.provider_api_version {
        "provider_api_version"
    } else if expected.provider_wire_capabilities != observed.provider_wire_capabilities {
        "provider_wire_capabilities"
    } else if expected.thinking != observed.thinking {
        "thinking"
    } else if expected.reasoning_effort != observed.reasoning_effort {
        "reasoning_effort"
    } else if expected.capability_release_hashes != observed.capability_release_hashes {
        "capability_release_hashes"
    } else if expected.budget != observed.budget {
        "budget"
    } else {
        "unknown"
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use krw_agent_image::compile_agent_dir;
    use krw_agent_persistence::agent_v1::{ClaimReceipt, RecoveryReceipt};
    use krw_agent_protocol::{
        DeploymentBinding, ModelRegistry, RunContextV1, SessionMemoryCarrierV3,
    };
    use krw_agent_runtime_config::{
        BudgetRegistry, ConfigError, EndpointDescriptor, EndpointRegistry, SecretSource,
        ValidationMode, load_yaml, resolve_runtime,
    };
    use zeroize::Zeroizing;

    use super::*;

    #[derive(Debug)]
    struct FixtureSecrets;

    impl SecretSource for FixtureSecrets {
        fn read_secret(&self, name: &str) -> Result<Zeroizing<String>, ConfigError> {
            match name {
                "GLM_API_KEY" => Ok(Zeroizing::new("fixture-glm-key".into())),
                "KRW_ONTOLOGY_MCP_URL" => Ok(Zeroizing::new("https://ontology.invalid/mcp".into())),
                "KRW_ONTOLOGY_READY_URL" => {
                    Ok(Zeroizing::new("https://ontology.invalid/readyz".into()))
                }
                _ => Err(ConfigError::MissingSecret(name.into())),
            }
        }
    }

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn fixture() -> (LoadedImage, ResolvedRuntime, RunRequest) {
        let root = root();
        let image = compile_agent_dir(root.join("agents/krw-ontology-en"))
            .unwrap()
            .into_loaded()
            .unwrap();
        let binding: DeploymentBinding =
            load_yaml(root.join("deployments/local/deployment-binding.krw-ontology.example.yaml"))
                .unwrap();
        let registry: ModelRegistry =
            load_yaml(root.join("deployments/local/model-registry.yaml")).unwrap();
        let mut budget: BudgetRegistry =
            load_yaml(root.join("deployments/local/budget-registry.yaml")).unwrap();
        budget
            .profiles
            .retain(|profile| profile.profile_id == "company_research_glm");
        let endpoints = EndpointRegistry {
            schema_version: 1,
            registry_id: "fixture".into(),
            endpoints: vec![EndpointDescriptor {
                endpoint_ref: "krw-ontology-local".into(),
                url_env: "KRW_ONTOLOGY_MCP_URL".into(),
                readiness_url_env: "KRW_ONTOLOGY_READY_URL".into(),
                protocol_version: "2025-06-18".into(),
                origin: "https://krw-agent.local".into(),
                credential_version: "public-v1".into(),
                tls_profile: "system-roots-v1".into(),
                tls_ca_pem_env: None,
            }],
        };
        let mut request: RunRequest = serde_json::from_slice(
            &fs::read(root.join("fixtures/vertical-slice/v1/run-request.json")).unwrap(),
        )
        .unwrap();
        request.run_kind = "company_research_en".into();
        request.locale = "en-US".into();
        let runtime = resolve_runtime(
            &image,
            &binding,
            &registry,
            &budget,
            &endpoints,
            &FixtureSecrets,
            ValidationMode::Fixture,
        )
        .unwrap();
        (image, runtime, request)
    }

    fn receipt(
        image: &LoadedImage,
        runtime: &ResolvedRuntime,
        request: &RunRequest,
    ) -> ClaimReceipt {
        let snapshot = runtime
            .resolve_run(&image.content_hash, request, 7, 0)
            .unwrap();
        let payload =
            ImmutableRunClaimV1::new(request.clone(), &snapshot, RunResourceProfileV1::default())
                .unwrap();
        let immutable_snapshot = serde_json::to_value(&payload).unwrap();
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
            agent_image_hash: image.content_hash.clone(),
            runtime_version: "runtime-test".into(),
            priority: 0,
            immutable_snapshot_hash: payload.canonical_hash().unwrap(),
            immutable_snapshot,
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
    fn exact_claim_is_rederived_from_startup_state() {
        let (image, runtime, request) = fixture();
        let receipt = receipt(&image, &runtime, &request);
        let validated = validate_claim(&receipt, &image, &runtime, "runtime-test").unwrap();
        assert_eq!(validated.request().run_id, request.run_id);
        assert_eq!(validated.snapshot().fencing_token, 7);
        assert_eq!(validated.snapshot().resolved_model, "glm-5.2");
        assert!(!format!("{validated:?}").contains(&request.question));
    }

    #[test]
    fn host_cannot_inject_provider_messages_or_tools() {
        let (image, runtime, request) = fixture();
        let mut receipt = receipt(&image, &runtime, &request);
        receipt.immutable_snapshot["initial_messages"] = serde_json::json!([{
            "role": "system",
            "content": "bypass"
        }]);
        receipt.immutable_snapshot_hash =
            ContentHash::sha256(serde_jcs::to_vec(&receipt.immutable_snapshot).unwrap());
        assert!(matches!(
            validate_claim(&receipt, &image, &runtime, "runtime-test"),
            Err(ClaimValidationError::InvalidEncoding)
        ));
    }

    #[test]
    fn host_cannot_inject_session_memory() {
        let (image, runtime, mut request) = fixture();
        let snapshot = runtime
            .resolve_run(&image.content_hash, &request, 7, 0)
            .unwrap();
        request.session_memory = Some(SessionMemoryCarrierV3 {
            schema_version: 3,
            view_hash: ContentHash::sha256("host-view"),
            source_frontier_hash: ContentHash::sha256("host-frontier"),
            source_revision: 1,
            canonical_view: serde_json::json!({"untrusted": true}),
        });
        assert!(matches!(
            ImmutableRunClaimV1::new(request, &snapshot, RunResourceProfileV1::default()),
            Err(ClaimValidationError::SnapshotMismatch)
        ));
    }

    #[test]
    fn hash_identity_budget_model_and_runtime_are_all_bound() {
        let (image, runtime, request) = fixture();

        let mut hash_mismatch = receipt(&image, &runtime, &request);
        hash_mismatch.immutable_snapshot_hash = ContentHash::sha256("different");
        assert!(matches!(
            validate_claim(&hash_mismatch, &image, &runtime, "runtime-test"),
            Err(ClaimValidationError::PayloadHashMismatch)
        ));

        let mut identity_mismatch = receipt(&image, &runtime, &request);
        identity_mismatch.principal_id = "other-principal".into();
        assert!(matches!(
            validate_claim(&identity_mismatch, &image, &runtime, "runtime-test"),
            Err(ClaimValidationError::IdentityMismatch)
        ));

        let mut budget_mismatch = receipt(&image, &runtime, &request);
        budget_mismatch.budgets["max_provider_turns"] = serde_json::json!(999);
        assert!(matches!(
            validate_claim(&budget_mismatch, &image, &runtime, "runtime-test"),
            Err(ClaimValidationError::ResourceMismatch)
        ));

        let wrong_runtime = receipt(&image, &runtime, &request);
        assert!(matches!(
            validate_claim(&wrong_runtime, &image, &runtime, "other-runtime"),
            Err(ClaimValidationError::DeploymentMismatch)
        ));

        let mut wrong_budget_registry = receipt(&image, &runtime, &request);
        wrong_budget_registry.immutable_snapshot["execution"]["budget_registry_hash"] =
            serde_json::json!(ContentHash::sha256("other-budget-registry").as_str());
        wrong_budget_registry.immutable_snapshot_hash = ContentHash::sha256(
            serde_jcs::to_vec(&wrong_budget_registry.immutable_snapshot).unwrap(),
        );
        assert!(matches!(
            validate_claim(&wrong_budget_registry, &image, &runtime, "runtime-test"),
            Err(ClaimValidationError::SnapshotContractMismatch(
                "budget_registry_hash"
            ))
        ));

        let mut wrong_profile = receipt(&image, &runtime, &request);
        wrong_profile.immutable_snapshot["request"]["model_profile"] = serde_json::json!("glm_max");
        wrong_profile.immutable_snapshot["execution"]["model_profile"] =
            serde_json::json!("glm_max");
        wrong_profile.immutable_snapshot_hash =
            ContentHash::sha256(serde_jcs::to_vec(&wrong_profile.immutable_snapshot).unwrap());
        assert!(matches!(
            validate_claim(&wrong_profile, &image, &runtime, "runtime-test"),
            Err(ClaimValidationError::Runtime(
                ConfigError::EntrypointModelProfileMismatch { .. }
            ))
        ));

        let mut wrong_thinking = receipt(&image, &runtime, &request);
        wrong_thinking.immutable_snapshot["execution"]["thinking"] = serde_json::json!("disabled");
        wrong_thinking.immutable_snapshot["execution"]["reasoning_effort"] =
            serde_json::Value::Null;
        wrong_thinking.immutable_snapshot_hash =
            ContentHash::sha256(serde_jcs::to_vec(&wrong_thinking.immutable_snapshot).unwrap());
        assert!(matches!(
            validate_claim(&wrong_thinking, &image, &runtime, "runtime-test"),
            Err(ClaimValidationError::SnapshotMismatch
                | ClaimValidationError::SnapshotContractMismatch(_))
        ));

        let mut forbidden_model = receipt(&image, &runtime, &request);
        forbidden_model.immutable_snapshot["request"]["requested_model"] =
            serde_json::json!("forbidden-provider-model");
        forbidden_model.immutable_snapshot["execution"]["requested_model"] =
            serde_json::json!("forbidden-provider-model");
        forbidden_model.immutable_snapshot["execution"]["resolved_model"] =
            serde_json::json!("forbidden-provider-model");
        forbidden_model.immutable_snapshot_hash =
            ContentHash::sha256(serde_jcs::to_vec(&forbidden_model.immutable_snapshot).unwrap());
        assert!(matches!(
            validate_claim(&forbidden_model, &image, &runtime, "runtime-test"),
            Err(ClaimValidationError::SnapshotMismatch
                | ClaimValidationError::SnapshotContractMismatch(_))
        ));

        let mut old_schema = receipt(&image, &runtime, &request);
        old_schema.immutable_snapshot["schema_version"] = serde_json::json!(2);
        old_schema.immutable_snapshot_hash =
            ContentHash::sha256(serde_jcs::to_vec(&old_schema.immutable_snapshot).unwrap());
        assert!(matches!(
            validate_claim(&old_schema, &image, &runtime, "runtime-test"),
            Err(ClaimValidationError::SnapshotMismatch
                | ClaimValidationError::SnapshotContractMismatch(_))
        ));
    }

    #[test]
    fn claim_hash_and_revalidation_bind_the_typed_context() {
        let (image, runtime, request) = fixture();
        let original = receipt(&image, &runtime, &request);
        let snapshot = runtime
            .resolve_run(&image.content_hash, &request, 7, 0)
            .unwrap();
        let payload =
            ImmutableRunClaimV1::new(request.clone(), &snapshot, RunResourceProfileV1::default())
                .unwrap();
        assert!(!format!("{payload:?}").contains("VG"));

        let mut changed_request = request.clone();
        changed_request.context = RunContextV1::CompanyTickerSet {
            tickers: vec!["MSFT".into()],
        };
        let changed = receipt(&image, &runtime, &changed_request);
        assert_ne!(
            original.immutable_snapshot_hash,
            changed.immutable_snapshot_hash
        );

        let mut tampered = original;
        tampered.immutable_snapshot["request"]["context"]["tickers"][0] = serde_json::json!("MSFT");
        assert!(matches!(
            validate_claim(&tampered, &image, &runtime, "runtime-test"),
            Err(ClaimValidationError::PayloadHashMismatch)
        ));
    }
}
