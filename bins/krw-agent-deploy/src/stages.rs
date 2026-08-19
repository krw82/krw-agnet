//! Forward-only stage machine and controller run modes.
//!
//! The 05-doc stage list is declared as a fixed table. Stages 1-2
//! (`preflight`, `preflight_receipt`) run inline here; stages 3-12 (build,
//! seal, frontend image prepare, migrations, admission close/open, local and
//! remote activation, deep readiness, terminal success receipt) execute
//! through [`crate::pipeline::run_deploy_pipeline`] with every world command
//! recorded as a structured [`crate::pipeline::StageCommand`]. Any stage
//! failure writes a TERMINAL FAILURE RECEIPT (admission `closed` from
//! `admission_close` onward, `not-touched` before) and exits non-zero. The
//! controller never half-activates and never rolls back binaries or DB
//! schema; recovery is always fix-forward through a fresh run.

use std::path::{Path, PathBuf};

use crate::checks::{self, CheckStatus, PreflightOutcome, PreflightPlan};
use crate::config::DeployConfig;
use crate::executor::PreflightExecutor;
use crate::hashing::sha256_bytes;
use crate::pipeline::{PipelineDeps, PipelineOutcome, StageExecutor};
use crate::receipts::{
    PreflightReceipt, ReceiptError, TerminalAdmission, TerminalReceipt, write_preflight_receipt,
    write_terminal_receipt,
};
use crate::target::TargetFile;
use crate::timeutil;

pub const STAGE_PREFLIGHT: &str = "preflight";
pub const STAGE_PREFLIGHT_RECEIPT: &str = "preflight_receipt";
pub const STAGE_BUILD: &str = "build";
pub const STAGE_SEAL: &str = "seal";
pub const STAGE_FRONTEND_IMAGE_PREPARE: &str = "frontend_image_prepare";
pub const STAGE_MIGRATIONS: &str = "migrations";
pub const STAGE_ADMISSION_CLOSE: &str = "admission_close";
pub const STAGE_LOCAL_ACTIVATION: &str = "local_activation";
pub const STAGE_REMOTE_ACTIVATION: &str = "remote_activation";
pub const STAGE_DEEP_READINESS: &str = "deep_readiness";
pub const STAGE_ADMISSION_OPEN: &str = "admission_open";
pub const STAGE_TERMINAL_SUCCESS_RECEIPT: &str = "terminal_success_receipt";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageStatus {
    Implemented,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageEntry {
    pub id: &'static str,
    pub status: StageStatus,
}

/// The 05-doc stage list, in forward-only order. Every stage is implemented:
/// 3-12 run through the stage executor in `crate::pipeline`.
pub const STAGE_TABLE: [StageEntry; 12] = [
    StageEntry {
        id: STAGE_PREFLIGHT,
        status: StageStatus::Implemented,
    },
    StageEntry {
        id: STAGE_PREFLIGHT_RECEIPT,
        status: StageStatus::Implemented,
    },
    StageEntry {
        id: STAGE_BUILD,
        status: StageStatus::Implemented,
    },
    StageEntry {
        id: STAGE_SEAL,
        status: StageStatus::Implemented,
    },
    StageEntry {
        id: STAGE_FRONTEND_IMAGE_PREPARE,
        status: StageStatus::Implemented,
    },
    StageEntry {
        id: STAGE_MIGRATIONS,
        status: StageStatus::Implemented,
    },
    StageEntry {
        id: STAGE_ADMISSION_CLOSE,
        status: StageStatus::Implemented,
    },
    StageEntry {
        id: STAGE_LOCAL_ACTIVATION,
        status: StageStatus::Implemented,
    },
    StageEntry {
        id: STAGE_REMOTE_ACTIVATION,
        status: StageStatus::Implemented,
    },
    StageEntry {
        id: STAGE_DEEP_READINESS,
        status: StageStatus::Implemented,
    },
    StageEntry {
        id: STAGE_ADMISSION_OPEN,
        status: StageStatus::Implemented,
    },
    StageEntry {
        id: STAGE_TERMINAL_SUCCESS_RECEIPT,
        status: StageStatus::Implemented,
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandMode {
    Preflight,
    DryRun,
    Deploy,
}

impl CommandMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preflight => "preflight",
            Self::DryRun => "dry-run",
            Self::Deploy => "deploy",
        }
    }
}

/// Deterministic per-run context (ids, dirs) derived from config + clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunContext {
    pub mode: CommandMode,
    pub run_ts: String,
    pub run_id: String,
    pub created_at_utc: String,
    pub config_sha256: String,
    pub output_dir: PathBuf,
    pub receipt_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutcome {
    pub exit_code: i32,
    /// `pass`/`fail` for preflight runs, `dry-run-ok`/`failure` otherwise.
    pub outcome: String,
    pub receipt_dir: Option<PathBuf>,
    pub preflight_receipt_path: Option<PathBuf>,
    pub terminal_receipt_path: Option<PathBuf>,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ControllerError {
    #[error("config unusable: {0}")]
    Config(#[from] crate::config::ConfigError),
    #[error("config unreadable: {0}")]
    ConfigRead(String),
    #[error("{0}")]
    Receipt(#[from] ReceiptError),
}

/// Build the run context: `<run_ts>-<run_id>` names both the receipt dir and
/// the (future) release output dir, where `run_id` is the 8-hex prefix of the
/// config file sha256.
pub fn prepare_run(
    config: &DeployConfig,
    config_sha256: &str,
    now_unix_seconds: u64,
    mode: CommandMode,
) -> RunContext {
    let run_ts = timeutil::format_utc_compact(now_unix_seconds);
    let run_id = crate::receipts::run_id_from_config_sha256(config_sha256);
    let run_name = format!("{run_ts}-{run_id}");
    RunContext {
        mode,
        run_ts,
        run_id,
        created_at_utc: timeutil::format_utc_iso(now_unix_seconds),
        config_sha256: config_sha256.to_owned(),
        output_dir: crate::receipts::release_output_dir(&config.operator_root, &run_name),
        receipt_dir: crate::receipts::receipt_run_dir(&config.operator_root, &run_name),
    }
}

/// Execute one controller command end to end (read-only in preflight and
/// dry-run modes; deploy runs all twelve stages through the stage
/// executor). The only filesystem writes are receipts under the run's
/// receipt dir plus the release output and operator signing trees a real
/// deploy legitimately produces.
pub fn run_command(
    mode: CommandMode,
    config_path: &Path,
    now_unix_seconds: u64,
    executor: &dyn PreflightExecutor,
    stage_executor: &dyn StageExecutor,
) -> Result<RunOutcome, ControllerError> {
    let config_bytes = std::fs::read(config_path).map_err(|error| {
        ControllerError::ConfigRead(format!("{}: {error}", config_path.display()))
    })?;
    let config_sha256 = sha256_bytes(&config_bytes);
    let config = DeployConfig::from_json_bytes(&config_bytes)?;
    let context = prepare_run(&config, &config_sha256, now_unix_seconds, mode);

    // Stage 1: read-only preflight.
    let plan = PreflightPlan {
        config: &config,
        config_path,
        config_sha256: config_sha256.clone(),
        output_dir: &context.output_dir,
        now_unix_seconds,
    };
    let outcome = checks::run_preflight(&plan, executor);

    // Stage 2: immutable preflight receipt.
    let preflight_receipt = PreflightReceipt::from_outcome(
        &outcome,
        &context.run_id,
        &context.created_at_utc,
        mode.as_str(),
        executor.kind(),
    );
    let preflight_path = write_preflight_receipt(&context.receipt_dir, &preflight_receipt)?;
    let preflight_path_string = preflight_path.display().to_string();

    match mode {
        CommandMode::Preflight => Ok(RunOutcome {
            exit_code: exit_code_for_verdict(&outcome),
            outcome: outcome.verdict.as_str().to_owned(),
            receipt_dir: Some(context.receipt_dir.clone()),
            preflight_receipt_path: Some(preflight_path),
            terminal_receipt_path: None,
            message: verdict_message(&outcome),
        }),
        CommandMode::DryRun => Ok(run_dry_run(
            &config,
            &context,
            &outcome,
            &preflight_path_string,
            executor,
        )),
        CommandMode::Deploy => Ok(run_deploy(
            &config,
            config_path,
            &context,
            &outcome,
            &preflight_path_string,
            stage_executor,
        )),
    }
}

fn run_dry_run(
    config: &DeployConfig,
    context: &RunContext,
    outcome: &PreflightOutcome,
    preflight_path: &str,
    executor: &dyn PreflightExecutor,
) -> RunOutcome {
    if outcome.verdict == CheckStatus::Fail {
        let failed_ids = outcome
            .failed_checks()
            .iter()
            .map(|check| check.id)
            .collect::<Vec<_>>()
            .join(", ");
        let receipt = TerminalReceipt::failure(
            &context.run_id,
            &context.created_at_utc,
            context.mode.as_str(),
            STAGE_PREFLIGHT,
            format!("preflight failed; failing checks: {failed_ids}"),
            TerminalAdmission::NotTouched,
            Some(preflight_path.to_owned()),
            Vec::new(),
            None,
            Vec::new(),
        );
        return finish_with_terminal(
            context,
            &receipt,
            1,
            format!("preflight failed: {failed_ids}"),
        );
    }

    // Validate that build/seal inputs resolve. No build is executed.
    let validations = dry_run_validations(config, executor);
    let all_ok = validations
        .iter()
        .all(|validation| validation.status == "pass");

    if all_ok {
        let receipt = TerminalReceipt::dry_run_ok(
            &context.run_id,
            &context.created_at_utc,
            context.mode.as_str(),
            validations,
            Some(preflight_path.to_owned()),
        );
        return finish_with_terminal(
            context,
            &receipt,
            0,
            "dry-run ok; read-only receipt complete".to_owned(),
        );
    }

    let failed = validations
        .iter()
        .filter(|validation| validation.status != "pass")
        .map(|validation| format!("{}: {}", validation.id, validation.detail))
        .collect::<Vec<_>>()
        .join("; ");
    let receipt = TerminalReceipt::failure(
        &context.run_id,
        &context.created_at_utc,
        context.mode.as_str(),
        "dry_run",
        format!("dry-run input validation failed: {failed}"),
        TerminalAdmission::NotTouched,
        Some(preflight_path.to_owned()),
        validations,
        None,
        Vec::new(),
    );
    finish_with_terminal(
        context,
        &receipt,
        1,
        format!("dry-run validation failed: {failed}"),
    )
}

fn dry_run_validations(
    config: &DeployConfig,
    executor: &dyn PreflightExecutor,
) -> Vec<crate::receipts::DryRunValidation> {
    let mut validations = Vec::new();

    // agent workspace resolvable (source root + Cargo.toml manifest).
    let agent_ok =
        config.agent_source_root.is_dir() && config.agent_source_root.join("Cargo.toml").is_file();
    validations.push(crate::receipts::DryRunValidation {
        id: "agent-workspace-resolvable".to_owned(),
        status: if agent_ok { "pass" } else { "fail" }.to_owned(),
        detail: format!(
            "agent_source_root {}",
            if agent_ok {
                "resolves with Cargo.toml"
            } else {
                "missing or has no Cargo.toml"
            }
        ),
    });

    // frontend source resolvable.
    let frontend_ok = config.frontend_source_root.is_dir();
    validations.push(crate::receipts::DryRunValidation {
        id: "frontend-source-resolvable".to_owned(),
        status: if frontend_ok { "pass" } else { "fail" }.to_owned(),
        detail: format!(
            "frontend_source_root {}",
            if frontend_ok { "resolves" } else { "missing" }
        ),
    });

    // cargo workspace metadata readable (read-only `cargo metadata --no-deps`).
    let metadata_detail = match executor.cargo_metadata(&config.agent_source_root) {
        Ok(text) if serde_json::from_str::<serde_json::Value>(&text).is_ok() => (
            "pass".to_owned(),
            "cargo metadata --no-deps readable".to_owned(),
        ),
        Ok(_) => (
            "fail".to_owned(),
            "cargo metadata output is not valid JSON".to_owned(),
        ),
        Err(error) => ("fail".to_owned(), format!("cargo metadata failed: {error}")),
    };
    validations.push(crate::receipts::DryRunValidation {
        id: "cargo-metadata-readable".to_owned(),
        status: metadata_detail.0,
        detail: metadata_detail.1,
    });

    validations
}

fn run_deploy(
    config: &DeployConfig,
    config_path: &Path,
    context: &RunContext,
    outcome: &PreflightOutcome,
    preflight_path: &str,
    stage_executor: &dyn StageExecutor,
) -> RunOutcome {
    if outcome.verdict == CheckStatus::Fail {
        let failed_ids = outcome
            .failed_checks()
            .iter()
            .map(|check| check.id)
            .collect::<Vec<_>>()
            .join(", ");
        let receipt = TerminalReceipt::failure(
            &context.run_id,
            &context.created_at_utc,
            context.mode.as_str(),
            STAGE_PREFLIGHT,
            format!(
                "preflight failed; failing checks: {failed_ids}; admission left closed; fix-forward"
            ),
            TerminalAdmission::Closed,
            Some(preflight_path.to_owned()),
            Vec::new(),
            None,
            Vec::new(),
        );
        return finish_with_terminal(
            context,
            &receipt,
            1,
            format!("deploy aborted: preflight failed: {failed_ids}"),
        );
    }

    // Stages 1-2 are complete (preflight + immutable receipt). Re-load the
    // target for the stage walk; a target that preflight accepted always
    // re-parses.
    let target = match TargetFile::from_path(&config.target_file) {
        Ok(target) => target,
        Err(error) => {
            let receipt = TerminalReceipt::failure(
                &context.run_id,
                &context.created_at_utc,
                context.mode.as_str(),
                STAGE_BUILD,
                format!("target file became unreadable after preflight: {error}"),
                TerminalAdmission::NotTouched,
                Some(preflight_path.to_owned()),
                Vec::new(),
                None,
                Vec::new(),
            );
            return finish_with_terminal(
                context,
                &receipt,
                1,
                format!(
                    "deploy stopped before stage `{STAGE_BUILD}`: target file unreadable: {error}"
                ),
            );
        }
    };
    let agent_head = outcome
        .input_hashes
        .get("agent_head")
        .cloned()
        .unwrap_or_else(|| crate::hashing::UNAVAILABLE_HASH.to_owned());
    let frontend_head = outcome
        .input_hashes
        .get("frontend_head")
        .cloned()
        .unwrap_or_else(|| crate::hashing::UNAVAILABLE_HASH.to_owned());
    let deps = PipelineDeps {
        config,
        config_path,
        target: &target,
        context,
        preflight_agent_head: &agent_head,
        preflight_frontend_head: &frontend_head,
        now_unix_seconds: now_unix_seconds_for(context),
    };
    let pipeline_outcome = crate::pipeline::run_deploy_pipeline(&deps, stage_executor);
    finish_pipeline(context, preflight_path, pipeline_outcome)
}

/// Recover the wall-clock the run was stamped with (deterministic in tests).
fn now_unix_seconds_for(context: &RunContext) -> u64 {
    timeutil::unix_seconds_from_compact(&context.run_ts).unwrap_or_default()
}

fn finish_pipeline(
    context: &RunContext,
    preflight_path: &str,
    pipeline: PipelineOutcome,
) -> RunOutcome {
    if pipeline.success {
        let artifacts =
            pipeline
                .artifacts
                .clone()
                .unwrap_or_else(|| crate::receipts::DeployArtifacts {
                    release_dir: context.output_dir.display().to_string(),
                    release_id: format!("{}-{}", context.run_ts, context.run_id),
                    descriptor_sha256: crate::hashing::UNAVAILABLE_HASH.to_owned(),
                    frontend_image_digest: "<unavailable>".to_owned(),
                    applied_migrations: Vec::new(),
                    previous_admission: "unknown".to_owned(),
                });
        let release_id = artifacts.release_id.clone();
        let receipt = TerminalReceipt::success(
            &context.run_id,
            &context.created_at_utc,
            context.mode.as_str(),
            Some(preflight_path.to_owned()),
            artifacts,
            pipeline.skipped.clone(),
        );
        let message = format!(
            "deploy success: release {release_id} sealed, activated, and open ({} stage-level skip(s))",
            pipeline.skipped.len()
        );
        return finish_with_terminal(context, &receipt, 0, message);
    }
    let failed_stage = pipeline
        .failed_stage
        .unwrap_or(STAGE_TERMINAL_SUCCESS_RECEIPT);
    let reason = pipeline
        .reason
        .clone()
        .unwrap_or_else(|| "deploy failed; admission left closed; fix-forward".to_owned());
    let admission_label = match pipeline.admission {
        TerminalAdmission::Closed => "closed",
        TerminalAdmission::NotTouched => "not-touched",
        TerminalAdmission::Open => "open",
    };
    let receipt = TerminalReceipt::failure(
        &context.run_id,
        &context.created_at_utc,
        context.mode.as_str(),
        failed_stage,
        format!("{reason}; admission {admission_label}; fix-forward"),
        pipeline.admission,
        Some(preflight_path.to_owned()),
        Vec::new(),
        pipeline.artifacts.clone(),
        pipeline.skipped.clone(),
    );
    let message = format!(
        "deploy failed at stage `{failed_stage}`: {reason} (admission {admission_label}; fix-forward)"
    );
    finish_with_terminal(context, &receipt, 1, message)
}

fn finish_with_terminal(
    context: &RunContext,
    receipt: &TerminalReceipt,
    exit_code: i32,
    message: String,
) -> RunOutcome {
    let outcome_label = receipt.outcome.clone();
    match write_terminal_receipt(&context.receipt_dir, receipt) {
        Ok(path) => RunOutcome {
            exit_code,
            outcome: outcome_label,
            receipt_dir: Some(context.receipt_dir.clone()),
            preflight_receipt_path: None,
            terminal_receipt_path: Some(path),
            message,
        },
        Err(error) => RunOutcome {
            exit_code: 2,
            outcome: "failure".to_owned(),
            receipt_dir: Some(context.receipt_dir.clone()),
            preflight_receipt_path: None,
            terminal_receipt_path: None,
            message: format!("terminal receipt write failed: {error}"),
        },
    }
}

fn exit_code_for_verdict(outcome: &PreflightOutcome) -> i32 {
    i32::from(outcome.verdict == CheckStatus::Fail)
}

fn verdict_message(outcome: &PreflightOutcome) -> String {
    if outcome.verdict == CheckStatus::Fail {
        let failed = outcome
            .failed_checks()
            .iter()
            .map(|check| format!("{}: {}", check.id, check.detail))
            .collect::<Vec<_>>()
            .join("; ");
        format!("preflight FAIL: {failed}")
    } else {
        format!(
            "preflight PASS: {} checks passed or skipped",
            outcome.checks.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::FixtureExecutor;
    use crate::pipeline::FixtureStageExecutor;
    use crate::receipts::{PREFLIGHT_RECEIPT_FILE, TERMINAL_RECEIPT_FILE};
    use std::sync::atomic::{AtomicU32, Ordering};

    static DIR_SEQUENCE: AtomicU32 = AtomicU32::new(0);

    struct Fixture {
        root: PathBuf,
        config_path: PathBuf,
    }

    impl Fixture {
        fn operator_root(&self) -> PathBuf {
            self.root.join("operator")
        }

        fn receipt_root(&self) -> PathBuf {
            self.operator_root().join("deploy-receipts")
        }

        fn single_run_dir(&self) -> PathBuf {
            std::fs::read_dir(self.receipt_root())
                .unwrap()
                .next()
                .expect("one run receipt directory")
                .unwrap()
                .path()
        }
    }

    fn write_fixture(tag: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!(
            "krw-agent-deploy-stages-{}-{tag}-{}",
            std::process::id(),
            DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let agent_root = root.join("agent");
        let frontend_root = root.join("front");
        let operator_root = root.join("operator");
        std::fs::create_dir_all(agent_root.join("src")).unwrap();
        std::fs::write(
            agent_root.join("Cargo.toml"),
            "[package]\nname = \"dummy-agent\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(&frontend_root).unwrap();
        std::fs::create_dir_all(operator_root.join("signing")).unwrap();
        std::fs::create_dir_all(operator_root.join("ops")).unwrap();
        std::fs::create_dir_all(operator_root.join("runtime")).unwrap();
        std::fs::write(
            operator_root.join("signing/release-private.pk8"),
            b"dummy-pkcs8",
        )
        .unwrap();
        std::fs::write(
            operator_root.join("runtime/krw-agent-deploy.env"),
            "# fixture env: dummy values only\nKRW_AGENT_DB_URL=postgresql://fixture-dummy\nKRW_AGENT_DATABASE_URL=postgresql://fixture-dummy-agent\n",
        )
        .unwrap();
        let registry = krw_agent_release_authorization::ReleaseTrustRegistryV1 {
            schema_version: 1,
            registry_id: "krw.deploy-stages-test".to_owned(),
            minimum_sequence: 1,
            keys: vec![krw_agent_release_authorization::ReleaseTrustKeyV1 {
                key_id: "stages-test-key".to_owned(),
                ed25519_public_key_hex: "22".repeat(32),
                not_before_unix_seconds: 0,
                not_after_unix_seconds: 4_102_444_800,
                revoked: false,
            }],
        };
        // Operator provider configuration consumed by the seal stage (both
        // providers: the seal loop mirrors the legacy dual-provider flow).
        for seal_provider in ["deepseek", "glm"] {
            let provider_config = operator_root.join("config").join(seal_provider);
            std::fs::create_dir_all(&provider_config).unwrap();
            std::fs::write(
                provider_config.join("deployment-binding.yaml"),
                format!("provider: {seal_provider}\n"),
            )
            .unwrap();
            std::fs::write(
                provider_config.join("endpoint-registry.yaml"),
                "endpoints: []\n",
            )
            .unwrap();
            std::fs::write(
                provider_config.join("release-trust-registry.json"),
                serde_jcs::to_vec(&registry).unwrap(),
            )
            .unwrap();
        }
        std::fs::write(
            operator_root.join("ops/agent-v1-deployment-contract.json"),
            crate::contract::canonical_fixture_contract_json(),
        )
        .unwrap();
        std::fs::write(
            operator_root.join("ops/production-target.json"),
            r#"{"provider": "deepseek", "local_ports": [], "required_env_keys": [], "supabase_migration_plan": [], "mcp_endpoints": []}"#,
        )
        .unwrap();
        let config_path = operator_root.join("ops/deploy-config.json");
        std::fs::write(
            &config_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "schema_version": 1,
                "provider": "deepseek",
                "agent_source_root": agent_root,
                "frontend_source_root": frontend_root,
                "operator_root": operator_root,
                "runtime_env": operator_root.join("runtime/krw-agent-deploy.env"),
                "target_file": operator_root.join("ops/production-target.json"),
                "frontend_contract": operator_root.join("ops/agent-v1-deployment-contract.json"),
                "timeouts": {"ssh_ms": 5000, "mcp_ms": 5000, "daemon_ready_ms": 90000, "public_ready_ms": 90000}
            }))
            .unwrap(),
        )
        .unwrap();
        Fixture { root, config_path }
    }

    const FIXED_NOW: u64 = 1_786_843_200; // 2026-08-16T01:20:00Z

    #[test]
    fn stage_table_declares_the_05_doc_order() {
        let ids = STAGE_TABLE.iter().map(|entry| entry.id).collect::<Vec<_>>();
        assert_eq!(
            ids,
            [
                "preflight",
                "preflight_receipt",
                "build",
                "seal",
                "frontend_image_prepare",
                "migrations",
                "admission_close",
                "local_activation",
                "remote_activation",
                "deep_readiness",
                "admission_open",
                "terminal_success_receipt"
            ]
        );
        assert!(
            STAGE_TABLE
                .iter()
                .all(|entry| entry.status == StageStatus::Implemented)
        );
    }

    #[test]
    fn preflight_mode_passes_and_writes_only_the_preflight_receipt() {
        let fixture = write_fixture("preflight-mode");
        let outcome = run_command(
            CommandMode::Preflight,
            &fixture.config_path,
            FIXED_NOW,
            &FixtureExecutor::passing(),
            &FixtureStageExecutor::passing(),
        )
        .unwrap();
        assert_eq!(outcome.exit_code, 0);
        assert_eq!(outcome.outcome, "pass");
        let run_dir = fixture.single_run_dir();
        let run_dir_name = run_dir.file_name().unwrap().to_str().unwrap();
        assert!(
            run_dir_name.starts_with("20260816T012000Z-")
                && run_dir_name.len() == "20260816T012000Z-".len() + 8,
            "run dir: {}",
            run_dir.display()
        );
        assert!(run_dir.join(PREFLIGHT_RECEIPT_FILE).is_file());
        assert!(!run_dir.join(TERMINAL_RECEIPT_FILE).exists());
        // No release output directory was created by a read-only run.
        assert!(!fixture.operator_root().join("releases").exists());
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[test]
    fn preflight_mode_fails_and_exits_nonzero_when_a_check_fails() {
        let fixture = write_fixture("preflight-fail");
        std::fs::remove_file(fixture.operator_root().join("signing/release-private.pk8")).unwrap();
        let outcome = run_command(
            CommandMode::Preflight,
            &fixture.config_path,
            FIXED_NOW,
            &FixtureExecutor::passing(),
            &FixtureStageExecutor::passing(),
        )
        .unwrap();
        assert_eq!(outcome.exit_code, 1);
        assert_eq!(outcome.outcome, "fail");
        assert!(
            outcome.message.contains("signing-trust-validity"),
            "message: {}",
            outcome.message
        );
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[test]
    fn dry_run_ok_writes_dry_run_terminal_receipt() {
        let fixture = write_fixture("dry-run-ok");
        let outcome = run_command(
            CommandMode::DryRun,
            &fixture.config_path,
            FIXED_NOW,
            &FixtureExecutor::passing(),
            &FixtureStageExecutor::passing(),
        )
        .unwrap();
        assert_eq!(outcome.exit_code, 0, "message: {}", outcome.message);
        assert_eq!(outcome.outcome, "dry-run-ok");
        let run_dir = fixture.single_run_dir();
        let terminal: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(run_dir.join(TERMINAL_RECEIPT_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(terminal["outcome"], "dry-run-ok");
        assert_eq!(terminal["reached_stage"], "dry_run");
        assert_eq!(terminal["admission"], "not-touched");
        let validations = terminal["validations"].as_array().unwrap();
        assert!(
            validations
                .iter()
                .any(|value| value["id"] == "cargo-metadata-readable")
        );
        assert!(
            terminal["receipt_path"]
                .as_str()
                .unwrap()
                .ends_with("preflight.json")
        );
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[test]
    fn dry_run_with_failing_preflight_records_not_touched_admission() {
        let fixture = write_fixture("dry-run-fail");
        std::fs::remove_file(fixture.operator_root().join("signing/release-private.pk8")).unwrap();
        let outcome = run_command(
            CommandMode::DryRun,
            &fixture.config_path,
            FIXED_NOW,
            &FixtureExecutor::passing(),
            &FixtureStageExecutor::passing(),
        )
        .unwrap();
        assert_eq!(outcome.exit_code, 1);
        let run_dir = fixture.single_run_dir();
        let terminal: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(run_dir.join(TERMINAL_RECEIPT_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(terminal["outcome"], "failure");
        assert_eq!(terminal["failed_stage"], "preflight");
        assert_eq!(terminal["admission"], "not-touched");
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[test]
    fn deploy_walks_all_twelve_stages_to_success_with_local_only_target() {
        // This fixture target has no gcp and no ssh_host: the full walk runs
        // in local-only mode and still succeeds end to end.
        let fixture = write_fixture("deploy-success");
        let outcome = run_command(
            CommandMode::Deploy,
            &fixture.config_path,
            FIXED_NOW,
            &FixtureExecutor::passing(),
            &FixtureStageExecutor::passing(),
        )
        .unwrap();
        assert_eq!(outcome.exit_code, 0, "message: {}", outcome.message);
        assert_eq!(outcome.outcome, "success");
        let run_dir = fixture.single_run_dir();
        for (index, stage) in [
            (3, "build"),
            (4, "seal"),
            (5, "frontend_image_prepare"),
            (6, "migrations"),
            (7, "admission_close"),
            (8, "local_activation"),
            (9, "remote_activation"),
            (10, "deep_readiness"),
            (11, "admission_open"),
            (12, "terminal_success_receipt"),
        ] {
            assert!(
                run_dir
                    .join(format!("stage-{index}-{stage}.json"))
                    .is_file(),
                "missing stage-{index}-{stage}.json in {}",
                run_dir.display()
            );
        }
        let terminal: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(run_dir.join(TERMINAL_RECEIPT_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(terminal["outcome"], "success");
        assert_eq!(terminal["admission"], "open");
        assert_eq!(terminal["reached_stage"], "terminal_success_receipt");
        let expected_run_id = crate::receipts::run_id_from_config_sha256(
            &crate::hashing::sha256_file(&fixture.config_path).unwrap(),
        );
        assert_eq!(
            terminal["artifacts"]["release_id"],
            format!("20260816T012000Z-{expected_run_id}"),
            "artifacts: {}",
            terminal["artifacts"]
        );
        assert!(
            terminal["artifacts"]["descriptor_sha256"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
        assert!(
            terminal["skipped"]
                .as_array()
                .unwrap()
                .iter()
                .any(|skip| skip["stage"] == "remote_activation")
        );
        let remote_stage: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(run_dir.join("stage-9-remote_activation.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(remote_stage["status"], "skipped");
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[test]
    fn deploy_with_failing_preflight_records_preflight_failure_admission_closed() {
        let fixture = write_fixture("deploy-preflight-fail");
        std::fs::remove_file(fixture.operator_root().join("signing/release-private.pk8")).unwrap();
        let outcome = run_command(
            CommandMode::Deploy,
            &fixture.config_path,
            FIXED_NOW,
            &FixtureExecutor::passing(),
            &FixtureStageExecutor::passing(),
        )
        .unwrap();
        assert_eq!(outcome.exit_code, 1);
        let run_dir = fixture.single_run_dir();
        let terminal: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(run_dir.join(TERMINAL_RECEIPT_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(terminal["failed_stage"], "preflight");
        assert_eq!(terminal["admission"], "closed");
        // No stage receipts: the walk never started.
        assert!(!run_dir.join("stage-3-build.json").exists());
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[test]
    fn unparseable_config_surfaces_controller_error_without_receipts() {
        let fixture = write_fixture("config-error");
        std::fs::write(&fixture.config_path, "{\"schema_version\": 1}").unwrap();
        let error = run_command(
            CommandMode::Deploy,
            &fixture.config_path,
            FIXED_NOW,
            &FixtureExecutor::passing(),
            &FixtureStageExecutor::passing(),
        )
        .unwrap_err();
        assert!(
            matches!(error, ControllerError::Config(_)),
            "unexpected error: {error}"
        );
        assert!(!fixture.receipt_root().exists());
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[test]
    fn dry_run_detects_missing_agent_workspace() {
        let fixture = write_fixture("dry-run-no-workspace");
        std::fs::remove_file(fixture.root.join("agent/Cargo.toml")).unwrap();
        let outcome = run_command(
            CommandMode::DryRun,
            &fixture.config_path,
            FIXED_NOW,
            &FixtureExecutor::passing(),
            &FixtureStageExecutor::passing(),
        )
        .unwrap();
        assert_eq!(outcome.exit_code, 1, "message: {}", outcome.message);
        assert_eq!(outcome.outcome, "failure");
        assert!(
            outcome.message.contains("agent-workspace-resolvable"),
            "message: {}",
            outcome.message
        );
        let _ = std::fs::remove_dir_all(&fixture.root);
    }
}
