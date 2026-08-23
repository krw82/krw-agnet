//! Reopen a latched admission without a full re-deploy.
//!
//! The deploy pipeline is forward-only: any failure from `admission_close`
//! (stage 7) onward leaves the GCP admission gate `closed` until a full
//! re-deploy succeeds. [`classify_recovery`] decides from one run's immutable
//! receipts whether the closed latch is provably safe to reopen: only runs
//! that completed `remote_activation` (stage 9 — the point where the web env
//! pins shipped to the target) qualify, because only then does flipping
//! admission `open` run on top of the new pins. Runs that died before stage 9
//! are refused: their web pins are stale and only a fresh deploy fixes that.
//!
//! [`run_reconcile`] then (1) proves the daemon still matches the config pins
//! through the same `readiness.db-heartbeat` probe the deploy's deep
//! readiness stage uses (the real executor polls `readiness.`-prefixed probes
//! to their timeout), and (2) replays the exact stage-11 command pair
//! (`admission.open` + `readiness.admission-open-verify`) through the same
//! public builders the deploy path uses, with the failed run's directory name
//! as the release id. Every attempt writes one
//! `reconcile-<unix_ts>-<run_name>.json` receipt under
//! `<operator_root>/deploy-receipts/`.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config::DeployConfig;
use crate::pipeline::{
    ADMISSION_ENV_KEY, PipelineDeps, StageExecutor, build_admission_open_commands_local,
    build_admission_open_commands_remote, build_db_heartbeat_command, parse_deep_health,
    resolve_remote_topology, upsert_env_value, verify_deep_observation,
};
use crate::stages::{CommandMode, RunContext, STAGE_REMOTE_ACTIVATION};
use crate::target::TargetFile;

/// Successful (`pass`) stage receipts prove how far a run really got; a run
/// that never produced one cannot be proven safe to reopen.
const STAGE_STATUS_PASS: &str = "pass";

#[derive(Debug, PartialEq, Eq)]
pub enum RecoveryPlan {
    /// Latest deploy succeeded — admission is already open, nothing to do.
    AlreadyOpen,
    /// The run completed remote activation (stage 9) and then failed —
    /// reopening is safe after the db-heartbeat precondition passes.
    Reopen { run_name: String },
    /// The run died before remote activation (stale web pins) or left no
    /// usable evidence — a full re-deploy is required.
    Blocked { reason: String },
}

/// Decide how to recover one deploy run directory (pure: reads receipts only).
///
/// `Reopen` is safe only when the failed run completed remote activation
/// (stage 9): that is the point where the web env pins were updated, so
/// flipping admission open cannot strand the gate on stale pins. Runs that
/// died before stage 9 need a full re-deploy.
///
/// Evidence rules, matched to the real receipt shapes (`terminal.json` with
/// `outcome`/`admission`/`reached_stage`; per-stage `stage-<n>-<id>.json`
/// with `status` pass/skipped/fail):
///
/// * `terminal.json` reports `outcome == "success"` → [`RecoveryPlan::AlreadyOpen`].
/// * otherwise the highest `stage-<n>-*.json` receipt with `status == "pass"`
///   is the last completed stage (the failed stage itself writes a `fail`
///   receipt, and interrupted runs simply stop writing) → `Reopen` iff that
///   index is at least `remote_activation`'s stage-table position (9).
pub fn classify_recovery(run_dir: &Path) -> RecoveryPlan {
    let run_name = run_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if terminal_reports_success(run_dir) {
        return RecoveryPlan::AlreadyOpen;
    }
    match highest_successful_stage(run_dir) {
        Some(index) if index >= remote_activation_index() => RecoveryPlan::Reopen { run_name },
        Some(_) => RecoveryPlan::Blocked {
            reason: "deploy died before remote activation; web pins are stale; \
                     re-run the full deploy"
                .to_owned(),
        },
        None => RecoveryPlan::Blocked {
            reason: "no successful stage receipts to prove remote activation; \
                     inspect the run directory manually"
                .to_owned(),
        },
    }
}

/// `terminal.json` outcome is `success` (the success receipt always records
/// admission `open`). Unreadable/unparseable terminals count as absent — the
/// stage receipts then decide.
fn terminal_reports_success(run_dir: &Path) -> bool {
    let Ok(body) = std::fs::read_to_string(run_dir.join(crate::receipts::TERMINAL_RECEIPT_FILE))
    else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) else {
        return false;
    };
    value.get("outcome").and_then(serde_json::Value::as_str) == Some("success")
}

/// Highest stage-table position with a `pass` receipt in the run directory.
fn highest_successful_stage(run_dir: &Path) -> Option<u8> {
    let mut best: Option<u8> = None;
    for entry in std::fs::read_dir(run_dir).ok()?.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(index) = stage_index_from_receipt_name(&name) else {
            continue;
        };
        let Ok(body) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) else {
            continue;
        };
        if value.get("status").and_then(serde_json::Value::as_str) == Some(STAGE_STATUS_PASS) {
            best = Some(best.map_or(index, |previous: u8| previous.max(index)));
        }
    }
    best
}

/// Parse the stage index out of a `stage-<n>-<id>.json` receipt file name.
fn stage_index_from_receipt_name(name: &str) -> Option<u8> {
    let rest = name.strip_prefix("stage-")?;
    let (index_part, _) = rest.split_once('-')?;
    index_part.parse::<u8>().ok()
}

/// Stage-table position of a stage id (`remote_activation` → 9).
fn stage_index_from_name(name: &str) -> Option<u8> {
    crate::stages::STAGE_TABLE
        .iter()
        .position(|entry| entry.id == name)
        .and_then(|position| u8::try_from(position + 1).ok())
}

/// Stage-table position of `remote_activation`; falls back to the documented
/// position only if the table were ever reordered without this file.
fn remote_activation_index() -> u8 {
    stage_index_from_name(STAGE_REMOTE_ACTIVATION).unwrap_or(9)
}

// ---------------------------------------------------------------------------
// Reconcile orchestration
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct ReconcileOutcome {
    pub exit_code: i32,
    pub message: String,
    /// Path of the reconcile receipt this attempt wrote (always present on
    /// `Ok`).
    pub receipt_path: Option<PathBuf>,
}

/// One `reconcile-<unix_ts>-<run_name>.json` receipt (simple audit trail; the
/// per-stage and terminal receipts of the underlying run stay untouched).
#[derive(Debug, Serialize)]
struct ReconcileReceipt {
    receipt_kind: &'static str,
    run_name: String,
    plan: String,
    outcome: String,
    message: String,
    created_at_unix: u64,
}

/// Recover a closed admission latch without a full re-deploy.
///
/// Exit-code contract (mirrors the CLI): 0 = nothing to do (already open) or
/// the reopen succeeded; 1 = blocked (pre-stage-9 failure, or a failed
/// precondition/open attempt — admission stays closed); 2 = controller error
/// (unreadable target, malformed receipts root, receipt write failure) via
/// `Err`.
pub fn run_reconcile(
    config: &DeployConfig,
    config_path: &Path,
    executor: &dyn StageExecutor,
    now_unix: u64,
) -> Result<ReconcileOutcome, String> {
    let receipts_root = config.operator_root.join("deploy-receipts");
    let latest = latest_run_dir(&receipts_root)?;
    let run_name = latest
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    match classify_recovery(&latest) {
        RecoveryPlan::AlreadyOpen => finish(
            &receipts_root,
            now_unix,
            &run_name,
            "already_open",
            "nothing-to-do",
            0,
            format!("latest deploy {run_name} succeeded; admission is open; nothing to do"),
        ),
        RecoveryPlan::Blocked { reason } => finish(
            &receipts_root,
            now_unix,
            &run_name,
            "blocked",
            "blocked",
            1,
            format!("reconcile blocked: {reason}"),
        ),
        RecoveryPlan::Reopen { run_name } => reopen_admission(
            config,
            config_path,
            executor,
            now_unix,
            &run_name,
            &receipts_root,
        ),
    }
}

/// Heartbeat precondition + the exact stage-11 command pair, assembled the
/// same way the deploy path assembles them (`PipelineDeps` over the failed
/// run's identity so `release_id == run_name`, which is what the on-VM stage
/// directory and deep-health `deployment_id` are named by).
fn reopen_admission(
    config: &DeployConfig,
    config_path: &Path,
    executor: &dyn StageExecutor,
    now_unix: u64,
    run_name: &str,
    receipts_root: &Path,
) -> Result<ReconcileOutcome, String> {
    // Target re-load, exactly like the deploy path's post-preflight reload.
    let target = TargetFile::from_path(&config.target_file).map_err(|error| error.to_string())?;
    // Rebuild the run identity from the receipt directory name
    // (`<utc-ts>-<run-id>`); every builder below reads it back as the
    // release id, so the stage-11 payloads target the failed run's on-VM
    // stage directory (healthz `deployment_id` uses the same name).
    let Some((run_ts, run_id)) = run_name.split_once('-') else {
        return Err(format!(
            "run directory name `{run_name}` does not match the `<utc-ts>-<run-id>` shape"
        ));
    };
    let context = RunContext {
        // Label only: the reconcile builders read run_ts/run_id and config
        // fields, never the recorded mode.
        mode: CommandMode::Deploy,
        run_ts: run_ts.to_owned(),
        run_id: run_id.to_owned(),
        created_at_utc: crate::timeutil::format_utc_iso(now_unix),
        config_sha256: crate::hashing::sha256_file(config_path)
            .unwrap_or_else(|_| crate::hashing::UNAVAILABLE_HASH.to_owned()),
        output_dir: crate::receipts::release_output_dir(&config.operator_root, run_name),
        receipt_dir: crate::receipts::receipt_run_dir(&config.operator_root, run_name),
    };
    let unavailable = crate::hashing::UNAVAILABLE_HASH.to_owned();
    let deps = PipelineDeps {
        config,
        config_path,
        target: &target,
        context: &context,
        preflight_agent_head: &unavailable,
        preflight_frontend_head: &unavailable,
        now_unix_seconds: now_unix,
    };

    // Precondition: the daemon must still match the config pins before the
    // gate may open. `readiness.`-prefixed, so the real executor polls it to
    // its timeout; reconcile is stricter than the deploy walk — no heartbeat
    // handle at all means the pins cannot be proven, so it blocks.
    let Some(heartbeat) = build_db_heartbeat_command(&deps) else {
        return finish(
            receipts_root,
            now_unix,
            run_name,
            "reopen",
            "blocked",
            1,
            "reconcile blocked: target records no db_url_env handle, so the daemon's \
             release pins cannot be proven; re-run the full deploy"
                .to_owned(),
        );
    };
    if let Err(reason) = verify_db_heartbeat(executor, config, &heartbeat, &context.receipt_dir) {
        return finish(
            receipts_root,
            now_unix,
            run_name,
            "reopen",
            "blocked",
            1,
            format!("reconcile blocked: {reason}; admission left closed"),
        );
    }

    // The exact stage-11 pair (remote transport, or the local compose path
    // for local-only targets), same builders and same verification as
    // `stage_admission_open`.
    let topology = resolve_remote_topology(&target)?;
    if let Some(remote) = &topology {
        for spec in build_admission_open_commands_remote(&deps, remote) {
            if let Err(reason) = run_admission_command(executor, config, &spec, run_name) {
                return open_failed(receipts_root, now_unix, run_name, &reason);
            }
        }
    } else {
        upsert_env_value(&config.runtime_env, ADMISSION_ENV_KEY, "open")
            .map_err(|error| format!("local admission upsert failed: {error}"))?;
        for spec in build_admission_open_commands_local(&deps) {
            if let Err(reason) = run_admission_command(executor, config, &spec, run_name) {
                return open_failed(receipts_root, now_unix, run_name, &reason);
            }
        }
    }
    finish(
        receipts_root,
        now_unix,
        run_name,
        "reopen",
        "reopened",
        0,
        format!(
            "admission reopened for release {run_name}: db heartbeat verified, \
             admission.open executed and deep health reports `open`"
        ),
    )
}

/// Record a failed admission-open attempt (the gate stays closed — the
/// payload re-closes itself in-environment whenever its own steps fail).
fn open_failed(
    receipts_root: &Path,
    now_unix: u64,
    run_name: &str,
    reason: &str,
) -> Result<ReconcileOutcome, String> {
    finish(
        receipts_root,
        now_unix,
        run_name,
        "reopen",
        "open-failed",
        1,
        format!("reconcile failed: {reason}; admission remains closed"),
    )
}

/// Execute one admission command and, for the verify probe, check the deep
/// health observation against the failed run's release id (same checks as
/// `stage_admission_open`).
fn run_admission_command(
    executor: &dyn StageExecutor,
    config: &DeployConfig,
    spec: &crate::pipeline::StageCommand,
    release_id: &str,
) -> Result<(), String> {
    let stdout = executor
        .execute(&config.runtime_env, spec)
        .map_err(|error| format!("command `{}` failed: {error}", spec.id))?
        .stdout;
    if spec.id.starts_with("readiness.admission-open-verify") {
        let observation = parse_deep_health(&stdout);
        verify_deep_observation(observation.clone(), release_id)?;
        if observation.and_then(|observed| observed.admission) != Some("open".to_owned()) {
            return Err(
                "deep health did not report agent_v1 admission `open` after the flip".to_owned(),
            );
        }
    }
    Ok(())
}

/// Run and verify the db heartbeat: provider must match the config provider,
/// `mcp_ready` must be `t`, and when the failed run's terminal receipt
/// recorded the sealed descriptor hash, the heartbeat must still carry it
/// (the deploy's deep-readiness check compares the same fields against the
/// in-run sealed descriptor). Never opens admission on a mismatch.
fn verify_db_heartbeat(
    executor: &dyn StageExecutor,
    config: &DeployConfig,
    heartbeat: &crate::pipeline::StageCommand,
    run_dir: &Path,
) -> Result<(), String> {
    let stdout = executor
        .execute(&config.runtime_env, heartbeat)
        .map_err(|error| {
            format!(
                "db heartbeat precheck (command `{}`) failed: {error}",
                heartbeat.id
            )
        })?
        .stdout;
    let parts: Vec<&str> = stdout.split('|').collect();
    if parts.len() != 4 {
        return Err(format!("db heartbeat row has the wrong shape: `{stdout}`"));
    }
    if parts[0] != config.provider {
        return Err(format!(
            "db heartbeat provider `{}` does not match config provider `{}`",
            parts[0], config.provider
        ));
    }
    if parts[3] != "t" {
        return Err(format!(
            "db heartbeat mcp_ready is `{}`, expected `t`",
            parts[3]
        ));
    }
    if let Some(sealed) = sealed_descriptor_from_terminal(run_dir)
        && parts[1] != sealed
    {
        return Err(format!(
            "db heartbeat descriptor `{}` does not match the sealed descriptor `{sealed}` \
             recorded by the failed run",
            parts[1]
        ));
    }
    Ok(())
}

/// `artifacts.descriptor_sha256` from the run's terminal receipt, when the
/// failed run got far enough to record it.
fn sealed_descriptor_from_terminal(run_dir: &Path) -> Option<String> {
    let body =
        std::fs::read_to_string(run_dir.join(crate::receipts::TERMINAL_RECEIPT_FILE)).ok()?;
    let value = serde_json::from_str::<serde_json::Value>(&body).ok()?;
    value
        .get("artifacts")?
        .get("descriptor_sha256")?
        .as_str()
        .map(str::to_owned)
}

/// Write the reconcile receipt and produce the CLI outcome.
fn finish(
    receipts_root: &Path,
    now_unix: u64,
    run_name: &str,
    plan: &str,
    outcome: &str,
    exit_code: i32,
    message: String,
) -> Result<ReconcileOutcome, String> {
    let receipt = ReconcileReceipt {
        receipt_kind: "reconcile",
        run_name: run_name.to_owned(),
        plan: plan.to_owned(),
        outcome: outcome.to_owned(),
        message: message.clone(),
        created_at_unix: now_unix,
    };
    let json = serde_json::to_vec_pretty(&receipt)
        .map_err(|error| format!("serialize reconcile receipt: {error}"))?;
    let path = receipts_root.join(format!("reconcile-{now_unix}-{run_name}.json"));
    std::fs::create_dir_all(receipts_root)
        .map_err(|error| format!("{}: {error}", receipts_root.display()))?;
    let mut bytes = json;
    bytes.push(b'\n');
    std::fs::write(&path, bytes).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(ReconcileOutcome {
        exit_code,
        message,
        receipt_path: Some(path),
    })
}

/// Latest run directory under the receipts root. Directory names are
/// `<utc-ts>-<run-id>` prefixed, so lexical comparison is chronological.
fn latest_run_dir(root: &Path) -> Result<PathBuf, String> {
    let mut best: Option<PathBuf> = None;
    for entry in std::fs::read_dir(root)
        .map_err(|error| format!("{}: {error}", root.display()))?
        .flatten()
    {
        let path = entry.path();
        if path.is_dir()
            && best
                .as_ref()
                .is_none_or(|previous: &PathBuf| path > *previous)
        {
            best = Some(path);
        }
    }
    best.ok_or_else(|| "no deploy receipts found".to_owned())
}

#[cfg(test)]
mod classify_tests {
    use super::{RecoveryPlan, classify_recovery};
    use std::fs;

    fn write_run(dir: &std::path::Path, terminal: Option<&str>, receipts: &[(u8, &str, &str)]) {
        fs::create_dir_all(dir).unwrap();
        if let Some(body) = terminal {
            fs::write(dir.join("terminal.json"), body).unwrap();
        }
        for (index, stage, status) in receipts {
            fs::write(
                dir.join(format!("stage-{index}-{stage}.json")),
                format!(r#"{{"status":"{status}"}}"#),
            )
            .unwrap();
        }
    }

    #[test]
    fn success_run_means_already_open() {
        let tmp = tempfile::tempdir().unwrap();
        write_run(
            tmp.path(),
            Some(r#"{"outcome":"success","admission":"open"}"#),
            &[(12, "terminal_success_receipt", "pass")],
        );
        assert_eq!(classify_recovery(tmp.path()), RecoveryPlan::AlreadyOpen);
    }

    #[test]
    fn stage10_or_11_failure_is_recoverable() {
        for stage in ["deep_readiness", "admission_open"] {
            let tmp = tempfile::tempdir().unwrap();
            write_run(
                tmp.path(),
                Some(&format!(
                    r#"{{"outcome":"failure","admission":"closed","reached_stage":"{stage}"}}"#
                )),
                &[
                    (9, "remote_activation", "pass"),
                    (10, "deep_readiness", "fail"),
                ],
            );
            let name = tmp
                .path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            assert_eq!(
                classify_recovery(tmp.path()),
                RecoveryPlan::Reopen { run_name: name },
                "stage {stage} failure must be reopenable"
            );
        }
    }

    #[test]
    fn pre_remote_failure_is_blocked() {
        let tmp = tempfile::tempdir().unwrap();
        write_run(
            tmp.path(),
            Some(
                r#"{"outcome":"failure","admission":"closed","reached_stage":"local_activation"}"#,
            ),
            &[
                (7, "admission_close", "pass"),
                (8, "local_activation", "fail"),
            ],
        );
        assert!(matches!(
            classify_recovery(tmp.path()),
            RecoveryPlan::Blocked { .. }
        ));
    }

    #[test]
    fn missing_terminal_receipt_falls_back_to_stage_receipts() {
        let tmp = tempfile::tempdir().unwrap();
        // Interrupted deploy (Ctrl-C): no terminal.json, stage 9 passed.
        write_run(tmp.path(), None, &[(9, "remote_activation", "pass")]);
        let name = tmp
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            classify_recovery(tmp.path()),
            RecoveryPlan::Reopen { run_name: name }
        );
    }

    #[test]
    fn failure_at_remote_activation_itself_is_blocked() {
        // The failed stage writes a `fail` receipt, so the highest `pass` is
        // stage 8: the web pins never fully shipped — re-deploy required.
        let tmp = tempfile::tempdir().unwrap();
        write_run(
            tmp.path(),
            Some(
                r#"{"outcome":"failure","admission":"closed","reached_stage":"remote_activation"}"#,
            ),
            &[
                (8, "local_activation", "pass"),
                (9, "remote_activation", "fail"),
            ],
        );
        assert!(matches!(
            classify_recovery(tmp.path()),
            RecoveryPlan::Blocked { .. }
        ));
    }

    #[test]
    fn preflight_failure_without_stage_receipts_is_blocked() {
        let tmp = tempfile::tempdir().unwrap();
        write_run(
            tmp.path(),
            Some(r#"{"outcome":"failure","admission":"closed","failed_stage":"preflight"}"#),
            &[],
        );
        let RecoveryPlan::Blocked { reason } = classify_recovery(tmp.path()) else {
            panic!("preflight failure must be blocked");
        };
        assert!(reason.contains("no successful stage receipts"), "{reason}");
    }

    #[test]
    fn skipped_stage_receipts_do_not_count_as_completed() {
        // Local-only runs record stage 9 as `skipped`, not `pass`; an
        // interrupted local-only run without a stage-10 pass is blocked.
        let tmp = tempfile::tempdir().unwrap();
        write_run(
            tmp.path(),
            None,
            &[
                (8, "local_activation", "pass"),
                (9, "remote_activation", "skipped"),
            ],
        );
        assert!(matches!(
            classify_recovery(tmp.path()),
            RecoveryPlan::Blocked { .. }
        ));
    }

    #[test]
    fn stage_table_keeps_remote_activation_at_position_nine() {
        assert_eq!(super::stage_index_from_name("remote_activation"), Some(9));
        assert_eq!(super::stage_index_from_name("deep_readiness"), Some(10));
        assert_eq!(super::stage_index_from_name("admission_open"), Some(11));
        assert_eq!(super::stage_index_from_name("preflight"), Some(1));
        assert_eq!(super::stage_index_from_name("dry_run"), None);
    }
}

#[cfg(test)]
mod run_tests {
    use super::run_reconcile;
    use crate::config::{DeployConfig, DeployTimeouts};
    use crate::pipeline::FixtureStageExecutor;
    use std::fs;
    use std::path::PathBuf;

    const RUN_NAME: &str = "20260823T004704Z-ffbd47c8";
    const NOW: u64 = 1_787_000_000;
    const DESCRIPTOR: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    struct Fixture {
        root: PathBuf,
        config: DeployConfig,
        config_path: PathBuf,
    }

    impl Fixture {
        fn receipts_root(&self) -> PathBuf {
            self.config.operator_root.join("deploy-receipts")
        }

        fn run_dir(&self, run_name: &str) -> PathBuf {
            self.receipts_root().join(run_name)
        }

        fn write_failed_run(&self, run_name: &str, reached_stage: &str, highest_pass: u8) {
            let dir = self.run_dir(run_name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("terminal.json"),
                format!(
                    r#"{{"outcome":"failure","admission":"closed","reached_stage":"{reached_stage}","artifacts":{{"descriptor_sha256":"{DESCRIPTOR}"}}}}"#
                ),
            )
            .unwrap();
            for index in 3..=highest_pass {
                let stage = match index {
                    3 => "build",
                    4 => "seal",
                    5 => "frontend_image_prepare",
                    6 => "migrations",
                    7 => "admission_close",
                    8 => "local_activation",
                    9 => "remote_activation",
                    10 => "deep_readiness",
                    11 => "admission_open",
                    _ => "terminal_success_receipt",
                };
                fs::write(
                    dir.join(format!("stage-{index}-{stage}.json")),
                    r#"{"status":"pass"}"#,
                )
                .unwrap();
            }
        }

        fn write_success_run(&self, run_name: &str) {
            let dir = self.run_dir(run_name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("terminal.json"),
                r#"{"outcome":"success","admission":"open"}"#,
            )
            .unwrap();
        }

        fn reconcile_receipt(&self, now: u64, run_name: &str) -> serde_json::Value {
            let path = self
                .receipts_root()
                .join(format!("reconcile-{now}-{run_name}.json"));
            serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
        }
    }

    fn write_fixture(tag: &str, remote: bool) -> Fixture {
        let root = std::env::temp_dir().join(format!(
            "krw-agent-deploy-reconcile-{tag}-{}-{NOW}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let operator_root = root.join("operator");
        fs::create_dir_all(operator_root.join("deploy-receipts")).unwrap();
        let runtime_env = root.join("runtime.env");
        fs::write(
            &runtime_env,
            "# fixture env: dummy values only\nKRW_AGENT_ADMISSION_MODE=closed\nKRW_AGENT_DB_URL=postgresql://fixture-dummy\n",
        )
        .unwrap();
        let target_json = if remote {
            r#"{
              "provider": "deepseek",
              "gcp": {"project": "krw-prod-dummy", "zone": "asia-northeast3-a", "instance": "krw-agent-prod-dummy"},
              "remote_front_dir": "/home/deploy/krw-ontology-front",
              "compose_project": "krw-ontology-front",
              "db_url_env": "KRW_AGENT_DB_URL",
              "site_origins": ["https://one.example.com", "https://two.example.com"]
            }"#
        } else {
            r#"{"provider": "deepseek", "local_ports": [], "required_env_keys": []}"#
        };
        let target_file = root.join("target.json");
        fs::write(&target_file, target_json).unwrap();
        let config_path = root.join("deploy-config.json");
        fs::write(&config_path, "{\"schema_version\":1}").unwrap();
        let config = DeployConfig {
            schema_version: 1,
            provider: "deepseek".to_owned(),
            agent_source_root: root.join("agent"),
            frontend_source_root: root.join("front"),
            operator_root,
            runtime_env,
            target_file,
            frontend_contract: root.join("contract.json"),
            timeouts: DeployTimeouts {
                ssh_ms: 5_000,
                mcp_ms: 5_000,
                daemon_ready_ms: 90_000,
                public_ready_ms: 90_000,
            },
        };
        Fixture {
            root,
            config,
            config_path,
        }
    }

    fn passing_executor() -> FixtureStageExecutor {
        let mut executor = FixtureStageExecutor::passing();
        executor.outputs.insert(
            "readiness.db-heartbeat".to_owned(),
            format!("deepseek|{DESCRIPTOR}|sha256:{}|t", "b".repeat(64)),
        );
        executor.outputs.insert(
            "readiness.admission-open-verify".to_owned(),
            format!(
                r#"{{"status":"ok","deployment_id":"{RUN_NAME}","checks":{{"agent_v1":{{"admission":"open"}}}}}}"#
            ),
        );
        executor
    }

    #[test]
    fn reopen_runs_heartbeat_then_the_stage11_pair_for_the_failed_run() {
        let fixture = write_fixture("reopen", true);
        fixture.write_failed_run(RUN_NAME, "deep_readiness", 9);
        let executor = passing_executor();
        let outcome = run_reconcile(&fixture.config, &fixture.config_path, &executor, NOW).unwrap();
        assert_eq!(outcome.exit_code, 0, "message: {}", outcome.message);
        assert_eq!(
            executor.command_ids(),
            [
                "readiness.db-heartbeat",
                "admission.open",
                "readiness.admission-open-verify"
            ]
        );
        // The stage-11 commands must carry the failed run's name as the
        // release id (gcloud transport; payload embeds the on-VM stage dir).
        let commands = executor.commands.borrow();
        let open = &commands[1];
        assert_eq!(open.argv[0], "gcloud");
        let payload = open.argv.last().unwrap();
        assert!(
            payload.contains(RUN_NAME),
            "payload must embed the failed run name:\n{payload}"
        );
        let verify = &commands[2];
        assert_eq!(verify.id, "readiness.admission-open-verify");
        let receipt = fixture.reconcile_receipt(NOW, RUN_NAME);
        assert_eq!(receipt["receipt_kind"], "reconcile");
        assert_eq!(receipt["run_name"], RUN_NAME);
        assert_eq!(receipt["plan"], "reopen");
        assert_eq!(receipt["outcome"], "reopened");
        assert_eq!(receipt["created_at_unix"], NOW);
        let _ = fs::remove_dir_all(&fixture.root);
    }

    #[test]
    fn pre_remote_failure_blocks_without_touching_the_executor() {
        let fixture = write_fixture("blocked", true);
        fixture.write_failed_run(RUN_NAME, "local_activation", 7);
        let executor = FixtureStageExecutor::passing();
        let outcome = run_reconcile(&fixture.config, &fixture.config_path, &executor, NOW).unwrap();
        assert_eq!(outcome.exit_code, 1, "message: {}", outcome.message);
        assert!(
            outcome.message.contains("reconcile blocked"),
            "{}",
            outcome.message
        );
        assert!(executor.commands.borrow().is_empty());
        let receipt = fixture.reconcile_receipt(NOW, RUN_NAME);
        assert_eq!(receipt["outcome"], "blocked");
        let _ = fs::remove_dir_all(&fixture.root);
    }

    #[test]
    fn already_open_run_exits_zero_without_commands() {
        let fixture = write_fixture("already-open", true);
        fixture.write_success_run(RUN_NAME);
        let executor = FixtureStageExecutor::passing();
        let outcome = run_reconcile(&fixture.config, &fixture.config_path, &executor, NOW).unwrap();
        assert_eq!(outcome.exit_code, 0, "message: {}", outcome.message);
        assert!(outcome.message.contains("nothing to do"));
        assert!(executor.commands.borrow().is_empty());
        let _ = fs::remove_dir_all(&fixture.root);
    }

    #[test]
    fn heartbeat_provider_mismatch_blocks_before_any_open() {
        let fixture = write_fixture("heartbeat-provider", true);
        fixture.write_failed_run(RUN_NAME, "deep_readiness", 9);
        let mut executor = FixtureStageExecutor::passing();
        executor.outputs.insert(
            "readiness.db-heartbeat".to_owned(),
            format!("glm|{DESCRIPTOR}|sha256:{}|t", "b".repeat(64)),
        );
        let outcome = run_reconcile(&fixture.config, &fixture.config_path, &executor, NOW).unwrap();
        assert_eq!(outcome.exit_code, 1, "message: {}", outcome.message);
        assert!(outcome.message.contains("glm"), "{}", outcome.message);
        assert!(outcome.message.contains("deepseek"), "{}", outcome.message);
        assert_eq!(executor.command_ids(), ["readiness.db-heartbeat"]);
        let _ = fs::remove_dir_all(&fixture.root);
    }

    #[test]
    fn heartbeat_descriptor_drift_against_the_failed_run_blocks() {
        let fixture = write_fixture("heartbeat-descriptor", true);
        fixture.write_failed_run(RUN_NAME, "deep_readiness", 9);
        let mut executor = FixtureStageExecutor::passing();
        executor.outputs.insert(
            "readiness.db-heartbeat".to_owned(),
            format!(
                "deepseek|sha256:{}|sha256:{}|t",
                "c".repeat(64),
                "b".repeat(64)
            ),
        );
        let outcome = run_reconcile(&fixture.config, &fixture.config_path, &executor, NOW).unwrap();
        assert_eq!(outcome.exit_code, 1, "message: {}", outcome.message);
        assert!(
            outcome.message.contains("sealed descriptor"),
            "message: {}",
            outcome.message
        );
        assert_eq!(executor.command_ids(), ["readiness.db-heartbeat"]);
        let _ = fs::remove_dir_all(&fixture.root);
    }

    #[test]
    fn missing_heartbeat_handle_blocks_instead_of_opening() {
        // Local-only fixture target records no db_url_env handle: the
        // precondition must fire before any command executes.
        let fixture = write_fixture("no-heartbeat", false);
        fixture.write_failed_run(RUN_NAME, "deep_readiness", 9);
        let executor = FixtureStageExecutor::passing();
        let outcome = run_reconcile(&fixture.config, &fixture.config_path, &executor, NOW).unwrap();
        assert_eq!(outcome.exit_code, 1, "message: {}", outcome.message);
        assert!(
            outcome.message.contains("db_url_env"),
            "message: {}",
            outcome.message
        );
        assert!(executor.commands.borrow().is_empty());
        let _ = fs::remove_dir_all(&fixture.root);
    }

    #[test]
    fn failed_admission_open_keeps_exit_one_and_names_the_command() {
        let fixture = write_fixture("open-failed", true);
        fixture.write_failed_run(RUN_NAME, "deep_readiness", 9);
        let mut executor = passing_executor();
        executor.failures.insert(
            "admission.open".to_owned(),
            "ssh transport unavailable".to_owned(),
        );
        let outcome = run_reconcile(&fixture.config, &fixture.config_path, &executor, NOW).unwrap();
        assert_eq!(outcome.exit_code, 1, "message: {}", outcome.message);
        assert!(
            outcome.message.contains("command `admission.open` failed"),
            "message: {}",
            outcome.message
        );
        assert!(
            outcome.message.contains("admission remains closed"),
            "message: {}",
            outcome.message
        );
        let receipt = fixture.reconcile_receipt(NOW, RUN_NAME);
        assert_eq!(receipt["outcome"], "open-failed");
        let _ = fs::remove_dir_all(&fixture.root);
    }

    #[test]
    fn latest_run_dir_picks_the_lexically_greatest_run() {
        let fixture = write_fixture("latest", true);
        fixture.write_failed_run("20260801T000000Z-00000001", "local_activation", 7);
        fixture.write_success_run(RUN_NAME);
        let executor = FixtureStageExecutor::passing();
        let outcome = run_reconcile(&fixture.config, &fixture.config_path, &executor, NOW).unwrap();
        assert_eq!(outcome.exit_code, 0, "message: {}", outcome.message);
        assert!(outcome.message.contains(RUN_NAME), "{}", outcome.message);
        let _ = fs::remove_dir_all(&fixture.root);
    }

    #[test]
    fn empty_receipts_root_is_a_controller_error() {
        let fixture = write_fixture("empty-root", true);
        let executor = FixtureStageExecutor::passing();
        let error =
            run_reconcile(&fixture.config, &fixture.config_path, &executor, NOW).unwrap_err();
        assert!(error.contains("no deploy receipts"), "{error}");
        let _ = fs::remove_dir_all(&fixture.root);
    }
}
