//! Offline, signed authorization for one immutable runtime release set.
//!
//! A content hash proves integrity only after a caller already knows which
//! bytes to trust. This crate adds a small, canonical Ed25519 authorization
//! envelope that binds the daemon's resolved public descriptor, release set,
//! exact runtime/kernel version, and the only permitted physical model.
//! Private key material is deliberately accepted only as caller-owned bytes;
//! it is never stored in an authorization, descriptor, or trust registry.

use std::collections::BTreeSet;

use krw_agent_protocol::{ContentHash, DEEPSEEK_MODEL_ID, PublicReleaseDescriptor};
use ring::rand::SystemRandom;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const RELEASE_AUTHORIZATION_SCHEMA_VERSION: u16 = 1;
pub const RELEASE_TRUST_REGISTRY_SCHEMA_VERSION: u16 = 1;
pub const SIGNATURE_ALGORITHM: &str = "ed25519";
pub const MAX_AUTHORIZATION_BYTES: usize = 64 * 1024;
pub const MAX_TRUST_REGISTRY_BYTES: usize = 64 * 1024;
pub const MAX_AUTHORIZATION_LIFETIME_SECONDS: u64 = 366 * 24 * 60 * 60;
pub const MAX_TRUST_KEYS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseAuthorizationPayloadV1 {
    pub schema_version: u16,
    pub key_id: String,
    /// Strictly positive deployment sequence. The trust registry carries the
    /// lowest sequence this daemon is allowed to admit.
    pub sequence: u64,
    pub issued_at_unix_seconds: u64,
    pub expires_at_unix_seconds: u64,
    pub release_descriptor_hash: ContentHash,
    pub release_set_hash: ContentHash,
    pub runtime_version: String,
    pub kernel_version: String,
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedReleaseAuthorizationV1 {
    pub payload: ReleaseAuthorizationPayloadV1,
    pub signature_algorithm: String,
    /// Lowercase hex of the raw 64-byte Ed25519 signature.
    pub signature_hex: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseTrustKeyV1 {
    pub key_id: String,
    /// Lowercase hex of the raw 32-byte Ed25519 public key.
    pub ed25519_public_key_hex: String,
    pub not_before_unix_seconds: u64,
    pub not_after_unix_seconds: u64,
    pub revoked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseTrustRegistryV1 {
    pub schema_version: u16,
    pub registry_id: String,
    /// Any signed release lower than this number is a rollback attempt.
    pub minimum_sequence: u64,
    pub keys: Vec<ReleaseTrustKeyV1>,
}

#[derive(Debug, Clone, Copy)]
pub struct VerificationContext<'a> {
    pub runtime_version: &'a str,
    pub kernel_version: &'a str,
    pub now_unix_seconds: u64,
}

pub fn sign(
    payload: ReleaseAuthorizationPayloadV1,
    private_key_pkcs8: &[u8],
) -> Result<SignedReleaseAuthorizationV1, ReleaseAuthorizationError> {
    validate_payload(&payload)?;
    let key_pair = Ed25519KeyPair::from_pkcs8(private_key_pkcs8)
        .map_err(|_| ReleaseAuthorizationError::InvalidPrivateKey)?;
    let signature = key_pair.sign(&canonical_payload_bytes(&payload)?);
    Ok(SignedReleaseAuthorizationV1 {
        payload,
        signature_algorithm: SIGNATURE_ALGORITHM.to_owned(),
        signature_hex: hex::encode(signature.as_ref()),
    })
}

pub fn public_key_hex_from_private_key(
    private_key_pkcs8: &[u8],
) -> Result<String, ReleaseAuthorizationError> {
    let key_pair = Ed25519KeyPair::from_pkcs8(private_key_pkcs8)
        .map_err(|_| ReleaseAuthorizationError::InvalidPrivateKey)?;
    Ok(hex::encode(key_pair.public_key().as_ref()))
}

pub fn generate_private_key_pkcs8() -> Result<Vec<u8>, ReleaseAuthorizationError> {
    let random = SystemRandom::new();
    let document = Ed25519KeyPair::generate_pkcs8(&random)
        .map_err(|_| ReleaseAuthorizationError::KeyGenerationFailed)?;
    Ok(document.as_ref().to_vec())
}

pub fn verify_for_descriptor(
    authorization: &SignedReleaseAuthorizationV1,
    trust: &ReleaseTrustRegistryV1,
    descriptor: &PublicReleaseDescriptor,
    context: VerificationContext<'_>,
) -> Result<(), ReleaseAuthorizationError> {
    validate_authorization(authorization)?;
    validate_trust_registry(trust)?;

    let payload = &authorization.payload;
    let descriptor_hash = public_descriptor_hash(descriptor)?;
    if payload.release_descriptor_hash != descriptor_hash {
        return Err(ReleaseAuthorizationError::DescriptorHashMismatch);
    }
    if payload.release_set_hash != descriptor.release_set_hash {
        return Err(ReleaseAuthorizationError::ReleaseSetHashMismatch);
    }
    if payload.runtime_version != context.runtime_version {
        return Err(ReleaseAuthorizationError::RuntimeVersionMismatch);
    }
    if payload.kernel_version != context.kernel_version {
        return Err(ReleaseAuthorizationError::KernelVersionMismatch);
    }
    if payload.sequence < trust.minimum_sequence {
        return Err(ReleaseAuthorizationError::SequenceDowngrade);
    }
    if context.now_unix_seconds < payload.issued_at_unix_seconds
        || context.now_unix_seconds >= payload.expires_at_unix_seconds
    {
        return Err(ReleaseAuthorizationError::AuthorizationExpiredOrNotYetValid);
    }
    let key = trust
        .keys
        .iter()
        .find(|key| key.key_id == payload.key_id)
        .ok_or(ReleaseAuthorizationError::UnknownKey)?;
    if key.revoked {
        return Err(ReleaseAuthorizationError::KeyRevoked);
    }
    if context.now_unix_seconds < key.not_before_unix_seconds
        || context.now_unix_seconds > key.not_after_unix_seconds
        || payload.issued_at_unix_seconds < key.not_before_unix_seconds
        || payload.issued_at_unix_seconds > key.not_after_unix_seconds
    {
        return Err(ReleaseAuthorizationError::KeyOutsideValidityWindow);
    }
    let public_key = decode_exact_hex(&key.ed25519_public_key_hex, 32, "public key")?;
    let signature = decode_exact_hex(&authorization.signature_hex, 64, "signature")?;
    UnparsedPublicKey::new(&ED25519, public_key)
        .verify(&canonical_payload_bytes(payload)?, &signature)
        .map_err(|_| ReleaseAuthorizationError::SignatureInvalid)
}

pub fn public_descriptor_hash(
    descriptor: &PublicReleaseDescriptor,
) -> Result<ContentHash, ReleaseAuthorizationError> {
    Ok(ContentHash::sha256(serde_jcs::to_vec(descriptor)?))
}

pub fn canonical_authorization_bytes(
    authorization: &SignedReleaseAuthorizationV1,
) -> Result<Vec<u8>, ReleaseAuthorizationError> {
    validate_authorization(authorization)?;
    Ok(serde_jcs::to_vec(authorization)?)
}

pub fn canonical_trust_registry_bytes(
    trust: &ReleaseTrustRegistryV1,
) -> Result<Vec<u8>, ReleaseAuthorizationError> {
    validate_trust_registry(trust)?;
    Ok(serde_jcs::to_vec(trust)?)
}

pub fn parse_canonical_authorization(
    bytes: &[u8],
) -> Result<SignedReleaseAuthorizationV1, ReleaseAuthorizationError> {
    parse_canonical(bytes, MAX_AUTHORIZATION_BYTES, |value| {
        validate_authorization(value)
    })
}

pub fn parse_canonical_trust_registry(
    bytes: &[u8],
) -> Result<ReleaseTrustRegistryV1, ReleaseAuthorizationError> {
    parse_canonical(bytes, MAX_TRUST_REGISTRY_BYTES, |value| {
        validate_trust_registry(value)
    })
}

pub fn validate_authorization(
    authorization: &SignedReleaseAuthorizationV1,
) -> Result<(), ReleaseAuthorizationError> {
    if authorization.signature_algorithm != SIGNATURE_ALGORITHM {
        return Err(ReleaseAuthorizationError::UnsupportedSignatureAlgorithm);
    }
    validate_payload(&authorization.payload)?;
    decode_exact_hex(&authorization.signature_hex, 64, "signature")?;
    Ok(())
}

pub fn validate_trust_registry(
    trust: &ReleaseTrustRegistryV1,
) -> Result<(), ReleaseAuthorizationError> {
    if trust.schema_version != RELEASE_TRUST_REGISTRY_SCHEMA_VERSION {
        return Err(ReleaseAuthorizationError::TrustRegistrySchemaVersion);
    }
    validate_identifier(&trust.registry_id, "registry id")?;
    if trust.keys.is_empty() || trust.keys.len() > MAX_TRUST_KEYS {
        return Err(ReleaseAuthorizationError::TrustKeyCount);
    }
    let mut seen = BTreeSet::new();
    for key in &trust.keys {
        validate_identifier(&key.key_id, "key id")?;
        if !seen.insert(&key.key_id) {
            return Err(ReleaseAuthorizationError::DuplicateKeyId);
        }
        if key.not_before_unix_seconds > key.not_after_unix_seconds {
            return Err(ReleaseAuthorizationError::InvalidKeyValidityWindow);
        }
        decode_exact_hex(&key.ed25519_public_key_hex, 32, "public key")?;
    }
    Ok(())
}

fn validate_payload(
    payload: &ReleaseAuthorizationPayloadV1,
) -> Result<(), ReleaseAuthorizationError> {
    if payload.schema_version != RELEASE_AUTHORIZATION_SCHEMA_VERSION {
        return Err(ReleaseAuthorizationError::AuthorizationSchemaVersion);
    }
    validate_identifier(&payload.key_id, "key id")?;
    if payload.sequence == 0 {
        return Err(ReleaseAuthorizationError::InvalidSequence);
    }
    let lifetime = payload
        .expires_at_unix_seconds
        .checked_sub(payload.issued_at_unix_seconds)
        .ok_or(ReleaseAuthorizationError::InvalidAuthorizationLifetime)?;
    if lifetime == 0 || lifetime > MAX_AUTHORIZATION_LIFETIME_SECONDS {
        return Err(ReleaseAuthorizationError::InvalidAuthorizationLifetime);
    }
    ContentHash::parse(payload.release_descriptor_hash.as_str())
        .map_err(|_| ReleaseAuthorizationError::InvalidContentHash)?;
    ContentHash::parse(payload.release_set_hash.as_str())
        .map_err(|_| ReleaseAuthorizationError::InvalidContentHash)?;
    validate_version(&payload.runtime_version, "runtime version")?;
    validate_version(&payload.kernel_version, "kernel version")?;
    if payload.model_id != DEEPSEEK_MODEL_ID {
        return Err(ReleaseAuthorizationError::ModelMismatch);
    }
    Ok(())
}

fn canonical_payload_bytes(
    payload: &ReleaseAuthorizationPayloadV1,
) -> Result<Vec<u8>, ReleaseAuthorizationError> {
    Ok(serde_jcs::to_vec(payload)?)
}

fn parse_canonical<T>(
    bytes: &[u8],
    max_bytes: usize,
    validate: impl FnOnce(&T) -> Result<(), ReleaseAuthorizationError>,
) -> Result<T, ReleaseAuthorizationError>
where
    T: for<'de> Deserialize<'de> + Serialize,
{
    if bytes.is_empty() || bytes.len() > max_bytes {
        return Err(ReleaseAuthorizationError::ArtifactSize);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| ReleaseAuthorizationError::ArtifactUtf8)?;
    let value: T = serde_json::from_str(text)?;
    validate(&value)?;
    if serde_jcs::to_vec(&value)? != bytes {
        return Err(ReleaseAuthorizationError::ArtifactNotCanonical);
    }
    Ok(value)
}

fn decode_exact_hex(
    value: &str,
    bytes: usize,
    kind: &'static str,
) -> Result<Vec<u8>, ReleaseAuthorizationError> {
    if value.len() != bytes.saturating_mul(2)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ReleaseAuthorizationError::InvalidHex(kind));
    }
    hex::decode(value).map_err(|_| ReleaseAuthorizationError::InvalidHex(kind))
}

fn validate_identifier(value: &str, kind: &'static str) -> Result<(), ReleaseAuthorizationError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err(ReleaseAuthorizationError::InvalidIdentifier(kind));
    }
    Ok(())
}

fn validate_version(value: &str, kind: &'static str) -> Result<(), ReleaseAuthorizationError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b'\\')
    {
        return Err(ReleaseAuthorizationError::InvalidVersion(kind));
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum ReleaseAuthorizationError {
    #[error("authorization schema version is unsupported")]
    AuthorizationSchemaVersion,
    #[error("trust registry schema version is unsupported")]
    TrustRegistrySchemaVersion,
    #[error("unsupported authorization signature algorithm")]
    UnsupportedSignatureAlgorithm,
    #[error("authorization artifact size is invalid")]
    ArtifactSize,
    #[error("authorization artifact is not UTF-8")]
    ArtifactUtf8,
    #[error("authorization artifact is not canonical JCS")]
    ArtifactNotCanonical,
    #[error("invalid identifier: {0}")]
    InvalidIdentifier(&'static str),
    #[error("invalid version: {0}")]
    InvalidVersion(&'static str),
    #[error("invalid content hash")]
    InvalidContentHash,
    #[error("authorization sequence must be positive")]
    InvalidSequence,
    #[error("authorization lifetime is invalid")]
    InvalidAuthorizationLifetime,
    #[error("authorization model is not exact DeepSeek Flash")]
    ModelMismatch,
    #[error("trust registry has an invalid key count")]
    TrustKeyCount,
    #[error("trust registry contains duplicate key ids")]
    DuplicateKeyId,
    #[error("trust key validity window is invalid")]
    InvalidKeyValidityWindow,
    #[error("invalid {0} hex")]
    InvalidHex(&'static str),
    #[error("private signing key is invalid")]
    InvalidPrivateKey,
    #[error("private signing key generation failed")]
    KeyGenerationFailed,
    #[error("authorization descriptor hash does not match the daemon descriptor")]
    DescriptorHashMismatch,
    #[error("authorization release-set hash does not match the daemon descriptor")]
    ReleaseSetHashMismatch,
    #[error("authorization runtime version does not match")]
    RuntimeVersionMismatch,
    #[error("authorization kernel version does not match")]
    KernelVersionMismatch,
    #[error("authorization sequence is below the trust-registry floor")]
    SequenceDowngrade,
    #[error("authorization is expired or not yet valid")]
    AuthorizationExpiredOrNotYetValid,
    #[error("authorization signing key is unknown")]
    UnknownKey,
    #[error("authorization signing key is revoked")]
    KeyRevoked,
    #[error("authorization signing key is outside its validity window")]
    KeyOutsideValidityWindow,
    #[error("authorization signature is invalid")]
    SignatureInvalid,
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use krw_agent_protocol::PUBLIC_RELEASE_DESCRIPTOR_SCHEMA_VERSION;
    use ring::rand::SystemRandom;
    use ring::signature::Ed25519KeyPair;

    use super::*;

    fn descriptor() -> PublicReleaseDescriptor {
        PublicReleaseDescriptor {
            schema_version: PUBLIC_RELEASE_DESCRIPTOR_SCHEMA_VERSION,
            release_set_hash: ContentHash::sha256("release-set"),
            runtime_version: "0.1.0".into(),
            entries: Vec::new(),
        }
    }

    fn fixture() -> (
        Vec<u8>,
        Vec<u8>,
        PublicReleaseDescriptor,
        ReleaseAuthorizationPayloadV1,
    ) {
        let rng = SystemRandom::new();
        let private = Ed25519KeyPair::generate_pkcs8(&rng)
            .unwrap()
            .as_ref()
            .to_vec();
        let public = public_key_hex_from_private_key(&private).unwrap();
        let descriptor = descriptor();
        let payload = ReleaseAuthorizationPayloadV1 {
            schema_version: RELEASE_AUTHORIZATION_SCHEMA_VERSION,
            key_id: "release-2026-a".into(),
            sequence: 7,
            issued_at_unix_seconds: 1_700_000_000,
            expires_at_unix_seconds: 1_700_086_400,
            release_descriptor_hash: public_descriptor_hash(&descriptor).unwrap(),
            release_set_hash: descriptor.release_set_hash.clone(),
            runtime_version: "0.1.0".into(),
            kernel_version: "0.1.0".into(),
            model_id: DEEPSEEK_MODEL_ID.into(),
        };
        (private, hex::decode(public).unwrap(), descriptor, payload)
    }

    fn trust(public: &[u8]) -> ReleaseTrustRegistryV1 {
        ReleaseTrustRegistryV1 {
            schema_version: RELEASE_TRUST_REGISTRY_SCHEMA_VERSION,
            registry_id: "production-a".into(),
            minimum_sequence: 7,
            keys: vec![ReleaseTrustKeyV1 {
                key_id: "release-2026-a".into(),
                ed25519_public_key_hex: hex::encode(public),
                not_before_unix_seconds: 1_699_000_000,
                not_after_unix_seconds: 1_701_000_000,
                revoked: false,
            }],
        }
    }

    fn context() -> VerificationContext<'static> {
        VerificationContext {
            runtime_version: "0.1.0",
            kernel_version: "0.1.0",
            now_unix_seconds: 1_700_000_100,
        }
    }

    #[test]
    fn signed_authorization_is_canonical_and_verifies() {
        let (private, public, descriptor, payload) = fixture();
        let signed = sign(payload, &private).unwrap();
        let bytes = canonical_authorization_bytes(&signed).unwrap();
        assert_eq!(parse_canonical_authorization(&bytes).unwrap(), signed);
        let trust = trust(&public);
        let trust_bytes = canonical_trust_registry_bytes(&trust).unwrap();
        assert_eq!(parse_canonical_trust_registry(&trust_bytes).unwrap(), trust);
        verify_for_descriptor(&signed, &trust, &descriptor, context()).unwrap();
    }

    #[test]
    fn descriptor_tamper_wrong_key_expiry_revoke_and_downgrade_fail_closed() {
        let (private, public, descriptor, payload) = fixture();
        let signed = sign(payload, &private).unwrap();
        let registry = trust(&public);

        let mut descriptor_tamper = descriptor.clone();
        descriptor_tamper.runtime_version = "0.1.1".into();
        assert!(matches!(
            verify_for_descriptor(&signed, &registry, &descriptor_tamper, context()),
            Err(ReleaseAuthorizationError::DescriptorHashMismatch)
        ));

        let (_, wrong_public, _, _) = fixture();
        assert!(matches!(
            verify_for_descriptor(&signed, &trust(&wrong_public), &descriptor, context()),
            Err(ReleaseAuthorizationError::SignatureInvalid)
        ));

        let expired = VerificationContext {
            now_unix_seconds: signed.payload.expires_at_unix_seconds,
            ..context()
        };
        assert!(matches!(
            verify_for_descriptor(&signed, &registry, &descriptor, expired),
            Err(ReleaseAuthorizationError::AuthorizationExpiredOrNotYetValid)
        ));

        let mut revoked = registry.clone();
        revoked.keys[0].revoked = true;
        assert!(matches!(
            verify_for_descriptor(&signed, &revoked, &descriptor, context()),
            Err(ReleaseAuthorizationError::KeyRevoked)
        ));

        let mut downgrade = registry;
        downgrade.minimum_sequence = signed.payload.sequence + 1;
        assert!(matches!(
            verify_for_descriptor(&signed, &downgrade, &descriptor, context()),
            Err(ReleaseAuthorizationError::SequenceDowngrade)
        ));
    }

    #[test]
    fn signature_and_model_tampering_fail_closed() {
        let (private, public, descriptor, mut payload) = fixture();
        payload.model_id = "not-deepseek-flash".into();
        assert!(matches!(
            sign(payload, &private),
            Err(ReleaseAuthorizationError::ModelMismatch)
        ));

        let (_, _, _, payload) = fixture();
        let mut signed = sign(payload, &private).unwrap();
        let replacement = if signed.signature_hex.starts_with('0') {
            "1"
        } else {
            "0"
        };
        signed.signature_hex.replace_range(0..1, replacement);
        let error =
            verify_for_descriptor(&signed, &trust(&public), &descriptor, context()).unwrap_err();
        assert!(matches!(
            error,
            ReleaseAuthorizationError::SignatureInvalid
                | ReleaseAuthorizationError::InvalidHex("signature")
        ));
    }
}
