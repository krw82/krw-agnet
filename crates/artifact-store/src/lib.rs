//! Scope-isolated, encrypted artifacts for durable agent-run recovery.
//!
//! This crate stores provider episodes, action arguments/results, and typed
//! recovery checkpoints outside `PostgreSQL`. Callers persist only [`ArtifactRef`]
//! values in the database. Every operation also requires the complete
//! tenant/principal/run [`ArtifactScope`], so possession of a reference alone is
//! insufficient to read or delete an artifact.
//!
//! # Cryptographic boundary
//!
//! Each artifact receives a random opaque identifier, HKDF salt, and `XChaCha20`
//! nonce. HKDF-SHA-256 derives an independent key from a versioned master key and
//! the artifact's scope, kind, and identifier. XChaCha20-Poly1305 authenticates
//! the format/key versions, kind, scope digest, identifier, plaintext digest and
//! length, timestamps, salt, and nonce as canonical AAD. Plaintext is never used
//! for naming or deduplication, including between principals.
//!
//! # Local-backend threat model
//!
//! [`LocalArtifactStore`] protects confidentiality and integrity if artifact
//! files or database references are copied, swapped, or modified. It uses a
//! private absolute directory, fixed-format random filenames, no-follow opens,
//! create-new publication, file/directory `fsync`, and restrictive Unix modes.
//! It does not defend a running process from an attacker who can read that
//! process's memory or replace files as the daemon's own OS account. Production
//! deployments should keep the root on a local filesystem controlled only by
//! the daemon account. The [`ArtifactStore`] trait is the boundary for a future
//! shared remote/KMS-backed implementation.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use zeroize::{Zeroize, Zeroizing};

const FILE_MAGIC: &[u8; 8] = b"KRWART01";
const FORMAT_VERSION: u16 = 1;
const ARTIFACT_ID_BYTES: usize = 32;
const SALT_BYTES: usize = 32;
const NONCE_BYTES: usize = 24;
const KEY_BYTES: usize = 32;
const AEAD_TAG_BYTES: usize = 16;
const MAX_HEADER_BYTES: usize = 4 * 1024;
const MAX_SCOPE_COMPONENT_BYTES: usize = 256;
const MAX_KEY_VERSIONS: usize = 16;
const HASH_PREFIX: &str = "sha256:";
const ARTIFACT_SUFFIX: &str = ".krwa";
const TEMP_PREFIX: &str = ".tmp-";
const ARTIFACTS_DIRECTORY: &str = "artifacts";
const STAGING_DIRECTORY: &str = "staging";
const MAINTENANCE_DIRECTORY: &str = "maintenance";
const MAINTENANCE_CURSOR_FILE: &str = ".maintenance-cursor";
const CURSOR_TEMP_PREFIX: &str = ".cursor-tmp-";
const MAINTENANCE_GENERATIONS: u8 = 2;
const SHARD_COUNT: u16 = 256;
const CURSOR_MAGIC: &[u8; 8] = b"KRWCUR01";
const HKDF_DOMAIN: &[u8] = b"krw-agent/artifact-key/v1\0";
const SCOPE_DOMAIN: &[u8] = b"krw-agent/artifact-scope/v1\0";

/// The security and ownership boundary for one run's artifacts.
#[derive(Clone, PartialEq, Eq)]
pub struct ArtifactScope {
    tenant: String,
    principal: String,
    run: String,
}

impl ArtifactScope {
    /// Constructs and validates an exact tenant/principal/run scope.
    pub fn new(
        tenant_id: impl Into<String>,
        principal_id: impl Into<String>,
        run_id: impl Into<String>,
    ) -> Result<Self, ArtifactStoreError> {
        let scope = Self {
            tenant: tenant_id.into(),
            principal: principal_id.into(),
            run: run_id.into(),
        };
        validate_scope_component("tenant_id", &scope.tenant)?;
        validate_scope_component("principal_id", &scope.principal)?;
        validate_scope_component("run_id", &scope.run)?;
        Ok(scope)
    }

    /// Exact tenant identifier for an authorized backend request.
    pub fn tenant_id(&self) -> &str {
        &self.tenant
    }

    /// Exact principal identifier for an authorized backend request.
    pub fn principal_id(&self) -> &str {
        &self.principal
    }

    /// Exact run identifier for an authorized backend request.
    pub fn run_id(&self) -> &str {
        &self.run
    }

    /// Stable length-delimited digest used in KDF/AAD and remote authorization.
    pub fn digest_sha256(&self) -> String {
        self.digest()
    }

    fn digest(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(SCOPE_DOMAIN);
        hash_len_prefixed(&mut hasher, self.tenant.as_bytes());
        hash_len_prefixed(&mut hasher, self.principal.as_bytes());
        hash_len_prefixed(&mut hasher, self.run.as_bytes());
        format_hash(hasher.finalize().as_slice())
    }
}

impl fmt::Debug for ArtifactScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ArtifactScope")
            .field("tenant_id", &"<redacted>")
            .field("principal_id", &"<redacted>")
            .field("run_id", &"<redacted>")
            .finish()
    }
}

/// A bounded set of durable artifact payload classes.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// Exact provider request/response replay material.
    ProviderEpisode,
    /// Canonical arguments associated with a durable action receipt.
    ActionArguments,
    /// Canonical capability output associated with a durable action receipt.
    ActionResult,
    /// Typed state-machine, budget, and evidence recovery state.
    RecoveryState,
}

/// An opaque durable reference safe to persist in `PostgreSQL`.
///
/// It intentionally contains no endpoint, key material, scope identifiers, or
/// artifact bytes. The plaintext digest supports end-to-end receipt checking;
/// random IDs mean it is never used for storage deduplication.
#[derive(Clone, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRef {
    scheme: String,
    format_version: u16,
    artifact_id: String,
    key_version: u32,
    kind: ArtifactKind,
    plaintext_sha256: String,
    plaintext_len: u64,
    created_at_unix_ms: i64,
    expires_at_unix_ms: i64,
}

impl ArtifactRef {
    /// Artifact reference scheme.
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// On-disk/envelope format version.
    pub const fn format_version(&self) -> u16 {
        self.format_version
    }

    /// The opaque, random artifact identifier.
    pub fn artifact_id(&self) -> &str {
        &self.artifact_id
    }

    /// The authenticated artifact kind.
    pub const fn kind(&self) -> ArtifactKind {
        self.kind
    }

    /// Version of the master key used to derive this artifact's key.
    pub const fn key_version(&self) -> u32 {
        self.key_version
    }

    /// The SHA-256 receipt for the plaintext.
    pub fn plaintext_sha256(&self) -> &str {
        &self.plaintext_sha256
    }

    /// The exact plaintext length.
    pub const fn plaintext_len(&self) -> u64 {
        self.plaintext_len
    }

    /// Creation timestamp in Unix milliseconds.
    pub const fn created_at_unix_ms(&self) -> i64 {
        self.created_at_unix_ms
    }

    /// The artifact expiration timestamp in Unix milliseconds.
    pub const fn expires_at_unix_ms(&self) -> i64 {
        self.expires_at_unix_ms
    }

    fn validate(&self, config: &ArtifactStoreConfig) -> Result<(), ArtifactStoreError> {
        if self.scheme != "krw-artifact" {
            return Err(ArtifactStoreError::InvalidReference("unknown scheme"));
        }
        if self.format_version != FORMAT_VERSION {
            return Err(ArtifactStoreError::InvalidReference(
                "unsupported format version",
            ));
        }
        if self.key_version == 0 {
            return Err(ArtifactStoreError::InvalidReference(
                "key version zero is reserved",
            ));
        }
        validate_lower_hex(&self.artifact_id, ARTIFACT_ID_BYTES, "artifact id")?;
        validate_hash(&self.plaintext_sha256, "plaintext hash")?;
        if self.plaintext_len > config.max_plaintext_bytes {
            return Err(ArtifactStoreError::ArtifactTooLarge {
                actual: self.plaintext_len,
                limit: config.max_plaintext_bytes,
            });
        }
        if self.created_at_unix_ms < 0 || self.expires_at_unix_ms <= self.created_at_unix_ms {
            return Err(ArtifactStoreError::InvalidReference(
                "invalid artifact timestamps",
            ));
        }
        let lifetime_ms = self
            .expires_at_unix_ms
            .checked_sub(self.created_at_unix_ms)
            .ok_or(ArtifactStoreError::InvalidReference(
                "invalid artifact lifetime",
            ))?;
        let max_lifetime_ms = i64::try_from(config.max_ttl.as_millis()).map_err(|_| {
            ArtifactStoreError::InvalidConfig("max_ttl exceeds timestamp representation")
        })?;
        if lifetime_ms > max_lifetime_ms {
            return Err(ArtifactStoreError::InvalidReference(
                "artifact lifetime exceeds deployment bound",
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for ArtifactRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ArtifactRef")
            .field("scheme", &self.scheme)
            .field("format_version", &self.format_version)
            .field("artifact_id", &"<redacted>")
            .field("key_version", &self.key_version)
            .field("kind", &self.kind)
            .field("plaintext_sha256", &"<redacted>")
            .field("plaintext_len", &self.plaintext_len)
            .field("created_at_unix_ms", &self.created_at_unix_ms)
            .field("expires_at_unix_ms", &self.expires_at_unix_ms)
            .finish()
    }
}

/// Zeroizing plaintext returned by an artifact read.
pub struct SecretArtifact(Zeroizing<Vec<u8>>);

impl SecretArtifact {
    /// Borrows the plaintext without copying it.
    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    /// Consumes the wrapper and returns a new zeroizing buffer.
    pub fn into_zeroizing(self) -> Zeroizing<Vec<u8>> {
        self.0
    }
}

impl fmt::Debug for SecretArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SecretArtifact")
            .field("bytes", &"<redacted>")
            .field("len", &self.0.len())
            .finish()
    }
}

/// Why an authenticated artifact was deleted.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeletionCause {
    /// A run/principal retention lineage was explicitly purged.
    ScopeRetention,
    /// The artifact's authenticated expiry elapsed.
    Expired,
    /// An operator-authorized remediation removed it.
    OperatorRemediation,
    /// The owning run completed and no replay material remains necessary.
    RunCompaction,
}

/// A receipt which callers can durably attach to their deletion lineage.
#[derive(Clone, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeletionReceipt {
    /// Opaque ID of the deleted artifact.
    pub artifact_id: String,
    /// Plaintext receipt hash; never the plaintext itself.
    pub plaintext_sha256: String,
    /// Authenticated artifact kind.
    pub kind: ArtifactKind,
    /// Deletion reason.
    pub cause: DeletionCause,
    /// Time the authenticated deletion operation began.
    pub deleted_at_unix_ms: i64,
}

impl fmt::Debug for DeletionReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeletionReceipt")
            .field("artifact_id", &"<redacted>")
            .field("plaintext_sha256", &"<redacted>")
            .field("kind", &self.kind)
            .field("cause", &self.cause)
            .field("deleted_at_unix_ms", &self.deleted_at_unix_ms)
            .finish()
    }
}

/// Work bounds for a maintenance operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaintenanceBudget {
    /// Maximum queued artifact entries examined.
    pub max_examined: usize,
    /// Maximum authenticated artifacts deleted.
    pub max_deleted: usize,
}

/// Bounded maintenance outcome. Re-run while `truncated` is true.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaintenanceReport {
    /// Number of queued artifact entries examined.
    pub examined: usize,
    /// Authenticated deletion receipts.
    pub deleted: Vec<DeletionReceipt>,
    /// Corrupt/unauthorized candidates retained for investigation.
    pub rejected: usize,
    /// Whether the configured work budget stopped the scan.
    pub truncated: bool,
}

/// Runtime limits for the local artifact backend.
#[derive(Clone, Debug)]
pub struct ArtifactStoreConfig {
    /// Exact maximum plaintext length accepted by this deployment.
    pub max_plaintext_bytes: u64,
    /// Longest accepted TTL.
    pub max_ttl: Duration,
    /// Largest per-call directory scan.
    pub max_maintenance_examined: usize,
    /// Largest per-call deletion batch.
    pub max_maintenance_deleted: usize,
    /// Maximum concurrent local blocking I/O/crypto jobs.
    pub max_blocking_operations: usize,
}

impl Default for ArtifactStoreConfig {
    fn default() -> Self {
        Self {
            max_plaintext_bytes: 4 * 1024 * 1024,
            max_ttl: Duration::from_hours(24),
            max_maintenance_examined: 2_048,
            max_maintenance_deleted: 256,
            max_blocking_operations: 16,
        }
    }
}

impl ArtifactStoreConfig {
    fn validate(&self) -> Result<(), ArtifactStoreError> {
        if self.max_plaintext_bytes == 0 {
            return Err(ArtifactStoreError::InvalidConfig(
                "max_plaintext_bytes must be positive",
            ));
        }
        if self.max_plaintext_bytes > usize::MAX as u64 {
            return Err(ArtifactStoreError::InvalidConfig(
                "max_plaintext_bytes exceeds platform size",
            ));
        }
        if self.max_ttl.is_zero() {
            return Err(ArtifactStoreError::InvalidConfig(
                "max_ttl must be positive",
            ));
        }
        if self.max_maintenance_examined == 0 || self.max_maintenance_deleted == 0 {
            return Err(ArtifactStoreError::InvalidConfig(
                "maintenance limits must be positive",
            ));
        }
        if self.max_maintenance_deleted > self.max_maintenance_examined {
            return Err(ArtifactStoreError::InvalidConfig(
                "delete limit cannot exceed scan limit",
            ));
        }
        if self.max_blocking_operations == 0 || self.max_blocking_operations > 1_024 {
            return Err(ArtifactStoreError::InvalidConfig(
                "blocking operation limit must be between 1 and 1024",
            ));
        }
        Ok(())
    }
}

/// One versioned 256-bit root key. Debug output and drops never expose it.
pub struct VersionedMasterKey {
    version: u32,
    material: Zeroizing<[u8; KEY_BYTES]>,
}

impl VersionedMasterKey {
    /// Wraps key bytes supplied by a secret manager.
    pub fn new(version: u32, material: [u8; KEY_BYTES]) -> Result<Self, ArtifactStoreError> {
        if version == 0 {
            return Err(ArtifactStoreError::InvalidKeyring(
                "key version zero is reserved",
            ));
        }
        if material.iter().all(|byte| *byte == 0) {
            return Err(ArtifactStoreError::InvalidKeyring(
                "all-zero master keys are forbidden",
            ));
        }
        Ok(Self {
            version,
            material: Zeroizing::new(material),
        })
    }
}

impl fmt::Debug for VersionedMasterKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VersionedMasterKey")
            .field("version", &self.version)
            .field("material", &"<redacted>")
            .finish()
    }
}

/// Active and historical key versions needed for short-TTL reads and rotation.
pub struct MasterKeyring {
    active_version: u32,
    keys: BTreeMap<u32, Zeroizing<[u8; KEY_BYTES]>>,
}

impl MasterKeyring {
    /// Builds a keyring and rejects duplicate/missing active versions.
    pub fn new(
        active_version: u32,
        keys: impl IntoIterator<Item = VersionedMasterKey>,
    ) -> Result<Self, ArtifactStoreError> {
        let mut by_version = BTreeMap::new();
        for mut key in keys {
            if by_version.len() >= MAX_KEY_VERSIONS {
                return Err(ArtifactStoreError::InvalidKeyring("too many key versions"));
            }
            if by_version
                .values()
                .any(|material: &Zeroizing<[u8; KEY_BYTES]>| {
                    constant_time_eq(material.as_slice(), key.material.as_slice())
                })
            {
                return Err(ArtifactStoreError::InvalidKeyring(
                    "master key material is reused across versions",
                ));
            }
            if by_version
                .insert(key.version, key.material.clone())
                .is_some()
            {
                return Err(ArtifactStoreError::InvalidKeyring("duplicate key version"));
            }
            key.material.zeroize();
        }
        if !by_version.contains_key(&active_version) {
            return Err(ArtifactStoreError::InvalidKeyring(
                "active key version is absent",
            ));
        }
        Ok(Self {
            active_version,
            keys: by_version,
        })
    }

    fn active(&self) -> (u32, &[u8; KEY_BYTES]) {
        let material = self
            .keys
            .get(&self.active_version)
            .expect("validated keyring always has active key");
        (self.active_version, material)
    }

    fn get(&self, version: u32) -> Result<&[u8; KEY_BYTES], ArtifactStoreError> {
        self.keys
            .get(&version)
            .map(|material| &**material)
            .ok_or(ArtifactStoreError::UnknownKeyVersion)
    }
}

impl fmt::Debug for MasterKeyring {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MasterKeyring")
            .field("active_version", &self.active_version)
            .field("available_versions", &self.keys.keys().collect::<Vec<_>>())
            .field("key_material", &"<redacted>")
            .finish()
    }
}

/// Store failures never embed paths, scope values, plaintext, or key material.
#[derive(Debug, Error)]
pub enum ArtifactStoreError {
    /// Deployment limits are inconsistent.
    #[error("invalid artifact-store configuration: {0}")]
    InvalidConfig(&'static str),
    /// Scope identifiers violate the bounded contract.
    #[error("invalid artifact scope component: {0}")]
    InvalidScope(&'static str),
    /// A database/reference value is not a canonical artifact reference.
    #[error("invalid artifact reference: {0}")]
    InvalidReference(&'static str),
    /// Keyring versions are inconsistent.
    #[error("invalid artifact keyring: {0}")]
    InvalidKeyring(&'static str),
    /// The referenced historical key is unavailable.
    #[error("artifact key version is unavailable")]
    UnknownKeyVersion,
    /// The payload exceeds its deployment bound.
    #[error("artifact payload is {actual} bytes; deployment limit is {limit}")]
    ArtifactTooLarge { actual: u64, limit: u64 },
    /// TTL is zero or above the deployment maximum.
    #[error("artifact TTL is outside deployment bounds")]
    InvalidTtl,
    /// OS cryptographic entropy failed.
    #[error("cryptographic entropy source failed")]
    Entropy,
    /// HKDF or AEAD rejected an internal operation.
    #[error("artifact cryptographic operation failed")]
    Cryptography,
    /// A reference is missing, outside the supplied scope, or failed AEAD.
    #[error("artifact was not found or was not authorized")]
    NotFoundOrUnauthorized,
    /// Authenticated content/hash/length/envelope invariants failed.
    #[error("artifact envelope is corrupt")]
    CorruptEnvelope,
    /// The authenticated artifact has expired.
    #[error("artifact has expired")]
    Expired,
    /// A random opaque artifact ID already exists; no overwrite occurred.
    #[error("artifact identifier collision; retry the write")]
    Collision,
    /// Maintenance request exceeds the configured bound.
    #[error("artifact maintenance budget is outside deployment bounds")]
    InvalidMaintenanceBudget,
    /// Local storage I/O failed; no path is included.
    #[error("artifact storage I/O failed during {operation}: {kind:?}")]
    Io {
        operation: &'static str,
        kind: std::io::ErrorKind,
    },
    /// Canonical metadata serialization failed.
    #[error("artifact metadata serialization failed")]
    Serialization,
    /// A blocking backend task failed to join.
    #[error("artifact backend task failed")]
    TaskJoin,
}

/// Durable encrypted artifact boundary, suitable for a future remote backend.
#[async_trait]
pub trait ArtifactStore: fmt::Debug + Send + Sync {
    /// Encrypts, durably publishes, and returns an opaque reference.
    async fn put(
        &self,
        scope: &ArtifactScope,
        kind: ArtifactKind,
        plaintext: &[u8],
        ttl: Duration,
    ) -> Result<ArtifactRef, ArtifactStoreError>;

    /// Authenticates scope/reference/envelope and returns zeroizing plaintext.
    async fn get(
        &self,
        scope: &ArtifactScope,
        reference: &ArtifactRef,
    ) -> Result<SecretArtifact, ArtifactStoreError>;

    /// Authenticates and durably deletes one exact artifact.
    async fn delete(
        &self,
        scope: &ArtifactScope,
        reference: &ArtifactRef,
        cause: DeletionCause,
    ) -> Result<DeletionReceipt, ArtifactStoreError>;

    /// Authenticates and deletes a bounded batch for one exact run lineage.
    async fn delete_lineage(
        &self,
        scope: &ArtifactScope,
        budget: MaintenanceBudget,
        cause: DeletionCause,
    ) -> Result<MaintenanceReport, ArtifactStoreError>;

    /// Deletes a bounded batch only after authenticating each expiry claim.
    async fn sweep_expired(
        &self,
        budget: MaintenanceBudget,
    ) -> Result<MaintenanceReport, ArtifactStoreError>;
}

fn validate_scope_component(name: &'static str, value: &str) -> Result<(), ArtifactStoreError> {
    if value.is_empty()
        || value.len() > MAX_SCOPE_COMPONENT_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(ArtifactStoreError::InvalidScope(name));
    }
    Ok(())
}

fn hash_len_prefixed(hasher: &mut Sha256, value: &[u8]) {
    let len = u64::try_from(value.len()).expect("bounded values fit u64");
    hasher.update(len.to_be_bytes());
    hasher.update(value);
}

fn format_hash(bytes: &[u8]) -> String {
    format!("{HASH_PREFIX}{}", hex::encode(bytes))
}

fn validate_hash(value: &str, field: &'static str) -> Result<(), ArtifactStoreError> {
    let hex_value = value
        .strip_prefix(HASH_PREFIX)
        .ok_or(ArtifactStoreError::InvalidReference(field))?;
    validate_lower_hex(hex_value, 32, field)
}

fn validate_lower_hex(
    value: &str,
    bytes: usize,
    field: &'static str,
) -> Result<(), ArtifactStoreError> {
    if value.len() != bytes.saturating_mul(2)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ArtifactStoreError::InvalidReference(field));
    }
    Ok(())
}

fn io_error(operation: &'static str, error: &std::io::Error) -> ArtifactStoreError {
    ArtifactStoreError::Io {
        operation,
        kind: error.kind(),
    }
}

#[derive(Clone, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct ArtifactHeader {
    format_version: u16,
    key_version: u32,
    kind: ArtifactKind,
    scope_sha256: String,
    artifact_id: String,
    plaintext_sha256: String,
    plaintext_len: u64,
    created_at_unix_ms: i64,
    expires_at_unix_ms: i64,
    salt_hex: String,
    nonce_hex: String,
}

impl fmt::Debug for ArtifactHeader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ArtifactHeader")
            .field("format_version", &self.format_version)
            .field("key_version", &self.key_version)
            .field("kind", &self.kind)
            .field("scope_sha256", &"<redacted>")
            .field("artifact_id", &"<redacted>")
            .field("plaintext_sha256", &"<redacted>")
            .field("plaintext_len", &self.plaintext_len)
            .field("created_at_unix_ms", &self.created_at_unix_ms)
            .field("expires_at_unix_ms", &self.expires_at_unix_ms)
            .field("salt_hex", &"<redacted>")
            .field("nonce_hex", &"<redacted>")
            .finish()
    }
}

impl ArtifactHeader {
    fn as_reference(&self) -> ArtifactRef {
        ArtifactRef {
            scheme: "krw-artifact".to_owned(),
            format_version: self.format_version,
            artifact_id: self.artifact_id.clone(),
            key_version: self.key_version,
            kind: self.kind,
            plaintext_sha256: self.plaintext_sha256.clone(),
            plaintext_len: self.plaintext_len,
            created_at_unix_ms: self.created_at_unix_ms,
            expires_at_unix_ms: self.expires_at_unix_ms,
        }
    }

    fn validate(&self, config: &ArtifactStoreConfig) -> Result<(), ArtifactStoreError> {
        self.as_reference().validate(config)?;
        validate_hash(&self.scope_sha256, "scope hash")?;
        validate_lower_hex(&self.salt_hex, SALT_BYTES, "HKDF salt")?;
        validate_lower_hex(&self.nonce_hex, NONCE_BYTES, "AEAD nonce")?;
        Ok(())
    }
}

trait Clock: fmt::Debug + Send + Sync {
    fn now_unix_ms(&self) -> Result<i64, ArtifactStoreError>;
}

#[derive(Debug)]
struct SystemClock;

impl Clock for SystemClock {
    fn now_unix_ms(&self) -> Result<i64, ArtifactStoreError> {
        let duration = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| ArtifactStoreError::InvalidConfig("system clock predates Unix epoch"))?;
        i64::try_from(duration.as_millis())
            .map_err(|_| ArtifactStoreError::InvalidConfig("system clock exceeds i64 millis"))
    }
}

trait EntropySource: fmt::Debug + Send + Sync {
    fn fill(&self, destination: &mut [u8]) -> Result<(), ArtifactStoreError>;
}

#[derive(Debug)]
struct OsEntropy;

impl EntropySource for OsEntropy {
    fn fill(&self, destination: &mut [u8]) -> Result<(), ArtifactStoreError> {
        getrandom::fill(destination).map_err(|_| ArtifactStoreError::Entropy)
    }
}

struct LocalArtifactStoreInner {
    root: PathBuf,
    keyring: MasterKeyring,
    config: ArtifactStoreConfig,
    clock: Arc<dyn Clock>,
    entropy: Arc<dyn EntropySource>,
    io_permits: Arc<Semaphore>,
    maintenance_cursor: Mutex<MaintenanceCursor>,
    enqueue_generation: AtomicU8,
}

impl fmt::Debug for LocalArtifactStoreInner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalArtifactStoreInner")
            .field("root", &"<redacted>")
            .field("keyring", &self.keyring)
            .field("config", &self.config)
            .field("clock", &self.clock)
            .field("entropy", &self.entropy)
            .field("io_permits", &"<bounded>")
            .field("maintenance_cursor", &"<bounded durable cursor>")
            .field("enqueue_generation", &"<bounded generation>")
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct MaintenanceCursor {
    generation: u8,
    shard: u8,
    in_progress: bool,
}

/// Atomic, durable local-filesystem implementation of [`ArtifactStore`].
#[derive(Clone)]
pub struct LocalArtifactStore {
    inner: Arc<LocalArtifactStoreInner>,
}

impl LocalArtifactStore {
    /// Opens or creates a private artifact directory.
    ///
    /// `root` must be absolute. An existing symlink is rejected, and Unix
    /// permissions are tightened to `0700` for the directory.
    pub fn open(
        root: impl AsRef<Path>,
        keyring: MasterKeyring,
        config: ArtifactStoreConfig,
    ) -> Result<Self, ArtifactStoreError> {
        Self::open_with_components(
            root.as_ref(),
            keyring,
            config,
            Arc::new(SystemClock),
            Arc::new(OsEntropy),
        )
    }

    fn open_with_components(
        root: &Path,
        keyring: MasterKeyring,
        config: ArtifactStoreConfig,
        clock: Arc<dyn Clock>,
        entropy: Arc<dyn EntropySource>,
    ) -> Result<Self, ArtifactStoreError> {
        config.validate()?;
        if !root.is_absolute() {
            return Err(ArtifactStoreError::InvalidConfig(
                "artifact root must be absolute",
            ));
        }
        let canonical_root = prepare_root(root)?;
        prepare_storage_layout(&canonical_root)?;
        let cursor = load_or_create_maintenance_cursor(&canonical_root, entropy.as_ref())?;
        let enqueue_generation = if cursor.in_progress {
            next_generation(cursor.generation)
        } else {
            cursor.generation
        };
        recover_temporary_links(&canonical_root, enqueue_generation)?;
        let max_blocking_operations = config.max_blocking_operations;
        Ok(Self {
            inner: Arc::new(LocalArtifactStoreInner {
                root: canonical_root,
                keyring,
                config,
                clock,
                entropy,
                io_permits: Arc::new(Semaphore::new(max_blocking_operations)),
                maintenance_cursor: Mutex::new(cursor),
                enqueue_generation: AtomicU8::new(enqueue_generation),
            }),
        })
    }

    fn validate_maintenance_budget(
        &self,
        budget: MaintenanceBudget,
    ) -> Result<(), ArtifactStoreError> {
        if budget.max_examined == 0
            || budget.max_deleted == 0
            || budget.max_deleted > budget.max_examined
            || budget.max_examined > self.inner.config.max_maintenance_examined
            || budget.max_deleted > self.inner.config.max_maintenance_deleted
        {
            return Err(ArtifactStoreError::InvalidMaintenanceBudget);
        }
        Ok(())
    }

    async fn acquire_io_permit(&self) -> Result<OwnedSemaphorePermit, ArtifactStoreError> {
        Arc::clone(&self.inner.io_permits)
            .acquire_owned()
            .await
            .map_err(|_| ArtifactStoreError::TaskJoin)
    }
}

impl fmt::Debug for LocalArtifactStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalArtifactStore")
            .field("root", &"<redacted>")
            .field("active_key_version", &self.inner.keyring.active_version)
            .field("config", &self.inner.config)
            .finish()
    }
}

struct LoadedEnvelope {
    header: ArtifactHeader,
    aad: Vec<u8>,
    ciphertext: Vec<u8>,
    identity: FileIdentity,
}

impl fmt::Debug for LoadedEnvelope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoadedEnvelope")
            .field("header", &self.header)
            .field("aad", &"<redacted>")
            .field("aad_len", &self.aad.len())
            .field("ciphertext", &"<redacted>")
            .field("ciphertext_len", &self.ciphertext.len())
            .field("identity", &self.identity)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    len: u64,
}

fn prepare_root(root: &Path) -> Result<PathBuf, ArtifactStoreError> {
    match fs::symlink_metadata(root) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(ArtifactStoreError::InvalidConfig(
                    "artifact root must be a real directory",
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = root.parent().ok_or(ArtifactStoreError::InvalidConfig(
                "artifact root has no parent",
            ))?;
            let parent_metadata = fs::symlink_metadata(parent)
                .map_err(|error| io_error("inspect root parent", &error))?;
            if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
                return Err(ArtifactStoreError::InvalidConfig(
                    "artifact root parent must be a real directory",
                ));
            }
            fs::create_dir(root).map_err(|error| io_error("create root", &error))?;
        }
        Err(error) => return Err(io_error("inspect root", &error)),
    }

    set_private_directory_permissions(root)?;
    let canonical =
        fs::canonicalize(root).map_err(|error| io_error("canonicalize root", &error))?;
    let metadata = fs::symlink_metadata(&canonical)
        .map_err(|error| io_error("inspect canonical root", &error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ArtifactStoreError::InvalidConfig(
            "canonical artifact root is not a real directory",
        ));
    }
    sync_directory(&canonical)?;
    Ok(canonical)
}

fn ensure_root_safe(root: &Path) -> Result<(), ArtifactStoreError> {
    let metadata = fs::symlink_metadata(root).map_err(|error| io_error("inspect root", &error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ArtifactStoreError::InvalidConfig(
            "artifact root changed type",
        ));
    }
    ensure_private_directory_permissions(&metadata)?;
    Ok(())
}

#[cfg(unix)]
fn ensure_private_directory_permissions(metadata: &fs::Metadata) -> Result<(), ArtifactStoreError> {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(ArtifactStoreError::InvalidConfig(
            "artifact root permissions are not private",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_private_directory_permissions(
    _metadata: &fs::Metadata,
) -> Result<(), ArtifactStoreError> {
    Ok(())
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<(), ArtifactStoreError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| io_error("set root permissions", &error))
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<(), ArtifactStoreError> {
    Ok(())
}

fn prepare_private_directory(path: &Path) -> Result<(), ArtifactStoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(ArtifactStoreError::InvalidConfig(
                    "artifact storage directory must be a real directory",
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|error| io_error("create storage directory", &error))?;
        }
        Err(error) => return Err(io_error("inspect storage directory", &error)),
    }
    set_private_directory_permissions(path)
}

fn ensure_private_directory_safe(path: &Path) -> Result<(), ArtifactStoreError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| io_error("inspect storage directory", &error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ArtifactStoreError::InvalidConfig(
            "artifact storage directory changed type",
        ));
    }
    ensure_private_directory_permissions(&metadata)
}

fn artifacts_root(root: &Path) -> PathBuf {
    root.join(ARTIFACTS_DIRECTORY)
}

fn maintenance_root(root: &Path) -> PathBuf {
    root.join(MAINTENANCE_DIRECTORY)
}

fn staging_root(root: &Path) -> PathBuf {
    root.join(STAGING_DIRECTORY)
}

fn shard_name(shard: u8) -> String {
    format!("{shard:02x}")
}

fn artifact_shard(root: &Path, shard: u8) -> PathBuf {
    artifacts_root(root).join(shard_name(shard))
}

fn maintenance_generation_root(root: &Path, generation: u8) -> PathBuf {
    maintenance_root(root).join(generation.to_string())
}

fn maintenance_shard(root: &Path, generation: u8, shard: u8) -> PathBuf {
    maintenance_generation_root(root, generation).join(shard_name(shard))
}

fn prepare_storage_layout(root: &Path) -> Result<(), ArtifactStoreError> {
    let artifact_root = artifacts_root(root);
    let staging = staging_root(root);
    let maintenance = maintenance_root(root);
    prepare_private_directory(&artifact_root)?;
    prepare_private_directory(&staging)?;
    prepare_private_directory(&maintenance)?;
    for generation in 0..MAINTENANCE_GENERATIONS {
        prepare_private_directory(&maintenance_generation_root(root, generation))?;
    }
    for shard in 0..SHARD_COUNT {
        let shard = u8::try_from(shard).expect("shard bound fits u8");
        prepare_private_directory(&artifact_shard(root, shard))?;
        for generation in 0..MAINTENANCE_GENERATIONS {
            prepare_private_directory(&maintenance_shard(root, generation, shard))?;
        }
    }
    sync_directory(&artifact_root)?;
    sync_directory(&staging)?;
    for generation in 0..MAINTENANCE_GENERATIONS {
        sync_directory(&maintenance_generation_root(root, generation))?;
    }
    sync_directory(&maintenance)?;
    sync_directory(root)
}

fn next_generation(generation: u8) -> u8 {
    debug_assert!(generation < MAINTENANCE_GENERATIONS);
    (generation + 1) % MAINTENANCE_GENERATIONS
}

fn cursor_path(root: &Path) -> PathBuf {
    root.join(MAINTENANCE_CURSOR_FILE)
}

fn encode_cursor(cursor: MaintenanceCursor) -> [u8; 11] {
    let mut bytes = [0_u8; 11];
    bytes[..CURSOR_MAGIC.len()].copy_from_slice(CURSOR_MAGIC);
    bytes[8] = cursor.generation;
    bytes[9] = cursor.shard;
    bytes[10] = u8::from(cursor.in_progress);
    bytes
}

fn decode_cursor(bytes: &[u8]) -> Result<MaintenanceCursor, ArtifactStoreError> {
    if bytes.len() != 11 || &bytes[..CURSOR_MAGIC.len()] != CURSOR_MAGIC {
        return Err(ArtifactStoreError::InvalidConfig(
            "maintenance cursor is corrupt",
        ));
    }
    let cursor = MaintenanceCursor {
        generation: bytes[8],
        shard: bytes[9],
        in_progress: match bytes[10] {
            0 => false,
            1 => true,
            _ => {
                return Err(ArtifactStoreError::InvalidConfig(
                    "maintenance cursor is corrupt",
                ));
            }
        },
    };
    if cursor.generation >= MAINTENANCE_GENERATIONS {
        return Err(ArtifactStoreError::InvalidConfig(
            "maintenance cursor is corrupt",
        ));
    }
    Ok(cursor)
}

fn persist_maintenance_cursor(
    root: &Path,
    cursor: MaintenanceCursor,
    entropy: &dyn EntropySource,
) -> Result<(), ArtifactStoreError> {
    ensure_root_safe(root)?;
    let random = fill_array::<16>(entropy)?;
    let temporary = root.join(format!("{CURSOR_TEMP_PREFIX}{}", hex::encode(random)));
    let operation = (|| {
        let mut file = private_create_new(&temporary)?;
        file.write_all(&encode_cursor(cursor))
            .map_err(|error| io_error("write maintenance cursor", &error))?;
        file.sync_all()
            .map_err(|error| io_error("sync maintenance cursor", &error))?;
        fs::rename(&temporary, cursor_path(root))
            .map_err(|error| io_error("publish maintenance cursor", &error))?;
        sync_directory(root)
    })();
    if operation.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    operation
}

fn load_or_create_maintenance_cursor(
    root: &Path,
    entropy: &dyn EntropySource,
) -> Result<MaintenanceCursor, ArtifactStoreError> {
    let path = cursor_path(root);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(ArtifactStoreError::InvalidConfig(
                    "maintenance cursor must be a real file",
                ));
            }
            let file = nofollow_open(&path)?;
            let mut bytes = Vec::with_capacity(11);
            file.take(12)
                .read_to_end(&mut bytes)
                .map_err(|error| io_error("read maintenance cursor", &error))?;
            decode_cursor(&bytes)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let cursor = MaintenanceCursor::default();
            persist_maintenance_cursor(root, cursor, entropy)?;
            Ok(cursor)
        }
        Err(error) => Err(io_error("inspect maintenance cursor", &error)),
    }
}

fn artifact_path(root: &Path, artifact_id: &str) -> Result<PathBuf, ArtifactStoreError> {
    validate_lower_hex(artifact_id, ARTIFACT_ID_BYTES, "artifact id")?;
    let shard = u8::from_str_radix(&artifact_id[..2], 16)
        .map_err(|_| ArtifactStoreError::InvalidReference("invalid artifact id"))?;
    Ok(artifact_shard(root, shard).join(format!("{artifact_id}{ARTIFACT_SUFFIX}")))
}

fn maintenance_token_path(
    root: &Path,
    generation: u8,
    artifact_id: &str,
) -> Result<PathBuf, ArtifactStoreError> {
    validate_lower_hex(artifact_id, ARTIFACT_ID_BYTES, "artifact id")?;
    if generation >= MAINTENANCE_GENERATIONS {
        return Err(ArtifactStoreError::InvalidConfig(
            "maintenance generation is invalid",
        ));
    }
    let shard = u8::from_str_radix(&artifact_id[..2], 16)
        .map_err(|_| ArtifactStoreError::InvalidReference("invalid artifact id"))?;
    Ok(maintenance_shard(root, generation, shard).join(format!("{artifact_id}{ARTIFACT_SUFFIX}")))
}

fn is_artifact_filename(name: &str) -> bool {
    name.strip_suffix(ARTIFACT_SUFFIX)
        .is_some_and(|id| validate_lower_hex(id, ARTIFACT_ID_BYTES, "artifact id").is_ok())
}

#[cfg(unix)]
fn private_create_new(path: &Path) -> Result<File, ArtifactStoreError> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                ArtifactStoreError::Collision
            } else {
                io_error("create artifact", &error)
            }
        })
}

#[cfg(not(unix))]
fn private_create_new(path: &Path) -> Result<File, ArtifactStoreError> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                ArtifactStoreError::Collision
            } else {
                io_error("create artifact", &error)
            }
        })
}

#[cfg(unix)]
fn nofollow_open(path: &Path) -> Result<File, ArtifactStoreError> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| {
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) {
                ArtifactStoreError::NotFoundOrUnauthorized
            } else {
                io_error("open artifact", &error)
            }
        })
}

#[cfg(not(unix))]
fn nofollow_open(path: &Path) -> Result<File, ArtifactStoreError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| ArtifactStoreError::NotFoundOrUnauthorized)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ArtifactStoreError::NotFoundOrUnauthorized);
    }
    OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|_| ArtifactStoreError::NotFoundOrUnauthorized)
}

fn sync_directory(path: &Path) -> Result<(), ArtifactStoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(path)
            .map_err(|error| io_error("open directory for sync", &error))?;
        directory
            .sync_all()
            .map_err(|error| io_error("sync directory", &error))?;
    }
    #[cfg(not(unix))]
    {
        let directory = File::open(path).map_err(|error| io_error("open directory", &error))?;
        directory
            .sync_all()
            .map_err(|error| io_error("sync directory", &error))?;
    }
    Ok(())
}

fn publish_maintenance_token(
    root: &Path,
    generation: u8,
    artifact_id: &str,
) -> Result<(), ArtifactStoreError> {
    let path = maintenance_token_path(root, generation, artifact_id)?;
    let parent = path.parent().ok_or(ArtifactStoreError::InvalidConfig(
        "maintenance token has no parent",
    ))?;
    ensure_private_directory_safe(parent)?;
    match private_create_new(&path) {
        Ok(file) => {
            file.sync_all()
                .map_err(|error| io_error("sync maintenance token", &error))?;
            sync_directory(parent)
        }
        Err(ArtifactStoreError::Collision) => {
            let metadata = fs::symlink_metadata(&path)
                .map_err(|error| io_error("inspect maintenance token", &error))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() != 0 {
                return Err(ArtifactStoreError::InvalidConfig(
                    "maintenance token is not a real empty file",
                ));
            }
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn temporary_artifact_id(name: &str) -> Option<&str> {
    let remainder = name.strip_prefix(TEMP_PREFIX)?;
    let (artifact_id, random) = remainder.split_once('-')?;
    if validate_lower_hex(artifact_id, ARTIFACT_ID_BYTES, "artifact id").is_err()
        || validate_lower_hex(random, 16, "temporary random value").is_err()
    {
        return None;
    }
    Some(artifact_id)
}

fn recover_temporary_links(root: &Path, enqueue_generation: u8) -> Result<(), ArtifactStoreError> {
    let directory = staging_root(root);
    ensure_private_directory_safe(&directory)?;
    let entries =
        fs::read_dir(&directory).map_err(|error| io_error("scan temporary files", &error))?;
    let mut changed = false;
    for entry in entries.take(2_048) {
        let entry = entry.map_err(|error| io_error("read temporary entry", &error))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let artifact_id = temporary_artifact_id(&name).ok_or(ArtifactStoreError::InvalidConfig(
            "staging entry has an invalid name",
        ))?;
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| io_error("inspect temporary artifact", &error))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(ArtifactStoreError::InvalidConfig(
                "staging entry must be a real file",
            ));
        }
        let target = artifact_path(root, artifact_id)?;
        match fs::symlink_metadata(&target) {
            Ok(target_metadata)
                if !target_metadata.file_type().is_symlink() && target_metadata.is_file() =>
            {
                publish_maintenance_token(root, enqueue_generation, artifact_id)?;
            }
            Ok(_) => {
                return Err(ArtifactStoreError::InvalidConfig(
                    "recovered artifact target changed type",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error("inspect recovered artifact", &error)),
        }
        fs::remove_file(entry.path())
            .map_err(|error| io_error("remove temporary artifact", &error))?;
        changed = true;
    }
    if changed {
        sync_directory(&directory)?;
    }
    Ok(())
}

fn fill_array<const N: usize>(entropy: &dyn EntropySource) -> Result<[u8; N], ArtifactStoreError> {
    let mut bytes = [0_u8; N];
    entropy.fill(&mut bytes)?;
    Ok(bytes)
}

fn plaintext_hash(plaintext: &[u8]) -> String {
    format_hash(Sha256::digest(plaintext).as_slice())
}

fn ttl_millis(ttl: Duration, max_ttl: Duration) -> Result<i64, ArtifactStoreError> {
    if ttl.is_zero() || ttl > max_ttl {
        return Err(ArtifactStoreError::InvalidTtl);
    }
    i64::try_from(ttl.as_millis()).map_err(|_| ArtifactStoreError::InvalidTtl)
}

fn derive_artifact_key(
    master_key: &[u8; KEY_BYTES],
    header: &ArtifactHeader,
) -> Result<Zeroizing<[u8; KEY_BYTES]>, ArtifactStoreError> {
    let salt = decode_array::<SALT_BYTES>(&header.salt_hex)?;
    let hkdf = Hkdf::<Sha256>::new(Some(&salt), master_key);
    let mut info = Vec::with_capacity(256);
    info.extend_from_slice(HKDF_DOMAIN);
    info.extend_from_slice(&header.format_version.to_be_bytes());
    info.extend_from_slice(&header.key_version.to_be_bytes());
    append_len_prefixed(&mut info, artifact_kind_bytes(header.kind))?;
    append_len_prefixed(&mut info, header.scope_sha256.as_bytes())?;
    append_len_prefixed(&mut info, header.artifact_id.as_bytes())?;
    let mut derived = Zeroizing::new([0_u8; KEY_BYTES]);
    hkdf.expand(&info, derived.as_mut())
        .map_err(|_| ArtifactStoreError::Cryptography)?;
    info.zeroize();
    Ok(derived)
}

fn artifact_kind_bytes(kind: ArtifactKind) -> &'static [u8] {
    match kind {
        ArtifactKind::ProviderEpisode => b"provider_episode",
        ArtifactKind::ActionArguments => b"action_arguments",
        ArtifactKind::ActionResult => b"action_result",
        ArtifactKind::RecoveryState => b"recovery_state",
    }
}

fn append_len_prefixed(destination: &mut Vec<u8>, value: &[u8]) -> Result<(), ArtifactStoreError> {
    let length = u32::try_from(value.len()).map_err(|_| ArtifactStoreError::Cryptography)?;
    destination.extend_from_slice(&length.to_be_bytes());
    destination.extend_from_slice(value);
    Ok(())
}

fn decode_array<const N: usize>(value: &str) -> Result<[u8; N], ArtifactStoreError> {
    validate_lower_hex(value, N, "encrypted metadata")?;
    let decoded = hex::decode(value).map_err(|_| ArtifactStoreError::CorruptEnvelope)?;
    decoded
        .try_into()
        .map_err(|_| ArtifactStoreError::CorruptEnvelope)
}

fn canonical_header(header: &ArtifactHeader) -> Result<Vec<u8>, ArtifactStoreError> {
    serde_jcs::to_vec(header).map_err(|_| ArtifactStoreError::Serialization)
}

fn encrypt_artifact(
    keyring: &MasterKeyring,
    header: &ArtifactHeader,
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, ArtifactStoreError> {
    let (_, master_key) = keyring.active();
    let derived = derive_artifact_key(master_key, header)?;
    let cipher = XChaCha20Poly1305::new(Key::from_slice(derived.as_ref()));
    let nonce_bytes = decode_array::<NONCE_BYTES>(&header.nonce_hex)?;
    let nonce = XNonce::from_slice(&nonce_bytes);
    let mut encrypted = Zeroizing::new(plaintext.to_vec());
    cipher
        .encrypt_in_place(nonce, aad, &mut *encrypted)
        .map_err(|_| ArtifactStoreError::Cryptography)?;
    Ok(encrypted.to_vec())
}

fn decrypt_artifact(
    keyring: &MasterKeyring,
    header: &ArtifactHeader,
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<SecretArtifact, ArtifactStoreError> {
    let master_key = keyring.get(header.key_version)?;
    let derived = derive_artifact_key(master_key, header)?;
    let cipher = XChaCha20Poly1305::new(Key::from_slice(derived.as_ref()));
    let nonce_bytes = decode_array::<NONCE_BYTES>(&header.nonce_hex)?;
    let nonce = XNonce::from_slice(&nonce_bytes);
    let mut plaintext = Zeroizing::new(ciphertext.to_vec());
    cipher
        .decrypt_in_place(nonce, aad, &mut *plaintext)
        .map_err(|_| ArtifactStoreError::NotFoundOrUnauthorized)?;
    Ok(SecretArtifact(plaintext))
}

fn encode_envelope(aad: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, ArtifactStoreError> {
    let header_len = u32::try_from(aad.len()).map_err(|_| ArtifactStoreError::Serialization)?;
    if aad.len() > MAX_HEADER_BYTES {
        return Err(ArtifactStoreError::Serialization);
    }
    let mut envelope = Vec::with_capacity(
        FILE_MAGIC
            .len()
            .saturating_add(4)
            .saturating_add(aad.len())
            .saturating_add(ciphertext.len()),
    );
    envelope.extend_from_slice(FILE_MAGIC);
    envelope.extend_from_slice(&header_len.to_be_bytes());
    envelope.extend_from_slice(aad);
    envelope.extend_from_slice(ciphertext);
    Ok(envelope)
}

fn decode_envelope(
    bytes: &[u8],
    config: &ArtifactStoreConfig,
    identity: FileIdentity,
) -> Result<LoadedEnvelope, ArtifactStoreError> {
    if bytes.len()
        < FILE_MAGIC
            .len()
            .saturating_add(4)
            .saturating_add(AEAD_TAG_BYTES)
    {
        return Err(ArtifactStoreError::CorruptEnvelope);
    }
    if &bytes[..FILE_MAGIC.len()] != FILE_MAGIC {
        return Err(ArtifactStoreError::CorruptEnvelope);
    }
    let header_start = FILE_MAGIC.len().saturating_add(4);
    let header_len_bytes: [u8; 4] = bytes[FILE_MAGIC.len()..header_start]
        .try_into()
        .map_err(|_| ArtifactStoreError::CorruptEnvelope)?;
    let header_len = usize::try_from(u32::from_be_bytes(header_len_bytes))
        .map_err(|_| ArtifactStoreError::CorruptEnvelope)?;
    if header_len == 0 || header_len > MAX_HEADER_BYTES {
        return Err(ArtifactStoreError::CorruptEnvelope);
    }
    let ciphertext_start = header_start
        .checked_add(header_len)
        .ok_or(ArtifactStoreError::CorruptEnvelope)?;
    if ciphertext_start > bytes.len() {
        return Err(ArtifactStoreError::CorruptEnvelope);
    }
    let aad = bytes[header_start..ciphertext_start].to_vec();
    let header: ArtifactHeader =
        serde_json::from_slice(&aad).map_err(|_| ArtifactStoreError::CorruptEnvelope)?;
    header
        .validate(config)
        .map_err(|_| ArtifactStoreError::CorruptEnvelope)?;
    let recanonical = canonical_header(&header)?;
    if !constant_time_eq(&recanonical, &aad) {
        return Err(ArtifactStoreError::CorruptEnvelope);
    }
    let ciphertext = bytes[ciphertext_start..].to_vec();
    let expected_ciphertext_len = usize::try_from(header.plaintext_len)
        .ok()
        .and_then(|length| length.checked_add(AEAD_TAG_BYTES))
        .ok_or(ArtifactStoreError::CorruptEnvelope)?;
    if ciphertext.len() != expected_ciphertext_len {
        return Err(ArtifactStoreError::CorruptEnvelope);
    }
    Ok(LoadedEnvelope {
        header,
        aad,
        ciphertext,
        identity,
    })
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let difference = left
        .iter()
        .zip(right)
        .fold(0_u8, |accumulator, (left, right)| {
            accumulator | (left ^ right)
        });
    difference == 0
}

fn file_identity(metadata: &fs::Metadata) -> Result<FileIdentity, ArtifactStoreError> {
    if !metadata.is_file() {
        return Err(ArtifactStoreError::NotFoundOrUnauthorized);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;
        if metadata.nlink() != 1 || metadata.permissions().mode() & 0o077 != 0 {
            return Err(ArtifactStoreError::NotFoundOrUnauthorized);
        }
        Ok(FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
            len: metadata.len(),
        })
    }
    #[cfg(not(unix))]
    {
        Ok(FileIdentity {
            len: metadata.len(),
        })
    }
}

fn maximum_file_bytes(config: &ArtifactStoreConfig) -> Result<u64, ArtifactStoreError> {
    let fixed = u64::try_from(
        FILE_MAGIC
            .len()
            .saturating_add(4)
            .saturating_add(MAX_HEADER_BYTES),
    )
    .map_err(|_| ArtifactStoreError::InvalidConfig("platform file bound overflow"))?;
    config
        .max_plaintext_bytes
        .checked_add(u64::try_from(AEAD_TAG_BYTES).expect("small constant fits u64"))
        .and_then(|size| size.checked_add(fixed))
        .ok_or(ArtifactStoreError::InvalidConfig(
            "artifact file bound overflow",
        ))
}

fn load_envelope(
    root: &Path,
    path: &Path,
    config: &ArtifactStoreConfig,
) -> Result<LoadedEnvelope, ArtifactStoreError> {
    ensure_root_safe(root)?;
    let path_parent = path
        .parent()
        .ok_or(ArtifactStoreError::NotFoundOrUnauthorized)?;
    let artifact_root = artifacts_root(root);
    if path_parent.parent() != Some(artifact_root.as_path()) {
        return Err(ArtifactStoreError::NotFoundOrUnauthorized);
    }
    ensure_private_directory_safe(&artifact_root)?;
    ensure_private_directory_safe(path_parent)?;
    let mut file = nofollow_open(path)?;
    let metadata = file
        .metadata()
        .map_err(|error| io_error("inspect artifact", &error))?;
    let identity = file_identity(&metadata)?;
    if identity.len > maximum_file_bytes(config)? {
        return Err(ArtifactStoreError::CorruptEnvelope);
    }
    let capacity =
        usize::try_from(identity.len).map_err(|_| ArtifactStoreError::CorruptEnvelope)?;
    let mut bytes = Vec::with_capacity(capacity);
    file.read_to_end(&mut bytes)
        .map_err(|error| io_error("read artifact", &error))?;
    if bytes.len() != capacity {
        return Err(ArtifactStoreError::CorruptEnvelope);
    }
    decode_envelope(&bytes, config, identity)
}

fn verify_header_matches_reference(
    header: &ArtifactHeader,
    reference: &ArtifactRef,
) -> Result<(), ArtifactStoreError> {
    if header.as_reference() != *reference {
        return Err(ArtifactStoreError::NotFoundOrUnauthorized);
    }
    Ok(())
}

fn authenticate_loaded(
    keyring: &MasterKeyring,
    loaded: &LoadedEnvelope,
) -> Result<SecretArtifact, ArtifactStoreError> {
    let plaintext = decrypt_artifact(keyring, &loaded.header, &loaded.aad, &loaded.ciphertext)?;
    let actual_len =
        u64::try_from(plaintext.expose().len()).map_err(|_| ArtifactStoreError::CorruptEnvelope)?;
    let actual_hash = plaintext_hash(plaintext.expose());
    if actual_len != loaded.header.plaintext_len
        || !constant_time_eq(
            actual_hash.as_bytes(),
            loaded.header.plaintext_sha256.as_bytes(),
        )
    {
        return Err(ArtifactStoreError::CorruptEnvelope);
    }
    Ok(plaintext)
}

fn publish_envelope(
    root: &Path,
    artifact_id: &str,
    bytes: &[u8],
    entropy: &dyn EntropySource,
    enqueue_generation: u8,
) -> Result<(), ArtifactStoreError> {
    ensure_root_safe(root)?;
    let target = artifact_path(root, artifact_id)?;
    let target_directory = target.parent().ok_or(ArtifactStoreError::InvalidConfig(
        "artifact target has no parent",
    ))?;
    ensure_private_directory_safe(target_directory)?;
    let staging = staging_root(root);
    ensure_private_directory_safe(&staging)?;
    let temporary_random = fill_array::<16>(entropy)?;
    let temporary = staging.join(format!(
        "{TEMP_PREFIX}{artifact_id}-{}",
        hex::encode(temporary_random)
    ));
    let mut target_published = false;
    let operation = (|| {
        let mut file = private_create_new(&temporary)?;
        file.write_all(bytes)
            .map_err(|error| io_error("write artifact", &error))?;
        file.sync_all()
            .map_err(|error| io_error("sync artifact", &error))?;
        sync_directory(&staging)?;
        publish_maintenance_token(root, enqueue_generation, artifact_id)?;
        fs::hard_link(&temporary, &target).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                ArtifactStoreError::Collision
            } else {
                io_error("publish artifact", &error)
            }
        })?;
        target_published = true;
        sync_directory(target_directory)?;
        fs::remove_file(&temporary)
            .map_err(|error| io_error("retire temporary artifact", &error))?;
        sync_directory(&staging)
    })();
    if operation.is_err() {
        if target_published {
            let _ = fs::remove_file(&target);
        }
        let _ = fs::remove_file(&temporary);
        let _ = sync_directory(target_directory);
        let _ = sync_directory(&staging);
    }
    operation
}

fn put_sync(
    inner: &LocalArtifactStoreInner,
    scope: &ArtifactScope,
    kind: ArtifactKind,
    plaintext: &[u8],
    ttl: Duration,
) -> Result<ArtifactRef, ArtifactStoreError> {
    let plaintext_len =
        u64::try_from(plaintext.len()).map_err(|_| ArtifactStoreError::ArtifactTooLarge {
            actual: u64::MAX,
            limit: inner.config.max_plaintext_bytes,
        })?;
    if plaintext_len > inner.config.max_plaintext_bytes {
        return Err(ArtifactStoreError::ArtifactTooLarge {
            actual: plaintext_len,
            limit: inner.config.max_plaintext_bytes,
        });
    }
    let ttl_ms = ttl_millis(ttl, inner.config.max_ttl)?;
    let created_at_unix_ms = inner.clock.now_unix_ms()?;
    let expires_at_unix_ms = created_at_unix_ms
        .checked_add(ttl_ms)
        .ok_or(ArtifactStoreError::InvalidTtl)?;
    let artifact_id = hex::encode(fill_array::<ARTIFACT_ID_BYTES>(inner.entropy.as_ref())?);
    let salt_hex = hex::encode(fill_array::<SALT_BYTES>(inner.entropy.as_ref())?);
    let nonce_hex = hex::encode(fill_array::<NONCE_BYTES>(inner.entropy.as_ref())?);
    let (key_version, _) = inner.keyring.active();
    let header = ArtifactHeader {
        format_version: FORMAT_VERSION,
        key_version,
        kind,
        scope_sha256: scope.digest(),
        artifact_id,
        plaintext_sha256: plaintext_hash(plaintext),
        plaintext_len,
        created_at_unix_ms,
        expires_at_unix_ms,
        salt_hex,
        nonce_hex,
    };
    let aad = canonical_header(&header)?;
    let ciphertext = encrypt_artifact(&inner.keyring, &header, &aad, plaintext)?;
    let envelope = encode_envelope(&aad, &ciphertext)?;
    publish_envelope(
        &inner.root,
        &header.artifact_id,
        &envelope,
        inner.entropy.as_ref(),
        inner.enqueue_generation.load(Ordering::Acquire),
    )?;
    Ok(header.as_reference())
}

fn get_sync(
    inner: &LocalArtifactStoreInner,
    scope: &ArtifactScope,
    reference: &ArtifactRef,
    allow_expired: bool,
) -> Result<(SecretArtifact, LoadedEnvelope), ArtifactStoreError> {
    reference.validate(&inner.config)?;
    let path = artifact_path(&inner.root, &reference.artifact_id)?;
    let loaded = load_envelope(&inner.root, &path, &inner.config)?;
    verify_header_matches_reference(&loaded.header, reference)?;
    let expected_scope = scope.digest();
    if !constant_time_eq(
        loaded.header.scope_sha256.as_bytes(),
        expected_scope.as_bytes(),
    ) {
        return Err(ArtifactStoreError::NotFoundOrUnauthorized);
    }
    let plaintext = authenticate_loaded(&inner.keyring, &loaded)?;
    if !allow_expired && inner.clock.now_unix_ms()? >= loaded.header.expires_at_unix_ms {
        return Err(ArtifactStoreError::Expired);
    }
    Ok((plaintext, loaded))
}

fn verify_current_identity(path: &Path, expected: FileIdentity) -> Result<(), ArtifactStoreError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| ArtifactStoreError::NotFoundOrUnauthorized)?;
    if metadata.file_type().is_symlink() {
        return Err(ArtifactStoreError::NotFoundOrUnauthorized);
    }
    let actual = file_identity(&metadata)?;
    if actual != expected {
        return Err(ArtifactStoreError::NotFoundOrUnauthorized);
    }
    Ok(())
}

fn durable_remove(
    root: &Path,
    reference: &ArtifactRef,
    identity: FileIdentity,
) -> Result<(), ArtifactStoreError> {
    let path = artifact_path(root, &reference.artifact_id)?;
    verify_current_identity(&path, identity)?;
    fs::remove_file(&path).map_err(|error| io_error("delete artifact", &error))?;
    let parent = path.parent().ok_or(ArtifactStoreError::InvalidConfig(
        "artifact target has no parent",
    ))?;
    sync_directory(parent)?;
    remove_maintenance_tokens(root, &reference.artifact_id)
}

fn remove_maintenance_tokens(root: &Path, artifact_id: &str) -> Result<(), ArtifactStoreError> {
    for generation in 0..MAINTENANCE_GENERATIONS {
        let path = maintenance_token_path(root, generation, artifact_id)?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(ArtifactStoreError::InvalidConfig(
                        "maintenance token changed type",
                    ));
                }
                fs::remove_file(&path)
                    .map_err(|error| io_error("delete maintenance token", &error))?;
                let parent = path.parent().ok_or(ArtifactStoreError::InvalidConfig(
                    "maintenance token has no parent",
                ))?;
                sync_directory(parent)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error("inspect maintenance token", &error)),
        }
    }
    Ok(())
}

fn deletion_receipt(
    reference: &ArtifactRef,
    cause: DeletionCause,
    deleted_at_unix_ms: i64,
) -> DeletionReceipt {
    DeletionReceipt {
        artifact_id: reference.artifact_id.clone(),
        plaintext_sha256: reference.plaintext_sha256.clone(),
        kind: reference.kind,
        cause,
        deleted_at_unix_ms,
    }
}

#[async_trait]
impl ArtifactStore for LocalArtifactStore {
    async fn put(
        &self,
        scope: &ArtifactScope,
        kind: ArtifactKind,
        plaintext: &[u8],
        ttl: Duration,
    ) -> Result<ArtifactRef, ArtifactStoreError> {
        let inner = Arc::clone(&self.inner);
        let scope = scope.clone();
        let permit = self.acquire_io_permit().await?;
        let plaintext = Zeroizing::new(plaintext.to_vec());
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            put_sync(&inner, &scope, kind, &plaintext, ttl)
        })
        .await
        .map_err(|_| ArtifactStoreError::TaskJoin)?
    }

    async fn get(
        &self,
        scope: &ArtifactScope,
        reference: &ArtifactRef,
    ) -> Result<SecretArtifact, ArtifactStoreError> {
        let inner = Arc::clone(&self.inner);
        let scope = scope.clone();
        let reference = reference.clone();
        let permit = self.acquire_io_permit().await?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            get_sync(&inner, &scope, &reference, false).map(|(plaintext, _)| plaintext)
        })
        .await
        .map_err(|_| ArtifactStoreError::TaskJoin)?
    }

    async fn delete(
        &self,
        scope: &ArtifactScope,
        reference: &ArtifactRef,
        cause: DeletionCause,
    ) -> Result<DeletionReceipt, ArtifactStoreError> {
        let inner = Arc::clone(&self.inner);
        let scope = scope.clone();
        let reference = reference.clone();
        let permit = self.acquire_io_permit().await?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let (plaintext, loaded) = get_sync(&inner, &scope, &reference, true)?;
            drop(plaintext);
            let deleted_at = inner.clock.now_unix_ms()?;
            durable_remove(&inner.root, &reference, loaded.identity)?;
            Ok(deletion_receipt(&reference, cause, deleted_at))
        })
        .await
        .map_err(|_| ArtifactStoreError::TaskJoin)?
    }

    async fn delete_lineage(
        &self,
        scope: &ArtifactScope,
        budget: MaintenanceBudget,
        cause: DeletionCause,
    ) -> Result<MaintenanceReport, ArtifactStoreError> {
        self.validate_maintenance_budget(budget)?;
        let inner = Arc::clone(&self.inner);
        let scope_digest = scope.digest();
        let permit = self.acquire_io_permit().await?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            maintenance_sync(&inner, budget, MaintenanceMode::Scope(&scope_digest), cause)
        })
        .await
        .map_err(|_| ArtifactStoreError::TaskJoin)?
    }

    async fn sweep_expired(
        &self,
        budget: MaintenanceBudget,
    ) -> Result<MaintenanceReport, ArtifactStoreError> {
        self.validate_maintenance_budget(budget)?;
        let inner = Arc::clone(&self.inner);
        let permit = self.acquire_io_permit().await?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            maintenance_sync(
                &inner,
                budget,
                MaintenanceMode::Expired,
                DeletionCause::Expired,
            )
        })
        .await
        .map_err(|_| ArtifactStoreError::TaskJoin)?
    }
}

#[derive(Clone, Copy)]
enum MaintenanceMode<'a> {
    Scope(&'a str),
    Expired,
}

fn maintenance_sync(
    inner: &LocalArtifactStoreInner,
    budget: MaintenanceBudget,
    mode: MaintenanceMode<'_>,
    cause: DeletionCause,
) -> Result<MaintenanceReport, ArtifactStoreError> {
    ensure_root_safe(&inner.root)?;
    let mut cursor = inner
        .maintenance_cursor
        .lock()
        .map_err(|_| ArtifactStoreError::InvalidConfig("maintenance cursor lock is unavailable"))?;
    if !cursor.in_progress {
        let started = MaintenanceCursor {
            in_progress: true,
            ..*cursor
        };
        persist_maintenance_cursor(&inner.root, started, inner.entropy.as_ref())?;
        *cursor = started;
        inner
            .enqueue_generation
            .store(next_generation(cursor.generation), Ordering::Release);
    }

    let mut examined = 0_usize;
    let mut rejected = 0_usize;
    let mut deleted = Vec::with_capacity(budget.max_deleted);
    let deleted_at = inner.clock.now_unix_ms()?;

    while examined < budget.max_examined && deleted.len() < budget.max_deleted {
        let active_generation = cursor.generation;
        let active_shard = cursor.shard;
        let directory = maintenance_shard(&inner.root, active_generation, active_shard);
        ensure_private_directory_safe(&directory)?;
        let mut entries =
            fs::read_dir(&directory).map_err(|error| io_error("scan maintenance shard", &error))?;
        let Some(entry) = entries.next() else {
            if active_shard == u8::MAX {
                cursor.generation = next_generation(active_generation);
                cursor.shard = 0;
                cursor.in_progress = false;
                inner
                    .enqueue_generation
                    .store(cursor.generation, Ordering::Release);
                break;
            }
            cursor.shard = active_shard.saturating_add(1);
            continue;
        };

        examined = examined.saturating_add(1);
        let entry = entry.map_err(|error| io_error("read maintenance entry", &error))?;
        let filename = entry.file_name();
        let filename_text = filename.to_string_lossy();
        let expected_id = filename_text
            .strip_suffix(ARTIFACT_SUFFIX)
            .filter(|_| is_artifact_filename(&filename_text));
        let inactive_generation = next_generation(active_generation);
        if let Some(expected_id) = expected_id {
            match inspect_maintenance_candidate(inner, expected_id, mode) {
                Ok(Some((reference, identity))) => {
                    if remove_maintenance_candidate(
                        &inner.root,
                        &entry.path(),
                        &reference,
                        identity,
                    )
                    .is_ok()
                    {
                        deleted.push(reference);
                    } else {
                        rejected = rejected.saturating_add(1);
                        move_maintenance_token(
                            &inner.root,
                            active_generation,
                            inactive_generation,
                            active_shard,
                            &entry.path(),
                            &filename,
                        )?;
                    }
                }
                Ok(None) => move_maintenance_token(
                    &inner.root,
                    active_generation,
                    inactive_generation,
                    active_shard,
                    &entry.path(),
                    &filename,
                )?,
                Err(_) => {
                    rejected = rejected.saturating_add(1);
                    let target = artifact_path(&inner.root, expected_id)?;
                    if target_missing(&target)? {
                        remove_stale_maintenance_token(&entry.path(), &directory)?;
                    } else {
                        move_maintenance_token(
                            &inner.root,
                            active_generation,
                            inactive_generation,
                            active_shard,
                            &entry.path(),
                            &filename,
                        )?;
                    }
                }
            }
        } else {
            rejected = rejected.saturating_add(1);
            move_maintenance_token(
                &inner.root,
                active_generation,
                inactive_generation,
                active_shard,
                &entry.path(),
                &filename,
            )?;
        }
    }

    persist_maintenance_cursor(&inner.root, *cursor, inner.entropy.as_ref())?;
    Ok(MaintenanceReport {
        examined,
        deleted: deleted
            .iter()
            .map(|reference| deletion_receipt(reference, cause, deleted_at))
            .collect(),
        rejected,
        truncated: cursor.in_progress,
    })
}

fn inspect_maintenance_candidate(
    inner: &LocalArtifactStoreInner,
    expected_id: &str,
    mode: MaintenanceMode<'_>,
) -> Result<Option<(ArtifactRef, FileIdentity)>, ArtifactStoreError> {
    let path = artifact_path(&inner.root, expected_id)?;
    let loaded = load_envelope(&inner.root, &path, &inner.config)?;
    if loaded.header.artifact_id != expected_id {
        return Err(ArtifactStoreError::CorruptEnvelope);
    }
    let selected = match mode {
        MaintenanceMode::Scope(scope_digest) => constant_time_eq(
            loaded.header.scope_sha256.as_bytes(),
            scope_digest.as_bytes(),
        ),
        MaintenanceMode::Expired => inner.clock.now_unix_ms()? >= loaded.header.expires_at_unix_ms,
    };
    if !selected {
        return Ok(None);
    }

    let plaintext = authenticate_loaded(&inner.keyring, &loaded)?;
    drop(plaintext);
    let reference = loaded.header.as_reference();
    Ok(Some((reference, loaded.identity)))
}

fn remove_maintenance_candidate(
    root: &Path,
    token_path: &Path,
    reference: &ArtifactRef,
    identity: FileIdentity,
) -> Result<(), ArtifactStoreError> {
    let path = artifact_path(root, &reference.artifact_id)?;
    verify_current_identity(&path, identity)?;
    fs::remove_file(&path).map_err(|error| io_error("delete artifact", &error))?;
    let artifact_parent = path.parent().ok_or(ArtifactStoreError::InvalidConfig(
        "artifact target has no parent",
    ))?;
    sync_directory(artifact_parent)?;
    fs::remove_file(token_path).map_err(|error| io_error("delete maintenance token", &error))?;
    let token_parent = token_path
        .parent()
        .ok_or(ArtifactStoreError::InvalidConfig(
            "maintenance token has no parent",
        ))?;
    sync_directory(token_parent)
}

fn target_missing(path: &Path) -> Result<bool, ArtifactStoreError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(io_error("inspect maintenance artifact", &error)),
    }
}

fn remove_stale_maintenance_token(
    token_path: &Path,
    token_parent: &Path,
) -> Result<(), ArtifactStoreError> {
    fs::remove_file(token_path)
        .map_err(|error| io_error("delete stale maintenance token", &error))?;
    sync_directory(token_parent)
}

fn move_maintenance_token(
    root: &Path,
    active_generation: u8,
    inactive_generation: u8,
    shard: u8,
    source: &Path,
    filename: &OsStr,
) -> Result<(), ArtifactStoreError> {
    let source_parent = maintenance_shard(root, active_generation, shard);
    if source.parent() != Some(source_parent.as_path()) {
        return Err(ArtifactStoreError::InvalidConfig(
            "maintenance token escaped its shard",
        ));
    }
    let destination_parent = maintenance_shard(root, inactive_generation, shard);
    ensure_private_directory_safe(&source_parent)?;
    ensure_private_directory_safe(&destination_parent)?;
    let destination = destination_parent.join(filename);
    match fs::symlink_metadata(&destination) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(ArtifactStoreError::InvalidConfig(
                    "maintenance token destination changed type",
                ));
            }
            fs::remove_file(source)
                .map_err(|error| io_error("deduplicate maintenance token", &error))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::rename(source, &destination)
                .map_err(|error| io_error("rotate maintenance token", &error))?;
        }
        Err(error) => return Err(io_error("inspect maintenance token destination", &error)),
    }
    sync_directory(&source_parent)?;
    sync_directory(&destination_parent)
}

#[cfg(test)]
mod tests {
    use std::io::{Seek, SeekFrom};
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

    use tempfile::TempDir;

    use super::*;

    const START_MS: i64 = 1_800_000_000_000;

    #[derive(Debug)]
    struct TestClock(AtomicI64);

    impl TestClock {
        fn new(now: i64) -> Self {
            Self(AtomicI64::new(now))
        }

        fn set(&self, now: i64) {
            self.0.store(now, Ordering::SeqCst);
        }
    }

    impl Clock for TestClock {
        fn now_unix_ms(&self) -> Result<i64, ArtifactStoreError> {
            Ok(self.0.load(Ordering::SeqCst))
        }
    }

    #[derive(Debug)]
    struct FixedEntropy(u8);

    impl EntropySource for FixedEntropy {
        fn fill(&self, destination: &mut [u8]) -> Result<(), ArtifactStoreError> {
            destination.fill(self.0);
            Ok(())
        }
    }

    #[derive(Debug)]
    struct ScriptedShardEntropy {
        shards: Vec<u8>,
        thirty_two_byte_fills: AtomicUsize,
    }

    impl ScriptedShardEntropy {
        fn new(shards: impl Into<Vec<u8>>) -> Self {
            Self {
                shards: shards.into(),
                thirty_two_byte_fills: AtomicUsize::new(0),
            }
        }
    }

    impl EntropySource for ScriptedShardEntropy {
        fn fill(&self, destination: &mut [u8]) -> Result<(), ArtifactStoreError> {
            if destination.len() == ARTIFACT_ID_BYTES {
                let fill = self.thirty_two_byte_fills.fetch_add(1, Ordering::SeqCst);
                let artifact_index = fill / 2;
                if fill.is_multiple_of(2) {
                    let unique = u8::try_from(artifact_index + 1).expect("small test script");
                    destination.fill(unique);
                    destination[0] = self.shards[artifact_index];
                    destination[1] = unique;
                    return Ok(());
                }
            }
            destination.fill(0xa5);
            Ok(())
        }
    }

    fn key(version: u32, byte: u8) -> VersionedMasterKey {
        VersionedMasterKey::new(version, [byte; KEY_BYTES]).expect("valid test key")
    }

    fn keyring(version: u32, byte: u8) -> MasterKeyring {
        MasterKeyring::new(version, [key(version, byte)]).expect("valid test keyring")
    }

    fn scope(run: &str) -> ArtifactScope {
        ArtifactScope::new("tenant-secret", "principal-secret", run).expect("valid scope")
    }

    fn test_store(
        directory: &TempDir,
        clock: Arc<TestClock>,
        entropy: Arc<dyn EntropySource>,
        config: ArtifactStoreConfig,
    ) -> LocalArtifactStore {
        LocalArtifactStore::open_with_components(
            &directory.path().join("artifacts"),
            keyring(7, 0x42),
            config,
            clock,
            entropy,
        )
        .expect("test store opens")
    }

    fn reference_path(store: &LocalArtifactStore, reference: &ArtifactRef) -> PathBuf {
        artifact_path(&store.inner.root, reference.artifact_id()).expect("valid reference path")
    }

    fn artifact_file_count(store: &LocalArtifactStore) -> usize {
        (0..SHARD_COUNT)
            .map(|shard| {
                let shard = u8::try_from(shard).expect("shard fits u8");
                fs::read_dir(artifact_shard(&store.inner.root, shard))
                    .expect("scan artifact shard")
                    .count()
            })
            .sum()
    }

    #[tokio::test]
    async fn encrypted_roundtrip_and_authenticated_delete() {
        let directory = TempDir::new().expect("temporary directory");
        let clock = Arc::new(TestClock::new(START_MS));
        let store = test_store(
            &directory,
            clock,
            Arc::new(OsEntropy),
            ArtifactStoreConfig::default(),
        );
        let scope = scope("run-roundtrip");
        let plaintext = b"opaque provider reasoning and private tool result";

        let reference = store
            .put(
                &scope,
                ArtifactKind::ProviderEpisode,
                plaintext,
                Duration::from_mins(1),
            )
            .await
            .expect("put succeeds");
        let persisted_reference: ArtifactRef =
            serde_json::from_slice(&serde_json::to_vec(&reference).expect("reference serializes"))
                .expect("reference deserializes");
        assert_eq!(persisted_reference, reference);
        let on_disk = fs::read(reference_path(&store, &reference)).expect("artifact file exists");
        assert!(
            !on_disk
                .windows(plaintext.len())
                .any(|window| window == plaintext)
        );

        let recovered = store
            .get(&scope, &persisted_reference)
            .await
            .expect("get succeeds");
        assert_eq!(recovered.expose(), plaintext);
        let receipt = store
            .delete(&scope, &reference, DeletionCause::RunCompaction)
            .await
            .expect("delete succeeds");
        assert_eq!(receipt.artifact_id, reference.artifact_id());
        assert_eq!(receipt.cause, DeletionCause::RunCompaction);
        assert!(matches!(
            store.get(&scope, &reference).await,
            Err(ArtifactStoreError::NotFoundOrUnauthorized)
        ));
    }

    #[tokio::test]
    async fn wrong_scope_and_wrong_key_cannot_decrypt() {
        let directory = TempDir::new().expect("temporary directory");
        let clock = Arc::new(TestClock::new(START_MS));
        let store = test_store(
            &directory,
            Arc::clone(&clock),
            Arc::new(OsEntropy),
            ArtifactStoreConfig::default(),
        );
        let owner = scope("run-owner");
        let reference = store
            .put(
                &owner,
                ArtifactKind::ActionResult,
                b"private result",
                Duration::from_mins(1),
            )
            .await
            .expect("put succeeds");

        assert!(matches!(
            store.get(&scope("run-attacker"), &reference).await,
            Err(ArtifactStoreError::NotFoundOrUnauthorized)
        ));

        let wrong_key_store = LocalArtifactStore::open_with_components(
            &store.inner.root,
            keyring(7, 0x99),
            ArtifactStoreConfig::default(),
            clock,
            Arc::new(OsEntropy),
        )
        .expect("store with wrong key opens");
        assert!(matches!(
            wrong_key_store.get(&owner, &reference).await,
            Err(ArtifactStoreError::NotFoundOrUnauthorized)
        ));
    }

    #[tokio::test]
    async fn historical_key_reads_after_rotation() {
        let directory = TempDir::new().expect("temporary directory");
        let clock = Arc::new(TestClock::new(START_MS));
        let store = LocalArtifactStore::open_with_components(
            &directory.path().join("artifacts"),
            keyring(1, 0x11),
            ArtifactStoreConfig::default(),
            Arc::clone(&clock) as Arc<dyn Clock>,
            Arc::new(OsEntropy),
        )
        .expect("initial store opens");
        let owner = scope("run-rotation");
        let reference = store
            .put(
                &owner,
                ArtifactKind::RecoveryState,
                b"checkpoint",
                Duration::from_mins(1),
            )
            .await
            .expect("put succeeds");
        drop(store);

        let rotated_keyring =
            MasterKeyring::new(2, [key(1, 0x11), key(2, 0x22)]).expect("rotated keyring");
        let rotated = LocalArtifactStore::open_with_components(
            &directory.path().join("artifacts"),
            rotated_keyring,
            ArtifactStoreConfig::default(),
            clock,
            Arc::new(OsEntropy),
        )
        .expect("rotated store opens");
        assert_eq!(
            rotated
                .get(&owner, &reference)
                .await
                .expect("historical key reads")
                .expose(),
            b"checkpoint"
        );
    }

    #[tokio::test]
    async fn ciphertext_tamper_is_retained_and_rejected() {
        let directory = TempDir::new().expect("temporary directory");
        let clock = Arc::new(TestClock::new(START_MS));
        let store = test_store(
            &directory,
            clock,
            Arc::new(OsEntropy),
            ArtifactStoreConfig::default(),
        );
        let owner = scope("run-tamper");
        let reference = store
            .put(
                &owner,
                ArtifactKind::ActionArguments,
                b"canonical args",
                Duration::from_mins(1),
            )
            .await
            .expect("put succeeds");
        let path = reference_path(&store, &reference);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open artifact for test tamper");
        file.seek(SeekFrom::End(-1)).expect("seek to tag");
        let mut byte = [0_u8; 1];
        file.read_exact(&mut byte).expect("read tag byte");
        byte[0] ^= 0x80;
        file.seek(SeekFrom::End(-1)).expect("seek to tag again");
        file.write_all(&byte).expect("tamper tag");
        file.sync_all().expect("sync tamper");

        assert!(matches!(
            store.get(&owner, &reference).await,
            Err(ArtifactStoreError::NotFoundOrUnauthorized)
        ));
        assert!(
            path.exists(),
            "failed authentication must not delete evidence"
        );
    }

    #[tokio::test]
    async fn swapping_artifact_files_cannot_swap_meaning() {
        let directory = TempDir::new().expect("temporary directory");
        let store = test_store(
            &directory,
            Arc::new(TestClock::new(START_MS)),
            Arc::new(OsEntropy),
            ArtifactStoreConfig::default(),
        );
        let owner = scope("run-swap");
        let first = store
            .put(
                &owner,
                ArtifactKind::ActionArguments,
                b"first",
                Duration::from_mins(1),
            )
            .await
            .expect("first put");
        let second = store
            .put(
                &owner,
                ArtifactKind::ActionResult,
                b"second",
                Duration::from_mins(1),
            )
            .await
            .expect("second put");
        let first_path = reference_path(&store, &first);
        let second_path = reference_path(&store, &second);
        let temporary = store.inner.root.join("swap-temporary");
        fs::rename(&first_path, &temporary).expect("move first aside");
        fs::rename(&second_path, &first_path).expect("move second to first");
        fs::rename(&temporary, &second_path).expect("move first to second");

        assert!(matches!(
            store.get(&owner, &first).await,
            Err(ArtifactStoreError::NotFoundOrUnauthorized)
        ));
        assert!(matches!(
            store.get(&owner, &second).await,
            Err(ArtifactStoreError::NotFoundOrUnauthorized)
        ));
    }

    #[tokio::test]
    async fn expiry_is_checked_after_authentication_and_swept_boundedly() {
        let directory = TempDir::new().expect("temporary directory");
        let clock = Arc::new(TestClock::new(START_MS));
        let store = test_store(
            &directory,
            Arc::clone(&clock),
            Arc::new(OsEntropy),
            ArtifactStoreConfig::default(),
        );
        let owner = scope("run-expiry");
        let reference = store
            .put(
                &owner,
                ArtifactKind::ProviderEpisode,
                b"short lived",
                Duration::from_secs(1),
            )
            .await
            .expect("put succeeds");
        clock.set(START_MS + 1_001);
        assert!(matches!(
            store.get(&owner, &reference).await,
            Err(ArtifactStoreError::Expired)
        ));

        let report = store
            .sweep_expired(MaintenanceBudget {
                max_examined: 10,
                max_deleted: 10,
            })
            .await
            .expect("sweep succeeds");
        assert_eq!(report.deleted.len(), 1);
        assert_eq!(report.deleted[0].cause, DeletionCause::Expired);
        assert!(!reference_path(&store, &reference).exists());
    }

    #[tokio::test]
    async fn small_sweeps_reach_a_late_shard_across_restarts() {
        let directory = TempDir::new().expect("temporary directory");
        let root = directory.path().join("artifacts");
        let clock = Arc::new(TestClock::new(START_MS));
        let store = test_store(
            &directory,
            Arc::clone(&clock),
            Arc::new(ScriptedShardEntropy::new([0x00, 0x40, 0xff])),
            ArtifactStoreConfig::default(),
        );
        let owner = scope("run-restart-fairness");
        for payload in [b"early".as_slice(), b"middle".as_slice()] {
            store
                .put(
                    &owner,
                    ArtifactKind::RecoveryState,
                    payload,
                    Duration::from_mins(1),
                )
                .await
                .expect("long-lived blocker is stored");
        }
        let late_expired = store
            .put(
                &owner,
                ArtifactKind::RecoveryState,
                b"late expired target",
                Duration::from_secs(1),
            )
            .await
            .expect("late target is stored");
        clock.set(START_MS + 2_000);

        let one = MaintenanceBudget {
            max_examined: 1,
            max_deleted: 1,
        };
        let first = store
            .sweep_expired(one)
            .await
            .expect("first sweep succeeds");
        assert!(first.deleted.is_empty());
        assert!(first.truncated);
        drop(store);

        let restarted = LocalArtifactStore::open_with_components(
            &root,
            keyring(7, 0x42),
            ArtifactStoreConfig::default(),
            Arc::clone(&clock) as Arc<dyn Clock>,
            Arc::new(OsEntropy),
        )
        .expect("first restart succeeds");
        let second = restarted
            .sweep_expired(one)
            .await
            .expect("second sweep succeeds");
        assert!(second.deleted.is_empty());
        assert!(second.truncated);
        drop(restarted);

        let restarted = LocalArtifactStore::open_with_components(
            &root,
            keyring(7, 0x42),
            ArtifactStoreConfig::default(),
            clock,
            Arc::new(OsEntropy),
        )
        .expect("second restart succeeds");
        let third = restarted
            .sweep_expired(one)
            .await
            .expect("third sweep succeeds");
        assert_eq!(third.deleted.len(), 1);
        assert_eq!(third.deleted[0].artifact_id, late_expired.artifact_id());
        assert!(!reference_path(&restarted, &late_expired).exists());
    }

    #[tokio::test]
    async fn tampered_expired_candidate_is_not_swept() {
        let directory = TempDir::new().expect("temporary directory");
        let clock = Arc::new(TestClock::new(START_MS));
        let store = test_store(
            &directory,
            Arc::clone(&clock),
            Arc::new(OsEntropy),
            ArtifactStoreConfig::default(),
        );
        let reference = store
            .put(
                &scope("run-expired-tamper"),
                ArtifactKind::RecoveryState,
                b"state",
                Duration::from_secs(1),
            )
            .await
            .expect("put succeeds");
        let path = reference_path(&store, &reference);
        let mut bytes = fs::read(&path).expect("read envelope");
        let last = bytes.last_mut().expect("ciphertext has tag");
        *last ^= 1;
        fs::write(&path, bytes).expect("write tampered envelope");
        clock.set(START_MS + 2_000);

        let report = store
            .sweep_expired(MaintenanceBudget {
                max_examined: 10,
                max_deleted: 10,
            })
            .await
            .expect("sweep completes without deleting unauthenticated data");
        assert!(report.deleted.is_empty());
        assert_eq!(report.rejected, 1);
        assert!(path.exists());
    }

    #[tokio::test]
    async fn aad_metadata_tamper_is_not_accepted_by_expiry_sweep() {
        let directory = TempDir::new().expect("temporary directory");
        let clock = Arc::new(TestClock::new(START_MS));
        let store = test_store(
            &directory,
            Arc::clone(&clock),
            Arc::new(OsEntropy),
            ArtifactStoreConfig::default(),
        );
        let reference = store
            .put(
                &scope("run-aad-tamper"),
                ArtifactKind::RecoveryState,
                b"state",
                Duration::from_secs(1),
            )
            .await
            .expect("put succeeds");
        let path = reference_path(&store, &reference);
        let mut bytes = fs::read(&path).expect("read envelope");
        let needle = format!("\"created_at_unix_ms\":{START_MS}");
        let position = bytes
            .windows(needle.len())
            .position(|window| window == needle.as_bytes())
            .expect("canonical timestamp is present");
        let final_digit = position + needle.len() - 1;
        bytes[final_digit] = b'1';
        fs::write(&path, bytes).expect("write metadata tamper");
        clock.set(START_MS + 2_000);

        let report = store
            .sweep_expired(MaintenanceBudget {
                max_examined: 10,
                max_deleted: 10,
            })
            .await
            .expect("sweep rejects tampered AAD without deleting");
        assert!(report.deleted.is_empty());
        assert_eq!(report.rejected, 1);
        assert!(path.exists());
    }

    #[tokio::test]
    async fn lineage_delete_is_exactly_scope_isolated() {
        let directory = TempDir::new().expect("temporary directory");
        let store = test_store(
            &directory,
            Arc::new(TestClock::new(START_MS)),
            Arc::new(OsEntropy),
            ArtifactStoreConfig::default(),
        );
        let selected = scope("run-selected");
        let retained = scope("run-retained");
        for payload in [b"one".as_slice(), b"two".as_slice()] {
            store
                .put(
                    &selected,
                    ArtifactKind::ActionResult,
                    payload,
                    Duration::from_mins(1),
                )
                .await
                .expect("selected put succeeds");
        }
        let retained_reference = store
            .put(
                &retained,
                ArtifactKind::ActionResult,
                b"other principal lineage",
                Duration::from_mins(1),
            )
            .await
            .expect("retained put succeeds");

        let report = store
            .delete_lineage(
                &selected,
                MaintenanceBudget {
                    max_examined: 10,
                    max_deleted: 10,
                },
                DeletionCause::ScopeRetention,
            )
            .await
            .expect("lineage delete succeeds");
        assert_eq!(report.deleted.len(), 2);
        assert_eq!(
            store
                .get(&retained, &retained_reference)
                .await
                .expect("other run remains")
                .expose(),
            b"other principal lineage"
        );
    }

    #[tokio::test]
    async fn repeated_small_lineage_purges_are_fair_and_scope_isolated() {
        let directory = TempDir::new().expect("temporary directory");
        let store = test_store(
            &directory,
            Arc::new(TestClock::new(START_MS)),
            Arc::new(ScriptedShardEntropy::new([0x00, 0x40, 0x80, 0xff])),
            ArtifactStoreConfig::default(),
        );
        let selected = scope("run-selected-small-budget");
        let retained = scope("run-retained-small-budget");
        let retained_early = store
            .put(
                &retained,
                ArtifactKind::ActionResult,
                b"retained early",
                Duration::from_mins(1),
            )
            .await
            .expect("early retained artifact is stored");
        let selected_middle = store
            .put(
                &selected,
                ArtifactKind::ActionResult,
                b"selected middle",
                Duration::from_mins(1),
            )
            .await
            .expect("middle selected artifact is stored");
        let retained_late = store
            .put(
                &retained,
                ArtifactKind::ActionResult,
                b"retained late",
                Duration::from_mins(1),
            )
            .await
            .expect("late retained artifact is stored");
        let selected_last = store
            .put(
                &selected,
                ArtifactKind::ActionResult,
                b"selected last",
                Duration::from_mins(1),
            )
            .await
            .expect("last selected artifact is stored");

        let mut deleted_ids = Vec::new();
        for _ in 0..8 {
            let report = store
                .delete_lineage(
                    &selected,
                    MaintenanceBudget {
                        max_examined: 1,
                        max_deleted: 1,
                    },
                    DeletionCause::ScopeRetention,
                )
                .await
                .expect("small lineage purge succeeds");
            deleted_ids.extend(
                report
                    .deleted
                    .iter()
                    .map(|receipt| receipt.artifact_id.clone()),
            );
            if !report.truncated {
                break;
            }
        }

        assert_eq!(deleted_ids.len(), 2);
        assert!(deleted_ids.contains(&selected_middle.artifact_id().to_owned()));
        assert!(deleted_ids.contains(&selected_last.artifact_id().to_owned()));
        assert_eq!(
            store
                .get(&retained, &retained_early)
                .await
                .expect("early foreign scope remains")
                .expose(),
            b"retained early"
        );
        assert_eq!(
            store
                .get(&retained, &retained_late)
                .await
                .expect("late foreign scope remains")
                .expose(),
            b"retained late"
        );
    }

    #[tokio::test]
    async fn strict_bounds_fail_before_publication() {
        let directory = TempDir::new().expect("temporary directory");
        let config = ArtifactStoreConfig {
            max_plaintext_bytes: 8,
            max_ttl: Duration::from_secs(10),
            max_maintenance_examined: 10,
            max_maintenance_deleted: 10,
            max_blocking_operations: 2,
        };
        let store = test_store(
            &directory,
            Arc::new(TestClock::new(START_MS)),
            Arc::new(OsEntropy),
            config,
        );
        let owner = scope("run-bounds");
        assert!(matches!(
            store
                .put(
                    &owner,
                    ArtifactKind::RecoveryState,
                    b"123456789",
                    Duration::from_secs(1)
                )
                .await,
            Err(ArtifactStoreError::ArtifactTooLarge { .. })
        ));
        assert!(matches!(
            store
                .put(
                    &owner,
                    ArtifactKind::RecoveryState,
                    b"ok",
                    Duration::from_secs(11)
                )
                .await,
            Err(ArtifactStoreError::InvalidTtl)
        ));
        assert_eq!(artifact_file_count(&store), 0);
    }

    #[tokio::test]
    async fn random_id_collision_never_overwrites() {
        let directory = TempDir::new().expect("temporary directory");
        let store = test_store(
            &directory,
            Arc::new(TestClock::new(START_MS)),
            Arc::new(FixedEntropy(0x55)),
            ArtifactStoreConfig::default(),
        );
        let owner = scope("run-collision");
        let first = store
            .put(
                &owner,
                ArtifactKind::ActionArguments,
                b"original",
                Duration::from_mins(1),
            )
            .await
            .expect("first write succeeds");
        assert!(matches!(
            store
                .put(
                    &owner,
                    ArtifactKind::ActionArguments,
                    b"replacement",
                    Duration::from_mins(1)
                )
                .await,
            Err(ArtifactStoreError::Collision)
        ));
        assert_eq!(
            store
                .get(&owner, &first)
                .await
                .expect("original remains")
                .expose(),
            b"original"
        );
    }

    #[tokio::test]
    async fn reference_json_and_debug_never_include_scope_plaintext_or_keys() {
        let directory = TempDir::new().expect("temporary directory");
        let store = test_store(
            &directory,
            Arc::new(TestClock::new(START_MS)),
            Arc::new(OsEntropy),
            ArtifactStoreConfig::default(),
        );
        let owner = scope("run-json-secret");
        let secret = "highly-distinct-private-payload";
        let reference = store
            .put(
                &owner,
                ArtifactKind::ProviderEpisode,
                secret.as_bytes(),
                Duration::from_mins(1),
            )
            .await
            .expect("put succeeds");
        let json = serde_json::to_string(&reference).expect("reference serializes");
        let debug = format!("{reference:?} {owner:?} {store:?}");
        for forbidden in [
            secret,
            "tenant-secret",
            "principal-secret",
            "run-json-secret",
            &hex::encode([0x42; KEY_BYTES]),
        ] {
            assert!(!json.contains(forbidden));
            assert!(!debug.contains(forbidden));
        }
        assert!(!json.contains("ciphertext"));
        assert!(!json.contains("nonce"));
        assert!(!json.contains("salt"));
        assert!(debug.contains("<redacted>"));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_root_is_rejected() {
        use std::os::unix::fs::symlink;

        let directory = TempDir::new().expect("temporary directory");
        let real = directory.path().join("real");
        fs::create_dir(&real).expect("create real directory");
        let linked = directory.path().join("linked");
        symlink(&real, &linked).expect("create root symlink");
        assert!(matches!(
            LocalArtifactStore::open(&linked, keyring(1, 1), ArtifactStoreConfig::default()),
            Err(ArtifactStoreError::InvalidConfig(_))
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_root_and_artifact_modes_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let directory = TempDir::new().expect("temporary directory");
        let store = test_store(
            &directory,
            Arc::new(TestClock::new(START_MS)),
            Arc::new(OsEntropy),
            ArtifactStoreConfig::default(),
        );
        let reference = store
            .put(
                &scope("run-modes"),
                ArtifactKind::RecoveryState,
                b"state",
                Duration::from_mins(1),
            )
            .await
            .expect("put succeeds");
        let root_mode = fs::metadata(&store.inner.root)
            .expect("root metadata")
            .permissions()
            .mode()
            & 0o777;
        let file_mode = fs::metadata(reference_path(&store, &reference))
            .expect("file metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(root_mode, 0o700);
        assert_eq!(file_mode, 0o600);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_at_generated_target_is_never_followed_or_overwritten() {
        use std::os::unix::fs::symlink;

        let directory = TempDir::new().expect("temporary directory");
        let store = test_store(
            &directory,
            Arc::new(TestClock::new(START_MS)),
            Arc::new(FixedEntropy(0x55)),
            ArtifactStoreConfig::default(),
        );
        let outside = directory.path().join("outside-secret");
        fs::write(&outside, b"must remain unchanged").expect("write outside file");
        let generated_id = hex::encode([0x55; ARTIFACT_ID_BYTES]);
        let target = artifact_path(&store.inner.root, &generated_id).expect("target path");
        symlink(&outside, &target).expect("place hostile target symlink");

        assert!(matches!(
            store
                .put(
                    &scope("run-symlink-target"),
                    ArtifactKind::RecoveryState,
                    b"replacement",
                    Duration::from_mins(1)
                )
                .await,
            Err(ArtifactStoreError::Collision)
        ));
        assert_eq!(
            fs::read(&outside).expect("outside file remains"),
            b"must remain unchanged"
        );
    }
}
