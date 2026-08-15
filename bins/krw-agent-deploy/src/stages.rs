//! Forward-only stage machine and controller run modes.
//!
//! The 05-doc stage list is declared as a fixed table. In this controller
//! revision only stages 1-2 (`preflight`, `preflight_receipt`) are
//! implemented. Every mutating stage (build, seal, frontend image prepare,
//! migrations, admission close/open, local/remote activation, deep
//! readiness, success receipt) is declared as
//! [`StageStatus::NotImplementedThisRevision`]: when a live `deploy` walk
//! reaches such a stage the controller writes a TERMINAL FAILURE RECEIPT
//! recording `admission: closed` and exits non-zero. The controller never
//! half-activates and never rolls back binaries or DB schema; recovery is
//! always fix-forward.

use std::path::{Path, PathBuf};

use crate::checks::{self, CheckStatus, PreflightOutcome, PreflightPlan};
use crate::config::DeployConfig;
use crate::executor::PreflightExecutor;
use crate::hashing::sha256_bytes;
use crate::receipts::{
    write_preflight_receipt, write_terminal_receipt, PreflightReceipt, ReceiptError, TerminalAdmission,
    TerminalReceipt,
};
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
    NotImplementedThisRevision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageEntry {
    pub id: &'static str,
    pub status: StageStatus,
}

/// The 05-doc stage list, in forward-only order.
pub const STAGE_TABLE: [StageEntry; 12] = [
    StageEntry { id: STAGE_PREFLIGHT, status: StageStatus::Implemented },
    StageEntry { id: STAGE_PREFLIGHT_RECEIPT, status: StageStatus::Implemented },
    StageEntry { id: STAGE_BUILD, status: StageStatus::NotImplementedThisRevision },
    StageEntry { id: STAGE_SEAL, status: StageStatus::NotImplementedThisRevision },
    StageEntry { id: STAGE_FRONTEND_IMAGE_PREPARE, status: StageStatus::NotImplementedThisRevision },
    StageEntry { id: STAGE_MIGRATIONS, status: StageStatus::NotImplementedThisRevision },
    StageEntry { id: STAGE_ADMISSION_CLOSE, status: StageStatus::NotImplementedThisRevision },
    StageEntry { id: STAGE_LOCAL_ACTIVATION, status: StageStatus::NotImplementedThisRevision },
    StageEntry { id: STAGE_REMOTE_ACTIVATION, status: StageStatus::NotImplementedThisRevision },
    StageEntry { id: STAGE_DEEP_READINESS, status: StageStatus::NotImplementedThisRevision },
    StageEntry { id: STAGE_ADMISSION_OPEN, status: StageStatus::NotImplementedThisRevision },
    StageEntry { id: STAGE_TERMINAL_SUCCESS_RECEIPT, status: StageStatus::NotImplementedThisRevision },
];

pub const NOT_IMPLEMENTED_REASON: &str =
    "controller build/seal/activation stages not yet enabled in this revision; admission left closed; fix-forward";

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
pub fn prepare_run(config: &DeployConfig, config_sha256: &str, now_unix_seconds: u64, mode: CommandMode) -> RunContext {
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
/// dry-run modes; deploy stops fail-closed at the first unimplemented stage).
/// The only filesystem writes are receipts under the run's receipt dir.
pub fn run_command(
    mode: CommandMode,
    config_path: &Path,
    now_unix_seconds: u64,
    executor: &dyn PreflightExecutor,
) -> Result<RunOutcome, ControllerError> {
    let config_bytes = std::fs::read(config_path)
        .map_err(|error| ControllerError::ConfigRead(format!("{}: {error}", config_path.display())))?;
    let config_sha256 = sha256_bytes(&config_bytes);
    let config = DeployConfig::from_json_bytes(&config_bytes)?;
    let context = prepare_run(&config, &config_sha256, now_unix_seconds, mode);

    // Stage 1: read-only preflight.
    let plan = PreflightPlan {
        config: &config,
        config_path,
        config_sha256: config_sha256.clone(),
        output_dir: &context.output_dir,
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
        CommandMode::DryRun => Ok(run_dry_run(&config, &context, &outcome, &preflight_path_string, executor)),
        CommandMode::Deploy => Ok(run_deploy(&context, &outcome, &preflight_path_string)),
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
        );
        return finish_with_terminal(context, &receipt, 1, format!("preflight failed: {failed_ids}"));
    }

    // Validate that build/seal inputs resolve. No build is executed.
    let validations = dry_run_validations(config, executor);
    let all_ok = validations.iter().all(|validation| validation.status == "pass");

    if all_ok {
        let receipt = TerminalReceipt::dry_run_ok(
            &context.run_id,
            &context.created_at_utc,
            context.mode.as_str(),
            validations,
            Some(preflight_path.to_owned()),
        );
        return finish_with_terminal(context, &receipt, 0, "dry-run ok; read-only receipt complete".to_owned());
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
    );
    finish_with_terminal(context, &receipt, 1, format!("dry-run validation failed: {failed}"))
}

fn dry_run_validations(
    config: &DeployConfig,
    executor: &dyn PreflightExecutor,
) -> Vec<crate::receipts::DryRunValidation> {
    let mut validations = Vec::new();

    // agent workspace resolvable (source root + Cargo.toml manifest).
    let agent_ok = config.agent_source_root.is_dir() && config.agent_source_root.join("Cargo.toml").is_file();
    validations.push(crate::receipts::DryRunValidation {
        id: "agent-workspace-resolvable".to_owned(),
        status: if agent_ok { "pass" } else { "fail" }.to_owned(),
        detail: format!(
            "agent_source_root {}",
            if agent_ok { "resolves with Cargo.toml" } else { "missing or has no Cargo.toml" }
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
        Ok(text) if serde_json::from_str::<serde_json::Value>(&text).is_ok() => {
            ("pass".to_owned(), "cargo metadata --no-deps readable".to_owned())
        }
        Ok(_) => ("fail".to_owned(), "cargo metadata output is not valid JSON".to_owned()),
        Err(error) => ("fail".to_owned(), format!("cargo metadata failed: {error}")),
    };
    validations.push(crate::receipts::DryRunValidation {
        id: "cargo-metadata-readable".to_owned(),
        status: metadata_detail.0,
        detail: metadata_detail.1,
    });

    validations
}

fn run_deploy(context: &RunContext, outcome: &PreflightOutcome, preflight_path: &str) -> RunOutcome {
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
            format!("preflight failed; failing checks: {failed_ids}; admission left closed; fix-forward"),
            TerminalAdmission::Closed,
            Some(preflight_path.to_owned()),
            Vec::new(),
        );
        return finish_with_terminal(context, &receipt, 1, format!("deploy aborted: preflight failed: {failed_ids}"));
    }

    // Forward-only walk. Stages 1-2 (preflight, preflight receipt) are done;
    // the first stage after them decides the outcome. In this revision that
    // is always `build`, declared not implemented: fail closed with a
    // terminal failure receipt and admission recorded closed.
    let next = STAGE_TABLE
        .iter()
        .find(|entry| entry.id != STAGE_PREFLIGHT && entry.id != STAGE_PREFLIGHT_RECEIPT)
        .expect("stage table always declares a post-preflight stage");
    debug_assert_eq!(next.id, STAGE_BUILD);
    let receipt = TerminalReceipt::failure(
        &context.run_id,
        &context.created_at_utc,
        context.mode.as_str(),
        next.id,
        NOT_IMPLEMENTED_REASON.to_owned(),
        TerminalAdmission::Closed,
        Some(preflight_path.to_owned()),
        Vec::new(),
    );
    finish_with_terminal(context, &receipt, 1, format!("deploy stopped at stage `{}`: {NOT_IMPLEMENTED_REASON}", next.id))
}

fn finish_with_terminal(context: &RunContext, receipt: &TerminalReceipt, exit_code: i32, message: String) -> RunOutcome {
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
        format!("preflight PASS: {} checks passed or skipped", outcome.checks.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::FixtureExecutor;
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
        std::fs::write(agent_root.join("Cargo.toml"), "[package]\nname = \"dummy-agent\"\n").unwrap();
        std::fs::create_dir_all(&frontend_root).unwrap();
        std::fs::create_dir_all(operator_root.join("signing")).unwrap();
        std::fs::create_dir_all(operator_root.join("ops")).unwrap();
        std::fs::write(operator_root.join("signing/release-private.pk8"), b"dummy-pkcs8").unwrap();
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
        assert_eq!(STAGE_TABLE.iter().filter(|entry| entry.status == StageStatus::Implemented).count(), 2);
    }

    #[test]
    fn preflight_mode_passes_and_writes_only_the_preflight_receipt() {
        let fixture = write_fixture("preflight-mode");
        let outcome = run_command(CommandMode::Preflight, &fixture.config_path, FIXED_NOW, &FixtureExecutor::passing())
            .unwrap();
        assert_eq!(outcome.exit_code, 0);
        assert_eq!(outcome.outcome, "pass");
        let run_dir = fixture.single_run_dir();
        let run_dir_name = run_dir.file_name().unwrap().to_str().unwrap();
        assert!(
            run_dir_name.starts_with("20260816T012000Z-") && run_dir_name.len() == "20260816T012000Z-".len() + 8,
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
        let outcome = run_command(CommandMode::Preflight, &fixture.config_path, FIXED_NOW, &FixtureExecutor::passing())
            .unwrap();
        assert_eq!(outcome.exit_code, 1);
        assert_eq!(outcome.outcome, "fail");
        assert!(outcome.message.contains("signing-trust-validity"), "message: {}", outcome.message);
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[test]
    fn dry_run_ok_writes_dry_run_terminal_receipt() {
        let fixture = write_fixture("dry-run-ok");
        let outcome = run_command(CommandMode::DryRun, &fixture.config_path, FIXED_NOW, &FixtureExecutor::passing())
            .unwrap();
        assert_eq!(outcome.exit_code, 0, "message: {}", outcome.message);
        assert_eq!(outcome.outcome, "dry-run-ok");
        let run_dir = fixture.single_run_dir();
        let terminal: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(run_dir.join(TERMINAL_RECEIPT_FILE)).unwrap()).unwrap();
        assert_eq!(terminal["outcome"], "dry-run-ok");
        assert_eq!(terminal["reached_stage"], "dry_run");
        assert_eq!(terminal["admission"], "not-touched");
        let validations = terminal["validations"].as_array().unwrap();
        assert!(validations.iter().any(|value| value["id"] == "cargo-metadata-readable"));
        assert!(terminal["receipt_path"].as_str().unwrap().ends_with("preflight.json"));
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[test]
    fn dry_run_with_failing_preflight_records_not_touched_admission() {
        let fixture = write_fixture("dry-run-fail");
        std::fs::remove_file(fixture.operator_root().join("signing/release-private.pk8")).unwrap();
        let outcome = run_command(CommandMode::DryRun, &fixture.config_path, FIXED_NOW, &FixtureExecutor::passing())
            .unwrap();
        assert_eq!(outcome.exit_code, 1);
        let run_dir = fixture.single_run_dir();
        let terminal: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(run_dir.join(TERMINAL_RECEIPT_FILE)).unwrap()).unwrap();
        assert_eq!(terminal["outcome"], "failure");
        assert_eq!(terminal["failed_stage"], "preflight");
        assert_eq!(terminal["admission"], "not-touched");
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[test]
    fn deploy_fails_closed_at_build_stage_with_admission_closed() {
        let fixture = write_fixture("deploy-fail-closed");
        let outcome = run_command(CommandMode::Deploy, &fixture.config_path, FIXED_NOW, &FixtureExecutor::passing())
            .unwrap();
        assert_eq!(outcome.exit_code, 1, "message: {}", outcome.message);
        let run_dir = fixture.single_run_dir();
        let terminal: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(run_dir.join(TERMINAL_RECEIPT_FILE)).unwrap()).unwrap();
        assert_eq!(terminal["outcome"], "failure");
        assert_eq!(terminal["failed_stage"], "build");
        assert_eq!(terminal["reached_stage"], "build");
        assert_eq!(terminal["admission"], "closed");
        assert!(terminal["reason"].as_str().unwrap().contains("not yet enabled"));
        assert!(terminal["reason"].as_str().unwrap().contains("fix-forward"));
        // Nothing was built: no release output tree beyond receipts.
        assert!(!fixture.operator_root().join("releases").exists());
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[test]
    fn deploy_with_failing_preflight_records_preflight_failure_admission_closed() {
        let fixture = write_fixture("deploy-preflight-fail");
        std::fs::remove_file(fixture.operator_root().join("signing/release-private.pk8")).unwrap();
        let outcome = run_command(CommandMode::Deploy, &fixture.config_path, FIXED_NOW, &FixtureExecutor::passing())
            .unwrap();
        assert_eq!(outcome.exit_code, 1);
        let run_dir = fixture.single_run_dir();
        let terminal: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(run_dir.join(TERMINAL_RECEIPT_FILE)).unwrap()).unwrap();
        assert_eq!(terminal["failed_stage"], "preflight");
        assert_eq!(terminal["admission"], "closed");
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[test]
    fn unparseable_config_surfaces_controller_error_without_receipts() {
        let fixture = write_fixture("config-error");
        std::fs::write(&fixture.config_path, "{\"schema_version\": 1}").unwrap();
        let error = run_command(CommandMode::Deploy, &fixture.config_path, FIXED_NOW, &FixtureExecutor::passing())
            .unwrap_err();
        assert!(matches!(error, ControllerError::Config(_)), "unexpected error: {error}");
        assert!(!fixture.receipt_root().exists());
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[test]
    fn dry_run_detects_missing_agent_workspace() {
        let fixture = write_fixture("dry-run-no-workspace");
        std::fs::remove_file(fixture.root.join("agent/Cargo.toml")).unwrap();
        let outcome = run_command(CommandMode::DryRun, &fixture.config_path, FIXED_NOW, &FixtureExecutor::passing())
            .unwrap();
        assert_eq!(outcome.exit_code, 1, "message: {}", outcome.message);
        assert_eq!(outcome.outcome, "failure");
        assert!(outcome.message.contains("agent-workspace-resolvable"), "message: {}", outcome.message);
        let _ = std::fs::remove_dir_all(&fixture.root);
    }
}
