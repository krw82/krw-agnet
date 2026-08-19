//! Thin CLI over the `krw_agent_deploy` library (doc/refetoring/05).
//!
//! `krw-agent-deploy --config <absolute production config> <command>` where
//! command is `preflight`, `dry-run`, or `deploy`. Exit codes: 0 = verdict
//! pass / dry-run ok / deploy success, 1 = fail-closed outcome, 2 =
//! controller error (config unreadable/unparseable, receipt write failure).

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
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
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mode = match cli.command {
        Command::Preflight => CommandMode::Preflight,
        Command::DryRun => CommandMode::DryRun,
        Command::Deploy => CommandMode::Deploy,
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
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
