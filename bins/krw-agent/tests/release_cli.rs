//! Process-level coverage for the offline release authorization operator path.
//!
//! The library tests cover Ed25519 and canonical parsing independently. This
//! test makes sure the shipped CLI preserves the important file-boundary
//! properties: it creates a new private key, signs a canonical descriptor,
//! verifies it against canonical public trust, and refuses a version mismatch.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use krw_agent_protocol::{
    BudgetLimits, ContentHash, EntrypointScope, GLM_MODEL_ID, PROTOCOL_VERSION,
    PUBLIC_RELEASE_DESCRIPTOR_SCHEMA_VERSION, PinnedExecutionContract, ProviderWireCapabilities,
    PublicReleaseDescriptor, PublicReleaseEntrypoint, RunContextKind, ScopeCardinalityKind,
    ThinkingMode,
};
use krw_agent_release_authorization::{
    RELEASE_TRUST_REGISTRY_SCHEMA_VERSION, ReleaseTrustKeyV1, ReleaseTrustRegistryV1,
    canonical_trust_registry_bytes,
};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock after Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "krw-agent-release-cli-{}-{nanos}-{}",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("new isolated release CLI test directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        // The path was created by this process and is checked again before
        // deletion. This keeps test artifacts out of the operator's temp dir
        // without accepting a symlink replacement.
        if let Ok(metadata) = fs::symlink_metadata(&self.0)
            && metadata.is_dir()
            && !metadata.file_type().is_symlink()
        {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_krw-agent"))
}

fn assert_success(output: Output) -> String {
    assert!(
        output.status.success(),
        "CLI failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("CLI stdout is UTF-8")
}

fn hash(label: &str) -> ContentHash {
    ContentHash::sha256(label)
}

fn descriptor() -> PublicReleaseDescriptor {
    PublicReleaseDescriptor {
        schema_version: PUBLIC_RELEASE_DESCRIPTOR_SCHEMA_VERSION,
        release_set_hash: hash("release-set"),
        runtime_version: "0.1.0".into(),
        entries: vec![PublicReleaseEntrypoint {
            run_kind: "route".into(),
            locale: "ko-KR".into(),
            agent_image_hash: hash("image"),
            model_profile: "glm_direct".into(),
            scope: EntrypointScope {
                context_kind: RunContextKind::RoutingRequest,
                cardinality: ScopeCardinalityKind::Exact,
                value: 0,
            },
            execution: PinnedExecutionContract {
                protocol_version: PROTOCOL_VERSION,
                agent_image_hash: hash("image"),
                deployment_binding_hash: hash("binding"),
                model_registry_hash: hash("models"),
                budget_registry_hash: hash("budget"),
                model_profile: "glm_direct".into(),
                requested_model: GLM_MODEL_ID.into(),
                resolved_model: GLM_MODEL_ID.into(),
                provider_api_version: "anthropic-messages-v1".into(),
                provider_max_context_tokens: 204_800,
                provider_wire_capabilities: ProviderWireCapabilities::glm_5_2(),
                thinking: ThinkingMode::Disabled,
                reasoning_effort: None,
                capability_release_hashes: BTreeMap::new(),
                budget: BudgetLimits {
                    max_provider_turns: 4,
                    max_capability_calls: 1,
                    max_replans: 1,
                    max_repairs: 1,
                    max_input_tokens: 16_000,
                    max_output_tokens: 2_000,
                    max_evidence_bytes: 1_048_576,
                    deadline_ms: 30_000,
                    capability_call_limits: BTreeMap::new(),
                },
            },
        }],
    }
}

#[test]
fn release_cli_keys_signs_and_verifies_an_exact_canonical_descriptor() {
    let directory = TestDirectory::new();
    let descriptor_path = directory.path().join("descriptor.json");
    fs::write(
        &descriptor_path,
        serde_jcs::to_vec(&descriptor()).expect("canonical descriptor"),
    )
    .expect("write descriptor");

    let private_key_path = directory.path().join("release.pk8");
    let keygen = assert_success(
        command()
            .args([
                "release",
                "keygen",
                "--private-key-out",
                private_key_path.to_str().expect("UTF-8 test path"),
            ])
            .output()
            .expect("run keygen"),
    );
    let public_key = keygen
        .trim()
        .strip_prefix("public_key_hex=")
        .expect("keygen public key output")
        .to_owned();
    assert_eq!(public_key.len(), 64);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            fs::metadata(&private_key_path)
                .expect("private key metadata")
                .mode()
                & 0o777,
            0o600
        );
    }

    let authorization_path = directory.path().join("authorization.json");
    assert_success(
        command()
            .args([
                "release",
                "sign",
                "--descriptor",
                descriptor_path.to_str().expect("UTF-8 test path"),
                "--private-key",
                private_key_path.to_str().expect("UTF-8 test path"),
                "--key-id",
                "release-2026-test",
                "--sequence",
                "7",
                "--issued-at-unix-seconds",
                "1700000000",
                "--expires-at-unix-seconds",
                "1700086400",
                "--runtime-version",
                "0.1.0",
                "--kernel-version",
                "0.1.0",
                "--out",
                authorization_path.to_str().expect("UTF-8 test path"),
            ])
            .output()
            .expect("run signing"),
    );

    let trust_path = directory.path().join("trust.json");
    let trust = ReleaseTrustRegistryV1 {
        schema_version: RELEASE_TRUST_REGISTRY_SCHEMA_VERSION,
        registry_id: "test-release-trust".into(),
        minimum_sequence: 7,
        keys: vec![ReleaseTrustKeyV1 {
            key_id: "release-2026-test".into(),
            ed25519_public_key_hex: public_key,
            not_before_unix_seconds: 1_699_000_000,
            not_after_unix_seconds: 1_701_000_000,
            revoked: false,
        }],
    };
    fs::write(
        &trust_path,
        canonical_trust_registry_bytes(&trust).expect("canonical trust registry"),
    )
    .expect("write trust registry");

    assert_success(
        command()
            .args([
                "release",
                "verify",
                "--descriptor",
                descriptor_path.to_str().expect("UTF-8 test path"),
                "--authorization",
                authorization_path.to_str().expect("UTF-8 test path"),
                "--trust-registry",
                trust_path.to_str().expect("UTF-8 test path"),
                "--runtime-version",
                "0.1.0",
                "--kernel-version",
                "0.1.0",
                "--now-unix-seconds",
                "1700000100",
            ])
            .output()
            .expect("run verification"),
    );

    let mismatch = command()
        .args([
            "release",
            "verify",
            "--descriptor",
            descriptor_path.to_str().expect("UTF-8 test path"),
            "--authorization",
            authorization_path.to_str().expect("UTF-8 test path"),
            "--trust-registry",
            trust_path.to_str().expect("UTF-8 test path"),
            "--runtime-version",
            "0.1.1",
            "--kernel-version",
            "0.1.0",
            "--now-unix-seconds",
            "1700000100",
        ])
        .output()
        .expect("run mismatched verification");
    assert!(
        !mismatch.status.success(),
        "runtime version drift was accepted"
    );
}
