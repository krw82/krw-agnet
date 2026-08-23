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
    match run_command(
        mode,
        &cli.config,
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
