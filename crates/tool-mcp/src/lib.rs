//! MCP transport contracts. Pool identity intentionally excludes `AgentImage` identity.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use krw_agent_protocol::{
    AuthScope, CapabilityBinding, ContentHash, McpToolSessionReuse, TransportKind,
};
use reqwest::header::{
    ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, ORIGIN,
};
use reqwest::redirect::Policy as RedirectPolicy;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::{Mutex, OnceCell, OwnedSemaphorePermit, Semaphore};
use zeroize::{Zeroize, Zeroizing};

const MAX_READINESS_RESPONSE_BYTES: usize = 64 * 1024;
/// Closed readiness document version shared by every MCP capability sidecar.
/// A different document shape is a new ABI, not an opportunistic fallback.
pub const MCP_READINESS_SCHEMA_VERSION: &str = "krw-capabilityd/readiness/v1";
const MAX_READINESS_TOOL_COUNT: u64 = 512;
/// Versioned server contract required before an initialized MCP tool session
/// may be shared beyond one run.
pub const STATELESS_TOOL_SESSION_CONTRACT_ID: &str = "krw-agent/mcp-tool-session-stateless/v1";
const INITIALIZE_SESSION_ATTESTATION_POINTER: &str =
    "/capabilities/experimental/krwAgentToolSession";
const TLS_PROFILE_SYSTEM_ROOTS_V1: &str = "system-roots-v1";
const TLS_PROFILE_SYSTEM_PLUS_PINNED_CA_V1: &str = "system-plus-pinned-ca-v1";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PoolKey {
    server_id: String,
    endpoint_ref: String,
    /// Hash of the fully resolved endpoint URL. The URL itself remains deployment data.
    endpoint_url_hash: ContentHash,
    readiness_url_hash: ContentHash,
    origin_hash: ContentHash,
    transport: TransportKind,
    protocol_version: String,
    server_build: String,
    server_schema_bundle_hash: ContentHash,
    data_release_hash: ContentHash,
    auth_scope: AuthScope,
    tool_session_reuse: McpToolSessionReuse,
    session_partition_hash: ContentHash,
    credential_version: String,
    tls_profile: String,
    tls_ca_pem_hash: Option<ContentHash>,
    max_connections: u16,
    request_timeout_ms: u64,
}

impl PoolKey {
    #[allow(clippy::too_many_arguments)]
    pub fn from_binding(
        server_id: impl Into<String>,
        binding: &CapabilityBinding,
        endpoint: &str,
        readiness_endpoint: &str,
        origin: &str,
        protocol_version: impl Into<String>,
        scope: &PoolScope,
        credential_version: impl Into<String>,
        tls_profile: impl Into<String>,
        tls_ca_pem_hash: Option<ContentHash>,
    ) -> Result<Self, McpError> {
        let protocol_version = protocol_version.into();
        // Stateful or unattested sessions are always isolated by the full run
        // identity, even when their data authorization scope is broader.
        let pool_scope = match binding.tool_session_reuse {
            McpToolSessionReuse::RunScoped => &AuthScope::Run,
            McpToolSessionReuse::AttestedStatelessV1 => &binding.auth_scope,
        };
        let session_partition_hash = scope.partition_hash(pool_scope)?;
        Ok(Self {
            server_id: server_id.into(),
            endpoint_ref: binding.endpoint_ref.clone(),
            endpoint_url_hash: ContentHash::sha256(endpoint),
            readiness_url_hash: ContentHash::sha256(readiness_endpoint),
            origin_hash: ContentHash::sha256(origin),
            transport: binding.transport.clone(),
            protocol_version,
            server_build: binding.server_build.clone(),
            server_schema_bundle_hash: binding.server_schema_bundle_hash.clone(),
            data_release_hash: binding.data_release_hash.clone(),
            auth_scope: binding.auth_scope.clone(),
            tool_session_reuse: binding.tool_session_reuse,
            session_partition_hash,
            credential_version: credential_version.into(),
            tls_profile: tls_profile.into(),
            tls_ca_pem_hash,
            max_connections: binding.max_connections,
            request_timeout_ms: binding.request_timeout_ms,
        })
    }

    fn validate_for(&self, config: &McpHttpConfig) -> Result<(), McpError> {
        if self.transport != TransportKind::McpHttp {
            return Err(McpError::PoolKeyMismatch("transport"));
        }
        if self.endpoint_url_hash != ContentHash::sha256(&config.endpoint) {
            return Err(McpError::PoolKeyMismatch("endpoint_url_hash"));
        }
        if self.readiness_url_hash != ContentHash::sha256(&config.readiness_endpoint) {
            return Err(McpError::PoolKeyMismatch("readiness_url_hash"));
        }
        if self.origin_hash != ContentHash::sha256(&config.origin) {
            return Err(McpError::PoolKeyMismatch("origin_hash"));
        }
        if usize::from(self.max_connections) != config.max_concurrency {
            return Err(McpError::PoolKeyMismatch("max_connections"));
        }
        if Duration::from_millis(self.request_timeout_ms) != config.request_timeout {
            return Err(McpError::PoolKeyMismatch("request_timeout_ms"));
        }
        if self.protocol_version != config.protocol_version {
            return Err(McpError::PoolKeyMismatch("protocol_version"));
        }
        if self.tls_profile != config.tls_profile {
            return Err(McpError::PoolKeyMismatch("tls_profile"));
        }
        let observed_tls_ca_pem_hash = config
            .tls_ca_pem
            .as_ref()
            .map(|pem| ContentHash::sha256(pem.as_bytes()));
        if self.tls_ca_pem_hash != observed_tls_ca_pem_hash {
            return Err(McpError::PoolKeyMismatch("tls_ca_pem_hash"));
        }
        let required_partitions = [
            (self.server_id.as_str(), "server_id"),
            (self.endpoint_ref.as_str(), "endpoint_ref"),
            (self.server_build.as_str(), "server_build"),
            (self.credential_version.as_str(), "credential_version"),
            (self.tls_profile.as_str(), "tls_profile"),
        ];
        if let Some((_, field)) = required_partitions
            .into_iter()
            .find(|(value, _)| value.is_empty())
        {
            return Err(McpError::PoolKeyMismatch(field));
        }
        Ok(())
    }

    fn expected_fingerprint(&self) -> ExpectedFingerprint {
        ExpectedFingerprint {
            server_build: self.server_build.clone(),
            server_schema_bundle_hash: self.server_schema_bundle_hash.clone(),
            data_release_hash: self.data_release_hash.clone(),
            protocol_version: self.protocol_version.clone(),
            tool_session_reuse: self.tool_session_reuse,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolScope {
    pub tenant_id: String,
    pub principal_id: String,
    pub run_id: String,
}

impl PoolScope {
    /// Derive the one canonical privacy partition used by both connection
    /// pooling and evidence provenance. Keeping this derivation here prevents
    /// adapters from accidentally sharing tenant- or principal-scoped data.
    pub fn partition_hash(&self, auth_scope: &AuthScope) -> Result<ContentHash, McpError> {
        let fields: &[&str] = match auth_scope {
            AuthScope::Public => &[],
            AuthScope::Tenant => &[&self.tenant_id],
            AuthScope::Principal => &[&self.tenant_id, &self.principal_id],
            AuthScope::Run => &[&self.tenant_id, &self.principal_id, &self.run_id],
        };
        if fields
            .iter()
            .any(|value| value.is_empty() || value.len() > 128)
        {
            return Err(McpError::InvalidScope);
        }
        let mut canonical = Vec::with_capacity(64);
        canonical.extend_from_slice(b"krw-agent/mcp-pool-scope/v1\0");
        canonical.extend_from_slice(match auth_scope {
            AuthScope::Public => b"public".as_slice(),
            AuthScope::Tenant => b"tenant".as_slice(),
            AuthScope::Principal => b"principal".as_slice(),
            AuthScope::Run => b"run".as_slice(),
        });
        for field in fields {
            canonical.push(0);
            canonical.extend_from_slice(field.len().to_string().as_bytes());
            canonical.push(b':');
            canonical.extend_from_slice(field.as_bytes());
        }
        Ok(ContentHash::sha256(canonical))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedFingerprint {
    pub server_build: String,
    pub server_schema_bundle_hash: ContentHash,
    pub data_release_hash: ContentHash,
    pub protocol_version: String,
    pub tool_session_reuse: McpToolSessionReuse,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedFingerprint {
    pub server_build: String,
    pub server_schema_bundle_hash: ContentHash,
    pub data_release_hash: ContentHash,
}

pub fn verify_fingerprint(
    expected: &ExpectedFingerprint,
    observed: &ObservedFingerprint,
) -> Result<(), McpError> {
    if expected.server_build != observed.server_build {
        return Err(McpError::FingerprintMismatch("server_build"));
    }
    if expected.server_schema_bundle_hash != observed.server_schema_bundle_hash {
        return Err(McpError::FingerprintMismatch("server_schema_bundle_hash"));
    }
    if expected.data_release_hash != observed.data_release_hash {
        return Err(McpError::FingerprintMismatch("data_release_hash"));
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct StatelessToolSessionAttestationEnvelope<'a> {
    contract_id: &'static str,
    protocol_version: &'a str,
    server_build: &'a str,
    server_schema_bundle_hash: &'a ContentHash,
    data_release_hash: &'a ContentHash,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ObservedToolSessionAttestation {
    contract_id: String,
    attestation_sha256: ContentHash,
}

/// Compute the exact JCS SHA-256 value that a server must return in both its
/// HTTPS readiness document and MCP initialize result. Binding the contract
/// version to the already pinned protocol/build/schema/data tuple prevents a
/// stale or different server from enabling cross-run reuse.
pub fn stateless_tool_session_attestation_hash(
    expected: &ExpectedFingerprint,
) -> Result<ContentHash, McpError> {
    Ok(ContentHash::sha256(serde_jcs::to_vec(
        &StatelessToolSessionAttestationEnvelope {
            contract_id: STATELESS_TOOL_SESSION_CONTRACT_ID,
            protocol_version: &expected.protocol_version,
            server_build: &expected.server_build,
            server_schema_bundle_hash: &expected.server_schema_bundle_hash,
            data_release_hash: &expected.data_release_hash,
        },
    )?))
}

fn verify_tool_session_attestation(
    observed: Option<&Value>,
    expected: &ExpectedFingerprint,
    source: &'static str,
) -> Result<(), McpError> {
    if expected.tool_session_reuse == McpToolSessionReuse::RunScoped {
        return Ok(());
    }
    let observed = observed.ok_or(McpError::MissingToolSessionAttestation(source))?;
    let observed: ObservedToolSessionAttestation = serde_json::from_value(observed.clone())
        .map_err(|_| McpError::InvalidToolSessionAttestation(source))?;
    if observed.contract_id != STATELESS_TOOL_SESSION_CONTRACT_ID {
        return Err(McpError::ToolSessionContractMismatch("contract_id"));
    }
    if observed.attestation_sha256 != stateless_tool_session_attestation_hash(expected)? {
        return Err(McpError::ToolSessionContractMismatch("attestation_sha256"));
    }
    Ok(())
}

fn validate_initialize_result(
    result: &Value,
    expected: &ExpectedFingerprint,
) -> Result<(), McpError> {
    if result.get("protocolVersion").and_then(Value::as_str)
        != Some(expected.protocol_version.as_str())
    {
        return Err(McpError::ProtocolVersionMismatch);
    }
    verify_tool_session_attestation(
        result.pointer(INITIALIZE_SESSION_ATTESTATION_POINTER),
        expected,
        "initialize",
    )
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: String,
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcResponse {
    Success {
        jsonrpc: String,
        id: String,
        result: Value,
    },
    Error {
        jsonrpc: String,
        id: String,
        error: JsonRpcError,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

#[derive(Debug, Error)]
pub enum McpError {
    #[error("MCP readiness fingerprint mismatch: {0}")]
    FingerprintMismatch(&'static str),
    #[error("MCP endpoint must use HTTPS")]
    InvalidEndpoint,
    #[error("MCP readiness endpoint must be same-origin HTTPS without credentials or query")]
    InvalidReadinessEndpoint,
    #[error("MCP client Origin must be an HTTPS origin without credentials, query, or path")]
    InvalidOrigin,
    #[error("invalid MCP authorization or protocol header")]
    InvalidHeader,
    #[error("invalid MCP client resource limits")]
    InvalidClientLimits,
    #[error("invalid MCP TLS trust profile")]
    InvalidTlsProfile,
    #[error("invalid additional MCP TLS CA certificate")]
    InvalidTlsCaCertificate,
    #[error("invalid MCP authentication partition scope")]
    InvalidScope,
    #[error("MCP HTTP transport failed ({kind:?}); redacted diagnostic {diagnostic_hash}")]
    Http {
        kind: HttpFailureKind,
        diagnostic_hash: ContentHash,
    },
    #[error("MCP response exceeded {0} bytes")]
    ResponseLimit(usize),
    #[error("MCP server returned HTTP {status}; redacted body hash {body_hash}")]
    HttpStatus { status: u16, body_hash: ContentHash },
    #[error("MCP readiness was rejected; redacted payload hash {0}")]
    ReadinessRejected(ContentHash),
    #[error("invalid MCP readiness payload field {0}")]
    InvalidReadinessPayload(&'static str),
    #[error("MCP {0} omitted the required stateless tool-session attestation")]
    MissingToolSessionAttestation(&'static str),
    #[error("MCP {0} stateless tool-session attestation is malformed")]
    InvalidToolSessionAttestation(&'static str),
    #[error("MCP stateless tool-session contract mismatch: {0}")]
    ToolSessionContractMismatch(&'static str),
    #[error("invalid MCP JSON response: {0}")]
    Json(#[from] serde_json::Error),
    #[error("MCP initialize response did not negotiate the pinned protocol version")]
    ProtocolVersionMismatch,
    #[error("MCP response id did not match request id")]
    ResponseIdMismatch,
    #[error("MCP protocol error {code}; redacted message hash {message_hash}")]
    ProtocolError {
        code: i64,
        message_hash: ContentHash,
    },
    #[error("MCP server sent an unsupported server-to-client request")]
    UnsupportedServerRequest,
    #[error("MCP SSE stream ended without the matching response")]
    MissingStreamResponse,
    #[error("MCP SSE frame is incomplete")]
    IncompleteSse,
    #[error("MCP semaphore was closed")]
    Closed,
    #[error("MCP pool capacity must be greater than zero")]
    InvalidPoolCapacity,
    #[error("MCP pool is at its hard limit of {max_entries} live entries")]
    PoolAtCapacity { max_entries: usize },
    #[error("MCP pool key does not match resolved deployment field {0}")]
    PoolKeyMismatch(&'static str),
    #[error("invalid MCP tools/call result envelope")]
    InvalidToolResult,
    #[error(
        "pooled MCP initialization failed; redacted error hash {error_hash}; retryable={retryable}"
    )]
    PooledInitialization {
        error_hash: ContentHash,
        retryable: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpFailureKind {
    Connect,
    Timeout,
    Request,
}

impl From<reqwest::Error> for McpError {
    fn from(error: reqwest::Error) -> Self {
        let kind = if error.is_connect() {
            HttpFailureKind::Connect
        } else if error.is_timeout() {
            HttpFailureKind::Timeout
        } else {
            HttpFailureKind::Request
        };
        // A reqwest error may retain the resolved URL. Keep only a one-way
        // diagnostic so deployment endpoints and query material never enter
        // ordinary logs or shared error values.
        let mut diagnostic = format!("{error:?}");
        let diagnostic_hash = ContentHash::sha256(&diagnostic);
        diagnostic.zeroize();
        Self::Http {
            kind,
            diagnostic_hash,
        }
    }
}

impl McpError {
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Http {
                kind: HttpFailureKind::Connect | HttpFailureKind::Timeout,
                ..
            }
            | Self::MissingStreamResponse
            | Self::IncompleteSse
            | Self::Closed
            | Self::PoolAtCapacity { .. } => true,
            Self::HttpStatus { status, .. } => matches!(*status, 429 | 500 | 502 | 503 | 504),
            Self::PooledInitialization { retryable, .. } => *retryable,
            _ => false,
        }
    }
}

pub struct McpHttpConfig {
    pub endpoint: String,
    pub readiness_endpoint: String,
    pub origin: String,
    pub bearer_token: Option<Zeroizing<String>>,
    pub protocol_version: String,
    pub client_name: String,
    pub client_version: String,
    /// Closed deployment trust profile; this controls whether an additional
    /// explicitly configured CA is accepted alongside platform roots.
    pub tls_profile: String,
    /// PEM bundle for `system-plus-pinned-ca-v1`, supplied only by the resolved
    /// deployment snapshot. It is never logged or carried in a pool key.
    pub tls_ca_pem: Option<Zeroizing<String>>,
    pub max_concurrency: usize,
    pub request_timeout: Duration,
    pub max_response_bytes: usize,
}

impl std::fmt::Debug for McpHttpConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpHttpConfig")
            .field("endpoint_hash", &ContentHash::sha256(&self.endpoint))
            .field(
                "readiness_endpoint_hash",
                &ContentHash::sha256(&self.readiness_endpoint),
            )
            .field("origin_hash", &ContentHash::sha256(&self.origin))
            .field(
                "bearer_token",
                &self.bearer_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("protocol_version", &self.protocol_version)
            .field("client_name", &self.client_name)
            .field("client_version", &self.client_version)
            .field("tls_profile", &self.tls_profile)
            .field(
                "tls_ca_pem_hash",
                &self
                    .tls_ca_pem
                    .as_ref()
                    .map(|pem| ContentHash::sha256(pem.as_bytes())),
            )
            .field("max_concurrency", &self.max_concurrency)
            .field("request_timeout", &self.request_timeout)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish()
    }
}

fn parse_additional_root_certificates(
    profile: &str,
    certificate_pem: Option<Zeroizing<String>>,
) -> Result<Vec<reqwest::Certificate>, McpError> {
    match (profile, certificate_pem) {
        (TLS_PROFILE_SYSTEM_ROOTS_V1, None) => Ok(Vec::new()),
        (TLS_PROFILE_SYSTEM_PLUS_PINNED_CA_V1, Some(mut pem)) => {
            let certificates = reqwest::Certificate::from_pem_bundle(pem.as_bytes())
                .map_err(|_| McpError::InvalidTlsCaCertificate)
                .and_then(|certificates| {
                    if certificates.is_empty() {
                        Err(McpError::InvalidTlsCaCertificate)
                    } else {
                        Ok(certificates)
                    }
                });
            pem.zeroize();
            certificates
        }
        _ => Err(McpError::InvalidTlsProfile),
    }
}

pub struct McpHttpClient {
    http: reqwest::Client,
    endpoint: String,
    protocol_version: HeaderValue,
    session_id: Option<HeaderValue>,
    semaphore: Arc<Semaphore>,
    timeout: Duration,
    max_response_bytes: usize,
    next_id: AtomicU64,
}

impl std::fmt::Debug for McpHttpClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpHttpClient")
            .field("endpoint_hash", &ContentHash::sha256(&self.endpoint))
            .field("has_session", &self.session_id.is_some())
            .field("available_permits", &self.semaphore.available_permits())
            .field("timeout", &self.timeout)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish_non_exhaustive()
    }
}

impl Drop for McpHttpClient {
    fn drop(&mut self) {
        self.endpoint.zeroize();
    }
}

impl McpHttpClient {
    pub async fn connect(
        mut config: McpHttpConfig,
        expected: ExpectedFingerprint,
    ) -> Result<Self, McpError> {
        if expected.protocol_version != config.protocol_version {
            return Err(McpError::ProtocolVersionMismatch);
        }
        let url = reqwest::Url::parse(&config.endpoint).map_err(|_| McpError::InvalidEndpoint)?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(McpError::InvalidEndpoint);
        }
        let readiness_url = reqwest::Url::parse(&config.readiness_endpoint)
            .map_err(|_| McpError::InvalidReadinessEndpoint)?;
        if readiness_url.scheme() != "https"
            || readiness_url.host_str().is_none()
            || !readiness_url.username().is_empty()
            || readiness_url.password().is_some()
            || readiness_url.query().is_some()
            || readiness_url.fragment().is_some()
            || readiness_url.scheme() != url.scheme()
            || readiness_url.host_str() != url.host_str()
            || readiness_url.port_or_known_default() != url.port_or_known_default()
        {
            return Err(McpError::InvalidReadinessEndpoint);
        }
        if config.max_concurrency == 0
            || config.request_timeout.is_zero()
            || config.max_response_bytes == 0
        {
            return Err(McpError::InvalidClientLimits);
        }
        let origin_url =
            reqwest::Url::parse(&config.origin).map_err(|_| McpError::InvalidOrigin)?;
        if origin_url.scheme() != "https"
            || origin_url.host_str().is_none()
            || !origin_url.username().is_empty()
            || origin_url.password().is_some()
            || origin_url.query().is_some()
            || origin_url.fragment().is_some()
            || origin_url.path() != "/"
        {
            return Err(McpError::InvalidOrigin);
        }
        let _ = rustls::crypto::ring::default_provider().install_default();
        let additional_roots =
            parse_additional_root_certificates(&config.tls_profile, config.tls_ca_pem.take())?;
        let mut headers = HeaderMap::new();
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        headers.insert(
            ORIGIN,
            HeaderValue::from_str(&config.origin).map_err(|_| McpError::InvalidHeader)?,
        );
        if let Some(mut token) = config.bearer_token {
            let mut bearer = String::with_capacity("Bearer ".len() + token.len());
            bearer.push_str("Bearer ");
            bearer.push_str(&token);
            let parsed = HeaderValue::from_str(&bearer);
            bearer.zeroize();
            token.zeroize();
            let mut value = parsed.map_err(|_| McpError::InvalidHeader)?;
            value.set_sensitive(true);
            headers.insert(AUTHORIZATION, value);
        }
        let mut http_builder = reqwest::Client::builder()
            .default_headers(headers)
            // Endpoint routing is an immutable deployment decision. Never let a
            // process-wide HTTP(S)_PROXY environment variable redirect MCP
            // traffic (and potentially its bearer credential) elsewhere.
            .no_proxy()
            .redirect(RedirectPolicy::none())
            .connect_timeout(config.request_timeout.min(Duration::from_secs(10)))
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(config.max_concurrency)
            .tcp_keepalive(Duration::from_secs(30));
        for certificate in additional_roots {
            http_builder = http_builder.add_root_certificate(certificate);
        }
        let http = http_builder.build()?;
        probe_readiness(
            &http,
            &config.readiness_endpoint,
            &expected,
            config.request_timeout,
            config.max_response_bytes.min(MAX_READINESS_RESPONSE_BYTES),
        )
        .await?;
        let protocol_version =
            HeaderValue::from_str(&config.protocol_version).map_err(|_| McpError::InvalidHeader)?;
        let initialize_id = "initialize-1";
        let initialize = serde_json::json!({
            "jsonrpc": "2.0",
            "id": initialize_id,
            "method": "initialize",
            "params": {
                "protocolVersion": config.protocol_version,
                "capabilities": {},
                "clientInfo": {"name": config.client_name, "version": config.client_version}
            }
        });
        let initialized = post_message(
            &http,
            &config.endpoint,
            &protocol_version,
            None,
            &initialize,
            Some(initialize_id),
            config.request_timeout,
            config.max_response_bytes,
        )
        .await?;
        let result = response_result(
            initialized
                .response
                .ok_or(McpError::MissingStreamResponse)?,
        )?;
        validate_initialize_result(&result, &expected)?;
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });
        post_message(
            &http,
            &config.endpoint,
            &protocol_version,
            initialized.session_id.as_ref(),
            &notification,
            None,
            config.request_timeout,
            config.max_response_bytes,
        )
        .await?;
        Ok(Self {
            http,
            endpoint: config.endpoint,
            protocol_version,
            session_id: initialized.session_id,
            semaphore: Arc::new(Semaphore::new(config.max_concurrency)),
            timeout: config.request_timeout,
            max_response_bytes: config.max_response_bytes,
            next_id: AtomicU64::new(1),
        })
    }

    pub async fn list_tools(&self) -> Result<Value, McpError> {
        self.request("tools/list", serde_json::json!({})).await
    }

    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
    ) -> Result<ToolCallOutcome, McpError> {
        if mcp_debug_metadata_enabled() {
            eprintln!(
                "[KRW_DEBUG_MCP] tool={name} phase=arguments bytes={} hash={}",
                canonical_json_len(&arguments),
                hash_json(&arguments),
            );
        }
        let result = self
            .request(
                "tools/call",
                serde_json::json!({"name": name, "arguments": arguments}),
            )
            .await?;
        if mcp_debug_metadata_enabled() {
            eprintln!(
                "[KRW_DEBUG_MCP] tool={name} phase=result bytes={} hash={}",
                canonical_json_len(&result),
                hash_json(&result),
            );
        }
        classify_tool_result(result)
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let _permit = self.acquire().await?;
        // Bounded retry over the JSON-RPC POST for retryable transport failures
        // only. A fresh JSON-RPC `id` (and request envelope) is minted on every
        // attempt: retryable errors such as `MissingStreamResponse` and
        // `IncompleteSse` occur *after* the server has observed the prior id, so
        // reusing it would risk duplicate dispatch against non-idempotent tools
        // or a spurious `ResponseIdMismatch`. On exhaustion the final error is
        // propagated to the caller via `?`.
        let max_attempts = 3u32;
        let mut attempt = 0u32;
        let outcome = loop {
            let id = format!("request-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
            let request = SensitiveJson(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            }));
            match post_message(
                &self.http,
                &self.endpoint,
                &self.protocol_version,
                self.session_id.as_ref(),
                &request.0,
                Some(&id),
                self.timeout,
                self.max_response_bytes,
            )
            .await
            {
                Ok(outcome) => break outcome,
                Err(error) => {
                    if attempt + 1 >= max_attempts || !error.is_retryable() {
                        return Err(error);
                    }
                }
            }
            attempt += 1;
            // Bounded exponential backoff, base 500ms, capped at 8s. The delay
            // is base * permille where permille is a deterministic function of
            // `attempt` in [750, 1250]; this is a deterministic spread around
            // the exponential base rather than AWS-style full-jitter
            // (uniform(0, base)). Concurrent callers are desynchronized by
            // wall-clock arrival and the per-model in-flight semaphore, so the
            // deterministic spread still breaks synchronization in practice.
            let base = 500_u64 * (1_u64 << attempt.min(4));
            let base = base.min(8_000);
            let permille: u64 = 750 + (u64::from(attempt) * 97) % 501;
            let delay =
                Duration::from_millis((base.saturating_mul(permille) / 1_000).clamp(1, 8_000));
            tokio::time::sleep(delay).await;
        };
        response_result(outcome.response.ok_or(McpError::MissingStreamResponse)?)
    }

    async fn acquire(&self) -> Result<OwnedSemaphorePermit, McpError> {
        self.semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| McpError::Closed)
    }
}

pub enum ToolCallOutcome {
    Success(Value),
    /// A tool-level control/error payload is distinct from transport failure.
    /// Callers may validate a typed correction contract, but must never ingest
    /// this value as evidence.
    ToolError(ToolErrorResult),
}

impl ToolCallOutcome {
    /// Consume a redacted outcome while preserving the protocol-level error
    /// bit. Any unused payload is scrubbed by `Drop`.
    pub fn into_payload(mut self) -> (Value, bool) {
        match &mut self {
            Self::Success(payload) => (std::mem::take(payload), false),
            Self::ToolError(error) => (std::mem::take(&mut error.payload), true),
        }
    }
}

impl Drop for ToolCallOutcome {
    fn drop(&mut self) {
        if let Self::Success(payload) = self {
            scrub_json(payload);
        }
    }
}

/// Classify the standard MCP `tools/call` result without interpreting its
/// capability-specific content. A malformed `isError` field fails closed.
pub fn classify_tool_result(mut result: Value) -> Result<ToolCallOutcome, McpError> {
    let Some(object) = result.as_object() else {
        scrub_json(&mut result);
        return Err(McpError::InvalidToolResult);
    };
    let is_error = match object.get("isError") {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => {
            scrub_json(&mut result);
            return Err(McpError::InvalidToolResult);
        }
    };
    if is_error {
        Ok(ToolCallOutcome::ToolError(ToolErrorResult::new(result)?))
    } else {
        Ok(ToolCallOutcome::Success(result))
    }
}

impl std::fmt::Debug for ToolCallOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Success(value) => formatter
                .debug_struct("ToolCallOutcome::Success")
                .field("result_hash", &hash_json(value))
                .field("payload", &"[REDACTED]")
                .finish(),
            Self::ToolError(error) => error.fmt(formatter),
        }
    }
}

pub struct ToolErrorResult {
    result_hash: ContentHash,
    payload: Value,
}

impl ToolErrorResult {
    fn new(payload: Value) -> Result<Self, McpError> {
        let canonical = Zeroizing::new(serde_jcs::to_vec(&payload)?);
        Ok(Self {
            result_hash: ContentHash::sha256(canonical.as_slice()),
            payload,
        })
    }

    pub fn result_hash(&self) -> &ContentHash {
        &self.result_hash
    }

    pub fn payload(&self) -> &Value {
        &self.payload
    }
}

impl std::fmt::Debug for ToolErrorResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolCallOutcome::ToolError")
            .field("result_hash", &self.result_hash)
            .field("payload", &"[REDACTED]")
            .finish()
    }
}

impl Drop for ToolErrorResult {
    fn drop(&mut self) {
        scrub_json(&mut self.payload);
    }
}

fn hash_json(value: &Value) -> ContentHash {
    serde_jcs::to_vec(value).map_or_else(
        |_| ContentHash::sha256(b"invalid-json"),
        |bytes| {
            let bytes = Zeroizing::new(bytes);
            ContentHash::sha256(bytes.as_slice())
        },
    )
}

/// MCP arguments and results can contain a user's question, source excerpts,
/// and deployment-scoped metadata.  Debug logging may expose only a stable
/// hash and canonical byte size, never the payload itself.  In particular, an
/// empty exported variable is disabled: the local launcher always forwards
/// the variable so that an omitted operator flag cannot accidentally turn on
/// raw-data logging.
fn mcp_debug_metadata_enabled() -> bool {
    mcp_debug_metadata_enabled_value(std::env::var("KRW_DEBUG_MCP_ARGS").ok().as_deref())
}

fn mcp_debug_metadata_enabled_value(value: Option<&str>) -> bool {
    matches!(
        value.map(str::trim),
        Some(value) if value == "1" || value.eq_ignore_ascii_case("true")
    )
}

fn canonical_json_len(value: &Value) -> usize {
    serde_jcs::to_vec(value).map_or(0, |bytes| bytes.len())
}

fn scrub_json(value: &mut Value) {
    match value {
        Value::String(text) => text.zeroize(),
        Value::Array(values) => values.iter_mut().for_each(scrub_json),
        Value::Object(values) => values.values_mut().for_each(scrub_json),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

struct SensitiveJson(Value);

impl Drop for SensitiveJson {
    fn drop(&mut self) {
        scrub_json(&mut self.0);
    }
}

/// A machine-wide MCP pool. Entry identity contains deployment, protocol,
/// release, authentication, principal, credential, and TLS partitions, but no
/// `AgentImage` identity, so compatible images share the same connection.
#[derive(Debug)]
pub struct McpClientPool {
    inner: BoundedClientPool<McpHttpClient>,
}

impl McpClientPool {
    pub fn new(max_entries: usize, idle_ttl: Duration) -> Result<Self, McpError> {
        Ok(Self {
            inner: BoundedClientPool::new(max_entries, idle_ttl)?,
        })
    }

    pub async fn get_or_connect(
        &self,
        key: PoolKey,
        config: McpHttpConfig,
    ) -> Result<Arc<McpHttpClient>, McpError> {
        key.validate_for(&config)?;
        let expected = key.expected_fingerprint();
        self.inner
            .get_or_create(key, || McpHttpClient::connect(config, expected))
            .await
    }

    /// Lazily construct deployment material only when a new pool entry wins
    /// single-flight initialization. Reused entries therefore do not clone a
    /// bearer token or endpoint strings on every tool call.
    pub async fn get_or_connect_with<F>(
        &self,
        key: PoolKey,
        config_factory: F,
    ) -> Result<Arc<McpHttpClient>, McpError>
    where
        F: FnOnce() -> Result<McpHttpConfig, McpError>,
    {
        let validation_key = key.clone();
        let expected = key.expected_fingerprint();
        self.inner
            .get_or_create(key, || async move {
                let config = config_factory()?;
                validation_key.validate_for(&config)?;
                McpHttpClient::connect(config, expected).await
            })
            .await
    }

    /// Removes idle connections that are not held by an active request/run.
    pub async fn evict_idle(&self) -> usize {
        self.inner.evict_idle().await
    }

    pub async fn stats(&self) -> PoolStats {
        self.inner.stats().await
    }

    pub async fn len(&self) -> usize {
        self.inner.len().await
    }

    pub async fn is_empty(&self) -> bool {
        self.inner.is_empty().await
    }
}

async fn probe_readiness(
    http: &reqwest::Client,
    readiness_endpoint: &str,
    expected: &ExpectedFingerprint,
    timeout: Duration,
    max_response_bytes: usize,
) -> Result<(), McpError> {
    let response = http.get(readiness_endpoint).timeout(timeout).send().await?;
    let status = response.status();
    let bytes = Zeroizing::new(read_bounded(response, max_response_bytes).await?);
    if !status.is_success() {
        return Err(McpError::HttpStatus {
            status: status.as_u16(),
            body_hash: ContentHash::sha256(bytes.as_slice()),
        });
    }
    let payload: Value = serde_json::from_slice(bytes.as_slice())?;
    validate_readiness_payload(&payload, ContentHash::sha256(bytes.as_slice()), expected)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadinessDocument {
    schema_version: String,
    ok: bool,
    fingerprint_match: bool,
    service: String,
    transport: String,
    protocol_version: String,
    build_id: String,
    tool_schema_sha256: String,
    release_manifest_sha256: String,
    tool_count: u64,
    #[serde(default)]
    tool_session_contract: Option<Value>,
}

fn validate_readiness_payload(
    payload: &Value,
    payload_hash: ContentHash,
    expected: &ExpectedFingerprint,
) -> Result<(), McpError> {
    let document: ReadinessDocument = serde_json::from_value(payload.clone())
        .map_err(|_| McpError::InvalidReadinessPayload("document"))?;
    if !document.ok || !document.fingerprint_match {
        return Err(McpError::ReadinessRejected(payload_hash));
    }
    if document.schema_version != MCP_READINESS_SCHEMA_VERSION {
        return Err(McpError::InvalidReadinessPayload("schema_version"));
    }
    if document.service.is_empty() || document.service.len() > 256 {
        return Err(McpError::InvalidReadinessPayload("service"));
    }
    if document.transport != "streamable-http" {
        return Err(McpError::InvalidReadinessPayload("transport"));
    }
    if document.protocol_version != expected.protocol_version {
        return Err(McpError::ProtocolVersionMismatch);
    }
    if document.tool_count == 0 || document.tool_count > MAX_READINESS_TOOL_COUNT {
        return Err(McpError::InvalidReadinessPayload("tool_count"));
    }
    if document.build_id.is_empty() || document.build_id.len() > 256 {
        return Err(McpError::InvalidReadinessPayload("build_id"));
    }
    let server_schema_bundle_hash =
        parse_readiness_hash(&document.tool_schema_sha256, "tool_schema_sha256")?;
    let data_release_hash =
        parse_readiness_hash(&document.release_manifest_sha256, "release_manifest_sha256")?;
    verify_fingerprint(
        expected,
        &ObservedFingerprint {
            server_build: document.build_id,
            server_schema_bundle_hash,
            data_release_hash,
        },
    )?;
    verify_tool_session_attestation(
        document.tool_session_contract.as_ref(),
        expected,
        "readiness",
    )
}

fn parse_readiness_hash(raw: &str, field: &'static str) -> Result<ContentHash, McpError> {
    if raw.is_empty() {
        return Err(McpError::InvalidReadinessPayload(field));
    }
    let normalized = if raw.starts_with("sha256:") {
        raw.to_owned()
    } else {
        format!("sha256:{raw}")
    };
    ContentHash::parse(normalized).map_err(|_| McpError::InvalidReadinessPayload(field))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolStats {
    pub entries: usize,
    pub ready: usize,
    pub initializing: usize,
    pub failed: usize,
    pub in_use: usize,
}

#[derive(Debug)]
struct SharedInitFailure {
    error_hash: ContentHash,
    retryable: bool,
}

struct PoolSlot<T> {
    cell: Arc<OnceCell<Result<Arc<T>, SharedInitFailure>>>,
    last_used: Instant,
    generation: u64,
}

struct PoolState<T> {
    entries: BTreeMap<PoolKey, PoolSlot<T>>,
    generation: u64,
}

struct BoundedClientPool<T> {
    state: Mutex<PoolState<T>>,
    max_entries: usize,
    idle_ttl: Duration,
    failure_retry_after: Duration,
}

impl<T> std::fmt::Debug for BoundedClientPool<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BoundedClientPool")
            .field("max_entries", &self.max_entries)
            .field("idle_ttl", &self.idle_ttl)
            .field("failure_retry_after", &self.failure_retry_after)
            .finish_non_exhaustive()
    }
}

impl<T> BoundedClientPool<T> {
    fn new(max_entries: usize, idle_ttl: Duration) -> Result<Self, McpError> {
        if max_entries == 0 {
            return Err(McpError::InvalidPoolCapacity);
        }
        Ok(Self {
            state: Mutex::new(PoolState {
                entries: BTreeMap::new(),
                generation: 0,
            }),
            max_entries,
            idle_ttl,
            failure_retry_after: Duration::from_secs(1),
        })
    }

    async fn get_or_create<F, Fut>(&self, key: PoolKey, factory: F) -> Result<Arc<T>, McpError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, McpError>>,
    {
        let cell = self.admit_or_reuse(&key).await?;
        let initialized = cell
            .get_or_init(|| async {
                factory()
                    .await
                    .map(Arc::new)
                    .map_err(|error| SharedInitFailure {
                        error_hash: ContentHash::sha256(error.to_string()),
                        retryable: error.is_retryable(),
                    })
            })
            .await;
        match initialized {
            Ok(client) => {
                // Take the caller's strong reference before yielding at the
                // metadata update, so an admission racing this handoff cannot
                // classify the freshly initialized client as unused.
                let client = Arc::clone(client);
                self.touch_if_current(&key, &cell).await;
                Ok(client)
            }
            Err(failure) => {
                self.touch_if_current(&key, &cell).await;
                Err(McpError::PooledInitialization {
                    error_hash: failure.error_hash.clone(),
                    retryable: failure.retryable,
                })
            }
        }
    }

    async fn admit_or_reuse(
        &self,
        key: &PoolKey,
    ) -> Result<Arc<OnceCell<Result<Arc<T>, SharedInitFailure>>>, McpError> {
        let now = Instant::now();
        let mut state = self.state.lock().await;
        state.generation = state.generation.wrapping_add(1);
        let generation = state.generation;
        let expired_failure = state.entries.get(key).is_some_and(|slot| {
            matches!(slot.cell.get(), Some(Err(_)))
                && now.saturating_duration_since(slot.last_used) >= self.failure_retry_after
                && Arc::strong_count(&slot.cell) == 1
        });
        if expired_failure {
            state.entries.remove(key);
        }
        if let Some(slot) = state.entries.get_mut(key) {
            slot.last_used = now;
            slot.generation = generation;
            return Ok(Arc::clone(&slot.cell));
        }

        Self::remove_idle_locked(&mut state, now, self.idle_ttl);
        if state.entries.len() >= self.max_entries {
            let eviction_key = state
                .entries
                .iter()
                .filter(|(_, slot)| Self::is_evictable(slot))
                .min_by_key(|(_, slot)| slot.generation)
                .map(|(key, _)| key.clone());
            let Some(eviction_key) = eviction_key else {
                return Err(McpError::PoolAtCapacity {
                    max_entries: self.max_entries,
                });
            };
            state.entries.remove(&eviction_key);
        }

        let cell = Arc::new(OnceCell::new());
        state.entries.insert(
            key.clone(),
            PoolSlot {
                cell: Arc::clone(&cell),
                last_used: now,
                generation,
            },
        );
        Ok(cell)
    }

    async fn touch_if_current(
        &self,
        key: &PoolKey,
        cell: &Arc<OnceCell<Result<Arc<T>, SharedInitFailure>>>,
    ) {
        let mut state = self.state.lock().await;
        state.generation = state.generation.wrapping_add(1);
        let generation = state.generation;
        if let Some(slot) = state.entries.get_mut(key)
            && Arc::ptr_eq(&slot.cell, cell)
        {
            slot.last_used = Instant::now();
            slot.generation = generation;
        }
    }

    async fn evict_idle(&self) -> usize {
        let mut state = self.state.lock().await;
        Self::remove_idle_locked(&mut state, Instant::now(), self.idle_ttl)
    }

    fn remove_idle_locked(state: &mut PoolState<T>, now: Instant, idle_ttl: Duration) -> usize {
        let before = state.entries.len();
        state.entries.retain(|_, slot| {
            now.saturating_duration_since(slot.last_used) < idle_ttl || !Self::is_evictable(slot)
        });
        before - state.entries.len()
    }

    fn is_evictable(slot: &PoolSlot<T>) -> bool {
        match slot.cell.get() {
            Some(Ok(client)) => Arc::strong_count(client) == 1,
            Some(Err(_)) | None => Arc::strong_count(&slot.cell) == 1,
        }
    }

    async fn stats(&self) -> PoolStats {
        let state = self.state.lock().await;
        let ready = state
            .entries
            .values()
            .filter(|slot| matches!(slot.cell.get(), Some(Ok(_))))
            .count();
        let failed = state
            .entries
            .values()
            .filter(|slot| matches!(slot.cell.get(), Some(Err(_))))
            .count();
        let in_use = state
            .entries
            .values()
            .filter_map(|slot| slot.cell.get())
            .filter_map(|result| result.as_ref().ok())
            .filter(|client| Arc::strong_count(client) > 1)
            .count();
        PoolStats {
            entries: state.entries.len(),
            ready,
            initializing: state.entries.len() - ready - failed,
            failed,
            in_use,
        }
    }

    async fn len(&self) -> usize {
        self.state.lock().await.entries.len()
    }

    async fn is_empty(&self) -> bool {
        self.state.lock().await.entries.is_empty()
    }
}

#[derive(Debug)]
struct PostOutcome {
    response: Option<JsonRpcResponse>,
    session_id: Option<HeaderValue>,
}

#[allow(clippy::too_many_arguments)]
async fn post_message(
    http: &reqwest::Client,
    endpoint: &str,
    protocol_version: &HeaderValue,
    session_id: Option<&HeaderValue>,
    message: &Value,
    expected_id: Option<&str>,
    timeout: Duration,
    max_response_bytes: usize,
) -> Result<PostOutcome, McpError> {
    let protocol_header = HeaderName::from_static("mcp-protocol-version");
    let session_header = HeaderName::from_static("mcp-session-id");
    let mut request = http
        .post(endpoint)
        .header(&protocol_header, protocol_version)
        .timeout(timeout)
        .json(message);
    if let Some(session_id) = session_id {
        request = request.header(&session_header, session_id);
    }
    let response = request.send().await?;
    let status = response.status();
    let response_session = response.headers().get(&session_header).cloned();
    if expected_id.is_none() && status == reqwest::StatusCode::ACCEPTED {
        return Ok(PostOutcome {
            response: None,
            session_id: response_session.or_else(|| session_id.cloned()),
        });
    }
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let bytes = Zeroizing::new(read_bounded(response, max_response_bytes).await?);
    if !status.is_success() {
        return Err(McpError::HttpStatus {
            status: status.as_u16(),
            body_hash: ContentHash::sha256(bytes.as_slice()),
        });
    }
    let rpc_response = if content_type.starts_with("text/event-stream") {
        decode_stream_response(
            bytes.as_slice(),
            expected_id.ok_or(McpError::MissingStreamResponse)?,
        )?
    } else {
        let response: JsonRpcResponse = serde_json::from_slice(bytes.as_slice())?;
        verify_response_id(
            &response,
            expected_id.ok_or(McpError::MissingStreamResponse)?,
        )?;
        response
    };
    Ok(PostOutcome {
        response: Some(rpc_response),
        session_id: response_session.or_else(|| session_id.cloned()),
    })
}

async fn read_bounded(response: reqwest::Response, max_bytes: usize) -> Result<Vec<u8>, McpError> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err(McpError::ResponseLimit(max_bytes));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn decode_stream_response(bytes: &[u8], expected_id: &str) -> Result<JsonRpcResponse, McpError> {
    let text = std::str::from_utf8(bytes).map_err(|_| McpError::IncompleteSse)?;
    let normalized = Zeroizing::new(text.replace("\r\n", "\n"));
    for frame in normalized.split("\n\n") {
        let data = frame
            .lines()
            .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
            .collect::<Vec<_>>();
        if data.is_empty() {
            continue;
        }
        let joined = Zeroizing::new(data.join("\n"));
        let mut value: Value = serde_json::from_str(joined.as_str())?;
        if value.get("method").is_some() && value.get("id").is_some() {
            scrub_json(&mut value);
            return Err(McpError::UnsupportedServerRequest);
        }
        if value.get("id").and_then(Value::as_str) != Some(expected_id) {
            scrub_json(&mut value);
            continue;
        }
        let response: JsonRpcResponse = serde_json::from_value(value)?;
        return Ok(response);
    }
    Err(McpError::MissingStreamResponse)
}

fn verify_response_id(response: &JsonRpcResponse, expected_id: &str) -> Result<(), McpError> {
    let id = match response {
        JsonRpcResponse::Success { id, .. } | JsonRpcResponse::Error { id, .. } => id,
    };
    if id == expected_id {
        Ok(())
    } else {
        Err(McpError::ResponseIdMismatch)
    }
}

fn response_result(response: JsonRpcResponse) -> Result<Value, McpError> {
    match response {
        JsonRpcResponse::Success { result, .. } => Ok(result),
        JsonRpcResponse::Error { mut error, .. } => {
            let message_hash = ContentHash::sha256(&error.message);
            error.message.zeroize();
            if let Some(data) = &mut error.data {
                scrub_json(data);
            }
            Err(McpError::ProtocolError {
                code: error.code,
                message_hash,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn mcp_debug_requires_an_explicit_truthy_value() {
        assert!(!mcp_debug_metadata_enabled_value(None));
        assert!(!mcp_debug_metadata_enabled_value(Some("")));
        assert!(!mcp_debug_metadata_enabled_value(Some("0")));
        assert!(!mcp_debug_metadata_enabled_value(Some("yes")));
        assert!(mcp_debug_metadata_enabled_value(Some("1")));
        assert!(mcp_debug_metadata_enabled_value(Some(" true ")));
        assert!(mcp_debug_metadata_enabled_value(Some("TRUE")));
    }

    #[test]
    fn debug_payload_summary_is_content_free_and_stable() {
        let payload = serde_json::json!({"question": "private question", "ticker": "AAPL"});

        assert!(canonical_json_len(&payload) > 0);
        assert_eq!(hash_json(&payload), hash_json(&payload));
    }

    fn binding(
        auth_scope: AuthScope,
        tool_session_reuse: McpToolSessionReuse,
    ) -> CapabilityBinding {
        CapabilityBinding {
            binding_key: "ontology.query_context".into(),
            mcp_tool_name: "krw_ontology_query_context".into(),
            transport: TransportKind::McpHttp,
            endpoint_ref: "krw-ontology-prod".into(),
            credential_ref: None,
            auth_scope,
            tool_session_reuse,
            server_schema_bundle_hash: ContentHash::sha256("schema"),
            server_build: "build-1".into(),
            data_release_hash: ContentHash::sha256("release"),
            max_connections: 1,
            request_timeout_ms: 10,
        }
    }

    fn expected(tool_session_reuse: McpToolSessionReuse) -> ExpectedFingerprint {
        ExpectedFingerprint {
            server_build: "build-1".into(),
            server_schema_bundle_hash: ContentHash::sha256("schema"),
            data_release_hash: ContentHash::sha256("release"),
            protocol_version: "2025-06-18".into(),
            tool_session_reuse,
        }
    }

    fn attestation_value(expected: &ExpectedFingerprint) -> Value {
        serde_json::json!({
            "contract_id": STATELESS_TOOL_SESSION_CONTRACT_ID,
            "attestation_sha256": stateless_tool_session_attestation_hash(expected).unwrap(),
        })
    }

    fn readiness_payload(expected: &ExpectedFingerprint) -> Value {
        serde_json::json!({
            "schema_version": MCP_READINESS_SCHEMA_VERSION,
            "ok": true,
            "fingerprint_match": true,
            "service": "fixture-capabilityd",
            "transport": "streamable-http",
            "protocol_version": expected.protocol_version,
            "build_id": expected.server_build,
            "tool_schema_sha256": expected.server_schema_bundle_hash,
            "release_manifest_sha256": expected.data_release_hash,
            "tool_count": 1,
        })
    }

    fn pool_key(name: &str) -> PoolKey {
        PoolKey {
            server_id: "krw-ontology".into(),
            endpoint_ref: "krw-ontology-prod".into(),
            endpoint_url_hash: ContentHash::sha256("https://mcp.example.test/rpc"),
            readiness_url_hash: ContentHash::sha256("https://mcp.example.test/readyz"),
            origin_hash: ContentHash::sha256("https://krw-agent.example.test"),
            transport: TransportKind::McpHttp,
            protocol_version: "2025-06-18".into(),
            server_build: "build-1".into(),
            server_schema_bundle_hash: ContentHash::sha256(format!("schema-{name}")),
            data_release_hash: ContentHash::sha256("release-1"),
            auth_scope: AuthScope::Principal,
            tool_session_reuse: McpToolSessionReuse::RunScoped,
            session_partition_hash: ContentHash::sha256(format!("principal-{name}")),
            credential_version: "credential-1".into(),
            tls_profile: "system-roots-v1".into(),
            tls_ca_pem_hash: None,
            max_connections: 1,
            request_timeout_ms: 10,
        }
    }

    #[test]
    fn mismatch_fails_before_dispatch() {
        let expected = ExpectedFingerprint {
            server_build: "a".into(),
            server_schema_bundle_hash: ContentHash::sha256("schema-a"),
            data_release_hash: ContentHash::sha256("release"),
            protocol_version: "2025-06-18".into(),
            tool_session_reuse: McpToolSessionReuse::RunScoped,
        };
        let observed = ObservedFingerprint {
            server_build: "a".into(),
            server_schema_bundle_hash: ContentHash::sha256("schema-b"),
            data_release_hash: ContentHash::sha256("release"),
        };
        assert!(matches!(
            verify_fingerprint(&expected, &observed),
            Err(McpError::FingerprintMismatch("server_schema_bundle_hash"))
        ));
    }

    #[test]
    fn krw_readiness_payload_must_match_all_three_pins() {
        let expected = ExpectedFingerprint {
            server_build: "build-1".into(),
            server_schema_bundle_hash: ContentHash::sha256("schema"),
            data_release_hash: ContentHash::sha256("release"),
            protocol_version: "2025-06-18".into(),
            tool_session_reuse: McpToolSessionReuse::RunScoped,
        };
        let payload = readiness_payload(&expected);
        assert!(
            validate_readiness_payload(&payload, ContentHash::sha256("payload"), &expected).is_ok()
        );

        let mut wrong_release = payload;
        wrong_release["release_manifest_sha256"] = Value::String(
            ContentHash::sha256("other-release")
                .as_str()
                .trim_start_matches("sha256:")
                .into(),
        );
        assert!(matches!(
            validate_readiness_payload(&wrong_release, ContentHash::sha256("wrong"), &expected),
            Err(McpError::FingerprintMismatch("data_release_hash"))
        ));
    }

    #[test]
    fn attested_stateless_requires_matching_readiness_and_initialize_contracts() {
        let expected = expected(McpToolSessionReuse::AttestedStatelessV1);
        let attestation = attestation_value(&expected);
        let mut readiness = readiness_payload(&expected);
        readiness["tool_session_contract"] = attestation;
        validate_readiness_payload(&readiness, ContentHash::sha256("readiness"), &expected)
            .expect("matching readiness attestation");

        let initialize = serde_json::json!({
            "protocolVersion": expected.protocol_version,
            "capabilities": {
                "experimental": {
                    "krwAgentToolSession": attestation_value(&expected),
                }
            }
        });
        validate_initialize_result(&initialize, &expected)
            .expect("matching initialize attestation");
    }

    #[test]
    fn stateless_reuse_fails_closed_on_missing_stateful_or_mismatched_attestation() {
        let expected = expected(McpToolSessionReuse::AttestedStatelessV1);
        assert!(matches!(
            verify_tool_session_attestation(None, &expected, "readiness"),
            Err(McpError::MissingToolSessionAttestation("readiness"))
        ));
        let initialize_without_attestation = serde_json::json!({
            "protocolVersion": expected.protocol_version,
            "capabilities": {},
        });
        assert!(matches!(
            validate_initialize_result(&initialize_without_attestation, &expected),
            Err(McpError::MissingToolSessionAttestation("initialize"))
        ));

        let stateful = serde_json::json!({
            "contract_id": "krw-agent/mcp-tool-session-stateful/v1",
            "attestation_sha256": stateless_tool_session_attestation_hash(&expected).unwrap(),
        });
        assert!(matches!(
            verify_tool_session_attestation(Some(&stateful), &expected, "readiness"),
            Err(McpError::ToolSessionContractMismatch("contract_id"))
        ));

        let mismatched = serde_json::json!({
            "contract_id": STATELESS_TOOL_SESSION_CONTRACT_ID,
            "attestation_sha256": ContentHash::sha256("different-contract"),
        });
        assert!(matches!(
            verify_tool_session_attestation(Some(&mismatched), &expected, "initialize"),
            Err(McpError::ToolSessionContractMismatch("attestation_sha256"))
        ));

        let malformed = serde_json::json!({
            "contract_id": STATELESS_TOOL_SESSION_CONTRACT_ID,
            "attestation_sha256": stateless_tool_session_attestation_hash(&expected).unwrap(),
            "unversioned_escape_hatch": true,
        });
        assert!(matches!(
            verify_tool_session_attestation(Some(&malformed), &expected, "readiness"),
            Err(McpError::InvalidToolSessionAttestation("readiness"))
        ));
    }

    #[test]
    fn readiness_requires_the_closed_versioned_document_shape() {
        let expected = expected(McpToolSessionReuse::RunScoped);
        let mut payload = readiness_payload(&expected);
        payload["schema_version"] = Value::String("krw-capabilityd/readiness/v0".into());
        assert!(matches!(
            validate_readiness_payload(&payload, ContentHash::sha256("old"), &expected),
            Err(McpError::InvalidReadinessPayload("schema_version"))
        ));

        let mut payload = readiness_payload(&expected);
        payload["undocumented_compatibility_field"] = Value::Bool(true);
        assert!(matches!(
            validate_readiness_payload(&payload, ContentHash::sha256("extra"), &expected),
            Err(McpError::InvalidReadinessPayload("document"))
        ));

        let mut payload = readiness_payload(&expected);
        payload["protocol_version"] = Value::String("2026-01-01".into());
        assert!(matches!(
            validate_readiness_payload(&payload, ContentHash::sha256("protocol"), &expected),
            Err(McpError::ProtocolVersionMismatch)
        ));
    }

    #[test]
    fn stateless_attestation_hash_is_bound_to_every_version_and_release_pin() {
        let expected = expected(McpToolSessionReuse::AttestedStatelessV1);
        let baseline = stateless_tool_session_attestation_hash(&expected).unwrap();
        for changed in [
            ExpectedFingerprint {
                protocol_version: "2026-01-01".into(),
                ..expected.clone()
            },
            ExpectedFingerprint {
                server_build: "build-2".into(),
                ..expected.clone()
            },
            ExpectedFingerprint {
                server_schema_bundle_hash: ContentHash::sha256("schema-2"),
                ..expected.clone()
            },
            ExpectedFingerprint {
                data_release_hash: ContentHash::sha256("release-2"),
                ..expected.clone()
            },
        ] {
            assert_ne!(
                baseline,
                stateless_tool_session_attestation_hash(&changed).unwrap()
            );
        }
    }

    #[test]
    fn run_scoped_and_principal_scoped_keys_prevent_cross_principal_reuse() {
        let principal_a_run_a = PoolScope {
            tenant_id: "tenant-a".into(),
            principal_id: "principal-a".into(),
            run_id: "run-a".into(),
        };
        let principal_b_run_b = PoolScope {
            tenant_id: "tenant-a".into(),
            principal_id: "principal-b".into(),
            run_id: "run-b".into(),
        };
        let args = (
            "https://mcp.example.test/rpc",
            "https://mcp.example.test/readyz",
            "https://krw-agent.example.test",
        );

        let stateful = binding(AuthScope::Public, McpToolSessionReuse::RunScoped);
        let stateful_a = PoolKey::from_binding(
            "server",
            &stateful,
            args.0,
            args.1,
            args.2,
            "2025-06-18",
            &principal_a_run_a,
            "credential-v1",
            "tls-v1",
            None,
        )
        .unwrap();
        let stateful_b = PoolKey::from_binding(
            "server",
            &stateful,
            args.0,
            args.1,
            args.2,
            "2025-06-18",
            &principal_b_run_b,
            "credential-v1",
            "tls-v1",
            None,
        )
        .unwrap();
        assert_ne!(stateful_a, stateful_b);

        let stateless = binding(
            AuthScope::Principal,
            McpToolSessionReuse::AttestedStatelessV1,
        );
        let stateless_a = PoolKey::from_binding(
            "server",
            &stateless,
            args.0,
            args.1,
            args.2,
            "2025-06-18",
            &principal_a_run_a,
            "credential-v1",
            "tls-v1",
            None,
        )
        .unwrap();
        let stateless_b = PoolKey::from_binding(
            "server",
            &stateless,
            args.0,
            args.1,
            args.2,
            "2025-06-18",
            &principal_b_run_b,
            "credential-v1",
            "tls-v1",
            None,
        )
        .unwrap();
        assert_ne!(stateless_a, stateless_b);
    }

    #[test]
    fn attested_stateless_keys_reuse_partition_across_runs_but_run_scoped_does_not() {
        // Same binding, tenant, principal, server identity. Only the run id
        // changes between the two scopes. The whole point of activation:
        // attested-stateless-v1 (auth_scope = tenant) must derive an identical
        // pool key so the second run reuses the first run's initialized session,
        // while run-scoped always forks a fresh run-private partition.
        let run_one = PoolScope {
            tenant_id: "tenant-a".into(),
            principal_id: "principal-a".into(),
            run_id: "run-1".into(),
        };
        let run_two = PoolScope {
            tenant_id: "tenant-a".into(),
            principal_id: "principal-a".into(),
            run_id: "run-2".into(),
        };
        let args = (
            "https://mcp.example.test/rpc",
            "https://mcp.example.test/readyz",
            "https://krw-agent.example.test",
        );

        let stateless = binding(AuthScope::Tenant, McpToolSessionReuse::AttestedStatelessV1);
        let stateless_run_one = PoolKey::from_binding(
            "server",
            &stateless,
            args.0,
            args.1,
            args.2,
            "2025-06-18",
            &run_one,
            "credential-v1",
            "tls-v1",
            None,
        )
        .expect("attested-stateless pool key for run one");
        let stateless_run_two = PoolKey::from_binding(
            "server",
            &stateless,
            args.0,
            args.1,
            args.2,
            "2025-06-18",
            &run_two,
            "credential-v1",
            "tls-v1",
            None,
        )
        .expect("attested-stateless pool key for run two");
        assert_eq!(
            stateless_run_one.session_partition_hash, stateless_run_two.session_partition_hash,
            "attested-stateless-v1 with tenant auth_scope must collapse to the \
             same partition across runs (cross-run pool reuse eligibility)",
        );
        assert_eq!(stateless_run_one, stateless_run_two);

        let run_scoped = binding(AuthScope::Tenant, McpToolSessionReuse::RunScoped);
        let run_scoped_one = PoolKey::from_binding(
            "server",
            &run_scoped,
            args.0,
            args.1,
            args.2,
            "2025-06-18",
            &run_one,
            "credential-v1",
            "tls-v1",
            None,
        )
        .expect("run-scoped pool key");
        assert_ne!(
            stateless_run_one, run_scoped_one,
            "run-scoped must partition by run even when the binding is otherwise identical",
        );
        assert_eq!(
            run_scoped_one.session_partition_hash,
            run_one.partition_hash(&AuthScope::Run).unwrap(),
            "run-scoped derives its partition from the Run scope regardless of data auth_scope",
        );
    }

    #[test]
    fn deployment_cannot_omit_the_session_reuse_policy() {
        let value = serde_json::json!({
            "binding_key": "ontology.query_context",
            "transport": "mcp-http",
            "endpoint_ref": "ontology",
            "credential_ref": null,
            "auth_scope": "tenant",
            "server_schema_bundle_hash": ContentHash::sha256("schema"),
            "server_build": "build-1",
            "data_release_hash": ContentHash::sha256("release"),
            "max_connections": 1,
            "request_timeout_ms": 10,
        });
        assert!(serde_json::from_value::<CapabilityBinding>(value).is_err());
    }

    #[test]
    fn pool_scope_is_derived_from_declared_auth_boundary() {
        let first = PoolScope {
            tenant_id: "tenant-a".into(),
            principal_id: "principal-a".into(),
            run_id: "run-a".into(),
        };
        let second = PoolScope {
            tenant_id: "tenant-a".into(),
            principal_id: "principal-b".into(),
            run_id: "run-b".into(),
        };
        let other_tenant = PoolScope {
            tenant_id: "tenant-b".into(),
            principal_id: "principal-a".into(),
            run_id: "run-a".into(),
        };

        assert_eq!(
            first.partition_hash(&AuthScope::Public).unwrap(),
            second.partition_hash(&AuthScope::Public).unwrap()
        );
        assert_eq!(
            first.partition_hash(&AuthScope::Tenant).unwrap(),
            second.partition_hash(&AuthScope::Tenant).unwrap()
        );
        assert_ne!(
            first.partition_hash(&AuthScope::Tenant).unwrap(),
            other_tenant.partition_hash(&AuthScope::Tenant).unwrap()
        );
        assert_ne!(
            first.partition_hash(&AuthScope::Principal).unwrap(),
            second.partition_hash(&AuthScope::Principal).unwrap()
        );
        assert_ne!(
            first.partition_hash(&AuthScope::Run).unwrap(),
            second.partition_hash(&AuthScope::Run).unwrap()
        );
    }

    #[test]
    fn tool_error_control_payload_is_available_but_debug_redacted() {
        let secret = "server correction secret";
        let error = ToolErrorResult::new(serde_json::json!({
            "isError": true,
            "status": "input_correction_required",
            "message": secret
        }))
        .unwrap();
        assert_eq!(
            error.payload()["status"],
            Value::String("input_correction_required".into())
        );
        let debug = format!("{error:?}");
        assert!(!debug.contains(secret));
        assert!(debug.contains(error.result_hash().as_str()));
    }

    #[test]
    fn tool_result_classifier_rejects_non_boolean_error_flags() {
        assert!(matches!(
            classify_tool_result(serde_json::json!({
                "content": [{"type": "text", "text": "{}"}],
                "isError": "false"
            })),
            Err(McpError::InvalidToolResult)
        ));
        assert!(matches!(
            classify_tool_result(serde_json::json!({
                "content": [{"type": "text", "text": "{}"}],
                "isError": false
            })),
            Ok(ToolCallOutcome::Success(_))
        ));
    }

    #[test]
    fn tls_profile_requires_an_explicit_valid_pinned_ca_when_not_using_system_roots() {
        assert!(parse_additional_root_certificates(TLS_PROFILE_SYSTEM_ROOTS_V1, None).is_ok());
        assert!(matches!(
            parse_additional_root_certificates(
                TLS_PROFILE_SYSTEM_ROOTS_V1,
                Some(Zeroizing::new("not permitted".into()))
            ),
            Err(McpError::InvalidTlsProfile)
        ));
        assert!(matches!(
            parse_additional_root_certificates(TLS_PROFILE_SYSTEM_PLUS_PINNED_CA_V1, None),
            Err(McpError::InvalidTlsProfile)
        ));
        assert!(matches!(
            parse_additional_root_certificates(
                TLS_PROFILE_SYSTEM_PLUS_PINNED_CA_V1,
                Some(Zeroizing::new("not PEM".into()))
            ),
            Err(McpError::InvalidTlsCaCertificate)
        ));
    }

    #[test]
    fn pool_key_pins_connection_limits_from_the_binding() {
        let key = pool_key("limits");
        let config = McpHttpConfig {
            endpoint: "https://mcp.example.test/rpc".into(),
            readiness_endpoint: "https://mcp.example.test/readyz".into(),
            origin: "https://krw-agent.example.test".into(),
            bearer_token: None,
            protocol_version: "2025-06-18".into(),
            client_name: "test".into(),
            client_version: "1".into(),
            tls_profile: "system-roots-v1".into(),
            tls_ca_pem: None,
            max_concurrency: 2,
            request_timeout: Duration::from_millis(10),
            max_response_bytes: 1024,
        };
        assert!(matches!(
            key.validate_for(&config),
            Err(McpError::PoolKeyMismatch("max_connections"))
        ));
    }

    #[tokio::test]
    async fn endpoint_rejects_credentials_and_query_before_network_io() {
        for endpoint in [
            "https://user:pass@mcp.example.test/rpc",
            "https://mcp.example.test/rpc?token=secret",
        ] {
            let config = McpHttpConfig {
                endpoint: endpoint.into(),
                readiness_endpoint: "https://mcp.example.test/readyz".into(),
                origin: "https://krw-agent.example.test".into(),
                bearer_token: None,
                protocol_version: "2025-06-18".into(),
                client_name: "test".into(),
                client_version: "1".into(),
                tls_profile: "system-roots-v1".into(),
                tls_ca_pem: None,
                max_concurrency: 1,
                request_timeout: Duration::from_millis(10),
                max_response_bytes: 1024,
            };
            assert!(matches!(
                McpHttpClient::connect(
                    config,
                    ExpectedFingerprint {
                        server_build: "build-1".into(),
                        server_schema_bundle_hash: ContentHash::sha256("schema"),
                        data_release_hash: ContentHash::sha256("release"),
                        protocol_version: "2025-06-18".into(),
                        tool_session_reuse: McpToolSessionReuse::RunScoped,
                    }
                )
                .await,
                Err(McpError::InvalidEndpoint)
            ));
        }
    }

    #[test]
    fn sse_decoder_ignores_unrelated_responses() {
        let stream = br#"event: message
data: {"jsonrpc":"2.0","id":"other","result":{}}

event: message
data: {"jsonrpc":"2.0","id":"wanted","result":{"tools":[]}}

"#;
        let response = decode_stream_response(stream, "wanted").expect("matching response");
        assert!(matches!(
            response,
            JsonRpcResponse::Success { id, .. } if id == "wanted"
        ));
    }

    #[tokio::test]
    async fn pool_reuses_exact_key_without_reinitializing() {
        let pool = BoundedClientPool::new(2, Duration::from_mins(1)).expect("pool");
        let calls = AtomicUsize::new(0);
        let key = pool_key("a");
        let first = pool
            .get_or_create(key.clone(), || async {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok(7_u64)
            })
            .await
            .expect("first client");
        let second = pool
            .get_or_create(key, || async {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok(8_u64)
            })
            .await
            .expect("reused client");

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(*second, 7);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_same_key_initializes_once() {
        let pool = Arc::new(
            BoundedClientPool::new(2, Duration::from_mins(1)).expect("bounded client pool"),
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for _ in 0..16 {
            let pool = Arc::clone(&pool);
            let calls = Arc::clone(&calls);
            let key = pool_key("singleflight");
            tasks.push(tokio::spawn(async move {
                pool.get_or_create(key, || async move {
                    calls.fetch_add(1, Ordering::Relaxed);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    Ok(11_u64)
                })
                .await
            }));
        }

        let mut clients = Vec::new();
        for task in tasks {
            clients.push(task.await.expect("task join").expect("pooled client"));
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(
            clients
                .iter()
                .all(|client| Arc::ptr_eq(client, &clients[0]))
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_failed_initialization_is_singleflight_and_negative_cached() {
        let pool = Arc::new(
            BoundedClientPool::<u64>::new(2, Duration::from_mins(1)).expect("bounded client pool"),
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for _ in 0..16 {
            let pool = Arc::clone(&pool);
            let calls = Arc::clone(&calls);
            let key = pool_key("failed-singleflight");
            tasks.push(tokio::spawn(async move {
                pool.get_or_create(key, || async move {
                    calls.fetch_add(1, Ordering::Relaxed);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    Err(McpError::InvalidEndpoint)
                })
                .await
            }));
        }
        for task in tasks {
            assert!(matches!(
                task.await.expect("task join"),
                Err(McpError::PooledInitialization {
                    retryable: false,
                    ..
                })
            ));
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        let stats = pool.stats().await;
        assert_eq!(stats.failed, 1);
        assert_eq!(stats.initializing, 0);
    }

    #[tokio::test]
    async fn pooled_initialization_preserves_transient_classification() {
        let pool = BoundedClientPool::<u64>::new(1, Duration::from_mins(1)).expect("pool");
        let error = pool
            .get_or_create(pool_key("transient"), || async {
                Err(McpError::MissingStreamResponse)
            })
            .await
            .expect_err("transient initialization failure");
        assert!(matches!(
            &error,
            McpError::PooledInitialization {
                retryable: true,
                ..
            }
        ));
        assert!(error.is_retryable());
    }

    #[tokio::test]
    async fn pool_key_partitions_principals_and_fingerprints() {
        let pool = BoundedClientPool::new(3, Duration::from_mins(1)).expect("pool");
        let key_a = pool_key("a");
        let mut key_other_principal = key_a.clone();
        key_other_principal.session_partition_hash = ContentHash::sha256("principal-b");
        let mut key_other_schema = key_a.clone();
        key_other_schema.server_schema_bundle_hash = ContentHash::sha256("schema-b");

        let a = pool
            .get_or_create(key_a, || async { Ok(1_u64) })
            .await
            .expect("first partition");
        let principal = pool
            .get_or_create(key_other_principal, || async { Ok(2_u64) })
            .await
            .expect("principal partition");
        let schema = pool
            .get_or_create(key_other_schema, || async { Ok(3_u64) })
            .await
            .expect("schema partition");

        assert!(!Arc::ptr_eq(&a, &principal));
        assert!(!Arc::ptr_eq(&a, &schema));
        assert_eq!(pool.len().await, 3);
    }

    #[tokio::test]
    async fn pool_enforces_cap_and_evicts_only_unused_lru() {
        let pool = BoundedClientPool::new(2, Duration::from_mins(1)).expect("pool");
        let held = pool
            .get_or_create(pool_key("held"), || async { Ok(1_u64) })
            .await
            .expect("held client");
        let unused = pool
            .get_or_create(pool_key("unused"), || async { Ok(2_u64) })
            .await
            .expect("unused client");
        drop(unused);

        let replacement = pool
            .get_or_create(pool_key("replacement"), || async { Ok(3_u64) })
            .await
            .expect("unused LRU was evicted");
        let error = pool
            .get_or_create(pool_key("over-cap"), || async { Ok(4_u64) })
            .await
            .expect_err("both resident clients are in use");
        assert!(matches!(error, McpError::PoolAtCapacity { max_entries: 2 }));
        assert_eq!(*held, 1);
        assert_eq!(*replacement, 3);
        assert_eq!(pool.len().await, 2);
    }

    #[tokio::test]
    async fn idle_eviction_releases_unreferenced_entries() {
        let pool = BoundedClientPool::new(2, Duration::ZERO).expect("pool");
        let client = pool
            .get_or_create(pool_key("idle"), || async { Ok(1_u64) })
            .await
            .expect("client");
        drop(client);

        assert_eq!(pool.evict_idle().await, 1);
        assert!(pool.is_empty().await);
    }

    #[tokio::test]
    async fn endpoint_hash_mismatch_fails_before_network_io() {
        let pool = McpClientPool::new(1, Duration::from_mins(1)).expect("pool");
        let mut key = pool_key("mismatch");
        key.endpoint_url_hash = ContentHash::sha256("https://different.example.test/rpc");
        let config = McpHttpConfig {
            endpoint: "https://mcp.example.test/rpc".into(),
            readiness_endpoint: "https://mcp.example.test/readyz".into(),
            origin: "https://krw-agent.example.test".into(),
            bearer_token: None,
            protocol_version: "2025-06-18".into(),
            client_name: "test".into(),
            client_version: "1".into(),
            tls_profile: "system-roots-v1".into(),
            tls_ca_pem: None,
            max_concurrency: 1,
            request_timeout: Duration::from_millis(10),
            max_response_bytes: 1024,
        };

        let error = pool
            .get_or_connect(key, config)
            .await
            .expect_err("must reject before network");
        assert!(matches!(
            error,
            McpError::PoolKeyMismatch("endpoint_url_hash")
        ));
    }
}
