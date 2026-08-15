//! Immutable deployment receipts (doc/refetoring/05 §controller 단계 2, 12).
//!
//! Two receipt kinds:
//!
//! * [`PreflightReceipt`] — every input hash, the target identity, all check
//!   results, and the overall verdict, written after the read-only preflight.
//! * [`TerminalReceipt`] — the final controller outcome for the run
//!   (`dry-run-ok` or `failure`), including the admission state and the
//!   stage that stopped the run.
//!
//! Receipts are deterministic JSON (`to_string_pretty`, `BTreeMap`-ordered
//! maps) and write-once: overwriting an existing receipt file is an error,
//! never a silent update.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::checks::PreflightOutcome;
use crate::hashing::sha256_bytes;

pub const PREFLIGHT_RECEIPT_FILE: &str = "preflight.json";
pub const TERMINAL_RECEIPT_FILE: &str = "terminal.json";
pub const RECEIPT_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReceiptError {
    #[error("receipt write failed: {0}")]
    Write(String),
    #[error("receipt already exists (receipts are immutable): {0}")]
    AlreadyExists(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreflightReceipt {
    pub schema_version: u16,
    pub receipt_kind: &'static str,
    pub run_id: String,
    pub created_at_utc: String,
    pub command: String,
    pub executor_kind: String,
    pub input_hashes: BTreeMap<String, String>,
    pub target_identity: BTreeMap<String, String>,
    pub checks: Vec<CheckRecord>,
    pub verdict: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckRecord {
    pub id: String,
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TerminalReceipt {
    pub schema_version: u16,
    pub receipt_kind: &'static str,
    pub run_id: String,
    pub created_at_utc: String,
    pub command: String,
    /// `dry-run-ok` or `failure`.
    pub outcome: String,
    /// `closed` (deploy failures; admission must stay closed) or
    /// `not-touched` (dry-run never mutates admission).
    pub admission: String,
    /// Last stage reached: `dry_run`, `preflight`, or a stage-table id.
    pub reached_stage: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_stage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Path of the run's preflight receipt (input evidence chain).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt_path: Option<String>,
    /// Dry-run-only validation records (build/seal input resolution).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub validations: Vec<DryRunValidation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DryRunValidation {
    pub id: String,
    pub status: String,
    pub detail: String,
}

impl PreflightReceipt {
    pub fn from_outcome(
        outcome: &PreflightOutcome,
        run_id: &str,
        created_at_utc: &str,
        command: &str,
        executor_kind: &str,
    ) -> Self {
        Self {
            schema_version: RECEIPT_SCHEMA_VERSION,
            receipt_kind: "preflight",
            run_id: run_id.to_owned(),
            created_at_utc: created_at_utc.to_owned(),
            command: command.to_owned(),
            executor_kind: executor_kind.to_owned(),
            input_hashes: outcome.input_hashes.clone(),
            target_identity: outcome.target_identity.clone(),
            checks: outcome
                .checks
                .iter()
                .map(|check| CheckRecord {
                    id: check.id.to_owned(),
                    status: check.status.as_str().to_owned(),
                    detail: check.detail.clone(),
                })
                .collect(),
            verdict: outcome.verdict.as_str().to_owned(),
        }
    }
}

impl TerminalReceipt {
    pub fn dry_run_ok(
        run_id: &str,
        created_at_utc: &str,
        command: &str,
        validations: Vec<DryRunValidation>,
        receipt_path: Option<String>,
    ) -> Self {
        Self {
            schema_version: RECEIPT_SCHEMA_VERSION,
            receipt_kind: "terminal",
            run_id: run_id.to_owned(),
            created_at_utc: created_at_utc.to_owned(),
            command: command.to_owned(),
            outcome: "dry-run-ok".to_owned(),
            admission: "not-touched".to_owned(),
            reached_stage: "dry_run".to_owned(),
            failed_stage: None,
            reason: Some("read-only dry-run reached; no build, migration, or activation executed".to_owned()),
            receipt_path,
            validations,
        }
    }

    #[allow(clippy::too_many_arguments)] // flat receipt schema mirrors the 05 terminal failure fields
    pub fn failure(
        run_id: &str,
        created_at_utc: &str,
        command: &str,
        failed_stage: &str,
        reason: String,
        admission: TerminalAdmission,
        receipt_path: Option<String>,
        validations: Vec<DryRunValidation>,
    ) -> Self {
        Self {
            schema_version: RECEIPT_SCHEMA_VERSION,
            receipt_kind: "terminal",
            run_id: run_id.to_owned(),
            created_at_utc: created_at_utc.to_owned(),
            command: command.to_owned(),
            outcome: "failure".to_owned(),
            admission: admission.as_str().to_owned(),
            reached_stage: failed_stage.to_owned(),
            failed_stage: Some(failed_stage.to_owned()),
            reason: Some(reason),
            receipt_path,
            validations,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalAdmission {
    Closed,
    NotTouched,
}

impl TerminalAdmission {
    fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::NotTouched => "not-touched",
        }
    }
}

/// Deterministic run directory name: `<utc-timestamp>-<run-id>` under
/// `<operator_root>/deploy-receipts/`.
pub fn receipt_run_dir(operator_root: &Path, run_name: &str) -> PathBuf {
    operator_root.join("deploy-receipts").join(run_name)
}

/// Deterministic run output directory: `<operator_root>/releases/<run-name>`.
pub fn release_output_dir(operator_root: &Path, run_name: &str) -> PathBuf {
    operator_root.join("releases").join(run_name)
}

/// Stable run id: 8-hex prefix of the config file sha256.
pub fn run_id_from_config_sha256(config_sha256: &str) -> String {
    let hex = config_sha256.strip_prefix("sha256:").unwrap_or(config_sha256);
    hex.chars().take(8).collect()
}

/// Write a preflight receipt under `dir` (creates `dir` on first write).
pub fn write_preflight_receipt(dir: &Path, receipt: &PreflightReceipt) -> Result<PathBuf, ReceiptError> {
    write_receipt(dir, PREFLIGHT_RECEIPT_FILE, receipt)
}

/// Write a terminal receipt under `dir` (creates `dir` on first write).
pub fn write_terminal_receipt(dir: &Path, receipt: &TerminalReceipt) -> Result<PathBuf, ReceiptError> {
    write_receipt(dir, TERMINAL_RECEIPT_FILE, receipt)
}

fn write_receipt<T: Serialize>(dir: &Path, file_name: &str, receipt: &T) -> Result<PathBuf, ReceiptError> {
    let path = dir.join(file_name);
    if path.exists() {
        return Err(ReceiptError::AlreadyExists(path));
    }
    std::fs::create_dir_all(dir).map_err(|error| ReceiptError::Write(format!("{}: {error}", dir.display())))?;
    let json = serde_json::to_string_pretty(receipt)
        .map_err(|error| ReceiptError::Write(format!("serialize {file_name}: {error}")))?;
    std::fs::write(&path, json + "\n").map_err(|error| ReceiptError::Write(format!("{}: {error}", path.display())))?;
    Ok(path)
}

/// Deterministic receipt identity helper for tests and callers: sha256 of the
/// serialized receipt bytes (drift evidence between stages).
pub fn receipt_digest<T: Serialize>(receipt: &T) -> Result<String, ReceiptError> {
    let bytes = serde_json::to_vec(receipt).map_err(|error| ReceiptError::Write(format!("serialize: {error}")))?;
    Ok(sha256_bytes(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checks::{self, PreflightPlan};
    use crate::config::{DeployConfig, DeployTimeouts};
    use crate::executor::FixtureExecutor;
    use std::sync::atomic::{AtomicU32, Ordering};

    static DIR_SEQUENCE: AtomicU32 = AtomicU32::new(0);

    fn outcome_fixture() -> PreflightOutcome {
        let root = std::env::temp_dir().join(format!(
            "krw-agent-deploy-receipts-{}-{}",
            std::process::id(),
            DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let agent_root = root.join("agent");
        let frontend_root = root.join("front");
        let operator_root = root.join("operator");
        std::fs::create_dir_all(&agent_root).unwrap();
        std::fs::create_dir_all(&frontend_root).unwrap();
        std::fs::create_dir_all(operator_root.join("signing")).unwrap();
        std::fs::write(operator_root.join("signing/release-private.pk8"), b"dummy").unwrap();
        let config = DeployConfig {
            schema_version: 1,
            provider: "deepseek".to_owned(),
            agent_source_root: agent_root,
            frontend_source_root: frontend_root,
            operator_root,
            runtime_env: root.join("runtime.env"),
            target_file: root.join("target.json"),
            frontend_contract: root.join("contract.json"),
            timeouts: DeployTimeouts {
                ssh_ms: 5_000,
                mcp_ms: 5_000,
                daemon_ready_ms: 90_000,
                public_ready_ms: 90_000,
            },
        };
        let config_path = root.join("deploy-config.json");
        std::fs::write(&config_path, "{\"schema_version\":1}").unwrap();
        let output_dir = root.join("out");
        let plan = PreflightPlan {
            config: &config,
            config_path: &config_path,
            config_sha256: sha256_bytes(b"receipt-test-config"),
            output_dir: &output_dir,
        };
        checks::run_preflight(&plan, &FixtureExecutor::passing())
    }

    #[test]
    fn preflight_receipt_serializes_deterministically() {
        let outcome = outcome_fixture();
        let receipt = PreflightReceipt::from_outcome(&outcome, "abc12345", "2026-08-16T00:00:00Z", "preflight", "fixture");
        let first = serde_json::to_string_pretty(&receipt).unwrap();
        let second = serde_json::to_string_pretty(&receipt).unwrap();
        assert_eq!(first, second);
        let value: serde_json::Value = serde_json::from_str(&first).unwrap();
        assert_eq!(value["receipt_kind"], "preflight");
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["verdict"], outcome.verdict.as_str());
        assert_eq!(value["run_id"], "abc12345");
        // Maps serialize in sorted key order (BTreeMap).
        let hash_keys = value["input_hashes"].as_object().unwrap().keys().cloned().collect::<Vec<_>>();
        let mut sorted = hash_keys.clone();
        sorted.sort();
        assert_eq!(hash_keys, sorted);
        // Check order is preserved exactly as executed.
        let check_ids = value["checks"].as_array().unwrap().iter().map(|check| check["id"].clone()).collect::<Vec<_>>();
        let expected_ids = outcome.checks.iter().map(|check| serde_json::Value::from(check.id)).collect::<Vec<_>>();
        assert_eq!(check_ids, expected_ids);
    }

    #[test]
    fn terminal_failure_receipt_matches_the_05_schema() {
        let receipt = TerminalReceipt::failure(
            "abc12345",
            "2026-08-16T00:00:00Z",
            "deploy",
            "build",
            "controller build/seal/activation stages not yet enabled in this revision; admission left closed; fix-forward".to_owned(),
            TerminalAdmission::Closed,
            Some("/operator/deploy-receipts/20260816T000000Z-abc12345/preflight.json".to_owned()),
            Vec::new(),
        );
        let value: serde_json::Value = serde_json::from_str(&serde_json::to_string_pretty(&receipt).unwrap()).unwrap();
        assert_eq!(value["outcome"], "failure");
        assert_eq!(value["admission"], "closed");
        assert_eq!(value["failed_stage"], "build");
        assert_eq!(value["reached_stage"], "build");
        assert!(value["reason"].as_str().unwrap().contains("fix-forward"));
        assert!(value.get("validations").is_none(), "empty validations are omitted");
    }

    #[test]
    fn dry_run_receipt_records_validations_and_untouched_admission() {
        let receipt = TerminalReceipt::dry_run_ok(
            "abc12345",
            "2026-08-16T00:00:00Z",
            "dry-run",
            vec![DryRunValidation {
                id: "agent-workspace-resolvable".to_owned(),
                status: "pass".to_owned(),
                detail: "Cargo.toml present".to_owned(),
            }],
            None,
        );
        let value: serde_json::Value = serde_json::from_str(&serde_json::to_string_pretty(&receipt).unwrap()).unwrap();
        assert_eq!(value["outcome"], "dry-run-ok");
        assert_eq!(value["admission"], "not-touched");
        assert_eq!(value["reached_stage"], "dry_run");
        assert_eq!(value["validations"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn write_creates_directory_and_refuses_overwrite() {
        let dir = std::env::temp_dir().join(format!(
            "krw-agent-deploy-receipt-write-{}-{}",
            std::process::id(),
            DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let outcome = outcome_fixture();
        let receipt = PreflightReceipt::from_outcome(&outcome, "abc12345", "2026-08-16T00:00:00Z", "preflight", "fixture");
        let path = write_preflight_receipt(&dir, &receipt).unwrap();
        assert!(path.ends_with(PREFLIGHT_RECEIPT_FILE));
        assert!(dir.join(PREFLIGHT_RECEIPT_FILE).is_file());
        let receipt = PreflightReceipt::from_outcome(&outcome, "abc12345", "2026-08-16T00:00:00Z", "preflight", "fixture");
        let error = write_preflight_receipt(&dir, &receipt).unwrap_err();
        assert!(matches!(error, ReceiptError::AlreadyExists(_)), "unexpected error: {error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_id_is_config_hash_prefix() {
        assert_eq!(
            run_id_from_config_sha256("sha256:164482f3fe99eb82977bcd77c07792d0472cae014f8218a0c492f42c1a29055e"),
            "164482f3"
        );
    }
}
