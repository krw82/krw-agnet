//! Durable encrypted-CAS adapters shared by the daemon and run engine.
//!
//! `PostgreSQL` receives only bounded canonical references and hashes. Plaintext
//! provider episodes, capability arguments/results, and typed recovery state
//! remain inside the scope-authenticated artifact store.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use krw_agent_artifact_store::{
    ArtifactKind, ArtifactRef, ArtifactScope, ArtifactStore, ArtifactStoreError,
};
use krw_agent_persistence::daemon::{
    RecoveryArtifactFailure, RecoveryArtifactKind, RecoveryArtifactRequest, RecoveryArtifactStore,
};
use krw_agent_protocol::ContentHash;
pub use krw_agent_protocol::{CLAIM_PAYLOAD_SCHEMA_VERSION, PinnedExecutionContract};
use thiserror::Error;
use zeroize::Zeroizing;

mod bridge;
mod claim;
mod episode_provider;
#[cfg(feature = "http")]
mod executor;
mod memory;

pub use bridge::{
    DurableRunPersistence, DurableRunStore, FinalizationPolicy, RuntimePersistenceError,
};
pub use claim::{
    ClaimValidationError, ImmutableRunClaimV1, RunResourceProfileV1, ValidatedClaim, WorkloadClass,
    validate_claim,
};
pub use episode_provider::PermitBoundProvider;
#[cfg(feature = "http")]
pub use executor::{
    ExecutorBuildError, ProductionClaimedRunExecutor, ProductionReleaseCatalog,
    ProductionReleaseEntry, ProviderCatalog, ProviderCatalogError, RoutedClaimError,
};
pub use memory::{
    MemoryResolutionError, MemoryResolutionReceipt, ResolvedSessionMemory,
    SessionMemoryPageAccumulator,
};

const MAX_REFERENCE_BYTES: usize = 2_048;
const MAX_REPOSITORY_ARTIFACT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArtifactTtlPolicy {
    pub provider_episode: Duration,
    pub action_arguments: Duration,
    pub action_result: Duration,
    pub recovery_state: Duration,
}

impl Default for ArtifactTtlPolicy {
    fn default() -> Self {
        Self {
            provider_episode: Duration::from_hours(6),
            action_arguments: Duration::from_hours(6),
            action_result: Duration::from_hours(6),
            recovery_state: Duration::from_hours(6),
        }
    }
}

impl ArtifactTtlPolicy {
    fn validate(self) -> Result<Self, ArtifactRepositoryError> {
        if [
            self.provider_episode,
            self.action_arguments,
            self.action_result,
            self.recovery_state,
        ]
        .into_iter()
        .any(|duration| duration.is_zero())
        {
            return Err(ArtifactRepositoryError::InvalidTtlPolicy);
        }
        Ok(self)
    }

    const fn for_kind(self, kind: ArtifactKind) -> Duration {
        match kind {
            ArtifactKind::ProviderEpisode => self.provider_episode,
            ArtifactKind::ActionArguments => self.action_arguments,
            ArtifactKind::ActionResult => self.action_result,
            ArtifactKind::RecoveryState => self.recovery_state,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ArtifactReceipt {
    pub artifact_ref: String,
    pub plaintext_hash: ContentHash,
    pub plaintext_size_bytes: u64,
    pub kind: ArtifactKind,
}

impl fmt::Debug for ArtifactReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ArtifactReceipt")
            .field("artifact_ref", &"[REDACTED]")
            .field("plaintext_hash", &self.plaintext_hash)
            .field("plaintext_size_bytes", &self.plaintext_size_bytes)
            .field("kind", &self.kind)
            .finish()
    }
}

#[derive(Debug, Error)]
pub enum ArtifactRepositoryError {
    #[error("artifact TTL policy contains a zero duration")]
    InvalidTtlPolicy,
    #[error("artifact repository payload exceeded its hard bound")]
    PayloadTooLarge,
    #[error("artifact store failed: {0}")]
    Store(#[from] ArtifactStoreError),
    #[error("artifact reference serialization failed")]
    ReferenceSerialization,
    #[error("artifact store returned an inconsistent receipt")]
    InconsistentReceipt,
}

#[derive(Clone)]
pub struct ArtifactRepository {
    store: Arc<dyn ArtifactStore>,
    ttl: ArtifactTtlPolicy,
}

impl ArtifactRepository {
    pub fn new(
        store: Arc<dyn ArtifactStore>,
        ttl: ArtifactTtlPolicy,
    ) -> Result<Self, ArtifactRepositoryError> {
        Ok(Self {
            store,
            ttl: ttl.validate()?,
        })
    }

    pub async fn put(
        &self,
        scope: &ArtifactScope,
        kind: ArtifactKind,
        plaintext: &[u8],
    ) -> Result<ArtifactReceipt, ArtifactRepositoryError> {
        if plaintext.is_empty() || plaintext.len() > MAX_REPOSITORY_ARTIFACT_BYTES {
            return Err(ArtifactRepositoryError::PayloadTooLarge);
        }
        let expected_hash = ContentHash::sha256(plaintext);
        let expected_size =
            u64::try_from(plaintext.len()).map_err(|_| ArtifactRepositoryError::PayloadTooLarge)?;
        let reference = self
            .store
            .put(scope, kind, plaintext, self.ttl.for_kind(kind))
            .await?;
        let encoded = encode_reference(&reference)?;
        if reference.kind() != kind
            || reference.plaintext_sha256() != expected_hash.as_str()
            || reference.plaintext_len() != expected_size
            || encoded.len() > MAX_REFERENCE_BYTES
        {
            return Err(ArtifactRepositoryError::InconsistentReceipt);
        }
        Ok(ArtifactReceipt {
            artifact_ref: encoded,
            plaintext_hash: expected_hash,
            plaintext_size_bytes: expected_size,
            kind,
        })
    }
}

impl fmt::Debug for ArtifactRepository {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ArtifactRepository")
            .field("store", &self.store)
            .field("ttl", &self.ttl)
            .finish()
    }
}

fn encode_reference(reference: &ArtifactRef) -> Result<String, ArtifactRepositoryError> {
    let bytes = serde_jcs::to_vec(reference)
        .map_err(|_| ArtifactRepositoryError::ReferenceSerialization)?;
    String::from_utf8(bytes).map_err(|_| ArtifactRepositoryError::ReferenceSerialization)
}

fn decode_reference(encoded: &str) -> Result<ArtifactRef, RecoveryArtifactFailure> {
    if encoded.is_empty() || encoded.len() > MAX_REFERENCE_BYTES {
        return Err(RecoveryArtifactFailure::redacted(
            "artifact_reference_size_invalid",
        ));
    }
    let reference: ArtifactRef = serde_json::from_str(encoded)
        .map_err(|_| RecoveryArtifactFailure::redacted("artifact_reference_invalid"))?;
    let canonical = serde_jcs::to_vec(&reference)
        .map_err(|_| RecoveryArtifactFailure::redacted("artifact_reference_invalid"))?;
    if canonical != encoded.as_bytes() {
        return Err(RecoveryArtifactFailure::redacted(
            "artifact_reference_not_canonical",
        ));
    }
    Ok(reference)
}

const fn artifact_kind(kind: RecoveryArtifactKind) -> ArtifactKind {
    match kind {
        RecoveryArtifactKind::RuntimeState => ArtifactKind::RecoveryState,
        RecoveryArtifactKind::ProviderEpisode => ArtifactKind::ProviderEpisode,
        RecoveryArtifactKind::ActionArguments => ArtifactKind::ActionArguments,
        RecoveryArtifactKind::ActionResult => ArtifactKind::ActionResult,
    }
}

#[async_trait]
impl RecoveryArtifactStore for ArtifactRepository {
    async fn load_verified(
        &self,
        request: &RecoveryArtifactRequest,
        max_bytes: usize,
    ) -> Result<Zeroizing<Vec<u8>>, RecoveryArtifactFailure> {
        if max_bytes == 0 || max_bytes > MAX_REPOSITORY_ARTIFACT_BYTES {
            return Err(RecoveryArtifactFailure::redacted(
                "artifact_read_bound_invalid",
            ));
        }
        let reference = decode_reference(&request.artifact_ref)?;
        let expected_kind = artifact_kind(request.kind);
        let expected_size = usize::try_from(reference.plaintext_len())
            .map_err(|_| RecoveryArtifactFailure::redacted("artifact_size_invalid"))?;
        if reference.kind() != expected_kind
            || reference.plaintext_sha256() != request.expected_hash.as_str()
            || expected_size == 0
            || expected_size > max_bytes
            || request
                .expected_size_bytes
                .is_some_and(|size| size != expected_size)
        {
            return Err(RecoveryArtifactFailure::redacted(
                "artifact_reference_receipt_mismatch",
            ));
        }
        let scope = ArtifactScope::new(
            request.tenant_id.clone(),
            request.principal_id.clone(),
            request.run_id.clone(),
        )
        .map_err(|_| RecoveryArtifactFailure::redacted("artifact_scope_invalid"))?;
        let secret = self.store.get(&scope, &reference).await.map_err(|error| {
            // `NotFoundOrUnauthorized` and `Expired` are permanent: the
            // referenced artifact bytes are not in this worker's local
            // store and never will be (stale checkpoint from a prior
            // daemon incarnation, or expired TTL). Retrying only burns
            // claim cycles in an infinite defer loop, so mark the
            // failure as non-retryable. Transient I/O errors remain
            // retryable.
            if matches!(
                error,
                ArtifactStoreError::NotFoundOrUnauthorized | ArtifactStoreError::Expired
            ) {
                RecoveryArtifactFailure::permanent("artifact_read_failed")
            } else {
                RecoveryArtifactFailure::redacted("artifact_read_failed")
            }
        })?;
        let bytes = secret.into_zeroizing();
        if bytes.len() != expected_size
            || ContentHash::sha256(bytes.as_slice()) != request.expected_hash
        {
            return Err(RecoveryArtifactFailure::redacted(
                "artifact_plaintext_receipt_mismatch",
            ));
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use krw_agent_artifact_store::{
        ArtifactStoreConfig, LocalArtifactStore, MasterKeyring, VersionedMasterKey,
    };
    use tempfile::TempDir;

    use super::*;

    fn repository(temp: &TempDir) -> ArtifactRepository {
        let key = VersionedMasterKey::new(1, [0x42; 32]).unwrap();
        let keyring = MasterKeyring::new(1, [key]).unwrap();
        let root = Path::new(temp.path()).join("artifacts");
        let store = LocalArtifactStore::open(
            root,
            keyring,
            ArtifactStoreConfig {
                max_plaintext_bytes: 8 * 1024 * 1024,
                ..ArtifactStoreConfig::default()
            },
        )
        .unwrap();
        ArtifactRepository::new(Arc::new(store), ArtifactTtlPolicy::default()).unwrap()
    }

    fn recovery_request(receipt: &ArtifactReceipt) -> RecoveryArtifactRequest {
        RecoveryArtifactRequest {
            kind: RecoveryArtifactKind::ProviderEpisode,
            run_id: "run-1".into(),
            tenant_id: "tenant-1".into(),
            principal_id: "principal-1".into(),
            action_key: None,
            artifact_ref: receipt.artifact_ref.clone(),
            expected_hash: receipt.plaintext_hash.clone(),
            expected_size_bytes: Some(usize::try_from(receipt.plaintext_size_bytes).unwrap()),
            schema_hash: None,
        }
    }

    #[tokio::test]
    async fn canonical_reference_round_trips_only_in_the_exact_scope() {
        let temp = TempDir::new().unwrap();
        let repository = repository(&temp);
        let scope = ArtifactScope::new("tenant-1", "principal-1", "run-1").unwrap();
        let receipt = repository
            .put(&scope, ArtifactKind::ProviderEpisode, br#"{"episode":1}"#)
            .await
            .unwrap();
        let request = recovery_request(&receipt);
        let loaded = repository.load_verified(&request, 1024).await.unwrap();
        assert_eq!(loaded.as_slice(), br#"{"episode":1}"#);

        let mut wrong_scope = request;
        wrong_scope.principal_id = "principal-2".into();
        assert!(repository.load_verified(&wrong_scope, 1024).await.is_err());
    }

    #[tokio::test]
    async fn kind_size_hash_and_canonical_encoding_are_bound() {
        let temp = TempDir::new().unwrap();
        let repository = repository(&temp);
        let scope = ArtifactScope::new("tenant-1", "principal-1", "run-1").unwrap();
        let receipt = repository
            .put(&scope, ArtifactKind::ProviderEpisode, b"episode")
            .await
            .unwrap();

        let mut wrong_kind = recovery_request(&receipt);
        wrong_kind.kind = RecoveryArtifactKind::ActionResult;
        assert!(repository.load_verified(&wrong_kind, 1024).await.is_err());

        let mut wrong_size = recovery_request(&receipt);
        wrong_size.expected_size_bytes = Some(999);
        assert!(repository.load_verified(&wrong_size, 1024).await.is_err());

        let mut noncanonical = recovery_request(&receipt);
        noncanonical.artifact_ref = format!(" {}", noncanonical.artifact_ref);
        assert!(repository.load_verified(&noncanonical, 1024).await.is_err());
    }

    #[tokio::test]
    async fn empty_and_oversize_payloads_fail_before_storage() {
        let temp = TempDir::new().unwrap();
        let repository = repository(&temp);
        let scope = ArtifactScope::new("tenant-1", "principal-1", "run-1").unwrap();
        assert!(
            repository
                .put(&scope, ArtifactKind::ActionArguments, b"")
                .await
                .is_err()
        );
        let payload = vec![0_u8; MAX_REPOSITORY_ARTIFACT_BYTES + 1];
        assert!(
            repository
                .put(&scope, ArtifactKind::ActionResult, &payload)
                .await
                .is_err()
        );
    }
}
