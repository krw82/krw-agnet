//! Startup-only deployment resolution. The hot run loop receives an immutable snapshot and never
//! reads YAML, environment variables, endpoint aliases, or credentials.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::sync::Arc;

use krw_agent_image::{
    AgentImageManifest, EntrypointSpec, ImageError, LoadedImage, ScopeCardinality,
};
use krw_agent_protocol::{
    ALLOWED_PROFILE_IDS, AuthScope, BudgetLimits, CapabilityBinding, ContentHash,
    DEEPSEEK_MODEL_ID, DeploymentBinding, EntrypointScope, GLM_DIRECT_PROFILE_ID,
    GLM_HIGH_PROFILE_ID, GLM_MAX_PROFILE_ID, GLM_MODEL_ID, McpToolSessionReuse, ModelDescriptor,
    ModelExecutionProfile, ModelRegistry, PROTOCOL_VERSION, PROVIDER_KINDS,
    PUBLIC_RELEASE_DESCRIPTOR_SCHEMA_VERSION, PinnedExecutionContract, ProviderKind,
    ProviderWireCapabilities, PublicReleaseDescriptor, PublicReleaseEntrypoint, ReasoningEffort,
    ResolvedExecutionSnapshot, RunRequest, ScopeCardinalityKind, ThinkingMode,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeroize::{Zeroize, Zeroizing};

const CONFIG_SCHEMA_VERSION: u16 = 1;
const DEPLOYMENT_BINDING_SCHEMA_VERSION: u16 = 4;
const MODEL_REGISTRY_SCHEMA_VERSION: u16 = 4;
const ZERO_HASH: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
pub const MAX_RELEASE_IMAGES: usize = 64;
pub const RESOLVED_CAPABILITY_FINGERPRINT_SCHEMA_VERSION: u16 = 5;
const TLS_PROFILE_SYSTEM_ROOTS_V1: &str = "system-roots-v1";
const TLS_PROFILE_SYSTEM_PLUS_PINNED_CA_V1: &str = "system-plus-pinned-ca-v1";

// Pinned GLM-5.3 deployment facts. These mirror the DeepSeek constants baked
// into `validate_model` and keep the GLM branch free of magic numbers. Both
// providers speak the Anthropic Messages API: GLM via z.ai's `/api/anthropic`
// facade and DeepSeek via `api.deepseek.com/anthropic`.
const DEEPSEEK_API_BASE: &str = "https://api.deepseek.com/anthropic";
const DEEPSEEK_API_VERSION: &str = "anthropic-messages-v1";
const GLM_API_BASE: &str = "https://api.z.ai/api/anthropic";
const GLM_API_VERSION: &str = "anthropic-messages-v1";
const GLM_MAX_CONTEXT_TOKENS: u32 = 204_800;
const GLM_MAX_OUTPUT_TOKENS: u32 = 131_072;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetProfile {
    pub profile_id: String,
    pub limits: BudgetLimits,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetRegistry {
    pub schema_version: u16,
    pub registry_id: String,
    pub profiles: Vec<BudgetProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointRegistry {
    pub schema_version: u16,
    pub registry_id: String,
    pub endpoints: Vec<EndpointDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointDescriptor {
    pub endpoint_ref: String,
    /// Name of an environment variable containing the HTTPS URL, never the URL itself.
    pub url_env: String,
    /// Name of an environment variable containing the same-origin readiness URL.
    pub readiness_url_env: String,
    pub protocol_version: String,
    pub origin: String,
    /// Non-secret rotation identifier included in the machine-wide pool key.
    pub credential_version: String,
    /// Closed TLS trust policy. `system-roots-v1` uses only platform roots;
    /// `system-plus-pinned-ca-v1` requires one PEM CA bundle from the named
    /// process environment entry below.
    pub tls_profile: String,
    /// Optional process-environment handle for an additional PEM CA bundle.
    /// The certificate bytes are deployment material, never image or YAML data.
    #[serde(default)]
    pub tls_ca_pem_env: Option<String>,
}

/// Canonical non-secret identity of one fully resolved physical capability.
/// Resolved URLs and symbolic credential references are represented only by
/// hashes; bearer material is deliberately excluded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedCapabilityFingerprint {
    pub schema_version: u16,
    pub binding_key: String,
    pub mcp_tool_name: String,
    pub endpoint_ref: String,
    pub endpoint_url_hash: ContentHash,
    pub readiness_url_hash: ContentHash,
    pub origin_hash: ContentHash,
    pub credential_ref_hash: Option<ContentHash>,
    pub protocol_version: String,
    pub credential_version: String,
    pub tls_profile: String,
    pub tls_ca_pem_hash: Option<ContentHash>,
    pub auth_scope: AuthScope,
    pub tool_session_reuse: McpToolSessionReuse,
    pub server_schema_bundle_hash: ContentHash,
    pub server_build: String,
    pub data_release_hash: ContentHash,
    pub max_connections: u16,
    pub request_timeout_ms: u64,
}

impl ResolvedCapabilityFingerprint {
    pub fn content_hash(&self) -> Result<ContentHash, ConfigError> {
        Ok(ContentHash::sha256(serde_jcs::to_vec(
            &ResolvedCapabilityFingerprintHashEnvelope {
                format: "krw.agent/resolved-capability-fingerprint-v5",
                fingerprint: self,
            },
        )?))
    }
}

#[derive(Serialize)]
struct ResolvedCapabilityFingerprintHashEnvelope<'a> {
    format: &'static str,
    fingerprint: &'a ResolvedCapabilityFingerprint,
}

#[derive(Clone)]
pub struct ResolvedCapability {
    pub binding: CapabilityBinding,
    pub endpoint: String,
    pub readiness_endpoint: String,
    pub bearer_token: Option<Zeroizing<String>>,
    pub protocol_version: String,
    pub origin: String,
    pub credential_version: String,
    pub tls_profile: String,
    pub tls_ca_pem: Option<Arc<Zeroizing<String>>>,
    pub tls_ca_pem_hash: Option<ContentHash>,
    fingerprint: ResolvedCapabilityFingerprint,
    fingerprint_hash: ContentHash,
}

impl std::fmt::Debug for ResolvedCapability {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResolvedCapability")
            .field("binding_key", &self.binding.binding_key)
            .field("mcp_tool_name", &self.binding.mcp_tool_name)
            .field("endpoint_ref", &self.binding.endpoint_ref)
            .field("endpoint", &"[REDACTED]")
            .field("readiness_endpoint", &"[REDACTED]")
            .field(
                "bearer_token",
                &self.bearer_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("protocol_version", &self.protocol_version)
            .field("credential_version", &self.credential_version)
            .field("tls_profile", &self.tls_profile)
            .field("tls_ca_pem_hash", &self.tls_ca_pem_hash)
            .field("fingerprint", &self.fingerprint)
            .field("fingerprint_hash", &self.fingerprint_hash)
            .finish_non_exhaustive()
    }
}

impl ResolvedCapability {
    pub fn fingerprint(&self) -> &ResolvedCapabilityFingerprint {
        &self.fingerprint
    }

    pub fn fingerprint_hash(&self) -> &ContentHash {
        &self.fingerprint_hash
    }
}

/// Machine-wide immutable configuration. Construct exactly once at startup and share behind
/// `Arc`; a run snapshot contains no endpoint strings or credential copies.
pub struct ResolvedRuntime {
    image_hash: ContentHash,
    binding_hash: ContentHash,
    registry_hash: ContentHash,
    entrypoints: BTreeMap<(String, String), EntrypointSpec>,
    model_registry: Arc<ModelRegistry>,
    budget_registry_hash: ContentHash,
    budget_profiles: BTreeMap<String, BudgetProfile>,
    /// Live provider credentials resolved once per advertised provider kind.
    /// Lookup is descriptor-driven: a kind absent from the compiled registry
    /// has no entry, and no second ad-hoc key field exists.
    provider_api_keys: BTreeMap<ProviderKind, Arc<Zeroizing<String>>>,
    /// Logical `AgentImage` capability id -> shared physical deployment binding.
    pub capabilities: BTreeMap<String, Arc<ResolvedCapability>>,
    /// Logical capability id -> immutable data-release hash.  The broader
    /// resolved endpoint/TLS/build/auth identity remains in `binding_hash`;
    /// collapsing the two domains here would make a valid release look like
    /// a release-pin mismatch at the execution boundary.
    capability_release_hashes: BTreeMap<String, ContentHash>,
}

/// One immutable image and only the deployment subset it references.
pub struct ResolvedRelease {
    pub image: Arc<LoadedImage>,
    pub deployment: Arc<DeploymentBinding>,
    pub runtime: Arc<ResolvedRuntime>,
}

impl std::fmt::Debug for ResolvedRelease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResolvedRelease")
            .field("image_hash", &self.image.content_hash)
            .field("agent_id", &self.image.body.metadata.id)
            .field("deployment_hash", &self.runtime.binding_hash)
            .finish_non_exhaustive()
    }
}

/// Complete immutable startup release set. It has no session or run state.
pub struct ResolvedReleaseSet {
    release_set_hash: ContentHash,
    releases: BTreeMap<ContentHash, Arc<ResolvedRelease>>,
    entrypoint_owners: BTreeMap<(String, String), ContentHash>,
    model_registry: Arc<ModelRegistry>,
    model_registry_hash: ContentHash,
    provider_api_keys: BTreeMap<ProviderKind, Arc<Zeroizing<String>>>,
}

impl std::fmt::Debug for ResolvedReleaseSet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResolvedReleaseSet")
            .field("release_set_hash", &self.release_set_hash)
            .field("image_hashes", &self.releases.keys().collect::<Vec<_>>())
            .field("entrypoint_owners", &self.entrypoint_owners)
            .field("model_registry_hash", &self.model_registry_hash)
            .field("provider_api_keys", &"[REDACTED]")
            .finish()
    }
}

impl std::fmt::Debug for ResolvedRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResolvedRuntime")
            .field("image_hash", &self.image_hash)
            .field("binding_hash", &self.binding_hash)
            .field("registry_hash", &self.registry_hash)
            .field("model_registry", &self.model_registry)
            .field("provider_api_keys", &"[REDACTED]")
            .field("capabilities", &self.capabilities)
            .finish()
    }
}

pub trait SecretSource {
    fn read_secret(&self, name: &str) -> Result<Zeroizing<String>, ConfigError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessEnvironment;

impl SecretSource for ProcessEnvironment {
    fn read_secret(&self, name: &str) -> Result<Zeroizing<String>, ConfigError> {
        validate_env_name(name)?;
        let value = std::env::var(name).map_err(|_| ConfigError::MissingSecret(name.into()))?;
        if value.is_empty() {
            return Err(ConfigError::MissingSecret(name.into()));
        }
        Ok(Zeroizing::new(value))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationMode {
    /// Validate a release that will be able to execute provider calls.
    Production,
    /// Validate the complete production release contract without constructing
    /// a provider client. This is used for sealed-descriptor authoring and
    /// database ABI diagnostics, neither of which can issue a model request.
    ///
    /// Production binding fingerprints and all non-provider secrets remain
    /// mandatory. Only the dormant execution-provider credential is omitted.
    ProductionDescriptor,
    /// Permit deliberately synthetic fixtures used by unit tests.
    Fixture,
}

impl ValidationMode {
    const fn requires_production_integrity(self) -> bool {
        matches!(self, Self::Production | Self::ProductionDescriptor)
    }

    const fn requires_provider_credentials(self) -> bool {
        matches!(self, Self::Production)
    }
}

pub fn load_yaml<T: serde::de::DeserializeOwned>(path: impl AsRef<Path>) -> Result<T, ConfigError> {
    Ok(serde_yaml_ng::from_slice(&fs::read(path)?)?)
}

pub fn resolve_runtime(
    image: &AgentImageManifest,
    binding: &DeploymentBinding,
    registry: &ModelRegistry,
    budget_registry: &BudgetRegistry,
    endpoint_registry: &EndpointRegistry,
    secrets: &impl SecretSource,
    mode: ValidationMode,
) -> Result<ResolvedRuntime, ConfigError> {
    let required_binding_keys = required_binding_keys(std::iter::once(image));
    let globals = prepare_globals(
        &required_binding_keys,
        binding,
        registry,
        budget_registry,
        endpoint_registry,
        secrets,
        mode,
    )?;
    resolve_image(image, binding, budget_registry, &globals, mode).map(|(_, runtime)| runtime)
}

/// Resolve 1..=64 verified images as one immutable machine-wide release set.
/// Duplicate exact image hashes are loaded and verified, then represented once.
pub fn resolve_release_set(
    images: Vec<LoadedImage>,
    binding: &DeploymentBinding,
    registry: &ModelRegistry,
    budget_registry: &BudgetRegistry,
    endpoint_registry: &EndpointRegistry,
    secrets: &impl SecretSource,
    mode: ValidationMode,
) -> Result<ResolvedReleaseSet, ConfigError> {
    if images.is_empty() || images.len() > MAX_RELEASE_IMAGES {
        return Err(ConfigError::ReleaseImageCount);
    }

    let mut unique_images = BTreeMap::<ContentHash, Arc<LoadedImage>>::new();
    for image in images {
        image.verify()?;
        unique_images
            .entry(image.content_hash.clone())
            .or_insert_with(|| Arc::new(image));
    }

    let mut identities = BTreeSet::new();
    let mut entrypoint_owners = BTreeMap::new();
    for (image_hash, image) in &unique_images {
        if !identities.insert(image.body.metadata.id.clone()) {
            return Err(ConfigError::DuplicateAgentIdentity(
                image.body.metadata.id.clone(),
            ));
        }
        for entrypoint in image.body.entrypoints.values() {
            let owner = (entrypoint.run_kind.clone(), entrypoint.locale.clone());
            if entrypoint_owners
                .insert(owner.clone(), image_hash.clone())
                .is_some()
            {
                return Err(ConfigError::DuplicateEntrypointOwnership {
                    run_kind: owner.0,
                    locale: owner.1,
                });
            }
        }
    }

    let required_binding_keys =
        required_binding_keys(unique_images.values().map(|image| &image.manifest));
    let globals = prepare_globals(
        &required_binding_keys,
        binding,
        registry,
        budget_registry,
        endpoint_registry,
        secrets,
        mode,
    )?;
    let mut releases = BTreeMap::new();
    for (image_hash, image) in unique_images {
        let (deployment, runtime) =
            resolve_image(&image.manifest, binding, budget_registry, &globals, mode)?;
        releases.insert(
            image_hash,
            Arc::new(ResolvedRelease {
                image,
                deployment: Arc::new(deployment),
                runtime: Arc::new(runtime),
            }),
        );
    }
    let release_set_hash = release_set_hash(releases.values(), &globals.registry_hash)?;
    Ok(ResolvedReleaseSet {
        release_set_hash,
        releases,
        entrypoint_owners,
        model_registry: Arc::clone(&globals.model_registry),
        model_registry_hash: globals.registry_hash.clone(),
        provider_api_keys: globals.provider_api_keys.clone(),
    })
}

struct PreparedGlobals {
    bindings: BTreeMap<String, CapabilityBinding>,
    budget_profiles: BTreeMap<String, BudgetProfile>,
    model_registry: Arc<ModelRegistry>,
    registry_hash: ContentHash,
    provider_api_keys: BTreeMap<ProviderKind, Arc<Zeroizing<String>>>,
    resolved_bindings: BTreeMap<String, Arc<ResolvedCapability>>,
}

fn prepare_globals(
    required_binding_keys: &BTreeSet<String>,
    binding: &DeploymentBinding,
    registry: &ModelRegistry,
    budget_registry: &BudgetRegistry,
    endpoint_registry: &EndpointRegistry,
    secrets: &impl SecretSource,
    mode: ValidationMode,
) -> Result<PreparedGlobals, ConfigError> {
    validate_versions(binding, registry, budget_registry, endpoint_registry)?;

    let budget_profiles = budget_registry
        .profiles
        .iter()
        .map(|profile| (profile.profile_id.clone(), profile.clone()))
        .collect::<BTreeMap<_, _>>();
    if budget_profiles.is_empty() || budget_profiles.len() != budget_registry.profiles.len() {
        return Err(ConfigError::DuplicateOrEmptyBudgetProfile);
    }
    for profile in budget_profiles.values() {
        validate_budget(&profile.limits, "budget profile")?;
    }

    let model_ids = registry
        .models
        .iter()
        .map(|model| model.model_id.as_str())
        .collect::<BTreeSet<_>>();
    if model_ids.len() != registry.models.len() || registry.models.is_empty() {
        return Err(ConfigError::DuplicateOrEmptyModel);
    }
    // Admit any non-empty subset of the compiled provider-kind inventory. The
    // protocol kind mapping is the single authority: every model id must
    // resolve to a known provider kind here at startup, so an unknown provider
    // fails release compilation and can never surface mid provider turn.
    let advertised_kinds = registry
        .models
        .iter()
        .filter_map(|model| ProviderKind::for_model_id(&model.model_id))
        .collect::<BTreeSet<_>>();
    if advertised_kinds.len() != registry.models.len() {
        return Err(ConfigError::ModelInventoryMismatch);
    }
    for model in &registry.models {
        validate_model(model)?;
    }
    let profile_ids = registry
        .profiles
        .iter()
        .map(|profile| profile.profile_id.as_str())
        .collect::<BTreeSet<_>>();
    if profile_ids.len() != registry.profiles.len() || registry.profiles.is_empty() {
        return Err(ConfigError::DuplicateOrEmptyModelProfile);
    }
    // Each provider admitted by this release must carry the one provider-
    // neutral high/max/direct profile triad. A provider registry changes the
    // model bound to a slot, never the logical profile vocabulary.
    if !advertised_kinds.is_empty() {
        let profiles_complete = [
            GLM_DIRECT_PROFILE_ID,
            GLM_HIGH_PROFILE_ID,
            GLM_MAX_PROFILE_ID,
        ]
        .iter()
        .all(|profile_id| profile_ids.contains(profile_id));
        if !profiles_complete {
            return Err(ConfigError::ModelProfileInventoryMismatch);
        }
    }
    if !profile_ids
        .iter()
        .all(|id| ALLOWED_PROFILE_IDS.contains(id))
    {
        return Err(ConfigError::ModelProfileInventoryMismatch);
    }
    for profile in &registry.profiles {
        let model = registry
            .model(&profile.model_id)
            .ok_or_else(|| ConfigError::UnknownProfileModel(profile.model_id.clone()))?;
        validate_model_profile(profile, model)?;
    }

    let endpoints = endpoint_registry
        .endpoints
        .iter()
        .map(|endpoint| (endpoint.endpoint_ref.as_str(), endpoint))
        .collect::<BTreeMap<_, _>>();
    if endpoints.len() != endpoint_registry.endpoints.len() {
        return Err(ConfigError::DuplicateEndpoint);
    }
    for descriptor in &endpoint_registry.endpoints {
        validate_endpoint_descriptor(descriptor)?;
    }

    let mut bindings = BTreeMap::new();
    for capability in &binding.capabilities {
        validate_binding(capability, mode)?;
        if !endpoints.contains_key(capability.endpoint_ref.as_str()) {
            return Err(ConfigError::UnknownEndpoint(
                capability.endpoint_ref.clone(),
            ));
        }
        if bindings
            .insert(capability.binding_key.clone(), capability.clone())
            .is_some()
        {
            return Err(ConfigError::DuplicateCapabilityBinding);
        }
    }
    for key in required_binding_keys {
        if !bindings.contains_key(key) {
            return Err(ConfigError::MissingBindingKey(key.clone()));
        }
    }

    let mut secret_cache = BTreeMap::<String, Arc<Zeroizing<String>>>::new();
    // A live registry requires credentials only for the provider kinds it
    // actually advertises, resolved once per kind through the kind's
    // `credential_ref` (`DEEPSEEK_API_KEY` / `GLM_API_KEY`). Descriptor and
    // database checks do not construct a provider client, so they validate
    // the same immutable release without requiring a dormant execution
    // credential. Kind-absent entries simply have no key in the map; the
    // values remain startup-local and are never exposed to a run.
    let mut provider_api_keys = BTreeMap::<ProviderKind, Arc<Zeroizing<String>>>::new();
    if mode.requires_provider_credentials() {
        for kind in PROVIDER_KINDS {
            if advertised_kinds.contains(kind) {
                let key = read_secret_once(secrets, &mut secret_cache, kind.credential_env())?;
                provider_api_keys.insert(*kind, key);
            }
        }
    }
    let mut resolved_bindings = BTreeMap::new();
    for key in required_binding_keys {
        let capability = bindings
            .get(key)
            .ok_or_else(|| ConfigError::MissingBindingKey(key.clone()))?;
        let descriptor = endpoints
            .get(capability.endpoint_ref.as_str())
            .ok_or_else(|| ConfigError::UnknownEndpoint(capability.endpoint_ref.clone()))?;
        let endpoint = read_secret_once(secrets, &mut secret_cache, &descriptor.url_env)?;
        let readiness_endpoint =
            read_secret_once(secrets, &mut secret_cache, &descriptor.readiness_url_env)?;
        if !endpoint.starts_with("https://") {
            return Err(ConfigError::InsecureEndpoint(
                capability.endpoint_ref.clone(),
            ));
        }
        if !readiness_endpoint.starts_with("https://") {
            return Err(ConfigError::InsecureReadinessEndpoint(
                capability.endpoint_ref.clone(),
            ));
        }
        let bearer_token = capability
            .credential_ref
            .as_deref()
            .map(|name| read_secret_once(secrets, &mut secret_cache, name))
            .transpose()?
            .map(|secret| Zeroizing::new(secret.as_str().to_owned()));
        let tls_ca_pem = descriptor
            .tls_ca_pem_env
            .as_deref()
            .map(|name| read_secret_once(secrets, &mut secret_cache, name))
            .transpose()?;
        let tls_ca_pem_hash = tls_ca_pem
            .as_ref()
            .map(|pem| ContentHash::sha256(pem.as_bytes()));
        let fingerprint = ResolvedCapabilityFingerprint {
            schema_version: RESOLVED_CAPABILITY_FINGERPRINT_SCHEMA_VERSION,
            binding_key: capability.binding_key.clone(),
            mcp_tool_name: capability.mcp_tool_name.clone(),
            endpoint_ref: capability.endpoint_ref.clone(),
            endpoint_url_hash: ContentHash::sha256(endpoint.as_bytes()),
            readiness_url_hash: ContentHash::sha256(readiness_endpoint.as_bytes()),
            origin_hash: ContentHash::sha256(descriptor.origin.as_bytes()),
            credential_ref_hash: capability.credential_ref.as_ref().map(ContentHash::sha256),
            protocol_version: descriptor.protocol_version.clone(),
            credential_version: descriptor.credential_version.clone(),
            tls_profile: descriptor.tls_profile.clone(),
            tls_ca_pem_hash: tls_ca_pem_hash.clone(),
            auth_scope: capability.auth_scope.clone(),
            tool_session_reuse: capability.tool_session_reuse,
            server_schema_bundle_hash: capability.server_schema_bundle_hash.clone(),
            server_build: capability.server_build.clone(),
            data_release_hash: capability.data_release_hash.clone(),
            max_connections: capability.max_connections,
            request_timeout_ms: capability.request_timeout_ms,
        };
        let fingerprint_hash = fingerprint.content_hash()?;
        resolved_bindings.insert(
            key.clone(),
            Arc::new(ResolvedCapability {
                binding: capability.clone(),
                endpoint: endpoint.as_str().to_owned(),
                readiness_endpoint: readiness_endpoint.as_str().to_owned(),
                bearer_token,
                protocol_version: descriptor.protocol_version.clone(),
                origin: descriptor.origin.clone(),
                credential_version: descriptor.credential_version.clone(),
                tls_profile: descriptor.tls_profile.clone(),
                tls_ca_pem,
                tls_ca_pem_hash,
                fingerprint,
                fingerprint_hash,
            }),
        );
    }

    Ok(PreparedGlobals {
        bindings,
        budget_profiles,
        model_registry: Arc::new(registry.clone()),
        registry_hash: semantic_model_registry_hash(registry)?,
        provider_api_keys,
        resolved_bindings,
    })
}

fn resolve_image(
    image: &AgentImageManifest,
    global_binding: &DeploymentBinding,
    global_budget_registry: &BudgetRegistry,
    globals: &PreparedGlobals,
    mode: ValidationMode,
) -> Result<(DeploymentBinding, ResolvedRuntime), ConfigError> {
    verify_manifest_identity(image)?;
    if mode.requires_production_integrity() {
        krw_agent_contracts::verify_registry()
            .map_err(|error| ConfigError::CanonicalContractRegistry(format!("{error:?}")))?;
        for contract in &image.body.contracts {
            krw_agent_contracts::verify_pin(&contract.id, &contract.content_hash)
                .map_err(|error| ConfigError::CanonicalContractRegistry(format!("{error:?}")))?;
        }
    }

    let mut entrypoints = BTreeMap::new();
    for entrypoint in image.body.entrypoints.values() {
        let key = (entrypoint.run_kind.clone(), entrypoint.locale.clone());
        if entrypoints.insert(key, entrypoint.clone()).is_some() {
            return Err(ConfigError::DuplicateRunKind);
        }
        if !globals
            .model_registry
            .profiles
            .iter()
            .any(|profile| profile.profile_id == entrypoint.required_model_profile)
        {
            return Err(ConfigError::ModelProfileSetMismatch);
        }
    }

    let required_profiles = image
        .body
        .entrypoints
        .values()
        .map(|entrypoint| entrypoint.required_budget_profile.clone())
        .collect::<BTreeSet<_>>();
    let mut budget_profiles = BTreeMap::new();
    for profile_id in required_profiles {
        let profile = globals
            .budget_profiles
            .get(&profile_id)
            .ok_or(ConfigError::BudgetProfileSetMismatch)?;
        budget_profiles.insert(profile_id, profile.clone());
    }
    let effective_budget_registry = BudgetRegistry {
        schema_version: global_budget_registry.schema_version,
        registry_id: format!("effective/{}", image.content_hash),
        profiles: budget_profiles.values().cloned().collect(),
    };

    let required_bindings = image
        .body
        .capabilities
        .iter()
        .filter_map(|capability| capability.remote_binding_key().map(str::to_owned))
        .collect::<BTreeSet<_>>();
    let effective_binding = DeploymentBinding {
        schema_version: global_binding.schema_version,
        deployment_id: format!("effective/{}", image.content_hash),
        capabilities: required_bindings
            .iter()
            .map(|key| {
                globals
                    .bindings
                    .get(key)
                    .cloned()
                    .ok_or_else(|| ConfigError::MissingBindingKey(key.clone()))
            })
            .collect::<Result<Vec<_>, _>>()?,
    };
    let effective_fingerprints = required_bindings
        .iter()
        .map(|key| {
            globals
                .resolved_bindings
                .get(key)
                .map(|resolved| resolved.fingerprint().clone())
                .ok_or_else(|| ConfigError::MissingBindingKey(key.clone()))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut resolved_capabilities = BTreeMap::new();
    let mut release_hashes = BTreeMap::new();
    for capability in &image.body.capabilities {
        let Some(binding_key) = capability.remote_binding_key() else {
            continue;
        };
        let resolved = globals
            .resolved_bindings
            .get(binding_key)
            .ok_or_else(|| ConfigError::MissingBindingKey(binding_key.to_owned()))?;
        if resolved_capabilities
            .insert(capability.id.clone(), Arc::clone(resolved))
            .is_some()
        {
            return Err(ConfigError::DuplicateLogicalCapability(
                capability.id.clone(),
            ));
        }
        // `ResolvedCapability::fingerprint_hash` binds the physical route and
        // is already included in the deployment-wide execution fingerprint.
        // The per-capability snapshot field is deliberately narrower: it is
        // the exact immutable data release later checked against the action
        // binding and evidence lineage.
        release_hashes.insert(
            capability.id.clone(),
            resolved.binding.data_release_hash.clone(),
        );
    }

    let runtime = ResolvedRuntime {
        image_hash: image.content_hash.clone(),
        binding_hash: effective_binding_hash(&effective_fingerprints)?,
        registry_hash: globals.registry_hash.clone(),
        budget_registry_hash: effective_budget_registry_hash(&effective_budget_registry)?,
        entrypoints,
        model_registry: Arc::clone(&globals.model_registry),
        budget_profiles,
        provider_api_keys: globals.provider_api_keys.clone(),
        capabilities: resolved_capabilities,
        capability_release_hashes: release_hashes,
    };
    Ok((effective_binding, runtime))
}

fn required_binding_keys<'a>(
    images: impl IntoIterator<Item = &'a AgentImageManifest>,
) -> BTreeSet<String> {
    images
        .into_iter()
        .flat_map(|image| image.body.capabilities.iter())
        .filter_map(|capability| capability.remote_binding_key().map(str::to_owned))
        .collect()
}

fn read_secret_once(
    secrets: &impl SecretSource,
    cache: &mut BTreeMap<String, Arc<Zeroizing<String>>>,
    name: &str,
) -> Result<Arc<Zeroizing<String>>, ConfigError> {
    if let Some(secret) = cache.get(name) {
        return Ok(Arc::clone(secret));
    }
    validate_env_name(name)?;
    let secret = secrets.read_secret(name)?;
    if secret.is_empty() {
        return Err(ConfigError::MissingSecret(name.into()));
    }
    let secret = Arc::new(secret);
    cache.insert(name.into(), Arc::clone(&secret));
    Ok(secret)
}

fn verify_manifest_identity(image: &AgentImageManifest) -> Result<(), ConfigError> {
    let expected = ContentHash::sha256(serde_jcs::to_vec(&image.body)?);
    if expected != image.content_hash {
        return Err(ConfigError::Image(ImageError::ImageHashMismatch {
            expected,
            observed: image.content_hash.clone(),
        }));
    }
    Ok(())
}

#[derive(Serialize)]
struct ReleaseSetHashEnvelope<'a> {
    format: &'static str,
    releases: Vec<ReleaseSetHashEntry<'a>>,
    model_registry_hash: &'a ContentHash,
}

#[derive(Serialize)]
struct ReleaseSetHashEntry<'a> {
    #[serde(rename = "image_hash")]
    image: &'a ContentHash,
    #[serde(rename = "effective_deployment_hash")]
    deployment: &'a ContentHash,
    #[serde(rename = "effective_budget_hash")]
    budget: &'a ContentHash,
}

#[derive(Serialize)]
struct EffectiveBindingHashEnvelope<'a> {
    format: &'static str,
    fingerprints: &'a [ResolvedCapabilityFingerprint],
}

fn effective_binding_hash(
    fingerprints: &[ResolvedCapabilityFingerprint],
) -> Result<ContentHash, ConfigError> {
    Ok(ContentHash::sha256(serde_jcs::to_vec(
        &EffectiveBindingHashEnvelope {
            format: "krw.agent/effective-resolved-deployment-v1",
            fingerprints,
        },
    )?))
}

#[derive(Serialize)]
struct EffectiveBudgetHashEnvelope<'a> {
    format: &'static str,
    schema_version: u16,
    profiles: &'a [BudgetProfile],
}

#[derive(Serialize)]
struct SemanticModelRegistryHashEnvelope<'a> {
    format: &'static str,
    schema_version: u16,
    models: &'a [ModelDescriptor],
    profiles: &'a [ModelExecutionProfile],
}

fn semantic_model_registry_hash(registry: &ModelRegistry) -> Result<ContentHash, ConfigError> {
    let mut models = registry.models.clone();
    models.sort_by(|left, right| {
        (
            left.model_id.as_str(),
            left.api_base.as_str(),
            left.api_version.as_str(),
        )
            .cmp(&(
                right.model_id.as_str(),
                right.api_base.as_str(),
                right.api_version.as_str(),
            ))
    });
    let mut profiles = registry.profiles.clone();
    profiles.sort_by(|left, right| left.profile_id.cmp(&right.profile_id));
    Ok(ContentHash::sha256(serde_jcs::to_vec(
        &SemanticModelRegistryHashEnvelope {
            format: "krw.agent/semantic-model-registry-v4",
            schema_version: registry.schema_version,
            models: &models,
            profiles: &profiles,
        },
    )?))
}

fn effective_budget_registry_hash(registry: &BudgetRegistry) -> Result<ContentHash, ConfigError> {
    Ok(ContentHash::sha256(serde_jcs::to_vec(
        &EffectiveBudgetHashEnvelope {
            format: "krw.agent/effective-budget-registry-v1",
            schema_version: registry.schema_version,
            profiles: &registry.profiles,
        },
    )?))
}

fn release_set_hash<'a>(
    releases: impl IntoIterator<Item = &'a Arc<ResolvedRelease>>,
    model_registry_hash: &'a ContentHash,
) -> Result<ContentHash, ConfigError> {
    let releases = releases
        .into_iter()
        .map(|release| ReleaseSetHashEntry {
            image: &release.image.content_hash,
            deployment: &release.runtime.binding_hash,
            budget: &release.runtime.budget_registry_hash,
        })
        .collect();
    Ok(ContentHash::sha256(serde_jcs::to_vec(
        &ReleaseSetHashEnvelope {
            format: "krw.agent/machine-execution-release-set-v1",
            releases,
            model_registry_hash,
        },
    )?))
}

impl ResolvedRuntime {
    pub fn image_hash(&self) -> &ContentHash {
        &self.image_hash
    }

    pub fn deployment_binding_hash(&self) -> &ContentHash {
        &self.binding_hash
    }

    pub fn model_registry_hash(&self) -> &ContentHash {
        &self.registry_hash
    }

    pub fn budget_registry_hash(&self) -> &ContentHash {
        &self.budget_registry_hash
    }

    /// Startup-owned credential for one provider kind, resolved once from the
    /// kind's `credential_ref` environment. Empty when the compiled registry
    /// does not advertise that kind. Callers may borrow it to build one
    /// provider catalog, but no run receives or clones the secret.
    pub fn provider_api_key(&self, kind: ProviderKind) -> &str {
        self.provider_api_keys
            .get(&kind)
            .map_or("", |key| key.as_str())
    }

    pub fn physical_binding_count(&self) -> usize {
        self.capabilities
            .values()
            .map(|resolved| resolved.binding.binding_key.as_str())
            .collect::<BTreeSet<_>>()
            .len()
    }

    pub fn resolve_run(
        &self,
        image_hash: &ContentHash,
        request: &RunRequest,
        fencing_token: u64,
        cancel_generation: u64,
    ) -> Result<ResolvedExecutionSnapshot, ConfigError> {
        if image_hash != &self.image_hash {
            return Err(ConfigError::ImageChangedAfterStartup);
        }
        let entrypoint = self
            .entrypoints
            .get(&(request.run_kind.clone(), request.locale.clone()))
            .ok_or_else(|| {
                self.entrypoints
                    .values()
                    .find(|entrypoint| entrypoint.run_kind == request.run_kind)
                    .map_or_else(
                        || ConfigError::UnknownRunKind(request.run_kind.clone()),
                        |entrypoint| ConfigError::LocaleMismatch {
                            required: entrypoint.locale.clone(),
                            observed: request.locale.clone(),
                        },
                    )
            })?;
        entrypoint
            .validate_run_context(&request.context)
            .map_err(|_| ConfigError::RunContextPolicyMismatch)?;
        validate_budget(&request.budget, "run request")?;
        let budget_profile = self
            .budget_profiles
            .get(&entrypoint.required_budget_profile)
            .ok_or(ConfigError::BudgetProfileSetMismatch)?;
        ensure_exact_budget(&request.budget, &budget_profile.limits)?;
        if request.model_profile != entrypoint.required_model_profile {
            return Err(ConfigError::EntrypointModelProfileMismatch {
                required: entrypoint.required_model_profile.clone(),
                observed: request.model_profile.clone(),
            });
        }
        let (profile, model) = self
            .model_registry
            .resolve_profile_exact(&request.model_profile, &request.requested_model)?;
        ensure_budget_fits_model_context(&request.budget, model)?;
        Ok(ResolvedExecutionSnapshot {
            protocol_version: PROTOCOL_VERSION,
            run_id: request.run_id.clone(),
            fencing_token,
            cancel_generation,
            agent_image_hash: self.image_hash.clone(),
            deployment_binding_hash: self.binding_hash.clone(),
            model_registry_hash: self.registry_hash.clone(),
            budget_registry_hash: self.budget_registry_hash.clone(),
            model_profile: profile.profile_id.clone(),
            requested_model: request.requested_model.clone(),
            resolved_model: model.model_id.clone(),
            provider_api_version: model.api_version.clone(),
            provider_max_context_tokens: model.max_context_tokens,
            provider_wire_capabilities: model.provider_wire_capabilities,
            thinking: profile.thinking,
            reasoning_effort: profile.reasoning_effort,
            capability_release_hashes: self.capability_release_hashes.clone(),
            budget: request.budget.clone(),
        })
    }

    pub fn model(&self, model_id: &str) -> Option<&ModelDescriptor> {
        self.model_registry.model(model_id)
    }

    pub fn model_profile(&self, profile_id: &str) -> Option<&ModelExecutionProfile> {
        self.model_registry
            .profiles
            .iter()
            .find(|profile| profile.profile_id == profile_id)
    }

    /// Exact startup-validated models used to build a shared provider pool.
    pub fn models(&self) -> impl ExactSizeIterator<Item = &ModelDescriptor> {
        self.model_registry.models.iter()
    }
}

impl ResolvedReleaseSet {
    pub fn release_set_hash(&self) -> &ContentHash {
        &self.release_set_hash
    }

    pub fn model_registry_hash(&self) -> &ContentHash {
        &self.model_registry_hash
    }

    pub fn len(&self) -> usize {
        self.releases.len()
    }

    pub fn is_empty(&self) -> bool {
        self.releases.is_empty()
    }

    pub fn release(&self, image_hash: &ContentHash) -> Option<&Arc<ResolvedRelease>> {
        self.releases.get(image_hash)
    }

    pub fn releases(&self) -> impl ExactSizeIterator<Item = &Arc<ResolvedRelease>> {
        self.releases.values()
    }

    pub fn accepted_image_hashes(&self) -> Vec<ContentHash> {
        self.releases.keys().cloned().collect()
    }

    pub fn owner(&self, run_kind: &str, locale: &str) -> Option<&ContentHash> {
        self.entrypoint_owners
            .get(&(run_kind.to_owned(), locale.to_owned()))
    }

    pub fn models(&self) -> impl ExactSizeIterator<Item = &ModelDescriptor> {
        self.model_registry.models.iter()
    }

    /// Startup-owned credential for one provider kind, resolved once from the
    /// kind's `credential_ref` environment. Empty when the release set does
    /// not advertise that kind.
    pub fn provider_api_key(&self, kind: ProviderKind) -> &str {
        self.provider_api_keys
            .get(&kind)
            .map_or("", |key| key.as_str())
    }

    /// Produce the only host-visible deployment contract from the same
    /// immutable release set used by workers. This deliberately contains no
    /// endpoint, credential reference, API base, prompt, or secret material.
    pub fn public_descriptor(
        &self,
        runtime_version: &str,
    ) -> Result<PublicReleaseDescriptor, ConfigError> {
        if runtime_version.is_empty()
            || runtime_version.len() > 64
            || !runtime_version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(ConfigError::InvalidRuntimeVersion);
        }

        let mut entries = Vec::new();
        for release in self.releases.values() {
            for entrypoint in release.image.body.entrypoints.values() {
                let budget = release
                    .runtime
                    .budget_profiles
                    .get(&entrypoint.required_budget_profile)
                    .ok_or(ConfigError::BudgetProfileSetMismatch)?
                    .limits
                    .clone();
                let (profile, model) = release.runtime.model_registry.resolve_profile_exact(
                    &entrypoint.required_model_profile,
                    &release
                        .runtime
                        .model_profile(&entrypoint.required_model_profile)
                        .ok_or(ConfigError::ModelProfileSetMismatch)?
                        .model_id,
                )?;
                ensure_budget_fits_model_context(&budget, model)?;
                let cardinality = match entrypoint.scope.cardinality {
                    ScopeCardinality::Exact { .. } => ScopeCardinalityKind::Exact,
                    ScopeCardinality::Max { .. } => ScopeCardinalityKind::Max,
                };
                let execution = PinnedExecutionContract {
                    protocol_version: PROTOCOL_VERSION,
                    agent_image_hash: release.image.content_hash.clone(),
                    deployment_binding_hash: release.runtime.binding_hash.clone(),
                    model_registry_hash: release.runtime.registry_hash.clone(),
                    budget_registry_hash: release.runtime.budget_registry_hash.clone(),
                    model_profile: profile.profile_id.clone(),
                    requested_model: model.model_id.clone(),
                    resolved_model: model.model_id.clone(),
                    provider_api_version: model.api_version.clone(),
                    provider_max_context_tokens: model.max_context_tokens,
                    provider_wire_capabilities: model.provider_wire_capabilities,
                    thinking: profile.thinking,
                    reasoning_effort: profile.reasoning_effort,
                    capability_release_hashes: release.runtime.capability_release_hashes.clone(),
                    budget,
                };
                entries.push(PublicReleaseEntrypoint {
                    run_kind: entrypoint.run_kind.clone(),
                    locale: entrypoint.locale.clone(),
                    agent_image_hash: release.image.content_hash.clone(),
                    model_profile: profile.profile_id.clone(),
                    scope: EntrypointScope {
                        context_kind: entrypoint.scope.allowed_context,
                        cardinality,
                        value: entrypoint.scope.cardinality.value(),
                    },
                    execution,
                });
            }
        }
        entries.sort_by(|left, right| {
            (left.run_kind.as_str(), left.locale.as_str())
                .cmp(&(right.run_kind.as_str(), right.locale.as_str()))
        });
        Ok(PublicReleaseDescriptor {
            schema_version: PUBLIC_RELEASE_DESCRIPTOR_SCHEMA_VERSION,
            release_set_hash: self.release_set_hash.clone(),
            runtime_version: runtime_version.to_owned(),
            entries,
        })
    }
}

fn validate_model(model: &ModelDescriptor) -> Result<(), ConfigError> {
    // The provider kind is derived once through the protocol registry mapping
    // and selects the branch; each branch applies that provider's exact wire
    // contract. An id with no kind is rejected with the provider-neutral
    // unknown-model error instead of being classified as a drifted DeepSeek
    // or GLM deployment.
    match ProviderKind::for_model_id(&model.model_id) {
        Some(ProviderKind::Deepseek) => validate_deepseek_model(model),
        Some(ProviderKind::Glm) => validate_glm_model(model),
        None => Err(ConfigError::UnknownProviderModel(model.model_id.clone())),
    }
}

fn validate_deepseek_model(model: &ModelDescriptor) -> Result<(), ConfigError> {
    if model.api_base != DEEPSEEK_API_BASE
        || model.api_version != DEEPSEEK_API_VERSION
        || model.max_context_tokens != 1_000_000
        || model.max_output_tokens != 384_000
        || !(1..=2_500).contains(&model.max_in_flight)
        || !model.provider_wire_capabilities.is_well_formed()
        || model.provider_wire_capabilities != ProviderWireCapabilities::deepseek_v4_flash()
        || model.model_id != DEEPSEEK_MODEL_ID
    {
        return Err(ConfigError::InvalidDeepSeekModel(model.model_id.clone()));
    }
    Ok(())
}

fn validate_glm_model(model: &ModelDescriptor) -> Result<(), ConfigError> {
    // Same shape as the DeepSeek branch, but with the GLM-5.3 wire contract.
    // `max_in_flight` uses the same 1..=2_500 bound so the GLM codec never has
    // to reason about a different concurrency envelope than DeepSeek.
    if model.api_base != GLM_API_BASE
        || model.api_version != GLM_API_VERSION
        || model.max_context_tokens != GLM_MAX_CONTEXT_TOKENS
        || model.max_output_tokens != GLM_MAX_OUTPUT_TOKENS
        || !(1..=2_500).contains(&model.max_in_flight)
        || !model.provider_wire_capabilities.is_well_formed()
        || model.provider_wire_capabilities != ProviderWireCapabilities::glm_5_3()
        || model.model_id != GLM_MODEL_ID
    {
        return Err(ConfigError::InvalidGlmModel(model.model_id.clone()));
    }
    Ok(())
}

fn validate_model_profile(
    profile: &ModelExecutionProfile,
    model: &ModelDescriptor,
) -> Result<(), ConfigError> {
    // Same provider-dispatch pattern as `validate_model`: the kind derived
    // from the descriptor's model_id selects the branch, and each branch
    // keeps the exact per-profile semantic checks (thinking mode + reasoning
    // effort) that the deployment contract requires. The historical DeepSeek
    // behavior is preserved verbatim in its branch.
    match ProviderKind::for_model_id(&model.model_id) {
        Some(ProviderKind::Deepseek) => validate_deepseek_model_profile(profile, model),
        Some(ProviderKind::Glm) => validate_glm_model_profile(profile, model),
        None => Err(ConfigError::UnknownProviderModel(model.model_id.clone())),
    }
}

fn validate_deepseek_model_profile(
    profile: &ModelExecutionProfile,
    model: &ModelDescriptor,
) -> Result<(), ConfigError> {
    let exact_profile =
        matches!(
            profile.profile_id.as_str(),
            GLM_HIGH_PROFILE_ID | GLM_MAX_PROFILE_ID | GLM_DIRECT_PROFILE_ID
        ) && profile_semantics(&profile.profile_id).is_some_and(|(thinking, reasoning_effort)| {
            profile.model_id == DEEPSEEK_MODEL_ID
                && profile.thinking == thinking
                && profile.reasoning_effort == reasoning_effort
        });
    if model.model_id != DEEPSEEK_MODEL_ID
        || !model
            .provider_wire_capabilities
            .for_thinking(profile.thinking)
            .supported
        || !exact_profile
    {
        return Err(ConfigError::InvalidModelProfile(profile.profile_id.clone()));
    }
    Ok(())
}

fn validate_glm_model_profile(
    profile: &ModelExecutionProfile,
    model: &ModelDescriptor,
) -> Result<(), ConfigError> {
    // Mirror of the DeepSeek profile contract, but only the shared `glm_*`
    // slots are admitted for a GLM registry. This keeps the public image
    // contract stable while preventing a provider-specific alias from being
    // silently reused for the wrong model.
    let exact_profile =
        matches!(
            profile.profile_id.as_str(),
            GLM_HIGH_PROFILE_ID | GLM_MAX_PROFILE_ID | GLM_DIRECT_PROFILE_ID
        ) && profile_semantics(&profile.profile_id).is_some_and(|(thinking, reasoning_effort)| {
            profile.model_id == GLM_MODEL_ID
                && profile.thinking == thinking
                && profile.reasoning_effort == reasoning_effort
        });
    if model.model_id != GLM_MODEL_ID
        || !model
            .provider_wire_capabilities
            .for_thinking(profile.thinking)
            .supported
        || !exact_profile
    {
        return Err(ConfigError::InvalidModelProfile(profile.profile_id.clone()));
    }
    Ok(())
}

fn profile_semantics(profile_id: &str) -> Option<(ThinkingMode, Option<ReasoningEffort>)> {
    match profile_id {
        GLM_HIGH_PROFILE_ID => Some((ThinkingMode::Enabled, Some(ReasoningEffort::High))),
        GLM_MAX_PROFILE_ID => Some((ThinkingMode::Enabled, Some(ReasoningEffort::Max))),
        GLM_DIRECT_PROFILE_ID => Some((ThinkingMode::Disabled, None)),
        _ => None,
    }
}

fn validate_versions(
    binding: &DeploymentBinding,
    registry: &ModelRegistry,
    budget: &BudgetRegistry,
    endpoints: &EndpointRegistry,
) -> Result<(), ConfigError> {
    if binding.schema_version != DEPLOYMENT_BINDING_SCHEMA_VERSION
        || registry.schema_version != MODEL_REGISTRY_SCHEMA_VERSION
        || budget.schema_version != CONFIG_SCHEMA_VERSION
        || endpoints.schema_version != CONFIG_SCHEMA_VERSION
    {
        return Err(ConfigError::UnsupportedConfigVersion);
    }
    Ok(())
}

fn validate_binding(binding: &CapabilityBinding, mode: ValidationMode) -> Result<(), ConfigError> {
    if binding.max_connections == 0
        || binding.request_timeout_ms == 0
        || !valid_mcp_tool_name(&binding.mcp_tool_name)
    {
        return Err(ConfigError::InvalidCapabilityBinding(
            binding.binding_key.clone(),
        ));
    }
    if mode.requires_production_integrity()
        && (binding.server_schema_bundle_hash.as_str() == ZERO_HASH
            || binding.data_release_hash.as_str() == ZERO_HASH
            || binding.server_build == "fixture")
    {
        return Err(ConfigError::FixtureFingerprintInProduction(
            binding.binding_key.clone(),
        ));
    }
    if binding.auth_scope == AuthScope::Public && binding.credential_ref.is_some() {
        return Err(ConfigError::PublicCapabilityHasCredential(
            binding.binding_key.clone(),
        ));
    }
    if let Some(name) = &binding.credential_ref {
        validate_env_name(name)?;
    }
    Ok(())
}

fn valid_mcp_tool_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn validate_endpoint_descriptor(descriptor: &EndpointDescriptor) -> Result<(), ConfigError> {
    validate_env_name(&descriptor.url_env)?;
    validate_env_name(&descriptor.readiness_url_env)?;
    let bounded_token = |value: &str, max: usize| {
        !value.is_empty()
            && value.len() <= max
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    };
    if descriptor.endpoint_ref.is_empty()
        || descriptor.endpoint_ref.len() > 128
        || !bounded_token(&descriptor.protocol_version, 64)
        || !bounded_token(&descriptor.credential_version, 128)
        || !bounded_token(&descriptor.tls_profile, 64)
        || !descriptor.origin.starts_with("https://")
        || descriptor.origin.len() > 2_048
    {
        return Err(ConfigError::InvalidEndpointDescriptor(
            descriptor.endpoint_ref.clone(),
        ));
    }
    match (
        descriptor.tls_profile.as_str(),
        descriptor.tls_ca_pem_env.as_deref(),
    ) {
        (TLS_PROFILE_SYSTEM_ROOTS_V1, None) => {}
        (TLS_PROFILE_SYSTEM_PLUS_PINNED_CA_V1, Some(certificate_env)) => {
            validate_env_name(certificate_env)?;
        }
        _ => {
            return Err(ConfigError::InvalidEndpointDescriptor(
                descriptor.endpoint_ref.clone(),
            ));
        }
    }
    Ok(())
}

fn validate_env_name(name: &str) -> Result<(), ConfigError> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        || !name.as_bytes()[0].is_ascii_uppercase()
    {
        return Err(ConfigError::InvalidSecretReference(name.into()));
    }
    Ok(())
}

fn validate_budget(limits: &BudgetLimits, source: &'static str) -> Result<(), ConfigError> {
    if limits.max_provider_turns == 0
        || limits.max_capability_calls == 0
        || limits.max_input_tokens == 0
        || limits.max_output_tokens == 0
        || limits.max_evidence_bytes == 0
        || limits.deadline_ms == 0
        || limits
            .capability_call_limits
            .values()
            .any(|limit| *limit == 0)
    {
        return Err(ConfigError::InvalidBudget(source));
    }
    Ok(())
}

fn ensure_exact_budget(request: &BudgetLimits, profile: &BudgetLimits) -> Result<(), ConfigError> {
    if request != profile {
        return Err(ConfigError::RunBudgetProfileMismatch);
    }
    Ok(())
}

fn ensure_budget_fits_model_context(
    limits: &BudgetLimits,
    model: &ModelDescriptor,
) -> Result<(), ConfigError> {
    let total = u64::from(limits.max_input_tokens)
        .checked_add(u64::from(limits.max_output_tokens))
        .ok_or(ConfigError::RunBudgetExceedsModelContext)?;
    if limits.max_output_tokens > model.max_output_tokens
        || total > u64::from(model.max_context_tokens)
    {
        return Err(ConfigError::RunBudgetExceedsModelContext);
    }
    Ok(())
}

impl Drop for ResolvedCapability {
    fn drop(&mut self) {
        self.endpoint.zeroize();
        self.readiness_endpoint.zeroize();
        self.origin.zeroize();
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("YAML failed: {0}")]
    Yaml(#[from] serde_yaml_ng::Error),
    #[error("canonical JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("canonical contract registry mismatch: {0}")]
    CanonicalContractRegistry(String),
    #[error(transparent)]
    Image(#[from] ImageError),
    #[error(transparent)]
    Contract(#[from] krw_agent_protocol::ContractError),
    #[error("unsupported deployment config schema version")]
    UnsupportedConfigVersion,
    #[error("unknown run kind: {0}")]
    UnknownRunKind(String),
    #[error("budget registry has no profile or repeats a profile id")]
    DuplicateOrEmptyBudgetProfile,
    #[error("budget registry profile set differs from AgentImage entrypoint requirements")]
    BudgetProfileSetMismatch,
    #[error("locale mismatch: required {required}, observed {observed}")]
    LocaleMismatch { required: String, observed: String },
    #[error("run context violates the selected AgentImage entrypoint scope")]
    RunContextPolicyMismatch,
    #[error("invalid {0} budget")]
    InvalidBudget(&'static str),
    #[error("run budget must exactly match its AgentImage deployment profile")]
    RunBudgetProfileMismatch,
    #[error("run budget exceeds the selected model's context or output capacity")]
    RunBudgetExceedsModelContext,
    #[error("model is not an exact supported DeepSeek deployment: {0}")]
    InvalidDeepSeekModel(String),
    #[error("model is not an exact supported GLM deployment: {0}")]
    InvalidGlmModel(String),
    #[error("model id has no provider kind in the compiled registry: {0}")]
    UnknownProviderModel(String),
    #[error("model registry has no model or repeats a model id")]
    DuplicateOrEmptyModel,
    #[error("model registry contains a model outside the pinned provider set")]
    ModelInventoryMismatch,
    #[error("model registry has no execution profile or repeats a profile id")]
    DuplicateOrEmptyModelProfile,
    #[error("model registry is missing or contains an invalid provider execution profile")]
    ModelProfileInventoryMismatch,
    #[error("model execution profile references an unknown model: {0}")]
    UnknownProfileModel(String),
    #[error("invalid model execution profile: {0}")]
    InvalidModelProfile(String),
    #[error("model profile set differs from AgentImage entrypoint requirements")]
    ModelProfileSetMismatch,
    #[error("entrypoint requires model profile {required}, observed {observed}")]
    EntrypointModelProfileMismatch { required: String, observed: String },
    #[error("agent image repeats a run kind")]
    DuplicateRunKind,
    #[error("startup release set must contain 1..=64 image directories")]
    ReleaseImageCount,
    #[error("startup release set repeats agent identity: {0}")]
    DuplicateAgentIdentity(String),
    #[error("startup release set repeats ownership of ({run_kind}, {locale})")]
    DuplicateEntrypointOwnership { run_kind: String, locale: String },
    #[error("agent image differs from the image validated at startup")]
    ImageChangedAfterStartup,
    #[error("agent image capability set differs from deployment binding")]
    CapabilitySetMismatch,
    #[error("duplicate capability binding")]
    DuplicateCapabilityBinding,
    #[error("AgentImage logical capability is duplicated: {0}")]
    DuplicateLogicalCapability(String),
    #[error("AgentImage references an unresolved physical binding key: {0}")]
    MissingBindingKey(String),
    #[error("duplicate endpoint descriptor")]
    DuplicateEndpoint,
    #[error("invalid endpoint descriptor: {0}")]
    InvalidEndpointDescriptor(String),
    #[error("invalid binding for capability: {0}")]
    InvalidCapabilityBinding(String),
    #[error("fixture fingerprint is forbidden in production for capability: {0}")]
    FixtureFingerprintInProduction(String),
    #[error("public capability unexpectedly has a credential: {0}")]
    PublicCapabilityHasCredential(String),
    #[error("unknown endpoint reference: {0}")]
    UnknownEndpoint(String),
    #[error("resolved endpoint must use HTTPS: {0}")]
    InsecureEndpoint(String),
    #[error("resolved readiness endpoint must use HTTPS: {0}")]
    InsecureReadinessEndpoint(String),
    #[error("invalid secret/environment reference: {0}")]
    InvalidSecretReference(String),
    #[error("missing required secret/environment value: {0}")]
    MissingSecret(String),
    #[error("runtime version is not a bounded public identifier")]
    InvalidRuntimeVersion,
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use super::*;
    use krw_agent_image::{PromptBlobInterner, compile_agent_dir};
    use krw_agent_protocol::RunContextV1;

    #[derive(Debug, Clone, Default)]
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

    #[derive(Debug)]
    struct CountingSecrets {
        values: BTreeMap<String, String>,
        reads: Mutex<BTreeMap<String, usize>>,
    }

    impl CountingSecrets {
        fn fixture() -> Self {
            Self {
                values: global_secret_values(),
                reads: Mutex::new(BTreeMap::new()),
            }
        }

        fn assert_read_once(&self) {
            let reads = self.reads.lock().unwrap();
            assert!(reads.values().all(|count| *count == 1));
            assert!(reads.keys().all(|name| self.values.contains_key(name)));
        }
    }

    fn global_secret_values() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("DEEPSEEK_API_KEY".into(), "fixture-deepseek".into()),
            ("GLM_API_KEY".into(), "fixture-glm".into()),
            (
                "KRW_ONTOLOGY_MCP_URL".into(),
                "https://ontology.invalid/mcp".into(),
            ),
            (
                "KRW_ONTOLOGY_READY_URL".into(),
                "https://ontology.invalid/readyz".into(),
            ),
            ("KRW_FEED_MCP_URL".into(), "https://feed.invalid/mcp".into()),
            (
                "KRW_FEED_MCP_READY_URL".into(),
                "https://feed.invalid/readyz".into(),
            ),
            ("KRW_FEED_MCP_TOKEN".into(), "feed-token".into()),
            (
                "KRW_FILINGS_MCP_URL".into(),
                "https://filings.invalid/mcp".into(),
            ),
            (
                "KRW_FILINGS_MCP_READY_URL".into(),
                "https://filings.invalid/readyz".into(),
            ),
            ("KRW_FILINGS_MCP_TOKEN".into(), "filings-token".into()),
            (
                "KRW_OPENBB_MCP_URL".into(),
                "https://openbb.invalid/mcp".into(),
            ),
            (
                "KRW_OPENBB_MCP_READY_URL".into(),
                "https://openbb.invalid/readyz".into(),
            ),
        ])
    }

    fn global_fixture_secrets() -> FixtureSecrets {
        FixtureSecrets(global_secret_values())
    }

    impl SecretSource for CountingSecrets {
        fn read_secret(&self, name: &str) -> Result<Zeroizing<String>, ConfigError> {
            *self
                .reads
                .lock()
                .unwrap()
                .entry(name.to_owned())
                .or_default() += 1;
            self.values
                .get(name)
                .cloned()
                .map(Zeroizing::new)
                .ok_or_else(|| ConfigError::MissingSecret(name.into()))
        }
    }

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn fixture() -> (
        AgentImageManifest,
        DeploymentBinding,
        ModelRegistry,
        BudgetRegistry,
        EndpointRegistry,
        RunRequest,
        FixtureSecrets,
    ) {
        let root = root();
        let image = compile_agent_dir(root.join("agents/krw-ontology"))
            .unwrap()
            .manifest;
        let binding =
            load_yaml(root.join("deployments/local/deployment-binding.krw-ontology.example.yaml"))
                .unwrap();
        let registry = load_yaml(root.join("deployments/local/model-registry.glm.yaml")).unwrap();
        let budget: BudgetRegistry =
            load_yaml(root.join("deployments/local/budget-registry.yaml")).unwrap();
        // The canonical endpoint template ships the same three local endpoints
        // (ontology, feed, filings) the deployment binding example references,
        // so fixture resolution validates the real registry document instead
        // of a diverging hardcoded copy.
        let endpoint_registry: EndpointRegistry =
            load_yaml(root.join("deployments/local/endpoint-registry.example.yaml")).unwrap();
        let request = serde_json::from_slice(
            &fs::read(root.join("fixtures/vertical-slice/v1/run-request.json")).unwrap(),
        )
        .unwrap();
        let secrets = global_fixture_secrets();
        (
            image,
            binding,
            registry,
            budget,
            endpoint_registry,
            request,
            secrets,
        )
    }

    fn all_images() -> Vec<LoadedImage> {
        let mut interner = PromptBlobInterner::default();
        [
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
            compile_agent_dir(root().join("agents").join(package))
                .unwrap()
                .into_loaded_with_interner(&mut interner)
                .unwrap()
        })
        .collect()
    }

    fn global_fixture() -> (
        DeploymentBinding,
        ModelRegistry,
        BudgetRegistry,
        EndpointRegistry,
    ) {
        let root = root();
        (
            load_yaml(root.join("deployments/local/deployment-binding.example.yaml")).unwrap(),
            load_yaml(root.join("deployments/local/model-registry.glm.yaml")).unwrap(),
            load_yaml(root.join("deployments/local/budget-registry.yaml")).unwrap(),
            load_yaml(root.join("deployments/local/endpoint-registry.example.yaml")).unwrap(),
        )
    }

    fn binding_mut<'a>(
        binding: &'a mut DeploymentBinding,
        binding_key: &str,
    ) -> &'a mut CapabilityBinding {
        binding
            .capabilities
            .iter_mut()
            .find(|capability| capability.binding_key == binding_key)
            .unwrap()
    }

    fn endpoint_mut<'a>(
        registry: &'a mut EndpointRegistry,
        endpoint_ref: &str,
    ) -> &'a mut EndpointDescriptor {
        registry
            .endpoints
            .iter_mut()
            .find(|endpoint| endpoint.endpoint_ref == endpoint_ref)
            .unwrap()
    }

    /// The curated openbb endpoint feeds the daemon's startup tools/list
    /// preflight exactly like feed/filings: the resolved release set carries
    /// the bound openbb tool names grouped under one endpoint, so the
    /// preflight rejects a server that does not advertise them. This test
    /// pins the data the preflight consumes (endpoint_ref, exact MCP tool
    /// names, no bearer on the local no-credential profile) and proves the
    /// fail-closed path when the endpoint registry omits the openbb entry.
    #[test]
    fn openbb_endpoint_resolves_the_fourteen_curated_tools_for_preflight() {
        let (image, binding, registry, budget, endpoints, _request, secrets) = fixture();
        let runtime = resolve_runtime(
            &image,
            &binding,
            &registry,
            &budget,
            &endpoints,
            &secrets,
            ValidationMode::Fixture,
        )
        .unwrap();

        let expected = [
            (
                "openbb.price_history",
                "openbb_equity_price_historical",
                "equity_price_historical",
            ),
            (
                "openbb.macro_series",
                "openbb_economy_fred_series",
                "economy_fred_series",
            ),
            ("openbb.macro_cpi", "openbb_economy_cpi", "economy_cpi"),
            ("openbb.quote", "openbb_equity_price_quote", "equity_price_quote"),
            (
                "openbb.metrics",
                "openbb_equity_fundamental_metrics",
                "equity_fundamental_metrics",
            ),
            (
                "openbb.income_statement",
                "openbb_equity_fundamental_income",
                "equity_fundamental_income",
            ),
            (
                "openbb.balance_statement",
                "openbb_equity_fundamental_balance",
                "equity_fundamental_balance",
            ),
            (
                "openbb.cash_statement",
                "openbb_equity_fundamental_cash",
                "equity_fundamental_cash",
            ),
            (
                "openbb.consensus",
                "openbb_equity_estimates_consensus",
                "equity_estimates_consensus",
            ),
            (
                "openbb.peers",
                "openbb_equity_compare_peers",
                "equity_compare_peers",
            ),
            (
                "openbb.earnings_calendar",
                "openbb_equity_calendar_earnings",
                "equity_calendar_earnings",
            ),
            (
                "openbb.yield_curve",
                "openbb_fixedincome_government_yield_curve",
                "fixedincome_government_yield_curve",
            ),
            (
                "openbb.macro_calendar",
                "openbb_economy_calendar",
                "economy_calendar",
            ),
            (
                "openbb.filings",
                "openbb_equity_fundamental_filings",
                "equity_fundamental_filings",
            ),
        ];
        for (capability_id, binding_key, tool_name) in expected {
            let resolved = runtime
                .capabilities
                .get(capability_id)
                .unwrap_or_else(|| panic!("{capability_id} resolved capability"));
            assert_eq!(resolved.binding.binding_key, binding_key);
            // The exact name the daemon preflight requires in tools/list.
            assert_eq!(resolved.binding.mcp_tool_name, tool_name);
            assert_eq!(resolved.binding.endpoint_ref, "krw-openbb-local");
            // Local openbb is unauthenticated: the honest profile carries no
            // bearer credential for these bindings.
            assert!(resolved.bearer_token.is_none());
            assert_eq!(resolved.credential_version, "openbb-public-v1");
        }

        // Fail closed: an endpoint registry without the openbb entry must
        // refuse resolution instead of silently skipping the new tools.
        let mut missing = endpoints.clone();
        missing
            .endpoints
            .retain(|endpoint| endpoint.endpoint_ref != "krw-openbb-local");
        let outcome = resolve_runtime(
            &image,
            &binding,
            &registry,
            &budget,
            &missing,
            &secrets,
            ValidationMode::Fixture,
        );
        assert!(matches!(
            outcome,
            Err(ConfigError::UnknownEndpoint(endpoint)) if endpoint == "krw-openbb-local"
        ));
    }

    #[test]
    fn resolves_an_immutable_snapshot_without_embedding_aliases_or_urls() {
        let (image, binding, registry, budget, endpoints, request, secrets) = fixture();
        let runtime = resolve_runtime(
            &image,
            &binding,
            &registry,
            &budget,
            &endpoints,
            &secrets,
            ValidationMode::Fixture,
        )
        .unwrap();
        let snapshot = runtime
            .resolve_run(&image.content_hash, &request, 9, 0)
            .unwrap();
        assert_eq!(snapshot.requested_model, GLM_MODEL_ID);
        assert_eq!(snapshot.resolved_model, GLM_MODEL_ID);
        assert_eq!(snapshot.fencing_token, 9);
        assert_eq!(
            runtime.capabilities.len(),
            image
                .body
                .capabilities
                .iter()
                .filter(|capability| capability.remote_binding_key().is_some())
                .count()
        );
        assert_eq!(
            runtime.physical_binding_count(),
            35,
            "universe aliases share the query bindings while local skill loading has no physical deployment binding; the filing/news ladder adds four, the observation series tools two, the curated openbb endpoint fourteen base bindings, and round 4/5 add company news plus the eight fmp estimate/mix/performance/profile/insider lanes"
        );
        assert!(Arc::ptr_eq(
            runtime.capabilities.get("ontology.query_context").unwrap(),
            runtime
                .capabilities
                .get("ontology.query_context_universe")
                .unwrap(),
        ));
        assert!(!format!("{runtime:?}").contains("fixture-glm"));
        assert!(!format!("{runtime:?}").contains("ontology.invalid"));
    }

    #[test]
    fn local_skill_loading_requires_no_physical_deployment_binding() {
        let (image, mut binding, registry, budget, endpoints, _request, secrets) = fixture();
        binding
            .capabilities
            .retain(|capability| capability.binding_key != "krw_skill_local");

        let runtime = resolve_runtime(
            &image,
            &binding,
            &registry,
            &budget,
            &endpoints,
            &secrets,
            ValidationMode::Fixture,
        )
        .expect("local skill.load must not depend on a physical MCP deployment binding");

        assert!(!runtime.capabilities.contains_key("skill.load"));
        // 26 base physical bindings + openbb news (round 4) + the eight
        // round-5 fmp lanes (revenue mix x2, performance, profile, insider,
        // forward eps/ebitda, historical estimates) = 35.
        assert_eq!(runtime.physical_binding_count(), 35);
    }

    /// The observation series bindings are held to the same fail-closed
    /// deployment contract as every other physical tool: dropping any
    /// required field must reject the whole binding document at load time.
    #[test]
    fn observation_series_bindings_fail_closed_on_missing_required_fields() {
        let text = fs::read_to_string(
            root().join("deployments/local/deployment-binding.krw-ontology.example.yaml"),
        )
        .unwrap();
        for binding_key in ["krw_market_series", "krw_macro_series"] {
            for required_field in [
                "mcp_tool_name",
                "endpoint_ref",
                "tool_session_reuse",
                "server_schema_bundle_hash",
                "server_build",
                "data_release_hash",
                "max_connections",
                "request_timeout_ms",
            ] {
                let mut in_block = false;
                let mut removed = false;
                let doctored = text
                    .lines()
                    .filter(|line| {
                        if line.starts_with("  - binding_key: ") {
                            in_block = line == &format!("  - binding_key: {binding_key}");
                            return true;
                        }
                        if in_block && line.starts_with("  - binding_key: ") {
                            in_block = false;
                        }
                        if in_block && line.trim_start().starts_with(required_field) {
                            removed = true;
                            return false;
                        }
                        true
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(
                    removed,
                    "fixture must still contain {binding_key}.{required_field}"
                );
                assert!(
                    serde_yaml_ng::from_str::<DeploymentBinding>(&doctored).is_err(),
                    "{binding_key} without {required_field} must fail closed at load time"
                );
            }
        }
    }

    #[test]
    fn observation_series_bindings_resolve_and_are_mandatory_for_the_image() {
        let (image, binding, registry, budget, endpoints, _request, secrets) = fixture();

        let runtime = resolve_runtime(
            &image,
            &binding,
            &registry,
            &budget,
            &endpoints,
            &secrets,
            ValidationMode::Fixture,
        )
        .unwrap();
        for capability_id in ["market.series", "macro.series"] {
            let resolved = runtime
                .capabilities
                .get(capability_id)
                .expect("observation series capability must resolve a physical binding");
            assert_eq!(resolved.binding.endpoint_ref, "krw-ontology-local");
            assert_eq!(resolved.binding.server_build, "fixture");
        }

        let mut missing_series = binding.clone();
        missing_series
            .capabilities
            .retain(|capability| capability.binding_key != "krw_market_series");
        assert!(matches!(
            resolve_runtime(
                &image,
                &missing_series,
                &registry,
                &budget,
                &endpoints,
                &secrets,
                ValidationMode::Fixture,
            ),
            Err(ConfigError::MissingBindingKey(key)) if key == "krw_market_series"
        ));

        let mut degenerate = binding.clone();
        binding_mut(&mut degenerate, "krw_macro_series").max_connections = 0;
        assert!(matches!(
            resolve_runtime(
                &image,
                &degenerate,
                &registry,
                &budget,
                &endpoints,
                &secrets,
                ValidationMode::Fixture,
            ),
            Err(ConfigError::InvalidCapabilityBinding(key)) if key == "krw_macro_series"
        ));
    }

    #[test]
    fn startup_accepts_the_glm_profile_triad_and_no_aliases() {
        let (image, binding, registry, budget, endpoints, _request, secrets) = fixture();

        let mut extra_model = registry.clone();
        let mut forbidden = extra_model.models[0].clone();
        forbidden.model_id = "forbidden-provider-model".into();
        extra_model.models.push(forbidden);
        assert!(matches!(
            resolve_runtime(
                &image,
                &binding,
                &extra_model,
                &budget,
                &endpoints,
                &secrets,
                ValidationMode::Fixture,
            ),
            Err(ConfigError::ModelInventoryMismatch)
        ));

        let mut extra_profile = registry.clone();
        let mut alias = extra_profile.profiles[0].clone();
        alias.profile_id = "flash_alias".into();
        extra_profile.profiles.push(alias);
        assert!(matches!(
            resolve_runtime(
                &image,
                &binding,
                &extra_profile,
                &budget,
                &endpoints,
                &secrets,
                ValidationMode::Fixture,
            ),
            Err(ConfigError::ModelProfileInventoryMismatch)
        ));

        let mut missing_profile = registry.clone();
        missing_profile
            .profiles
            .retain(|profile| profile.profile_id != GLM_DIRECT_PROFILE_ID);
        assert!(matches!(
            resolve_runtime(
                &image,
                &binding,
                &missing_profile,
                &budget,
                &endpoints,
                &secrets,
                ValidationMode::Fixture,
            ),
            Err(ConfigError::ModelProfileInventoryMismatch)
        ));

        let mut semantic_alias = registry;
        semantic_alias
            .profiles
            .iter_mut()
            .find(|profile| profile.profile_id == GLM_HIGH_PROFILE_ID)
            .unwrap()
            .reasoning_effort = Some(ReasoningEffort::Max);
        assert!(matches!(
            resolve_runtime(
                &image,
                &binding,
                &semantic_alias,
                &budget,
                &endpoints,
                &secrets,
                ValidationMode::Fixture,
            ),
            Err(ConfigError::InvalidModelProfile(profile)) if profile == GLM_HIGH_PROFILE_ID
        ));
    }

    #[test]
    fn deepseek_rejects_retired_flash_profile_aliases() {
        let registry: ModelRegistry =
            load_yaml(root().join("deployments/local/model-registry.deepseek.yaml")).unwrap();
        let model = registry.model(DEEPSEEK_MODEL_ID).unwrap();
        let retired = ModelExecutionProfile {
            profile_id: "flash_high".into(),
            model_id: DEEPSEEK_MODEL_ID.into(),
            thinking: ThinkingMode::Enabled,
            reasoning_effort: Some(ReasoningEffort::High),
        };

        assert!(matches!(
            validate_model_profile(&retired, model),
            Err(ConfigError::InvalidModelProfile(profile)) if profile == "flash_high"
        ));
    }

    /// GLM-5.3 model and profile validation is the per-provider mirror of the
    /// `DeepSeek` branch. This test exercises `validate_model` and
    /// `validate_model_profile` directly, including the exact published GLM
    /// context and output limits.
    #[test]
    fn glm_model_and_profile_validation_mirrors_deepseek_shape() {
        let glm_model = ModelDescriptor {
            model_id: GLM_MODEL_ID.into(),
            api_base: GLM_API_BASE.into(),
            api_version: GLM_API_VERSION.into(),
            max_context_tokens: GLM_MAX_CONTEXT_TOKENS,
            max_output_tokens: GLM_MAX_OUTPUT_TOKENS,
            max_in_flight: 64,
            provider_wire_capabilities: ProviderWireCapabilities::glm_5_3(),
        };
        validate_model(&glm_model).expect("well-formed GLM model validates");

        // Every drifted field surfaces the GLM-specific error variant so the
        // DeepSeek error path stays a DeepSeek-only signal.
        let mut bad_base = glm_model.clone();
        bad_base.api_base = DEEPSEEK_API_BASE.into();
        assert!(matches!(
            validate_model(&bad_base),
            Err(ConfigError::InvalidGlmModel(_))
        ));

        let mut bad_caps = glm_model.clone();
        bad_caps.provider_wire_capabilities = ProviderWireCapabilities::deepseek_v4_flash();
        assert!(matches!(
            validate_model(&bad_caps),
            Err(ConfigError::InvalidGlmModel(_))
        ));

        let mut over_context = glm_model.clone();
        over_context.max_context_tokens = GLM_MAX_CONTEXT_TOKENS - 1;
        assert!(matches!(
            validate_model(&over_context),
            Err(ConfigError::InvalidGlmModel(_))
        ));

        let mut over_output = glm_model.clone();
        over_output.max_output_tokens = GLM_MAX_OUTPUT_TOKENS - 1;
        assert!(matches!(
            validate_model(&over_output),
            Err(ConfigError::InvalidGlmModel(_))
        ));

        // The three GLM semantic profiles mirror DeepSeek's high/max/direct.
        let glm_high = ModelExecutionProfile {
            profile_id: GLM_HIGH_PROFILE_ID.into(),
            model_id: GLM_MODEL_ID.into(),
            thinking: ThinkingMode::Enabled,
            reasoning_effort: Some(ReasoningEffort::High),
        };
        validate_model_profile(&glm_high, &glm_model).expect("glm_high validates");

        let glm_max = ModelExecutionProfile {
            profile_id: GLM_MAX_PROFILE_ID.into(),
            model_id: GLM_MODEL_ID.into(),
            thinking: ThinkingMode::Enabled,
            reasoning_effort: Some(ReasoningEffort::Max),
        };
        validate_model_profile(&glm_max, &glm_model).expect("glm_max validates");

        let glm_direct = ModelExecutionProfile {
            profile_id: GLM_DIRECT_PROFILE_ID.into(),
            model_id: GLM_MODEL_ID.into(),
            thinking: ThinkingMode::Disabled,
            reasoning_effort: None,
        };
        validate_model_profile(&glm_direct, &glm_model).expect("glm_direct validates");

        // A GLM profile that does not match one of the three pinned shapes is
        // rejected, exactly like an unknown DeepSeek profile id.
        let mut wrong_effort = glm_high.clone();
        wrong_effort.reasoning_effort = Some(ReasoningEffort::Max);
        assert!(matches!(
            validate_model_profile(&wrong_effort, &glm_model),
            Err(ConfigError::InvalidModelProfile(profile))
                if profile == GLM_HIGH_PROFILE_ID
        ));

        // A GLM profile bound to the wrong model_id is rejected.
        let mut cross_model = glm_high.clone();
        cross_model.model_id = DEEPSEEK_MODEL_ID.into();
        assert!(matches!(
            validate_model_profile(&cross_model, &glm_model),
            Err(ConfigError::InvalidModelProfile(profile))
                if profile == GLM_HIGH_PROFILE_ID
        ));
    }

    /// Unknown model ids are rejected with a provider-neutral error: an
    /// unrecognised provider is never classified as a drifted DeepSeek (or
    /// GLM) deployment. The provider kind mapping is the single authority.
    #[test]
    fn unknown_model_id_is_rejected_with_a_provider_neutral_error() {
        let mut unknown = ModelDescriptor {
            model_id: DEEPSEEK_MODEL_ID.into(),
            api_base: DEEPSEEK_API_BASE.into(),
            api_version: DEEPSEEK_API_VERSION.into(),
            max_context_tokens: 1_000_000,
            max_output_tokens: 384_000,
            max_in_flight: 64,
            provider_wire_capabilities: ProviderWireCapabilities::deepseek_v4_flash(),
        };
        unknown.model_id = "qwen-3.5".into();
        assert!(matches!(
            validate_model(&unknown),
            Err(ConfigError::UnknownProviderModel(model)) if model == "qwen-3.5"
        ));
    }

    #[test]
    fn deployment_binding_v3_is_rejected_after_transport_selector_removal() {
        let (image, mut binding, registry, budget, endpoints, _request, secrets) = fixture();
        binding.schema_version = 3;
        assert!(matches!(
            resolve_runtime(
                &image,
                &binding,
                &registry,
                &budget,
                &endpoints,
                &secrets,
                ValidationMode::Fixture,
            ),
            Err(ConfigError::UnsupportedConfigVersion)
        ));
    }

    #[test]
    fn production_refuses_unpinned_fixture_fingerprints() {
        let (image, binding, registry, budget, endpoints, _request, secrets) = fixture();
        assert!(matches!(
            resolve_runtime(
                &image,
                &binding,
                &registry,
                &budget,
                &endpoints,
                &secrets,
                ValidationMode::Production,
            ),
            Err(ConfigError::FixtureFingerprintInProduction(_))
        ));
    }

    #[test]
    fn host_cannot_change_the_deployment_budget_or_model_profile() {
        let (image, binding, registry, budget, endpoints, mut request, secrets) = fixture();
        let runtime = resolve_runtime(
            &image,
            &binding,
            &registry,
            &budget,
            &endpoints,
            &secrets,
            ValidationMode::Fixture,
        )
        .unwrap();
        request.budget.max_provider_turns += 1;
        assert!(matches!(
            runtime.resolve_run(&image.content_hash, &request, 9, 0),
            Err(ConfigError::RunBudgetProfileMismatch)
        ));

        let (_, _, _, _, _, mut request, _) = fixture();
        request.budget.max_provider_turns -= 1;
        assert!(matches!(
            runtime.resolve_run(&image.content_hash, &request, 9, 0),
            Err(ConfigError::RunBudgetProfileMismatch)
        ));

        let (_, _, _, _, _, mut request, _) = fixture();
        request.model_profile = "glm_max".into();
        assert!(matches!(
            runtime.resolve_run(&image.content_hash, &request, 9, 0),
            Err(ConfigError::EntrypointModelProfileMismatch { .. })
        ));
    }

    #[test]
    fn resolve_run_rejects_budget_that_exceeds_the_bound_model_context() {
        let (image, binding, registry, mut budget, endpoints, mut request, secrets) = fixture();
        let limits = {
            let profile = budget
                .profiles
                .iter_mut()
                .find(|profile| profile.profile_id == "company_research_glm")
                .unwrap();
            profile.limits.max_input_tokens = 200_000;
            profile.limits.clone()
        };
        request.budget = limits;
        let runtime = resolve_runtime(
            &image,
            &binding,
            &registry,
            &budget,
            &endpoints,
            &secrets,
            ValidationMode::Fixture,
        )
        .unwrap();

        assert!(matches!(
            runtime.resolve_run(&image.content_hash, &request, 9, 0),
            Err(ConfigError::RunBudgetExceedsModelContext)
        ));
    }

    #[test]
    fn resolve_run_rejects_context_kind_case_and_duplicates() {
        let (image, binding, registry, budget, endpoints, request, secrets) = fixture();
        let runtime = resolve_runtime(
            &image,
            &binding,
            &registry,
            &budget,
            &endpoints,
            &secrets,
            ValidationMode::Fixture,
        )
        .unwrap();

        for context in [
            RunContextV1::QuestionOnly {},
            RunContextV1::CompanyTickerSet {
                tickers: vec!["aapl".into()],
            },
            RunContextV1::CompanyTickerSet {
                tickers: vec!["AAPL".into(), "AAPL".into()],
            },
        ] {
            let mut invalid = request.clone();
            invalid.context = context;
            assert!(matches!(
                runtime.resolve_run(&image.content_hash, &invalid, 9, 0),
                Err(ConfigError::RunContextPolicyMismatch)
            ));
        }
    }

    #[test]
    fn all_checked_in_images_form_one_deterministic_shared_release_set() {
        let mut images = all_images();
        images.push(images[0].clone());
        let (binding, models, budgets, endpoints) = global_fixture();
        let secrets = CountingSecrets::fixture();
        let releases = resolve_release_set(
            images,
            &binding,
            &models,
            &budgets,
            &endpoints,
            &secrets,
            ValidationMode::Fixture,
        )
        .unwrap();
        secrets.assert_read_once();

        assert_eq!(releases.len(), 8);
        assert_eq!(releases.accepted_image_hashes().len(), 8);
        let ko_hash = releases.owner("company_research", "ko-KR").unwrap();
        let en_hash = releases.owner("company_research_en", "en-US").unwrap();
        assert_ne!(ko_hash, en_hash);
        let ko = releases.release(ko_hash).unwrap();
        let en = releases.release(en_hash).unwrap();
        assert!(Arc::ptr_eq(
            ko.runtime.capabilities.get("ontology.query").unwrap(),
            en.runtime.capabilities.get("ontology.query").unwrap()
        ));

        let mut reversed = all_images();
        reversed.reverse();
        let second = resolve_release_set(
            reversed,
            &binding,
            &models,
            &budgets,
            &endpoints,
            &CountingSecrets::fixture(),
            ValidationMode::Fixture,
        )
        .unwrap();
        assert_eq!(releases.release_set_hash(), second.release_set_hash());
    }

    #[test]
    fn production_descriptor_validation_does_not_require_provider_api_key() {
        let (mut binding, models, budgets, endpoints) = global_fixture();
        for capability in &mut binding.capabilities {
            capability.server_schema_bundle_hash =
                krw_agent_protocol::ContentHash::sha256("production-schema");
            capability.data_release_hash =
                krw_agent_protocol::ContentHash::sha256("production-data-release");
            capability.server_build = "production-test".into();
        }
        let mut values = global_secret_values();
        values.remove("GLM_API_KEY");
        let secrets = FixtureSecrets(values);

        let descriptor_only = resolve_release_set(
            all_images(),
            &binding,
            &models,
            &budgets,
            &endpoints,
            &secrets,
            ValidationMode::ProductionDescriptor,
        )
        .expect("descriptor validation must not construct a GLM provider client");
        assert!(
            descriptor_only
                .provider_api_key(ProviderKind::Glm)
                .is_empty()
        );

        assert!(matches!(
            resolve_release_set(
                all_images(),
                &binding,
                &models,
                &budgets,
                &endpoints,
                &secrets,
                ValidationMode::Production,
            ),
            Err(ConfigError::MissingSecret(name)) if name == "GLM_API_KEY"
        ));
    }

    #[test]
    fn public_descriptor_is_deterministic_complete_and_secret_free() {
        let (binding, models, budgets, endpoints) = global_fixture();
        let releases = resolve_release_set(
            all_images(),
            &binding,
            &models,
            &budgets,
            &endpoints,
            &CountingSecrets::fixture(),
            ValidationMode::Fixture,
        )
        .unwrap();
        let descriptor = releases.public_descriptor("krw-agentd-2026-08").unwrap();
        assert_eq!(
            descriptor.schema_version,
            PUBLIC_RELEASE_DESCRIPTOR_SCHEMA_VERSION
        );
        // Eight checked-in images currently publish 21 public entrypoints:
        // one router, five Guru lenses, six core ontology workflows (the five
        // company/universe lanes plus the ticker-less open_research free
        // door), one English company workflow, five feed workflows (including
        // the wide news discovery variant), and one each for source filing,
        // notebook, and answer composition.
        assert_eq!(descriptor.entries.len(), 21);
        assert!(descriptor.entries.windows(2).all(|pair| {
            (pair[0].run_kind.as_str(), pair[0].locale.as_str())
                < (pair[1].run_kind.as_str(), pair[1].locale.as_str())
        }));
        let company = descriptor
            .entries
            .iter()
            .find(|entry| entry.run_kind == "company_research")
            .unwrap();
        assert_eq!(company.model_profile, "glm_high");
        assert_eq!(company.execution.requested_model, GLM_MODEL_ID);
        assert_eq!(company.execution.thinking, ThinkingMode::Enabled);
        let route = descriptor
            .entries
            .iter()
            .find(|entry| entry.run_kind == "route")
            .unwrap();
        assert_eq!(route.model_profile, "glm_direct");
        assert_eq!(route.execution.requested_model, GLM_MODEL_ID);
        assert_eq!(route.execution.thinking, ThinkingMode::Disabled);
        assert_eq!(route.execution.reasoning_effort, None);

        let encoded = String::from_utf8(serde_jcs::to_vec(&descriptor).unwrap()).unwrap();
        for forbidden in [
            "fixture-deepseek",
            "guru-token",
            "feed-token",
            "filings-token",
            "https://",
            "credential_ref",
            "endpoint_ref",
            "api_base",
        ] {
            assert!(!encoded.contains(forbidden), "leaked {forbidden}");
        }
        let second = releases.public_descriptor("krw-agentd-2026-08").unwrap();
        assert_eq!(
            serde_jcs::to_vec(&descriptor).unwrap(),
            serde_jcs::to_vec(&second).unwrap()
        );
        assert!(matches!(
            releases.public_descriptor("bad/runtime"),
            Err(ConfigError::InvalidRuntimeVersion)
        ));
    }

    #[test]
    fn unrelated_global_supersets_do_not_change_effective_snapshot_hashes() {
        let image = compile_agent_dir(root().join("agents/krw-ontology-en"))
            .unwrap()
            .into_loaded()
            .unwrap();
        let (binding, models, budgets, endpoints) = global_fixture();
        let baseline = resolve_release_set(
            vec![image.clone()],
            &binding,
            &models,
            &budgets,
            &endpoints,
            &CountingSecrets::fixture(),
            ValidationMode::Fixture,
        )
        .unwrap();

        let mut binding_with_extra = binding.clone();
        binding_with_extra.deployment_id = "different-global-deployment-revision".into();
        let mut extra_binding = binding.capabilities[0].clone();
        extra_binding.binding_key = "unreferenced_physical_binding".into();
        extra_binding.endpoint_ref = "unreferenced-endpoint".into();
        extra_binding.auth_scope = AuthScope::Principal;
        extra_binding.server_schema_bundle_hash = ContentHash::sha256("unreferenced-schema");
        extra_binding.server_build = "unreferenced-build".into();
        extra_binding.data_release_hash = ContentHash::sha256("unreferenced-data");
        extra_binding.max_connections += 1;
        extra_binding.request_timeout_ms += 1;
        binding_with_extra.capabilities.insert(0, extra_binding);
        let mut budgets_with_extra = budgets.clone();
        budgets_with_extra.registry_id = "different-global-budget-revision".into();
        let mut extra_profile = budgets.profiles[0].clone();
        extra_profile.profile_id = "unreferenced_budget_profile".into();
        budgets_with_extra.profiles.insert(0, extra_profile);
        let mut endpoints_with_extra = endpoints.clone();
        endpoints_with_extra.registry_id = "different-global-endpoint-revision".into();
        let mut extra_endpoint = endpoints.endpoints[0].clone();
        extra_endpoint.endpoint_ref = "unreferenced-endpoint".into();
        extra_endpoint.url_env = "UNREFERENCED_MCP_URL".into();
        extra_endpoint.readiness_url_env = "UNREFERENCED_MCP_READY_URL".into();
        extra_endpoint.protocol_version = "2099-01-01".into();
        extra_endpoint.origin = "https://unreferenced.invalid".into();
        extra_endpoint.credential_version = "unreferenced-v9".into();
        extra_endpoint.tls_profile = TLS_PROFILE_SYSTEM_ROOTS_V1.into();
        endpoints_with_extra.endpoints.insert(0, extra_endpoint);
        let mut models_with_display_id = models.clone();
        models_with_display_id.registry_id = "different-global-model-revision".into();
        let superset = resolve_release_set(
            vec![image.clone()],
            &binding_with_extra,
            &models_with_display_id,
            &budgets_with_extra,
            &endpoints_with_extra,
            &CountingSecrets::fixture(),
            ValidationMode::Fixture,
        )
        .unwrap();

        let baseline_release = baseline.release(&image.content_hash).unwrap();
        let superset_release = superset.release(&image.content_hash).unwrap();
        assert_eq!(
            baseline_release.runtime.deployment_binding_hash(),
            superset_release.runtime.deployment_binding_hash()
        );
        assert_eq!(
            baseline_release.runtime.budget_registry_hash(),
            superset_release.runtime.budget_registry_hash()
        );
        assert_eq!(
            baseline_release.runtime.model_registry_hash(),
            superset_release.runtime.model_registry_hash()
        );
        assert_eq!(baseline.release_set_hash(), superset.release_set_hash());
        assert_eq!(
            baseline_release.deployment.as_ref(),
            superset_release.deployment.as_ref()
        );
    }

    #[test]
    fn capability_fingerprint_is_complete_canonical_and_secret_free() {
        let image = compile_agent_dir(root().join("agents/krw-feed"))
            .unwrap()
            .into_loaded()
            .unwrap();
        let (binding, models, budgets, endpoints) = global_fixture();
        let secrets = global_fixture_secrets();
        let releases = resolve_release_set(
            vec![image.clone()],
            &binding,
            &models,
            &budgets,
            &endpoints,
            &secrets,
            ValidationMode::Fixture,
        )
        .unwrap();
        let release = releases.release(&image.content_hash).unwrap();
        let capability = release.runtime.capabilities.get("feed.get_items").unwrap();
        let fingerprint = capability.fingerprint();
        let fingerprint_hash = fingerprint.content_hash().unwrap();

        assert_eq!(capability.fingerprint_hash(), &fingerprint_hash);
        assert_eq!(
            release
                .runtime
                .capability_release_hashes
                .get("feed.get_items"),
            Some(&fingerprint.data_release_hash)
        );
        assert_eq!(
            fingerprint.endpoint_url_hash,
            ContentHash::sha256("https://feed.invalid/mcp")
        );
        assert_eq!(
            fingerprint.readiness_url_hash,
            ContentHash::sha256("https://feed.invalid/readyz")
        );
        assert_eq!(
            fingerprint.origin_hash,
            ContentHash::sha256("https://krw-agent.local")
        );
        assert_eq!(
            fingerprint.credential_ref_hash,
            Some(ContentHash::sha256("KRW_FEED_MCP_TOKEN"))
        );

        let canonical = String::from_utf8(serde_jcs::to_vec(fingerprint).unwrap()).unwrap();
        let debug = format!("{capability:?}");
        for sensitive in [
            "https://feed.invalid/mcp",
            "https://feed.invalid/readyz",
            "https://krw-agent.local",
            "feed-token",
            "KRW_FEED_MCP_URL",
            "KRW_FEED_MCP_READY_URL",
            "KRW_FEED_MCP_TOKEN",
        ] {
            assert!(
                !canonical.contains(sensitive),
                "fingerprint leaked {sensitive}"
            );
            assert!(!debug.contains(sensitive), "debug leaked {sensitive}");
        }

        macro_rules! assert_fingerprint_drift {
            ($field:ident, $value:expr) => {{
                let mut drifted = fingerprint.clone();
                drifted.$field = $value;
                assert_ne!(
                    drifted.content_hash().unwrap(),
                    fingerprint_hash,
                    "{} was omitted from the fingerprint hash",
                    stringify!($field)
                );
            }};
        }

        assert_fingerprint_drift!(schema_version, fingerprint.schema_version + 1);
        assert_fingerprint_drift!(binding_key, "other_binding".into());
        assert_fingerprint_drift!(mcp_tool_name, "other_mcp_tool".into());
        assert_fingerprint_drift!(endpoint_ref, "other-endpoint".into());
        assert_fingerprint_drift!(endpoint_url_hash, ContentHash::sha256("other endpoint"));
        assert_fingerprint_drift!(readiness_url_hash, ContentHash::sha256("other readiness"));
        assert_fingerprint_drift!(origin_hash, ContentHash::sha256("other origin"));
        assert_fingerprint_drift!(
            credential_ref_hash,
            Some(ContentHash::sha256("OTHER_CREDENTIAL"))
        );
        assert_fingerprint_drift!(protocol_version, "other-protocol".into());
        assert_fingerprint_drift!(credential_version, "other-credential-version".into());
        assert_fingerprint_drift!(tls_profile, "other-tls-profile".into());
        assert_fingerprint_drift!(tls_ca_pem_hash, Some(ContentHash::sha256("other CA")));
        assert_fingerprint_drift!(auth_scope, AuthScope::Run);
        assert_fingerprint_drift!(tool_session_reuse, McpToolSessionReuse::AttestedStatelessV1);
        assert_fingerprint_drift!(
            server_schema_bundle_hash,
            ContentHash::sha256("other schema")
        );
        assert_fingerprint_drift!(server_build, "other-build".into());
        assert_fingerprint_drift!(data_release_hash, ContentHash::sha256("other data"));
        assert_fingerprint_drift!(max_connections, fingerprint.max_connections + 1);
        assert_fingerprint_drift!(request_timeout_ms, fingerprint.request_timeout_ms + 1);

        let mut rotated_secret = secrets.clone();
        rotated_secret
            .0
            .insert("KRW_FEED_MCP_TOKEN".into(), "new-secret-value".into());
        let rotated = resolve_release_set(
            vec![image.clone()],
            &binding,
            &models,
            &budgets,
            &endpoints,
            &rotated_secret,
            ValidationMode::Fixture,
        )
        .unwrap();
        let rotated_release = rotated.release(&image.content_hash).unwrap();
        assert_eq!(releases.release_set_hash(), rotated.release_set_hash());
        assert_eq!(
            release.runtime.deployment_binding_hash(),
            rotated_release.runtime.deployment_binding_hash()
        );
        assert_eq!(
            release.runtime.capability_release_hashes,
            rotated_release.runtime.capability_release_hashes
        );
    }

    #[test]
    fn every_resolved_endpoint_and_binding_drift_changes_release_identity() {
        let image = compile_agent_dir(root().join("agents/krw-ontology-en"))
            .unwrap()
            .into_loaded()
            .unwrap();
        let (binding, models, budgets, endpoints) = global_fixture();
        let secrets = global_fixture_secrets();
        let baseline = resolve_release_set(
            vec![image.clone()],
            &binding,
            &models,
            &budgets,
            &endpoints,
            &secrets,
            ValidationMode::Fixture,
        )
        .unwrap();
        let baseline_release = baseline.release(&image.content_hash).unwrap();
        let baseline_data_release_hash = baseline_release
            .runtime
            .capability_release_hashes
            .get("ontology.query")
            .unwrap()
            .clone();

        let mut variants = Vec::new();

        let mut endpoint_url_secrets = secrets.clone();
        endpoint_url_secrets.0.insert(
            "KRW_ONTOLOGY_MCP_URL".into(),
            "https://ontology-v2.invalid/mcp".into(),
        );
        variants.push((
            "endpoint URL",
            binding.clone(),
            endpoints.clone(),
            endpoint_url_secrets,
        ));

        let mut readiness_url_secrets = secrets.clone();
        readiness_url_secrets.0.insert(
            "KRW_ONTOLOGY_READY_URL".into(),
            "https://ontology-v2.invalid/readyz".into(),
        );
        variants.push((
            "readiness URL",
            binding.clone(),
            endpoints.clone(),
            readiness_url_secrets,
        ));

        let mut endpoint_ref_binding = binding.clone();
        let mut endpoint_ref_registry = endpoints.clone();
        let mut endpoint_alias =
            endpoint_mut(&mut endpoint_ref_registry, "krw-ontology-local").clone();
        endpoint_alias.endpoint_ref = "krw-ontology-alias".into();
        endpoint_ref_registry.endpoints.push(endpoint_alias);
        binding_mut(&mut endpoint_ref_binding, "krw_ontology_query").endpoint_ref =
            "krw-ontology-alias".into();
        variants.push((
            "endpoint ref",
            endpoint_ref_binding,
            endpoint_ref_registry,
            secrets.clone(),
        ));

        let mut protocol_endpoints = endpoints.clone();
        endpoint_mut(&mut protocol_endpoints, "krw-ontology-local").protocol_version =
            "2026-08-02".into();
        variants.push((
            "protocol version",
            binding.clone(),
            protocol_endpoints,
            secrets.clone(),
        ));

        let mut origin_endpoints = endpoints.clone();
        endpoint_mut(&mut origin_endpoints, "krw-ontology-local").origin =
            "https://other-origin.invalid".into();
        variants.push(("origin", binding.clone(), origin_endpoints, secrets.clone()));

        let mut credential_version_endpoints = endpoints.clone();
        endpoint_mut(&mut credential_version_endpoints, "krw-ontology-local").credential_version =
            "public-v2".into();
        variants.push((
            "credential version",
            binding.clone(),
            credential_version_endpoints,
            secrets.clone(),
        ));

        let mut tls_endpoints = endpoints.clone();
        let tls_endpoint = endpoint_mut(&mut tls_endpoints, "krw-ontology-local");
        tls_endpoint.tls_profile = TLS_PROFILE_SYSTEM_PLUS_PINNED_CA_V1.into();
        tls_endpoint.tls_ca_pem_env = Some("KRW_ONTOLOGY_CA_PEM".into());
        let mut tls_secrets = secrets.clone();
        tls_secrets
            .0
            .insert("KRW_ONTOLOGY_CA_PEM".into(), "fixture CA PEM".into());
        variants.push(("TLS profile", binding.clone(), tls_endpoints, tls_secrets));

        let mut credential_ref_binding = binding.clone();
        binding_mut(&mut credential_ref_binding, "krw_ontology_query").credential_ref =
            Some("KRW_FEED_MCP_TOKEN".into());
        variants.push((
            "credential ref",
            credential_ref_binding,
            endpoints.clone(),
            secrets.clone(),
        ));

        let mut auth_binding = binding.clone();
        binding_mut(&mut auth_binding, "krw_ontology_query").auth_scope = AuthScope::Run;
        variants.push((
            "auth scope",
            auth_binding,
            endpoints.clone(),
            secrets.clone(),
        ));

        let mut session_reuse_binding = binding.clone();
        binding_mut(&mut session_reuse_binding, "krw_ontology_query").tool_session_reuse =
            McpToolSessionReuse::RunScoped;
        variants.push((
            "tool-session reuse policy",
            session_reuse_binding,
            endpoints.clone(),
            secrets.clone(),
        ));

        let mut schema_binding = binding.clone();
        binding_mut(&mut schema_binding, "krw_ontology_query").server_schema_bundle_hash =
            ContentHash::sha256("schema-v2");
        variants.push((
            "server schema bundle",
            schema_binding,
            endpoints.clone(),
            secrets.clone(),
        ));

        let mut build_binding = binding.clone();
        binding_mut(&mut build_binding, "krw_ontology_query").server_build = "fixture-v2".into();
        variants.push((
            "server build",
            build_binding,
            endpoints.clone(),
            secrets.clone(),
        ));

        let mut data_binding = binding.clone();
        binding_mut(&mut data_binding, "krw_ontology_query").data_release_hash =
            ContentHash::sha256("data-v2");
        variants.push((
            "data release",
            data_binding,
            endpoints.clone(),
            secrets.clone(),
        ));

        let mut pool_binding = binding.clone();
        binding_mut(&mut pool_binding, "krw_ontology_query").max_connections += 1;
        variants.push((
            "connection bound",
            pool_binding,
            endpoints.clone(),
            secrets.clone(),
        ));

        let mut timeout_binding = binding.clone();
        binding_mut(&mut timeout_binding, "krw_ontology_query").request_timeout_ms += 1;
        variants.push((
            "request timeout",
            timeout_binding,
            endpoints.clone(),
            secrets,
        ));

        for (dimension, drifted_binding, drifted_endpoints, drifted_secrets) in variants {
            let drifted = resolve_release_set(
                vec![image.clone()],
                &drifted_binding,
                &models,
                &budgets,
                &drifted_endpoints,
                &drifted_secrets,
                ValidationMode::Fixture,
            )
            .unwrap_or_else(|error| panic!("{dimension} did not resolve: {error:?}"));
            let drifted_release = drifted.release(&image.content_hash).unwrap();
            assert_ne!(
                baseline_release.runtime.deployment_binding_hash(),
                drifted_release.runtime.deployment_binding_hash(),
                "{dimension} did not change the effective deployment hash"
            );
            assert_ne!(
                baseline.release_set_hash(),
                drifted.release_set_hash(),
                "{dimension} did not change the release-set hash"
            );
            let drifted_data_release_hash = drifted_release
                .runtime
                .capability_release_hashes
                .get("ontology.query")
                .unwrap();
            if dimension == "data release" {
                assert_ne!(
                    &baseline_data_release_hash, drifted_data_release_hash,
                    "{dimension} did not change the immutable data release hash"
                );
            } else {
                assert_eq!(
                    &baseline_data_release_hash, drifted_data_release_hash,
                    "{dimension} incorrectly changed the immutable data release hash"
                );
            }
        }
    }

    #[test]
    fn release_set_uses_exact_semantic_model_registry_identity() {
        let image = compile_agent_dir(root().join("agents/krw-ontology-en"))
            .unwrap()
            .into_loaded()
            .unwrap();
        let (binding, models, budgets, endpoints) = global_fixture();
        let secrets = global_fixture_secrets();
        let baseline = resolve_release_set(
            vec![image.clone()],
            &binding,
            &models,
            &budgets,
            &endpoints,
            &secrets,
            ValidationMode::Fixture,
        )
        .unwrap();

        let mut reordered_display_revision = models.clone();
        reordered_display_revision.registry_id = "display-only-revision".into();
        reordered_display_revision.models.reverse();
        reordered_display_revision.profiles.reverse();
        let equivalent = resolve_release_set(
            vec![image.clone()],
            &binding,
            &reordered_display_revision,
            &budgets,
            &endpoints,
            &secrets,
            ValidationMode::Fixture,
        )
        .unwrap();
        assert_eq!(
            baseline.model_registry_hash(),
            equivalent.model_registry_hash()
        );
        assert_eq!(baseline.release_set_hash(), equivalent.release_set_hash());

        let mut semantic_drift = models;
        semantic_drift.models[0].max_in_flight += 1;
        let changed = resolve_release_set(
            vec![image],
            &binding,
            &semantic_drift,
            &budgets,
            &endpoints,
            &secrets,
            ValidationMode::Fixture,
        )
        .unwrap();
        assert_ne!(
            baseline.model_registry_hash(),
            changed.model_registry_hash()
        );
        assert_ne!(baseline.release_set_hash(), changed.release_set_hash());
    }

    #[test]
    fn release_set_rejects_ambiguous_identity_ownership_and_bindings() {
        let images = all_images();
        let (binding, models, budgets, endpoints) = global_fixture();

        let mut duplicate_identity = images[0].clone();
        duplicate_identity.manifest.body.metadata.id = images[1].body.metadata.id.clone();
        duplicate_identity.manifest.content_hash =
            ContentHash::sha256(serde_jcs::to_vec(&duplicate_identity.manifest.body).unwrap());
        assert!(matches!(
            resolve_release_set(
                vec![duplicate_identity, images[1].clone()],
                &binding,
                &models,
                &budgets,
                &endpoints,
                &CountingSecrets::fixture(),
                ValidationMode::Fixture,
            ),
            Err(ConfigError::DuplicateAgentIdentity(_))
        ));

        let ontology = images
            .iter()
            .find(|image| {
                image
                    .body
                    .entrypoints
                    .values()
                    .any(|entrypoint| entrypoint.run_kind == "company_research")
            })
            .unwrap();
        let mut duplicate_owner = images
            .iter()
            .find(|image| {
                image
                    .body
                    .entrypoints
                    .values()
                    .any(|entrypoint| entrypoint.run_kind == "company_research_en")
            })
            .unwrap()
            .clone();
        let owner = ontology.body.entrypoints.values().next().unwrap();
        let entrypoint = duplicate_owner
            .manifest
            .body
            .entrypoints
            .values_mut()
            .next()
            .unwrap();
        entrypoint.run_kind = owner.run_kind.clone();
        entrypoint.locale = owner.locale.clone();
        duplicate_owner.manifest.content_hash =
            ContentHash::sha256(serde_jcs::to_vec(&duplicate_owner.manifest.body).unwrap());
        assert!(matches!(
            resolve_release_set(
                vec![ontology.clone(), duplicate_owner],
                &binding,
                &models,
                &budgets,
                &endpoints,
                &CountingSecrets::fixture(),
                ValidationMode::Fixture,
            ),
            Err(ConfigError::DuplicateEntrypointOwnership { .. })
        ));

        let mut duplicate_binding = binding.clone();
        let mut conflict = duplicate_binding.capabilities[0].clone();
        conflict.endpoint_ref = "krw-feed-local".into();
        duplicate_binding.capabilities.push(conflict);
        assert!(matches!(
            resolve_release_set(
                vec![ontology.clone()],
                &duplicate_binding,
                &models,
                &budgets,
                &endpoints,
                &CountingSecrets::fixture(),
                ValidationMode::Fixture,
            ),
            Err(ConfigError::DuplicateCapabilityBinding)
        ));

        let mut missing_binding = binding;
        missing_binding
            .capabilities
            .retain(|item| item.binding_key != "krw_ontology_query");
        assert!(matches!(
            resolve_release_set(
                vec![ontology.clone()],
                &missing_binding,
                &models,
                &budgets,
                &endpoints,
                &CountingSecrets::fixture(),
                ValidationMode::Fixture,
            ),
            Err(ConfigError::MissingBindingKey(_))
        ));
    }

    #[test]
    fn release_set_enforces_the_machine_image_bound_before_deduplication() {
        let image = all_images().remove(0);
        let (binding, models, budgets, endpoints) = global_fixture();
        assert!(matches!(
            resolve_release_set(
                vec![image; MAX_RELEASE_IMAGES + 1],
                &binding,
                &models,
                &budgets,
                &endpoints,
                &CountingSecrets::fixture(),
                ValidationMode::Fixture,
            ),
            Err(ConfigError::ReleaseImageCount)
        ));
    }
}
