//! Preflight check executors.
//!
//! Every check that must observe the outside world (git, toolchain, GCP,
//! SSH, Postgres, TCP) goes through [`PreflightExecutor`]. The
//! [`RealCommandExecutor`] shells out strictly read-only (`describe`,
//! `command -v`, `ssh ... true`, `psql -c 'select 1'`, TCP connects) and
//! never issues a create/apply/update/delete against any system. The
//! [`FixtureExecutor`] lets unit and integration tests drive the registry
//! deterministically.

use std::collections::BTreeSet;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::target::GcpTarget;

pub trait PreflightExecutor: std::fmt::Debug {
    /// Stable executor label recorded in receipt details (`real`, `fixture`).
    fn kind(&self) -> &'static str;

    /// `git status --porcelain` output for a source root (empty = clean).
    fn git_porcelain(&self, root: &Path) -> Result<String, String>;

    /// `git rev-parse HEAD` for a source root.
    fn git_head(&self, root: &Path) -> Result<String, String>;

    /// `command -v` probe for a required local command.
    fn command_available(&self, name: &str) -> Result<bool, String>;

    /// `cargo metadata --no-deps` for the agent workspace (read-only).
    fn cargo_metadata(&self, agent_root: &Path) -> Result<String, String>;

    /// `gcloud compute instances describe ... --format=json` (read-only).
    fn gcp_describe_instance(&self, target: &GcpTarget) -> Result<String, String>;

    /// `ssh -o BatchMode=yes <host> true` connectivity probe (no writes).
    fn ssh_batch_true(&self, host: &str, ssh_ms: u64) -> Result<(), String>;

    /// `psql <url> -c 'select 1'` reachability probe. The connection string
    /// is resolved from a process-env handle name and never recorded.
    fn db_select_one(&self, db_url_env: &str) -> Result<(), String>;

    /// TCP connect probe (MCP endpoints; connectivity only, never LLM calls).
    fn tcp_connect(&self, host: &str, port: u16, timeout_ms: u64) -> Result<(), String>;
}

/// Read-only shell-out executor. `path_override` exists so tests can prove
/// the `command -v` probe fails closed under an empty `PATH`.
#[derive(Debug, Clone)]
pub struct RealCommandExecutor {
    pub path_override: Option<PathBuf>,
}

impl RealCommandExecutor {
    pub fn new() -> Self {
        Self { path_override: None }
    }

    fn shell(&self, program: &str, args: &[&str]) -> Result<String, String> {
        let mut command = Command::new(program);
        command.args(args);
        if let Some(path) = &self.path_override {
            command.env("PATH", path);
        }
        let output = command
            .output()
            .map_err(|error| format!("cannot spawn `{program}`: {error}"))?;
        if output.status.success() {
            String::from_utf8(output.stdout)
                .map_err(|error| format!("`{program}` output not UTF-8: {error}"))
        } else {
            Err(format!(
                "`{program}` exited with status {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }
}

impl Default for RealCommandExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl PreflightExecutor for RealCommandExecutor {
    fn kind(&self) -> &'static str {
        "real"
    }

    fn git_porcelain(&self, root: &Path) -> Result<String, String> {
        self.shell(
            "git",
            &["-C", &root.display().to_string(), "status", "--porcelain=v1", "--untracked-files=normal"],
        )
    }

    fn git_head(&self, root: &Path) -> Result<String, String> {
        self.shell("git", &["-C", &root.display().to_string(), "rev-parse", "HEAD"])
            .map(|stdout| stdout.trim().to_owned())
    }

    fn command_available(&self, name: &str) -> Result<bool, String> {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(format!("command -v {name} >/dev/null 2>&1"));
        if let Some(path) = &self.path_override {
            command.env("PATH", path);
        }
        let output = command
            .output()
            .map_err(|error| format!("cannot probe `{name}`: {error}"))?;
        Ok(output.status.success())
    }

    fn cargo_metadata(&self, agent_root: &Path) -> Result<String, String> {
        let manifest = agent_root.join("Cargo.toml");
        self.shell(
            "cargo",
            &[
                "metadata",
                "--format-version",
                "1",
                "--no-deps",
                "--manifest-path",
                &manifest.display().to_string(),
            ],
        )
    }

    fn gcp_describe_instance(&self, target: &GcpTarget) -> Result<String, String> {
        if target.project.is_empty() || target.zone.is_empty() || target.instance.is_empty() {
            return Err("gcp target must record non-empty project, zone, and instance".to_owned());
        }
        self.shell(
            "gcloud",
            &[
                "compute",
                "instances",
                "describe",
                &target.instance,
                "--zone",
                &target.zone,
                "--project",
                &target.project,
                "--format=json",
            ],
        )
    }

    fn ssh_batch_true(&self, host: &str, ssh_ms: u64) -> Result<(), String> {
        let connect_option = format!("ConnectTimeout={}", (ssh_ms / 1000).max(1));
        self.shell(
            "ssh",
            &["-o", "BatchMode=yes", "-o", &connect_option, host, "true"],
        )
        .map(|_| ())
    }

    fn db_select_one(&self, db_url_env: &str) -> Result<(), String> {
        let Some(url) = std::env::var_os(db_url_env) else {
            return Err(format!("env handle `{db_url_env}` is not set (secret values are never recorded)"));
        };
        let mut command = Command::new("psql");
        command
            .arg(&url)
            .arg("-c")
            .arg("select 1")
            .arg("-tA")
            .arg("-v")
            .arg("ON_ERROR_STOP=1");
        let output = command
            .output()
            .map_err(|error| format!("cannot spawn `psql`: {error}"))?;
        if output.status.success() {
            Ok(())
        } else {
            // Never include stderr: it may echo the connection string.
            Err(format!("`psql select 1` failed with status {}", output.status))
        }
    }

    fn tcp_connect(&self, host: &str, port: u16, timeout_ms: u64) -> Result<(), String> {
        tcp_connect_once(host, port, timeout_ms)
    }
}

/// Shared read-only TCP connect probe (preflight checks and stage readiness
/// layers). Connectivity only; never an LLM call.
pub fn tcp_connect_once(host: &str, port: u16, timeout_ms: u64) -> Result<(), String> {
    let address = format!("{host}:{port}");
    let resolved = address
        .to_socket_addrs()
        .map_err(|error| format!("cannot resolve `{address}`: {error}"))?
        .collect::<Vec<_>>();
    let mut last_error = format!("`{address}` did not resolve to any address");
    for socket in resolved {
        match TcpStream::connect_timeout(&socket, Duration::from_millis(timeout_ms.max(1))) {
            Ok(_) => return Ok(()),
            Err(error) => last_error = format!("connect {socket}: {error}"),
        }
    }
    Err(last_error)
}

/// Deterministic fixture executor: every probe succeeds by default and tests
/// override exactly what they need.
#[derive(Debug, Clone)]
pub struct FixtureExecutor {
    pub porcelain: Result<String, String>,
    pub head: Result<String, String>,
    pub missing_commands: BTreeSet<String>,
    pub cargo_metadata: Result<String, String>,
    pub gcp_describe: Result<String, String>,
    pub ssh: Result<(), String>,
    pub db: Result<(), String>,
    pub tcp: Result<(), String>,
}

impl FixtureExecutor {
    pub fn passing() -> Self {
        Self {
            porcelain: Ok(String::new()),
            head: Ok("0123456789abcdef0123456789abcdef01234567".to_owned()),
            missing_commands: BTreeSet::new(),
            cargo_metadata: Ok("{\"packages\":[]}".to_owned()),
            gcp_describe: Ok("{\"status\":\"RUNNING\"}".to_owned()),
            ssh: Ok(()),
            db: Ok(()),
            tcp: Ok(()),
        }
    }
}

impl PreflightExecutor for FixtureExecutor {
    fn kind(&self) -> &'static str {
        "fixture"
    }

    fn git_porcelain(&self, _root: &Path) -> Result<String, String> {
        self.porcelain.clone()
    }

    fn git_head(&self, _root: &Path) -> Result<String, String> {
        self.head.clone()
    }

    fn command_available(&self, name: &str) -> Result<bool, String> {
        Ok(!self.missing_commands.contains(name))
    }

    fn cargo_metadata(&self, _agent_root: &Path) -> Result<String, String> {
        self.cargo_metadata.clone()
    }

    fn gcp_describe_instance(&self, _target: &GcpTarget) -> Result<String, String> {
        self.gcp_describe.clone()
    }

    fn ssh_batch_true(&self, _host: &str, _ssh_ms: u64) -> Result<(), String> {
        self.ssh.clone()
    }

    fn db_select_one(&self, _db_url_env: &str) -> Result<(), String> {
        self.db.clone()
    }

    fn tcp_connect(&self, _host: &str, _port: u16, _timeout_ms: u64) -> Result<(), String> {
        self.tcp.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "krw-agent-deploy-executor-test-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn real_command_probe_fails_closed_with_empty_path() {
        let empty_path = temp_dir("empty-path");
        let executor = RealCommandExecutor { path_override: Some(empty_path) };
        assert!(!executor.command_available("git").unwrap());
    }

    #[test]
    fn real_tcp_connect_detects_open_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let executor = RealCommandExecutor::new();
        assert!(executor.tcp_connect("127.0.0.1", port, 2_000).is_ok());
        assert!(executor.tcp_connect("127.0.0.1", 1, 200).is_err());
    }

    #[test]
    fn real_git_executor_reports_head_of_this_repository() {
        let executor = RealCommandExecutor::new();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
        let head = executor.git_head(&root).unwrap();
        assert_eq!(head.len(), 40, "HEAD must be a 40-char sha1, got `{head}`");
    }

    #[test]
    fn fixture_executor_defaults_pass() {
        let executor = FixtureExecutor::passing();
        assert_eq!(executor.kind(), "fixture");
        assert!(executor.git_porcelain(Path::new("/x")).unwrap().is_empty());
        assert!(executor.command_available("docker").unwrap());
    }
}
