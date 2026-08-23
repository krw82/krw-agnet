//! Thin CLI over the `krw_agent_deploy` library (doc/refetoring/05).
//!
//! `krw-agent-deploy --config <absolute production config> <command>` where
//! command is `preflight`, `dry-run`, `deploy`, or `reconcile`. Exit codes:
//! 0 = verdict pass / dry-run ok / deploy success / reconcile nothing-to-do
//! or reopened, 1 = fail-closed outcome / reconcile blocked (pre-stage-9
//! failure or failed precondition), 2 = controller error (config
//! unreadable/unparseable, receipt write failure).

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use krw_agent_deploy::config::DeployConfig;
use krw_agent_deploy::executor::RealCommandExecutor;
use krw_agent_deploy::pipeline::RealStageExecutor;
use krw_agent_deploy::stages::{CommandMode, run_command};

#[derive(Debug, Parser)]
#[command(
    name = "krw-agent-deploy",
    version,
    about = "KRW Agent deployment controller (read-only preflight, receipts, forward-only staged deployment)"
)]
struct Cli {
    /// Absolute path to the production deployment config JSON.
    #[arg(long)]
    config: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the read-only preflight and write the immutable preflight receipt.
    Preflight,
    /// Preflight plus build/seal input resolution; writes a dry-run receipt.
    /// Executes no builds and touches nothing outside the receipt directory.
    DryRun,
    /// Forward-only deployment walk through all twelve stages. Any failure
    /// writes a terminal failure receipt with admission closed (from
    /// `admission_close` onward) and exits non-zero; recovery is
    /// fix-forward only — the controller never rolls back.
    Deploy,
    /// Recover a closed admission latch from the latest failed deploy
    /// (stages 9+) without a full re-deploy: verify the db heartbeat still
    /// matches the config pins, then replay the stage-11 admission-open
    /// pair. Runs that died before remote activation (stage 9) are refused
    /// with a reason — their web pins are stale and need a full re-deploy.
    Reconcile,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    if let Command::Reconcile = cli.command {
        return run_reconcile_command(&cli.config, now);
    }
    let mode = match cli.command {
        Command::Preflight => CommandMode::Preflight,
        Command::DryRun => CommandMode::DryRun,
        Command::Deploy => CommandMode::Deploy,
        Command::Reconcile => unreachable!("handled above"),
    };
    // Dirty frontend checkouts were the #1 deploy-failure cause (11 of 45
    // in the 2026-08 receipts). For dry-run and deploy, silently swap the
    // configured (dirty) frontend source for a clean detached worktree at
    // its HEAD by writing a side config. Preflight keeps the configured
    // path: it must report the true state of the operator's checkout.
    let config_path = match mode {
        CommandMode::Preflight => cli.config.clone(),
        CommandMode::DryRun | CommandMode::Deploy => {
            match repoint_config_to_clean_front(&cli.config) {
                Ok(path) => path,
                Err(error) => {
                    eprintln!(
                        "krw-agent-deploy: clean-worktree repoint failed; continuing with \
                         the configured source so preflight reports its real state: {error}"
                    );
                    cli.config.clone()
                }
            }
        }
    };
    match run_command(
        mode,
        &config_path,
        now,
        &RealCommandExecutor::new(),
        &RealStageExecutor::new(),
    ) {
        Ok(outcome) => {
            if let Some(path) = &outcome.preflight_receipt_path {
                println!("preflight receipt: {}", path.display());
            }
            if let Some(path) = &outcome.terminal_receipt_path {
                println!("terminal receipt: {}", path.display());
            }
            println!("{}", outcome.message);
            ExitCode::from(u8::try_from(outcome.exit_code.max(0)).unwrap_or(2))
        }
        Err(error) => {
            eprintln!("krw-agent-deploy: {error}");
            ExitCode::from(2)
        }
    }
}

/// Repoint `frontend_source_root` at a clean source for dry-run/deploy.
///
/// Reads the config file as raw JSON, asks
/// [`krw_agent_deploy::front_worktree::materialize_clean_front_worktree`]
/// for a clean frontend tree, and — only when the configured checkout was
/// dirty — writes a side config (`<stem>.worktree.json` under the system
/// temp dir) with `frontend_source_root` replaced by the worktree path.
/// A clean source returns the original config path unchanged and writes
/// nothing. The operator's checkout is never modified.
fn repoint_config_to_clean_front(config_path: &Path) -> Result<PathBuf, String> {
    let bytes = std::fs::read(config_path).map_err(|e| e.to_string())?;
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let front = value
        .get("frontend_source_root")
        .and_then(|v| v.as_str())
        .ok_or("config missing frontend_source_root")?;
    let operator_root = value
        .get("operator_root")
        .and_then(|v| v.as_str())
        .unwrap_or("/tmp");
    let dest_root = Path::new(operator_root).join("front-worktrees");
    let resolved = krw_agent_deploy::front_worktree::materialize_clean_front_worktree(
        Path::new(front),
        &dest_root,
    )?;
    if resolved == Path::new(front) {
        return Ok(config_path.to_path_buf());
    }
    value["frontend_source_root"] =
        serde_json::Value::String(resolved.to_string_lossy().into_owned());
    let side = std::env::temp_dir().join(format!(
        "{}.worktree.json",
        config_path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
    ));
    let serialized = serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?;
    std::fs::write(&side, serialized).map_err(|e| e.to_string())?;
    eprintln!(
        "frontend source was dirty; using clean worktree {} via side config {}",
        resolved.display(),
        side.display()
    );
    Ok(side)
}

/// Load the config (controller errors exit 2), then reconcile the latest
/// run's admission latch. 0 = nothing to do or reopened; 1 = blocked; the
/// reconcile receipt path is printed alongside the human-readable message.
fn run_reconcile_command(config_path: &Path, now: u64) -> ExitCode {
    let config_bytes = match std::fs::read(config_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!(
                "krw-agent-deploy: config unreadable {}: {error}",
                config_path.display()
            );
            return ExitCode::from(2);
        }
    };
    let config = match DeployConfig::from_json_bytes(&config_bytes) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("krw-agent-deploy: config unusable: {error}");
            return ExitCode::from(2);
        }
    };
    match krw_agent_deploy::reconcile::run_reconcile(
        &config,
        config_path,
        &RealStageExecutor::new(),
        now,
    ) {
        Ok(outcome) => {
            if let Some(path) = &outcome.receipt_path {
                println!("reconcile receipt: {}", path.display());
            }
            println!("{}", outcome.message);
            ExitCode::from(u8::try_from(outcome.exit_code.max(0)).unwrap_or(2))
        }
        Err(error) => {
            eprintln!("krw-agent-deploy: {error}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod repoint_tests {
    use super::repoint_config_to_clean_front;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(["-C", dir.to_str().unwrap()])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn make_repo(dir: &Path, dirty: bool) {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "t@t"]);
        git(dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("f.txt"), "v1").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", "1"]);
        if dirty {
            std::fs::write(dir.join("f.txt"), "v2 uncommitted").unwrap();
        }
    }

    fn write_config(dir: &Path, name: &str, front: &Path, operator_root: &Path) -> PathBuf {
        let json = format!(
            r#"{{"schema_version":1,"provider":"p","agent_source_root":"/tmp/agent",
"frontend_source_root":{},"operator_root":{},"runtime_env":"/tmp/runtime.env",
"target_file":"/tmp/target.json","frontend_contract":"/tmp/contract.json",
"timeouts":{{"ssh_ms":5000,"mcp_ms":5000,"daemon_ready_ms":90000,"public_ready_ms":900000}}}}"#,
            serde_json::to_string(front).unwrap(),
            serde_json::to_string(operator_root).unwrap(),
        );
        let path = dir.join(name);
        std::fs::write(&path, json).unwrap();
        path
    }

    #[test]
    fn clean_source_keeps_the_original_config() {
        let scratch = tempfile::tempdir().unwrap();
        let front = scratch.path().join("front");
        make_repo(&front, false);
        let operator_root = scratch.path().join("operator");
        let config = write_config(
            scratch.path(),
            "task8-clean-source.json",
            &front,
            &operator_root,
        );
        let resolved = repoint_config_to_clean_front(&config).unwrap();
        assert_eq!(
            resolved, config,
            "clean source must not spawn a side config"
        );
        assert!(!operator_root.join("front-worktrees").exists());
    }

    #[test]
    fn dirty_source_yields_a_side_config_pointing_at_a_clean_worktree() {
        let scratch = tempfile::tempdir().unwrap();
        let front = scratch.path().join("front");
        make_repo(&front, true);
        let operator_root = scratch.path().join("operator");
        let config = write_config(
            scratch.path(),
            "task8-dirty-source.json",
            &front,
            &operator_root,
        );
        let resolved = repoint_config_to_clean_front(&config).unwrap();
        assert_ne!(resolved, config);
        let side: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&resolved).unwrap()).unwrap();
        let repointed = PathBuf::from(side["frontend_source_root"].as_str().unwrap());
        assert!(
            repointed.starts_with(operator_root.join("front-worktrees")),
            "worktree must live under <operator_root>/front-worktrees: {}",
            repointed.display()
        );
        let porcelain = Command::new("git")
            .args(["-C", repointed.to_str().unwrap(), "status", "--porcelain"])
            .output()
            .unwrap();
        assert!(
            porcelain.stdout.is_empty(),
            "worktree must be clean at HEAD"
        );
        // Other config fields survive the round trip untouched.
        assert_eq!(side["provider"].as_str(), Some("p"));
        assert_eq!(side["schema_version"].as_u64(), Some(1));
        let _ = std::fs::remove_file(&resolved);
    }

    #[test]
    fn missing_frontend_source_root_is_an_error() {
        let scratch = tempfile::tempdir().unwrap();
        let config = scratch.path().join("task8-no-front.json");
        std::fs::write(&config, r#"{"schema_version":1}"#).unwrap();
        let error = repoint_config_to_clean_front(&config).unwrap_err();
        assert!(
            error.contains("frontend_source_root"),
            "unexpected error: {error}"
        );
    }
}
