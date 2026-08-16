//! Real deployment stage runners 3-12 (doc/refetoring/05 §controller).
//!
//! Every world interaction of a mutating stage goes through the
//! [`StageExecutor`] trait. Commands are described as structured
//! [`StageCommand`] records — `{stage, id, argv, cwd, env_keys_used,
//! timeout_ms}` — so tests assert the exact argv without executing anything.
//! Secret values never appear in records: argv elements keep the
//! `<env:NAME>` placeholder form and `env_keys_used` lists env var NAMES
//! only. The [`RealStageExecutor`] resolves placeholders against the
//! operator runtime env file and sources that file (`set -a; . file`) into
//! full-envelope packaging subprocesses; the [`FixtureStageExecutor`]
//! records every spec for deterministic tests.
//!
//! Stage layout (migration ordering rules 3-4 of the 05 doc):
//!
//! | # | stage | executor command ids |
//! |---|---|---|
//! | 3 | `build` | `build.dual-provider-bundles` |
//! | 4 | `seal` | `seal.prepare-production-candidate`, `seal.sign-release-authorization`, `seal.seal-production-candidate`, `seal.finalize-dual-release` (+ trust-registry active key resolution, pure) |
//! | 5 | `frontend_image_prepare` | `frontend-image.resolve-commit`, `frontend-image.archive-source`, `frontend-image.descriptor-copy` (local SOURCE preparation only; the image is built on the VM in stage 9) |
//! | 6 | `migrations` | `migrations.db-push-dry-run` (read-only gate) |
//! | 7 | `admission_close` | `admission.read-previous`, `admission.close`, `admission.verify-closed`, then `migrations.db-push` and `migrations.abi-verify-procedure-N`/`-column-N` |
//! | 8 | `local_activation` | `activation.agentd-stage`, `activation.capabilityd-activate`, `activation.agentd-activate` |
//! | 9 | `remote_activation` | `ship.scp-front-archive`, `ship.scp-agent-descriptor`, `ship.remote-prepare` (remote build + candidate preflight), `remote.candidate-abi`, `remote.web-up`, `readiness.remote-web-healthz` (Skipped for local-only targets) |
//! | 10 | `deep_readiness` | `readiness.daemon-metrics`, `readiness.mcp-tcp-N` (TCP probes), `readiness.mcp-ready-N`, `readiness.web-deep`, `readiness.db-heartbeat` |
//! | 11 | `admission_open` | `admission.open`, `readiness.admission-open-verify` |
//! | 12 | `terminal_success_receipt` | receipt only |
//!
//! Failure policy: any stage failure stops the walk, writes the stage
//! receipt (status `fail`) and the terminal failure receipt, and exits 1.
//! No rollback commands exist anywhere in this module; recovery is
//! fix-forward through a fresh run. Local-only targets (no `ssh_host`) skip
//! the remote stage and run admission against the local gateway stack.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::config::DeployConfig;
use crate::hashing::{sha256_file, UNAVAILABLE_HASH};
use crate::receipts::{
    DeployArtifacts, SkippedRecord, StageCommandRecord, StageReceipt, StageReceiptStatus,
    TcpProbeRecord, TerminalAdmission,
};
use crate::stages::{RunContext, STAGE_TABLE};
use crate::target::TargetFile;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Marker env-key entry meaning "the whole runtime env file is exported into
/// the child process" (legacy `set -a; . <runtime-env>` packaging behavior).
pub const RUNTIME_ENV_ALL: &str = "<runtime-env:all>";

pub const ADMISSION_ENV_KEY: &str = "KRW_AGENT_ADMISSION_MODE";
pub const LOCAL_INSTALL_ROOT_ENV_KEY: &str = "KRW_AGENT_LOCAL_INSTALL_ROOT";
pub const METRICS_PORT_ENV_KEY: &str = "KRW_AGENT_METRICS_PORT";
pub const INTERNAL_API_KEY_ENV_KEY: &str = "INTERNAL_API_KEY";

/// Installer default from `install-local-mac-agentd-release.sh` usage.
pub const DEFAULT_METRICS_PORT: &str = "15520";

pub const AUTHORIZATION_TTL_SECONDS: u64 = 2_592_000; // 30 days (legacy default)
pub const RUNTIME_VERSION: &str = "0.1.0";
pub const KERNEL_VERSION: &str = "0.1.0";

pub const BUILD_TIMEOUT_MS: u64 = 3_600_000;
pub const SEAL_TIMEOUT_MS: u64 = 1_800_000;
pub const DB_DRY_RUN_TIMEOUT_MS: u64 = 600_000;
pub const DB_PUSH_TIMEOUT_MS: u64 = 1_800_000;
pub const SCHEMA_SMOKE_TIMEOUT_MS: u64 = 600_000;
pub const SEALED_ABI_TIMEOUT_MS: u64 = 300_000;
pub const PSQL_TIMEOUT_MS: u64 = 120_000;
pub const INSTALL_TIMEOUT_MS: u64 = 1_800_000;
pub const SSH_QUICK_TIMEOUT_MS: u64 = 120_000;
pub const SSH_COMPOSE_TIMEOUT_MS: u64 = 900_000;
pub const SSH_PREPARE_TIMEOUT_MS: u64 = 1_800_000;
pub const SCP_TIMEOUT_MS: u64 = 1_800_000;
pub const SCP_MAX_ATTEMPTS: u32 = 3;

/// Local ship directory under the run's release output tree.
pub const SHIP_DIR_NAME: &str = "ship";

/// Default tenant partition from the legacy remote prepare (a stable product
/// partition, never a user identity or release id).
pub const DEFAULT_TENANT_ID: &str = "krw-ontology-prod";

/// Ordered database URL env-key candidates the legacy script falls through
/// (`KRW_AGENT_DATABASE_URL`, `AGENT_V1_DATABASE_URL`,
/// `AGENT_V1_OUTBOX_DATABASE_URL`, `AGENT_QUEUE_LISTEN_DATABASE_URL`).
pub const AGENT_DB_URL_ENV_KEYS: [&str; 4] = [
    "KRW_AGENT_DATABASE_URL",
    "AGENT_V1_DATABASE_URL",
    "AGENT_V1_OUTBOX_DATABASE_URL",
    "AGENT_QUEUE_LISTEN_DATABASE_URL",
];

/// Runtime-env key pointing at the operator-owned Rust daemon envelope.
pub const RUNTIME_ENV_FILE_ENV_KEY: &str = "KRW_AGENT_RUNTIME_ENV_FILE";
/// Optional staged env candidate activated forward before the daemon flips.
pub const RUNTIME_ENV_CANDIDATE_ENV_KEY: &str = "KRW_AGENT_RUNTIME_ENV_CANDIDATE";
pub const TENANT_ID_ENV_KEY: &str = "KRW_AGENT_TENANT_ID";

/// Readiness probes are retried by the real executor until `timeout_ms`.
const READINESS_ID_PREFIX: &str = "readiness.";
const READINESS_POLL_INTERVAL_MS: u64 = 2_000;

pub const LOCAL_ONLY_SKIP_REASON: &str =
    "local-only target: no gcp block and no ssh_host recorded; remote activation skipped";

/// Local gateway stack endpoints (frontend compose runs on the same host).
const LOCAL_WEB_BASE_URL: &str = "http://127.0.0.1:3000";

pub const PROCEDURE_ABI_PREFIX: &str = "migrations.abi-verify-procedure-";
pub const COLUMN_ABI_PREFIX: &str = "migrations.abi-verify-column-";

// ---------------------------------------------------------------------------
// Command records and the executor trait
// ---------------------------------------------------------------------------

/// One structured world-command. `argv` may contain `<env:NAME>` placeholder
/// elements (or embedded substrings) that the real executor resolves from
/// the runtime env file — the placeholder itself is what gets recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageCommand {
    pub stage: &'static str,
    pub id: String,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    /// Env var NAMES the command consumes; the special [`RUNTIME_ENV_ALL`]
    /// entry means the full runtime env file is sourced into the child.
    pub env_keys_used: Vec<String>,
    pub timeout_ms: u64,
}

impl StageCommand {
    pub fn new(
        stage: &'static str,
        id: &str,
        argv: Vec<String>,
        cwd: &Path,
        env_keys_used: Vec<String>,
        timeout_ms: u64,
    ) -> Self {
        Self {
            stage,
            id: id.to_owned(),
            argv,
            cwd: cwd.to_path_buf(),
            env_keys_used,
            timeout_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageCommandOutcome {
    /// Trimmed stdout. Stderr is never captured into outcomes or receipts.
    pub stdout: String,
}

/// TCP connectivity probe record (MCP readiness layer 1; never LLM calls).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TcpProbe {
    pub stage: &'static str,
    pub id: String,
    pub host: String,
    pub port: u16,
    pub timeout_ms: u64,
}

/// Executor for mutating-stage world interaction, modeled on the preflight
/// `PreflightExecutor`. `git_head` exists for the preflight-receipt drift
/// guard re-verification before mutating stages.
pub trait StageExecutor: std::fmt::Debug {
    /// Stable executor label recorded in receipts (`real`, `fixture`).
    fn kind(&self) -> &'static str;

    /// Execute one structured command. `env_file` is the operator runtime
    /// env file the command's env keys resolve from; resolved values only
    /// ever flow into the child process, never into errors or receipts.
    fn execute(&self, env_file: &Path, command: &StageCommand) -> Result<StageCommandOutcome, String>;

    /// `git rev-parse HEAD` re-read for the drift guard.
    fn git_head(&self, root: &Path) -> Result<String, String>;

    /// TCP connect probe (bounded by `timeout_ms`).
    fn tcp_connect(&self, probe: &TcpProbe) -> Result<(), String>;
}

/// Build an unresolved `<env:NAME>` argv placeholder element.
pub fn env_placeholder(name: &str) -> String {
    format!("<env:{name}>")
}

// ---------------------------------------------------------------------------
// Runtime env file helpers (values never logged)
// ---------------------------------------------------------------------------

/// Read one KEY=VALUE from an env file. Last occurrence wins (legacy
/// `value_of ... | tail -n 1` semantics); surrounding double then single
/// quotes are stripped. Errors name the KEY only, never a value.
pub fn runtime_env_value(env_file: &Path, key: &str) -> Result<String, String> {
    let text = std::fs::read_to_string(env_file).map_err(|error| format!("{}: {error}", env_file.display()))?;
    let mut found: Option<String> = None;
    for line in text.lines() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        if let Some((candidate, value)) = line.split_once('=') {
            if candidate.trim_start_matches("export ").trim() == key {
                let cleaned = value.trim().trim_matches('"').trim_matches('\'').to_owned();
                found = Some(cleaned);
            }
        }
    }
    found.ok_or_else(|| format!("env key `{key}` not present in {}", env_file.display()))
}

/// KEY NAMES present in an env file (presence probe only).
pub fn runtime_env_has_key(env_file: &Path, key: &str) -> bool {
    runtime_env_value(env_file, key).is_ok()
}

/// Upsert one KEY=VALUE into an env file with the legacy awk semantics:
/// the first matching assignment is replaced in place, later duplicates are
/// dropped, and the key is appended when absent. Values are never returned.
pub fn upsert_env_value(env_file: &Path, key: &str, value: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(env_file).map_err(|error| format!("{}: {error}", env_file.display()))?;
    let mut output = String::new();
    let mut seen = false;
    for line in text.lines() {
        if let Some((candidate, _)) = line.split_once('=') {
            if candidate == key {
                // First matching assignment is replaced in place; later
                // duplicates are dropped (legacy awk `next` semantics).
                if !seen {
                    output.push_str(key);
                    output.push('=');
                    output.push_str(value);
                    output.push('\n');
                    seen = true;
                }
                continue;
            }
        }
        output.push_str(line);
        output.push('\n');
    }
    if !seen {
        output.push_str(key);
        output.push('=');
        output.push_str(value);
        output.push('\n');
    }
    std::fs::write(env_file, output).map_err(|error| format!("{}: {error}", env_file.display()))
}

// ---------------------------------------------------------------------------
// Real executor
// ---------------------------------------------------------------------------

/// Shell-out stage executor. Resolves `<env:NAME>` argv placeholders from
/// the runtime env file, sources the full file for `<runtime-env:all>`
/// commands (child env only), and retries `readiness.`-prefixed probes
/// until their timeout. Error messages carry the command id and exit status
/// only — never stdout/stderr contents (they may echo connection data).
#[derive(Debug, Clone)]
pub struct RealStageExecutor {
    pub path_override: Option<PathBuf>,
}

impl RealStageExecutor {
    pub fn new() -> Self {
        Self { path_override: None }
    }

    fn spawn(
        &self,
        env_file: &Path,
        command: &StageCommand,
        resolved_argv: &[String],
    ) -> Result<String, String> {
        let mut child = if command.env_keys_used.iter().any(|key| key == RUNTIME_ENV_ALL) {
            let mut shell = Command::new("/bin/sh");
            shell
                .arg("-c")
                // $0 = env file, "$@" = argv; `set -a` exports every sourced var.
                .arg("set -a; . \"$0\"; exec \"$@\"")
                .arg(env_file)
                .args(resolved_argv);
            shell
        } else {
            let mut plain = Command::new(&resolved_argv[0]);
            plain.args(&resolved_argv[1..]);
            if let Some(path) = &self.path_override {
                plain.env("PATH", path);
            }
            plain
        };
        child.current_dir(&command.cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        for key in &command.env_keys_used {
            if key == RUNTIME_ENV_ALL {
                continue;
            }
            let value = runtime_env_value(env_file, key)
                .map_err(|error| format!("command `{}` needs env key: {error}", command.id))?;
            child.env(key, value);
        }
        let mut spawned =
            child.spawn().map_err(|error| format!("cannot spawn `{}`: {error}", resolved_argv[0]))?;
        let mut stdout_pipe = spawned
            .stdout
            .take()
            .ok_or_else(|| format!("command `{}`: stdout pipe unavailable", command.id))?;
        let reader = std::thread::spawn(move || {
            let mut buffer = String::new();
            let _ = stdout_pipe.read_to_string(&mut buffer);
            buffer
        });
        let mut stderr_pipe = spawned
            .stderr
            .take()
            .ok_or_else(|| format!("command `{}`: stderr pipe unavailable", command.id))?;
        let stderr_reader = std::thread::spawn(move || {
            let mut buffer = String::new();
            let _ = stderr_pipe.read_to_string(&mut buffer);
            buffer
        });
        let deadline = Instant::now() + Duration::from_millis(command.timeout_ms.max(1));
        loop {
            match spawned.try_wait() {
                Ok(Some(status)) => {
                    let stdout = reader.join().unwrap_or_default();
                    if status.success() {
                        return Ok(stdout);
                    }
                    // Surface the child's own diagnostics on the operator's
                    // stderr. The content never enters receipts or records;
                    // scrubbing rules for stored artifacts are unchanged.
                    let stderr = stderr_reader.join().unwrap_or_default();
                    if !stderr.trim().is_empty() {
                        eprintln!("---- {}/{} stderr ----\n{}\n------------------------", command.stage, command.id, stderr.trim_end());
                    }
                    return Err(format!(
                        "command `{}` (stage `{}`) exited with status {status}",
                        command.id, command.stage
                    ));
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = spawned.kill();
                        let _ = spawned.wait();
                        return Err(format!(
                            "command `{}` (stage `{}`) timed out after {}ms",
                            command.id, command.stage, command.timeout_ms
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(error) => {
                    return Err(format!("command `{}` wait failed: {error}", command.id));
                }
            }
        }
    }

    fn resolve_placeholders(&self, env_file: &Path, command: &StageCommand) -> Result<Vec<String>, String> {
        let mut resolved = Vec::with_capacity(command.argv.len());
        for element in &command.argv {
            let mut value = element.clone();
            while let Some(start) = value.find("<env:") {
                let rest = &value[start + "<env:".len()..];
                let Some(name_len) = rest.find('>') else {
                    return Err(format!("command `{}` has a malformed env placeholder: {element}", command.id));
                };
                let name = &rest[..name_len];
                let resolved_value = runtime_env_value(env_file, name)
                    .map_err(|error| format!("command `{}` cannot resolve placeholder: {error}", command.id))?;
                value = format!("{}{}{}", &value[..start], resolved_value, &rest[name_len + 1..]);
            }
            resolved.push(value);
        }
        Ok(resolved)
    }
}

impl Default for RealStageExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl StageExecutor for RealStageExecutor {
    fn kind(&self) -> &'static str {
        "real"
    }

    fn execute(&self, env_file: &Path, command: &StageCommand) -> Result<StageCommandOutcome, String> {
        if command.argv.is_empty() {
            return Err(format!("command `{}` has an empty argv", command.id));
        }
        let resolved = self.resolve_placeholders(env_file, command)?;
        let started = Instant::now();
        loop {
            match self.spawn(env_file, command, &resolved) {
                Ok(stdout) => {
                    return Ok(StageCommandOutcome {
                        stdout: stdout.trim().to_owned(),
                    });
                }
                Err(error) => {
                    let pollable = command.id.starts_with(READINESS_ID_PREFIX);
                    let deadline = Duration::from_millis(command.timeout_ms.max(1));
                    if pollable && started.elapsed() + Duration::from_millis(READINESS_POLL_INTERVAL_MS) < deadline {
                        std::thread::sleep(Duration::from_millis(READINESS_POLL_INTERVAL_MS));
                        continue;
                    }
                    return Err(error);
                }
            }
        }
    }

    fn git_head(&self, root: &Path) -> Result<String, String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["rev-parse", "HEAD"])
            .output()
            .map_err(|error| format!("cannot spawn `git`: {error}"))?;
        if output.status.success() {
            String::from_utf8(output.stdout)
                .map(|stdout| stdout.trim().to_owned())
                .map_err(|error| format!("`git rev-parse` output not UTF-8: {error}"))
        } else {
            Err(format!("`git rev-parse HEAD` exited with status {}", output.status))
        }
    }

    fn tcp_connect(&self, probe: &TcpProbe) -> Result<(), String> {
        crate::executor::tcp_connect_once(&probe.host, probe.port, probe.timeout_ms)
    }
}

// ---------------------------------------------------------------------------
// Fixture executor
// ---------------------------------------------------------------------------

/// Deterministic fixture stage executor: records every command spec and TCP
/// probe, fails exactly the command/probe ids a test injects, and (with
/// `side_effects`) materializes the sealed-bundle marker files a real
/// `build_dual_provider_release.sh` run would leave behind. It learns the
/// run's release id, provider, and descriptor hash from the observed argv
/// and side-effect files so canned healthz/heartbeat outputs validate.
#[derive(Debug, Clone)]
pub struct FixtureStageExecutor {
    /// Every command spec seen, in order (assert exact argv here).
    pub commands: std::cell::RefCell<Vec<StageCommand>>,
    /// Every TCP probe seen, in order.
    pub probes: std::cell::RefCell<Vec<TcpProbe>>,
    /// Injected failures keyed by command id.
    pub failures: BTreeMap<String, String>,
    /// Injected TCP probe failures keyed by probe id.
    pub probe_failures: BTreeMap<String, String>,
    /// Canned stdout overrides by command id (defaults otherwise).
    pub outputs: BTreeMap<String, String>,
    /// Create build side-effect marker files (default true).
    pub side_effects: bool,
    /// `git_head` result for the drift guard.
    pub head: String,
    pub head_error: Option<String>,
    learned: std::cell::RefCell<FixtureLearned>,
}

#[derive(Debug, Clone, Default)]
struct FixtureLearned {
    release_id: Option<String>,
    output_dir: Option<PathBuf>,
    provider: Option<String>,
}

impl FixtureStageExecutor {
    pub fn passing() -> Self {
        Self {
            commands: std::cell::RefCell::new(Vec::new()),
            probes: std::cell::RefCell::new(Vec::new()),
            failures: BTreeMap::new(),
            probe_failures: BTreeMap::new(),
            outputs: BTreeMap::new(),
            side_effects: true,
            head: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            head_error: None,
            learned: std::cell::RefCell::new(FixtureLearned::default()),
        }
    }

    /// Observed command ids in execution order.
    pub fn command_ids(&self) -> Vec<String> {
        self.commands.borrow().iter().map(|command| command.id.clone()).collect()
    }

    fn learn_from(&self, command: &StageCommand) {
        let mut learned = self.learned.borrow_mut();
        if learned.output_dir.is_none() {
            if let Some(position) = command.argv.iter().position(|flag| flag == "--output-root") {
                if let Some(dir) = command.argv.get(position + 1) {
                    learned.output_dir = Some(PathBuf::from(dir));
                    learned.release_id =
                        PathBuf::from(dir).file_name().map(|name| name.to_string_lossy().to_string());
                }
            }
        }
        if learned.provider.is_none() && command.id == "seal.prepare-production-candidate" {
            if let Some(position) = command.argv.iter().position(|flag| flag == "--provider") {
                if let Some(provider) = command.argv.get(position + 1) {
                    learned.provider = Some(provider.clone());
                }
            }
        }
    }

    fn simulate_side_effects(&self, command: &StageCommand) {
        if !self.side_effects {
            return;
        }
        let learned = self.learned.borrow();
        let Some(dir) = learned.output_dir.clone() else {
            return;
        };
        if command.id == "build.dual-provider-bundles" {
            for provider in ["glm", "deepseek"] {
                let bundle = dir.join(provider);
                let _ = std::fs::create_dir_all(&bundle);
                let model = if provider == "glm" { "glm-5.3" } else { "deepseek-v4-flash" };
                // The raw build emits per-bundle manifests and the dual
                // index; the sealed public descriptor appears only after the
                // seal stage's prepare step (mirrors the real scripts).
                let _ = std::fs::write(
                    bundle.join("release-manifest.json"),
                    format!("{{\"provider\":\"{provider}\",\"model\":\"{model}\"}}\n"),
                );
                let _ = std::fs::write(dir.join("dual-release-index.json"), "{\"providers\":[\"glm\",\"deepseek\"]}\n");
                let _ = std::fs::create_dir_all(bundle.join("packaging/launchd"));
                let _ = std::fs::write(bundle.join("apply_migrations.sh"), "#!/bin/sh\nexit 0\n");
                for launcher in [
                    "install-local-mac-agentd-release.sh",
                    "install-local-mac-capabilityd-release.sh",
                    "krw-agentd-start-local",
                ] {
                    let _ = std::fs::write(bundle.join("packaging/launchd").join(launcher), "#!/bin/sh\nexit 0\n");
                }
            }
            return;
        }
        let Some(provider) = learned.provider.clone() else {
            return;
        };
        match command.id.as_str() {
            // prepare_production_candidate.sh applies the operator endpoint
            // bindings to the bundle named by its own --provider argument and
            // emits the public descriptor (both providers seal; the id may
            // carry a -<provider> suffix for the non-selected bundle).
            id if id == "seal.prepare-production-candidate"
                || (id.starts_with("seal.prepare-production-candidate-") && id.len() > "seal.prepare-production-candidate-".len()) =>
            {
                let argv_provider = command
                    .argv
                    .iter()
                    .position(|flag| flag == "--provider")
                    .and_then(|position| command.argv.get(position + 1))
                    .cloned()
                    .unwrap_or_else(|| provider.clone());
                let model = if argv_provider == "glm" { "glm-5.3" } else { "deepseek-v4-flash" };
                let _ = std::fs::write(
                    dir.join(&argv_provider).join("public-release.json"),
                    format!(
                        "{{\"entries\":[{{\"execution\":{{\"resolved_model\":\"{model}\"}}}}],\"release_set_hash\":\"sha256:{}\",\"provider\":\"{argv_provider}\"}}\n",
                        "c".repeat(64),
                    ),
                );
            }
            // The archive command produces the deterministic source tarball
            // into the run's RECEIPT ship workspace (the launchd installer
            // requires the release root to hold only the sealed bundles).
            "frontend-image.archive-source" => {
                if let Some(path) = extract_ship_file_path(&command.argv[2], "front.tar.gz") {
                    if let Some(parent) = std::path::Path::new(&path).parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let _ = std::fs::write(&path, "fixture-front-archive\n");
                }
            }
            // The descriptor copy must be byte-identical to the sealed
            // descriptor so the controller-side sha256 gate passes.
            "frontend-image.descriptor-copy" => {
                let sealed = dir.join(&provider).join("public-release.json");
                if let (Ok(bytes), Some(path)) = (
                    std::fs::read(&sealed),
                    extract_ship_file_path(&command.argv[2], "public-release.json"),
                ) {
                    if let Some(parent) = std::path::Path::new(&path).parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let _ = std::fs::write(&path, bytes);
                }
            }
            _ => {}
        }
    }

    fn canned_output(&self, command: &StageCommand) -> String {
        if let Some(output) = self.outputs.get(&command.id) {
            return output.clone();
        }
        let learned = self.learned.borrow();
        let release_id = learned
            .release_id
            .clone()
            .unwrap_or_else(|| "00000000T000000Z-00000000".to_owned());
        // The checked-in fixture contract pins exactly
        // `agent_v1.enqueue_run(jsonb)`; tests with other ABI lists override
        // `outputs`.
        if command.id.starts_with(PROCEDURE_ABI_PREFIX) {
            return "1|jsonb".to_owned();
        }
        if command.id.starts_with(COLUMN_ABI_PREFIX) {
            return "1".to_owned();
        }
        if command.id.starts_with("readiness.public-healthz-") {
            return format!(r#"{{"status":"ok","deployment_id":"{release_id}"}}"#);
        }
        match command.id.as_str() {
            "admission.read-previous" => "open".to_owned(),
            "admission.verify-closed-local" => r#"{"status":"ok"}"#.to_owned(),
            "frontend-image.resolve-commit" => self.head.clone(),
            "ship.remote-prepare" => format!(
                "CANDIDATE_IMAGE=sha256:{}\nPrepared fast VM candidate: {release_id}\n",
                "a".repeat(64)
            ),
            "remote.candidate-abi" => {
                r#"{"status":"ok","check":"gcp_agent_v1_candidate_abi","queue_and_outbox_abi":"verified"}"#.to_owned()
            }
            "migrations.db-push" | "migrations.db-push-include-all" => {
                "Applying migration 0001_agent_v1\nApplying migration 0022_daemon_mcp_readiness\n".to_owned()
            }
            "readiness.remote-web-healthz" | "readiness.web-deep" => format!(
                r#"{{"status":"ok","deployment_id":"{release_id}","checks":{{"agent_v1":{{"admission":"closed"}}}}}}"#
            ),
            "readiness.admission-open-verify" | "readiness.admission-open-verify-local" => format!(
                r#"{{"status":"ok","deployment_id":"{release_id}","checks":{{"agent_v1":{{"admission":"open"}}}}}}"#
            ),
            "readiness.db-heartbeat" => {
                let provider = learned.provider.clone().unwrap_or_else(|| "deepseek".to_owned());
                let descriptor = learned
                    .output_dir
                    .as_ref()
                    .and_then(|dir| sha256_file(&dir.join(&provider).join("public-release.json")).ok())
                    .unwrap_or_else(|| format!("sha256:{}", "0".repeat(64)));
                format!("{provider}|{descriptor}|sha256:{}|t", "b".repeat(64))
            }
            _ => String::new(),
        }
    }
}

impl StageExecutor for FixtureStageExecutor {
    fn kind(&self) -> &'static str {
        "fixture"
    }

    fn execute(&self, _env_file: &Path, command: &StageCommand) -> Result<StageCommandOutcome, String> {
        self.commands.borrow_mut().push(command.clone());
        self.learn_from(command);
        if let Some(reason) = self.failures.get(&command.id) {
            return Err(reason.clone());
        }
        self.simulate_side_effects(command);
        Ok(StageCommandOutcome {
            stdout: self.canned_output(command),
        })
    }

    fn git_head(&self, _root: &Path) -> Result<String, String> {
        match &self.head_error {
            Some(error) => Err(error.clone()),
            None => Ok(self.head.clone()),
        }
    }

    fn tcp_connect(&self, probe: &TcpProbe) -> Result<(), String> {
        self.probes.borrow_mut().push(probe.clone());
        if let Some(reason) = self.probe_failures.get(&probe.id) {
            return Err(reason.clone());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Trust-registry active key resolution (pure)
// ---------------------------------------------------------------------------

/// Resolve the exactly-one active (non-revoked, valid now) signing key id
/// from a canonical `ReleaseTrustRegistryV1`, porting the legacy Python
/// gate in `build_sealed_dual_provider_release.sh`.
pub fn resolve_active_key_id(registry_bytes: &[u8], now_unix_seconds: u64) -> Result<String, String> {
    let registry = krw_agent_release_authorization::parse_canonical_trust_registry(registry_bytes)
        .map_err(|error| format!("trust registry invalid: {error}"))?;
    let active: Vec<&krw_agent_release_authorization::ReleaseTrustKeyV1> = registry
        .keys
        .iter()
        .filter(|key| !key.revoked && key.not_before_unix_seconds <= now_unix_seconds && now_unix_seconds <= key.not_after_unix_seconds)
        .collect();
    match active.len() {
        1 => Ok(active[0].key_id.clone()),
        0 => Err("trust registry has no active signing key valid now (exactly one required)".to_owned()),
        count => Err(format!("trust registry has {count} active signing keys (exactly one required)")),
    }
}

// ---------------------------------------------------------------------------
// Migration output / ABI / healthz parsing (pure)
// ---------------------------------------------------------------------------

/// Migration names parsed from `supabase db push` output lines of the form
/// `Applying migration <name>...`.
pub fn parse_applied_migrations(stdout: &str) -> Vec<String> {
    const PREFIX: &str = "Applying migration ";
    stdout
        .lines()
        .filter_map(|line| {
            line.find(PREFIX).map(|position| {
                line[position + PREFIX.len()..]
                    .trim()
                    .trim_end_matches('.')
                    .trim()
                    .to_owned()
            })
        })
        .filter(|name| !name.is_empty())
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcedureAbi {
    pub schema: String,
    pub name: String,
    pub arg_list: String,
}

/// Parse a contract entry like `agent_v1.enqueue_run(jsonb)`.
pub fn parse_procedure_entry(entry: &str) -> Result<ProcedureAbi, String> {
    let entry = entry.trim();
    if entry.contains('\'') || entry.contains('"') {
        return Err(format!("procedure entry `{entry}` must not contain quote characters"));
    }
    if !entry.ends_with(')') {
        return Err(format!("procedure entry `{entry}` must be schema.name(args)"));
    }
    let (path, args) = entry.split_once('(').ok_or_else(|| format!("procedure entry `{entry}` must be schema.name(args)"))?;
    let arg_list = args.trim_end_matches(')').trim().to_owned();
    let (schema, name) =
        path.trim().rsplit_once('.').ok_or_else(|| format!("procedure entry `{entry}` must be schema.name(args)"))?;
    if schema.is_empty() || name.is_empty() {
        return Err(format!("procedure entry `{entry}` must be schema.name(args)"));
    }
    Ok(ProcedureAbi {
        schema: schema.to_owned(),
        name: name.to_owned(),
        arg_list,
    })
}

impl ProcedureAbi {
    /// Existence + exact IN-argument shape, one `psql -tA` query.
    pub fn sql(&self) -> String {
        format!(
            "select (select count(*) from information_schema.routines where routine_schema = '{}' and routine_name = '{}' and routine_type = 'FUNCTION') || '|' || coalesce((select string_agg(udt_name, ',' order by ordinal_position) from information_schema.parameters where specific_schema = '{}' and specific_name = '{}' and parameter_mode = 'IN'), '')",
            self.schema, self.name, self.schema, self.name
        )
    }

    pub fn expected_output(&self) -> String {
        format!("1|{}", self.arg_list)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnAbi {
    pub schema: String,
    pub table: String,
    pub column: String,
}

/// Parse a contract entry like `agent_v1_daemon_heartbeats.mcp_ready`
/// (2-part defaults to the `public` schema) or `public.t.c` (3-part).
pub fn parse_column_entry(entry: &str) -> Result<ColumnAbi, String> {
    let entry = entry.trim();
    if entry.contains('\'') || entry.contains('"') {
        return Err(format!("column entry `{entry}` must not contain quote characters"));
    }
    let parts: Vec<&str> = entry.split('.').collect();
    let (schema, table, column) = match parts.as_slice() {
        [table, column] => ("public".to_owned(), (*table).to_owned(), (*column).to_owned()),
        [schema, table, column] => ((*schema).to_owned(), (*table).to_owned(), (*column).to_owned()),
        _ => return Err(format!("column entry `{entry}` must be table.column or schema.table.column")),
    };
    if table.is_empty() || column.is_empty() {
        return Err(format!("column entry `{entry}` has empty table or column"));
    }
    Ok(ColumnAbi {
        schema: schema.to_owned(),
        table,
        column,
    })
}

impl ColumnAbi {
    pub fn sql(&self) -> String {
        format!(
            "select count(*) from information_schema.columns where table_schema = '{}' and table_name = '{}' and column_name = '{}'",
            self.schema, self.table, self.column
        )
    }

    pub fn expected_output(&self) -> String {
        "1".to_owned()
    }
}

/// Deep healthz observation parsed from the probe output. `None` when the
/// payload did not return JSON (the probe's own exit status already
/// enforced the contract in that case).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepHealthObservation {
    pub status: Option<String>,
    pub deployment_id: Option<String>,
    pub admission: Option<String>,
}

pub fn parse_deep_health(stdout: &str) -> Option<DeepHealthObservation> {
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    let string = |key: &str| value.get(key).and_then(serde_json::Value::as_str).map(str::to_owned);
    Some(DeepHealthObservation {
        status: string("status"),
        deployment_id: string("deployment_id"),
        admission: value
            .get("checks")
            .and_then(|checks| checks.get("agent_v1"))
            .and_then(|agent| agent.get("admission"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
    })
}

// ---------------------------------------------------------------------------
// Remote topology and ssh payloads
// ---------------------------------------------------------------------------

/// How remote shell commands reach the deploy target. The production
/// transport is `gcloud compute ssh` (legacy `run_remote_buffered_script`);
/// plain ssh survives only for targets that declare `ssh_host` WITHOUT a
/// gcp block. Payloads are identical across transports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteTransport {
    Gcloud {
        instance: String,
        project: String,
        zone: String,
    },
    Ssh {
        host: String,
    },
}

impl RemoteTransport {
    /// argv prefix in front of the shell payload (no payload element).
    fn argv_prefix(&self, ssh_ms: u64) -> Vec<String> {
        match self {
            Self::Gcloud { instance, project, zone } => vec![
                "gcloud".to_owned(),
                "compute".to_owned(),
                "ssh".to_owned(),
                instance.clone(),
                "--project".to_owned(),
                project.clone(),
                "--zone".to_owned(),
                zone.clone(),
                "--command".to_owned(),
            ],
            Self::Ssh { host } => {
                let connect_seconds = (ssh_ms / 1000).max(1);
                vec![
                    "ssh".to_owned(),
                    "-o".to_owned(),
                    "BatchMode=yes".to_owned(),
                    "-o".to_owned(),
                    format!("ConnectTimeout={connect_seconds}"),
                    host.clone(),
                ]
            }
        }
    }

    /// Human label for receipts and error messages (no secrets either way).
    pub fn label(&self) -> String {
        match self {
            Self::Gcloud { instance, project, zone } => {
                format!("gcloud compute ssh `{instance}` (project `{project}`, zone `{zone}`)")
            }
            Self::Ssh { host } => format!("ssh `{host}`"),
        }
    }

    /// `upload_archive` argv (without the local/remote path pair): the legacy
    /// gcloud scp flags keep long archives alive through flaky links.
    fn scp_argv_prefix(&self, ssh_ms: u64) -> Vec<String> {
        match self {
            Self::Gcloud { project, zone, .. } => vec![
                "gcloud".to_owned(),
                "compute".to_owned(),
                "scp".to_owned(),
                "--project".to_owned(),
                project.clone(),
                "--zone".to_owned(),
                zone.clone(),
                "--scp-flag=-oServerAliveInterval=15".to_owned(),
                "--scp-flag=-oServerAliveCountMax=8".to_owned(),
            ],
            Self::Ssh { .. } => {
                let connect_seconds = (ssh_ms / 1000).max(1);
                vec![
                    "scp".to_owned(),
                    "-o".to_owned(),
                    "BatchMode=yes".to_owned(),
                    "-o".to_owned(),
                    format!("ConnectTimeout={connect_seconds}"),
                ]
            }
        }
    }

    /// Remote destination expression `user@host:path` / `instance:path`.
    fn scp_destination(&self, remote_path: &str) -> String {
        match self {
            Self::Gcloud { instance, .. } => format!("{instance}:{remote_path}"),
            Self::Ssh { host } => format!("{host}:{remote_path}"),
        }
    }
}

/// Remote release stage directory (the legacy `REMOTE_STAGE`):
/// `<remote_front_dir>/.simple-deploy/releases/<release_id>`.
pub fn remote_stage_dir(topology: &RemoteTopology, release_id: &str) -> String {
    format!("{}/.simple-deploy/releases/{release_id}", topology.remote_front_dir)
}

/// Remote upload paths (legacy: `REMOTE_ARCHIVE_DIR=/tmp`).
pub fn remote_front_archive_path(release_id: &str) -> String {
    format!("/tmp/krw-front-{release_id}.tar.gz")
}

pub fn remote_agent_descriptor_path(release_id: &str) -> String {
    format!("/tmp/krw-agent-public-release-{release_id}.json")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteTopology {
    pub transport: RemoteTransport,
    pub remote_front_dir: String,
    pub compose_project: String,
}

/// Resolve the remote deployment topology and transport. Selection rule:
/// a `gcp` block selects the gcloud transport (production); otherwise an
/// `ssh_host` selects plain ssh; neither means a local-only target (`None`).
/// Any remote transport without `remote_front_dir` fails closed, and a gcp
/// block must carry non-empty project/zone/instance.
pub fn resolve_remote_topology(target: &TargetFile) -> Result<Option<RemoteTopology>, String> {
    let transport = if let Some(gcp) = &target.gcp {
        if gcp.project.is_empty() || gcp.zone.is_empty() || gcp.instance.is_empty() {
            return Err("target gcp block must record non-empty project, zone, and instance".to_owned());
        }
        RemoteTransport::Gcloud {
            instance: gcp.instance.clone(),
            project: gcp.project.clone(),
            zone: gcp.zone.clone(),
        }
    } else if let Some(host) = target.ssh_host.as_deref() {
        RemoteTransport::Ssh { host: host.to_owned() }
    } else {
        return Ok(None);
    };
    let remote_front_dir = target.remote_front_dir.as_deref().ok_or_else(|| {
        "target declares a remote transport but no remote_front_dir; remote admission/activation cannot run".to_owned()
    })?;
    let compose_project = target.compose_project.clone().unwrap_or_else(|| {
        PathBuf::from(remote_front_dir)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| remote_front_dir.to_owned())
    });
    Ok(Some(RemoteTopology {
        transport,
        remote_front_dir: remote_front_dir.to_owned(),
        compose_project,
    }))
}

/// Render a payload template by substituting `@TOKEN@` markers. Tokens keep
/// big legacy shell payloads auditable without `format!` brace escaping.
fn render(template: &str, values: &[(&str, &str)]) -> String {
    let mut payload = template.to_owned();
    for (token, value) in values {
        payload = payload.replace(&format!("@{token}@"), value);
    }
    payload
}

/// Single-quote a value for a POSIX shell string (legacy `'"'"'` idiom).
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Legacy `value_of` awk reader against the shell variable `$e`.
fn shell_value_of(key: &str) -> String {
    format!(
        "awk -F= -v key={key} '$0 !~ /^[[:space:]]*#/ && $1 == key {{ sub(/^[^=]*=/, \"\"); gsub(/^[[:space:]]+|[[:space:]]+$/, \"\"); gsub(/^\"|\"$/, \"\"); gsub(/^'\\''|'\\''$/, \"\"); print }}' \"$e\" | tail -n 1"
    )
}

/// Legacy `upsert_env_value` awk body against `$e`.
fn shell_upsert_snippet(key: &str, value: &str) -> String {
    format!(
        "t=$(mktemp \"${{e}}.XXXXXX\")\nawk -v key={key} -v value={value} '$0 !~ /^[[:space:]]*#/ && $0 ~ (\"^\" key \"=\") {{ if (!seen++) print key \"=\" value; next }} {{ print }} END {{ if (!seen) print key \"=\" value }}' \"$e\" > \"$t\" && chmod 600 \"$t\" && mv \"$t\" \"$e\""
    )
}

/// Shared remote-script helpers: `value_of KEY` and
/// `upsert_env_value FILE KEY VALUE` with the exact legacy awk semantics.
const REMOTE_ENV_HELPERS: &str = r#"value_of() {
  awk -F= -v key="$2" '$0 !~ /^[[:space:]]*#/ && $1 == key { sub(/^[^=]*=/, ""); gsub(/^[[:space:]]+|[[:space:]]+$/, ""); gsub(/^"|"$/, ""); gsub(/^'\''|'\''$/, ""); print }' "$1" | tail -n 1
}
upsert_env_value() {
  env_file=$1
  env_key=$2
  env_value=$3
  tmp_file=$(mktemp "${env_file}.XXXXXX")
  awk -v key="$env_key" -v value="$env_value" '$0 !~ /^[[:space:]]*#/ && $0 ~ ("^" key "=") { if (!seen++) print key "=" value; next } { print } END { if (!seen) print key "=" value }' "$env_file" > "$tmp_file"
  chmod 600 "$tmp_file"
  mv "$tmp_file" "$env_file"
}"#;

/// Resolve the ACTIVE release root (`r`) and its runtime env (`e`) exactly
/// like the legacy close/open helpers: the base front dir unless
/// `.simple-deploy/current` is a symlink to a release carrying runtime.env.
fn active_env_snippet(front_dir: &str) -> String {
    format!(
        "r='{front}'\ne='{front}/.env'\nif [ -L '{front}/.simple-deploy/current' ] && [ -f '{front}/.simple-deploy/current/runtime.env' ]; then r=$(cd '{front}/.simple-deploy/current' && pwd -P); e=\"$r/runtime.env\"; fi",
        front = front_dir
    )
}

/// Legacy `compose_active` prefix: compose against the ACTIVE release root
/// resolved by [`active_env_snippet`] (`$r`/`$e`), not the base front dir —
/// the base docker-compose.yml has no `agent-v1-outbox` service.
fn compose_active_prefix(topology: &RemoteTopology) -> String {
    format!(
        "FRONT_DIR=\"$r\" docker compose --project-name '{project}' --project-directory \"$r\" --env-file \"$e\" -f \"$r/docker-compose.yml\"",
        project = topology.compose_project,
    )
}

/// Read `KRW_AGENT_ADMISSION_MODE` from the active release runtime.env
/// (legacy `value_of` awk logic, `tail -n 1`).
pub fn payload_read_previous_admission(topology: &RemoteTopology) -> String {
    format!(
        "{}\nawk -F= -v key={key} '$0 !~ /^[[:space:]]*#/ && $1 == key {{ sub(/^[^=]*=/, \"\"); gsub(/^[[:space:]]+|[[:space:]]+$/, \"\"); print }}' \"$e\" | tail -n 1",
        active_env_snippet(&topology.remote_front_dir),
        key = ADMISSION_ENV_KEY,
    )
}

/// Upsert admission to `closed` and recreate web + outbox on the ACTIVE
/// release (legacy `run_remote_close_research_admission`, including its
/// backend-mode gate: a non-rust active release needs no admission change).
pub fn payload_close_admission(topology: &RemoteTopology) -> String {
    format!(
        "{env_probe}\n{mode_gate}\n{upsert}\n{compose} up -d --no-deps --force-recreate --wait web agent-v1-outbox",
        env_probe = active_env_snippet(&topology.remote_front_dir),
        mode_gate = format!(
            "if [ \"$( {value_of} )\" != \"rust\" ]; then echo \"Active web release does not use Rust Agent V1; no research admission change is needed.\"; exit 0; fi",
            value_of = shell_value_of("KRW_AGENT_BACKEND_MODE"),
        ),
        upsert = shell_upsert_snippet(ADMISSION_ENV_KEY, "closed"),
        // Legacy `compose_active` runs against the ACTIVE release root
        // (`.simple-deploy/current`), where the agent-v1-outbox service lives.
        compose = compose_active_prefix(topology),
    )
}

/// In-container `GET /api/healthz` expecting `status == "ok"` (legacy close
/// verification against the ACTIVE release).
pub fn payload_verify_healthz(topology: &RemoteTopology) -> String {
    format!(
        "{probe}\n{prefix} exec -T web node -e 'fetch(\"http://127.0.0.1:3000/api/healthz\").then(async r => {{ const v = await r.json().catch(() => null); if (!r.ok || v?.status !== \"ok\") process.exit(1); }}).catch(() => process.exit(1));'",
        probe = active_env_snippet(&topology.remote_front_dir),
        prefix = compose_active_prefix(topology),
    )
}

/// Full legacy `run_remote_activate` payload: retire GCP ontology sidecars,
/// promote the candidate image to the stable `local` tag, recreate the whole
/// service set, wait for in-container + public healthz at the new deployment
/// id, start the conditional market workers, and flip the `current` symlink.
pub fn payload_remote_web_up(topology: &RemoteTopology, release_id: &str, origins: &[String]) -> String {
    let origin_one = origins.first().cloned().unwrap_or_default();
    let origin_two = origins.get(1).cloned().unwrap_or_default();
    render(
        r#"set -e
RELEASE_ID='@RELEASE_ID@'
REMOTE_STAGE='@STAGE@'
REMOTE_FRONT_DIR='@FRONT_DIR@'
COMPOSE_PROJECT='@PROJECT@'
. "$REMOTE_STAGE/forward-activation.env"
SERVICES="web reverse-proxy agent-v1-outbox llm-gateway filings-mcp document-worker document-outbox-dispatcher filing-notification-worker filing-brief-worker billing-worker"
compose_stage() {
  FRONT_DIR="$REMOTE_STAGE" NEXT_PUBLIC_APP_VERSION="$RELEASE_ID" docker compose --project-name "$COMPOSE_PROJECT" --project-directory "$REMOTE_STAGE" --env-file "$REMOTE_STAGE/runtime.env" -f "$REMOTE_STAGE/docker-compose.yml" "$@"
}
runtime_value() {
  awk -F= -v key="$1" '$0 !~ /^[[:space:]]*#/ && $1 == key { sub(/^[^=]*=/, ""); gsub(/^[[:space:]]+|[[:space:]]+$/, ""); gsub(/^"|"$/, ""); gsub(/^'\''|'\''$/, ""); print }' "$REMOTE_STAGE/runtime.env" | tail -n 1
}
market_issue_x_ingestion_enabled() {
  value=$(runtime_value MARKET_ISSUE_X_INGESTION_ENABLED)
  case "$value" in 0|false|FALSE|off|OFF|no|NO) return 1 ;; *) return 0 ;; esac
}
market_issue_stream_enabled() {
  value=$(runtime_value MARKET_ISSUE_STREAM_ENABLED)
  case "$value" in 1|true|TRUE|on|ON|yes|YES) return 0 ;; *) return 1 ;; esac
}
matches_version() {
  payload=$(printf '%s' "$1" | tr -d '[:space:]')
  expected=$2
  case "$payload" in *'"status":"ok"'*) ;; *) return 1 ;; esac
  case "$payload" in *'"deployment_id":"'"$expected"'"'*) return 0 ;; *) return 1 ;; esac
}
legacy_sidecar_ids=$( {
  docker ps -aq --filter "label=com.docker.compose.project=$COMPOSE_PROJECT" --filter "label=com.docker.compose.service=agent-worker"
  docker ps -aq --filter "label=com.docker.compose.project=$COMPOSE_PROJECT" --filter "label=com.docker.compose.service=ontology-mcp"
} | awk 'NF && !seen[$0]++')
if [ -n "$legacy_sidecar_ids" ]; then
  echo "Removing retired GCP ontology sidecars."
  docker rm -f $legacy_sidecar_ids
fi
docker tag "$CANDIDATE_IMAGE" krw-ontology-front-web:local
compose_stage up -d --force-recreate --wait $SERVICES
web_ready=0
attempt=1
while [ "$attempt" -le 30 ]; do
  if compose_stage exec -T -e EXPECTED_RELEASE_ID="$RELEASE_ID" web node -e 'fetch("http://127.0.0.1:3000/api/healthz").then(async response=>{const value=await response.json(); if(!response.ok||value.status!=="ok"||value.deployment_id!==process.env.EXPECTED_RELEASE_ID) process.exit(1);}).catch(()=>process.exit(1));' >/dev/null 2>&1; then
    web_ready=1
    break
  fi
  sleep 2
  attempt=$((attempt + 1))
done
[ "$web_ready" = "1" ]
for origin in '@ORIGIN_ONE@' '@ORIGIN_TWO@'; do
  public_ready=0
  attempt=1
  while [ "$attempt" -le 30 ]; do
    health=$(curl --connect-timeout 3 --max-time 10 -fsS "$origin/api/healthz" 2>/dev/null || true)
    if matches_version "$health" "$RELEASE_ID"; then
      public_ready=1
      break
    fi
    sleep 2
    attempt=$((attempt + 1))
  done
  [ "$public_ready" = "1" ]
done
compose_stage up -d --force-recreate --wait market-web-source-worker
compose_stage exec -T market-web-source-worker node -e '
  const sources = JSON.parse(process.env.MARKET_ISSUE_WEB_SOURCES || "[]");
  console.log(JSON.stringify({ worker: "market-web-source-worker", configuredSources: Array.isArray(sources) ? sources.length : 0 }));
'
if market_issue_x_ingestion_enabled; then
  compose_stage up -d --force-recreate --wait market-issue-worker
  echo "Started X market-issue worker (token verified in pre-activation step)."
else
  compose_stage stop market-issue-worker || true
  echo "Skipped X market-issue worker because MARKET_ISSUE_X_INGESTION_ENABLED is disabled."
fi
if market_issue_stream_enabled; then
  compose_stage up -d --force-recreate --wait market-issue-stream-worker
  echo "Started X market-issue stream worker (token verified in pre-activation step)."
else
  compose_stage stop market-issue-stream-worker || true
  echo "Skipped X market-issue stream worker because MARKET_ISSUE_STREAM_ENABLED is disabled."
fi
compose_stage up -d --force-recreate --wait market-issue-enrichment-worker
rm -f "$REMOTE_FRONT_DIR/.simple-deploy/current"
ln -s "$REMOTE_STAGE" "$REMOTE_FRONT_DIR/.simple-deploy/current"
echo "Activated fast VM candidate: $RELEASE_ID""#,
        &[
            ("RELEASE_ID", release_id),
            ("STAGE", &remote_stage_dir(topology, release_id)),
            ("FRONT_DIR", &topology.remote_front_dir),
            ("PROJECT", &topology.compose_project),
            ("ORIGIN_ONE", &origin_one),
            ("ORIGIN_TWO", &origin_two),
        ],
    )
}

/// In-container healthz expecting `deployment_id == release id`; prints the
/// compact observation JSON for the controller to double-check. Retries on
/// a bounded budget: freshly recreated containers can fail the first probe.
pub fn payload_remote_web_healthz(topology: &RemoteTopology, release_id: &str) -> String {
    render(
        r#"set -eu
RELEASE_ID='@RELEASE_ID@'
REMOTE_STAGE='@STAGE@'
COMPOSE_PROJECT='@PROJECT@'
compose_stage() {
  FRONT_DIR="$REMOTE_STAGE" NEXT_PUBLIC_APP_VERSION="$RELEASE_ID" docker compose --project-name "$COMPOSE_PROJECT" --project-directory "$REMOTE_STAGE" --env-file "$REMOTE_STAGE/runtime.env" -f "$REMOTE_STAGE/docker-compose.yml" "$@"
}
probe() {
  compose_stage exec -T -e EXPECTED_RELEASE_ID="$RELEASE_ID" web node -e 'fetch("http://127.0.0.1:3000/api/healthz").then(async r => { const v = await r.json().catch(() => null); process.stdout.write(JSON.stringify({ status: v && v.status, deployment_id: v && v.deployment_id })); if (!r.ok || !v || v.status !== "ok" || v.deployment_id !== process.env.EXPECTED_RELEASE_ID) process.exit(1); }).catch(() => process.exit(1));'
}
observed=
attempt=1
while [ "$attempt" -le 20 ]; do
  if observed=$(probe 2>/dev/null); then
    printf '%s\n' "$observed"
    exit 0
  fi
  sleep 2
  attempt=$((attempt + 1))
done
echo "In-container web healthz did not report the release id after retries." >&2
exit 1"#,
        &[
            ("RELEASE_ID", release_id),
            ("STAGE", &remote_stage_dir(topology, release_id)),
            ("PROJECT", &topology.compose_project),
        ],
    )
}

/// Deep health with the internal-key headers the production proxy supplies;
/// enforces `deployment_id == release id` and prints the observation JSON.
/// Retries on a bounded budget like the healthz probe.
pub fn payload_remote_web_deep(topology: &RemoteTopology, release_id: &str) -> String {
    render(
        r#"set -eu
RELEASE_ID='@RELEASE_ID@'
REMOTE_STAGE='@STAGE@'
COMPOSE_PROJECT='@PROJECT@'
compose_stage() {
  FRONT_DIR="$REMOTE_STAGE" NEXT_PUBLIC_APP_VERSION="$RELEASE_ID" docker compose --project-name "$COMPOSE_PROJECT" --project-directory "$REMOTE_STAGE" --env-file "$REMOTE_STAGE/runtime.env" -f "$REMOTE_STAGE/docker-compose.yml" "$@"
}
probe() {
  compose_stage exec -T web node -e 'const headers = { "x-internal-key": process.env.INTERNAL_API_KEY || "", "x-krw-client-ip": "127.0.0.1" }; fetch("http://127.0.0.1:3000/api/healthz/deep", { headers }).then(async r => { let v = null; try { v = await r.json(); } catch {} process.stdout.write(JSON.stringify({ status: v && v.status, deployment_id: v && v.deployment_id, admission: v && v.checks && v.checks.agent_v1 && v.checks.agent_v1.admission })); if (!r.ok || !v || v.status !== "ok" || v.deployment_id !== process.env.NEXT_PUBLIC_APP_VERSION) process.exit(1); }).catch(() => process.exit(1));'
}
observed=
attempt=1
while [ "$attempt" -le 20 ]; do
  if observed=$(probe 2>/dev/null); then
    printf '%s\n' "$observed"
    exit 0
  fi
  sleep 2
  attempt=$((attempt + 1))
done
echo "Deep health did not report the release id after retries." >&2
exit 1"#,
        &[
            ("RELEASE_ID", release_id),
            ("STAGE", &remote_stage_dir(topology, release_id)),
            ("PROJECT", &topology.compose_project),
        ],
    )
}

/// Legacy `run_remote_open_research_admission` against the STAGE runtime
/// env: prove deep health reports `closed` (daemon heartbeat + descriptor
/// pins while admission is still closed), flip to `open`, recreate, and
/// verify `open` — re-closing the gate in-environment if any step fails.
pub fn payload_open_admission(topology: &RemoteTopology, release_id: &str) -> String {
    render(
        r#"set -eu
RELEASE_ID='@RELEASE_ID@'
REMOTE_STAGE='@STAGE@'
COMPOSE_PROJECT='@PROJECT@'
e="$REMOTE_STAGE/runtime.env"
@HELPERS@
if [ "$(value_of "$e" KRW_AGENT_BACKEND_MODE)" != "rust" ]; then
  echo "Research admission is not owned by Rust Agent V1 for this release."
  exit 0
fi
test "$(value_of "$e" KRW_AGENT_ADMISSION_MODE)" = "closed"
compose_stage() {
  FRONT_DIR="$REMOTE_STAGE" NEXT_PUBLIC_APP_VERSION="$RELEASE_ID" docker compose --project-name "$COMPOSE_PROJECT" --project-directory "$REMOTE_STAGE" --env-file "$e" -f "$REMOTE_STAGE/docker-compose.yml" "$@"
}
verify_deep_admission() {
  expected_admission=$1
  compose_stage exec -T web node -e 'const expected=process.argv[1]; const headers={"x-internal-key":process.env.INTERNAL_API_KEY||"","x-krw-client-ip":"127.0.0.1"}; fetch("http://127.0.0.1:3000/api/healthz/deep",{headers}).then(async response=>{const value=await response.json().catch(()=>null); if(!response.ok||value?.status!=="ok"||value?.checks?.agent_v1?.admission!==expected) process.exit(1);}).catch(()=>process.exit(1));' "$expected_admission"
}
re_close() {
  upsert_env_value "$e" KRW_AGENT_ADMISSION_MODE closed
  compose_stage up -d --no-deps --force-recreate --wait web agent-v1-outbox || true
}
verify_deep_admission closed
upsert_env_value "$e" KRW_AGENT_ADMISSION_MODE open
if ! compose_stage up -d --no-deps --force-recreate --wait web agent-v1-outbox; then
  re_close
  exit 1
fi
if ! verify_deep_admission open; then
  re_close
  exit 1
fi
echo "Research admission opened after deep readiness: $RELEASE_ID""#,
        &[
            ("RELEASE_ID", release_id),
            ("STAGE", &remote_stage_dir(topology, release_id)),
            ("PROJECT", &topology.compose_project),
            ("HELPERS", REMOTE_ENV_HELPERS),
        ],
    )
}

/// Deep health expecting `checks.agent_v1.admission == expected`; prints
/// the observation JSON for the controller to double-check. Retries on a
/// bounded budget: the admission flip force-recreates the web container and
/// the first deep probe after recreation can fail transiently (observed in
/// production run 20260816T113245Z).
pub fn payload_verify_admission(topology: &RemoteTopology, release_id: &str, expected: &str) -> String {
    render(
        r#"set -eu
RELEASE_ID='@RELEASE_ID@'
REMOTE_STAGE='@STAGE@'
COMPOSE_PROJECT='@PROJECT@'
EXPECTED_ADMISSION='@EXPECTED@'
compose_stage() {
  FRONT_DIR="$REMOTE_STAGE" NEXT_PUBLIC_APP_VERSION="$RELEASE_ID" docker compose --project-name "$COMPOSE_PROJECT" --project-directory "$REMOTE_STAGE" --env-file "$REMOTE_STAGE/runtime.env" -f "$REMOTE_STAGE/docker-compose.yml" "$@"
}
probe() {
  compose_stage exec -T web node -e 'const expected = process.argv[1]; const headers = { "x-internal-key": process.env.INTERNAL_API_KEY || "", "x-krw-client-ip": "127.0.0.1" }; fetch("http://127.0.0.1:3000/api/healthz/deep", { headers }).then(async r => { const v = await r.json().catch(() => null); process.stdout.write(JSON.stringify({ status: v && v.status, deployment_id: v && v.deployment_id, admission: v && v.checks && v.checks.agent_v1 && v.checks.agent_v1.admission })); if (!r.ok || !v || v.status !== "ok" || v.deployment_id !== process.env.NEXT_PUBLIC_APP_VERSION || v.checks.agent_v1.admission !== expected) process.exit(1); }).catch(() => process.exit(1));' "$EXPECTED_ADMISSION"
}
observed=
attempt=1
while [ "$attempt" -le 20 ]; do
  if observed=$(probe 2>/dev/null); then
    printf '%s\n' "$observed"
    exit 0
  fi
  sleep 2
  attempt=$((attempt + 1))
done
echo "Deep health did not report agent_v1 admission '$EXPECTED_ADMISSION' after retries." >&2
exit 1"#,
        &[
            ("RELEASE_ID", release_id),
            ("STAGE", &remote_stage_dir(topology, release_id)),
            ("PROJECT", &topology.compose_project),
            ("EXPECTED", expected),
        ],
    )
}

/// Legacy `run_remote_prepare`, adapted to the controller's locally built and
/// candidate-validated image: the source archive plus the image tarball are
/// shipped and the remote LOADS the image instead of rebuilding it. Every
/// runtime.env pin (descriptor install + hash gate, provider, DB/TLS/tenant
/// bindings, closed admission, old-image preservation, candidate tag,
/// forward-activation record) is the legacy mechanism verbatim.
pub fn payload_remote_prepare(topology: &RemoteTopology, release_id: &str, front_commit: &str, descriptor_hash: &str, release_set_hash: &str, provider: &str, tenant_id: &str) -> String {
    render(
        r#"set -e
RELEASE_ID='@RELEASE_ID@'
FRONT_GIT_COMMIT='@FRONT_COMMIT@'
REMOTE_FRONT_DIR='@FRONT_DIR@'
REMOTE_STAGE='@STAGE@'
REMOTE_FRONT_ARCHIVE='@FRONT_ARCHIVE@'
REMOTE_AGENT_DESCRIPTOR='@AGENT_DESCRIPTOR@'
COMPOSE_PROJECT='@PROJECT@'
AGENT_PROVIDER='@PROVIDER@'
AGENT_DESCRIPTOR_HASH='@DESCRIPTOR_HASH@'
AGENT_RELEASE_SET_HASH='@RELEASE_SET_HASH@'
test -f "$REMOTE_FRONT_DIR/.env"
test ! -e "$REMOTE_STAGE"
mkdir -p "$REMOTE_STAGE"
tar -xzf "$REMOTE_FRONT_ARCHIVE" -C "$REMOTE_STAGE"
test -f "$REMOTE_STAGE/docker-compose.yml"
test -f "$REMOTE_STAGE/Dockerfile.web"
cp "$REMOTE_FRONT_DIR/.env" "$REMOTE_STAGE/runtime.env"
chmod 600 "$REMOTE_STAGE/runtime.env"
e="$REMOTE_STAGE/runtime.env"
@HELPERS@
require_market_issue_x_token() {
  value=$(value_of "$e" MARKET_ISSUE_X_BEARER_TOKEN)
  case "$value" in ""|*"..."*)
    echo "Production market-issue worker requires MARKET_ISSUE_X_BEARER_TOKEN." >&2
    exit 1
    ;;
  esac
}
market_issue_x_ingestion_enabled() {
  value=$(value_of "$e" MARKET_ISSUE_X_INGESTION_ENABLED)
  case "$value" in 0|false|FALSE|off|OFF|no|NO) return 1 ;; *) return 0 ;; esac
}
market_issue_stream_enabled() {
  value=$(value_of "$e" MARKET_ISSUE_STREAM_ENABLED)
  case "$value" in 1|true|TRUE|on|ON|yes|YES) return 0 ;; *) return 1 ;; esac
}
if market_issue_x_ingestion_enabled; then
  require_market_issue_x_token
  echo "Verified market-issue X bearer token is present in the GCP runtime env."
else
  echo "Market-issue X ingestion is disabled for this deployment."
fi
if market_issue_stream_enabled; then
  require_market_issue_x_token
  echo "Verified market-issue stream worker token is present in the GCP runtime env."
else
  echo "Market-issue filtered-stream worker is disabled for this deployment."
fi
printf '\nFRONT_DIR=%s\nNEXT_PUBLIC_APP_VERSION=%s\n' "$REMOTE_STAGE" "$RELEASE_ID" >> "$e"
test -f "$REMOTE_AGENT_DESCRIPTOR"
test -f "$REMOTE_STAGE/ops/certificates/supabase-root-2021-ca.crt"
install -d -m 0755 "$REMOTE_STAGE/agent-release"
install -m 0644 "$REMOTE_AGENT_DESCRIPTOR" "$REMOTE_STAGE/agent-release/public-release.json"
observed_descriptor_hash="sha256:$(sha256sum "$REMOTE_STAGE/agent-release/public-release.json" | awk '{print $1}')"
test "$observed_descriptor_hash" = "$AGENT_DESCRIPTOR_HASH"
rm -f "$REMOTE_AGENT_DESCRIPTOR"
upsert_env_value "$e" KRW_AGENT_BACKEND_MODE rust
upsert_env_value "$e" KRW_AGENT_PROVIDER "$AGENT_PROVIDER"
upsert_env_value "$e" KRW_AGENT_PUBLIC_DESCRIPTOR_HOST_PATH "$REMOTE_STAGE/agent-release/public-release.json"
upsert_env_value "$e" KRW_AGENT_RELEASE_DESCRIPTOR_PATH /run/krw-agent/public-release.json
upsert_env_value "$e" KRW_AGENT_RELEASE_ARTIFACT_HASH "$AGENT_DESCRIPTOR_HASH"
upsert_env_value "$e" KRW_AGENT_RELEASE_SET_HASH "$AGENT_RELEASE_SET_HASH"
upsert_env_value "$e" KRW_AGENT_FRONT_COMMIT "$FRONT_GIT_COMMIT"
upsert_env_value "$e" KRW_AGENT_DAEMON_HEARTBEAT_REQUIRED 1
upsert_env_value "$e" KRW_RUNTIME_ENVIRONMENT prod
agent_database_url=$(value_of "$e" AGENT_V1_DATABASE_URL)
if [ -z "$agent_database_url" ]; then
  agent_database_url=$(value_of "$e" AGENT_V1_OUTBOX_DATABASE_URL)
fi
if [ -z "$agent_database_url" ]; then
  agent_database_url=$(value_of "$e" AGENT_QUEUE_LISTEN_DATABASE_URL)
fi
case "$agent_database_url" in
  postgres://*|postgresql://*) ;;
  *)
    echo "Rust Agent V1 candidate requires a Supabase PostgreSQL URL from the GCP runtime env." >&2
    exit 1
    ;;
esac
case "$agent_database_url" in
  *sslmode=disable*|*sslmode=DISABLE*)
    echo "Rust Agent V1 candidate refuses a non-TLS Supabase database URL." >&2
    exit 1
    ;;
esac
agent_tenant_id=$(value_of "$e" KRW_AGENT_TENANT_ID)
if [ -z "$agent_tenant_id" ]; then
  agent_tenant_id='@TENANT@'
fi
upsert_env_value "$e" KRW_AGENT_TENANT_ID "$agent_tenant_id"
upsert_env_value "$e" AGENT_V1_DATABASE_URL "$agent_database_url"
upsert_env_value "$e" AGENT_V1_OUTBOX_DATABASE_URL "$agent_database_url"
upsert_env_value "$e" AGENT_V1_DATABASE_CA_FILE /run/krw-agent/agent-v1-ca.pem
upsert_env_value "$e" KRW_AGENT_ADMISSION_MODE closed
OLD_WEB=$(docker compose --project-name "$COMPOSE_PROJECT" --project-directory "$REMOTE_FRONT_DIR" --env-file "$REMOTE_FRONT_DIR/.env" -f "$REMOTE_FRONT_DIR/docker-compose.yml" ps -q web 2>/dev/null || true)
OLD_IMAGE=
if [ -n "$OLD_WEB" ]; then
  OLD_IMAGE=$(docker inspect "$OLD_WEB" --format '{{.Image}}' 2>/dev/null || true)
fi
if [ -z "$OLD_IMAGE" ]; then
  OLD_IMAGE=$(docker image inspect --format '{{.Id}}' krw-ontology-front-web:local 2>/dev/null || true)
fi
test -n "$OLD_IMAGE"
if docker image inspect "$OLD_IMAGE" >/dev/null 2>&1; then
  :
else
  # A bootstrap container can refer to image metadata removed by an earlier
  # build. Rebuild the still-active source only so the stable local tag can
  # remain unchanged while the candidate image is prepared.
  test -n "$OLD_WEB"
  FRONT_DIR="$REMOTE_FRONT_DIR" docker compose --project-name "$COMPOSE_PROJECT" --project-directory "$REMOTE_FRONT_DIR" --env-file "$REMOTE_FRONT_DIR/.env" -f "$REMOTE_FRONT_DIR/docker-compose.yml" build web
  OLD_IMAGE=$(docker image inspect --format '{{.Id}}' krw-ontology-front-web:local)
fi
# Pin the old image under a stable tag BEFORE the stage build: the build
# retags :local to the candidate, leaving the old image unreferenced, and
# BuildKit garbage-collects unreferenced images mid-build (observed in
# production run 20260816T095011Z: the closing `docker tag $OLD_IMAGE
# :local` then failed with "No such image"). The prev-<release> tag
# keeps it referenced and is covered by the post-activation prune pattern.
OLD_TAG="krw-ontology-front-web:prev-$RELEASE_ID"
docker tag "$OLD_IMAGE" "$OLD_TAG"
compose_stage() {
  FRONT_DIR="$REMOTE_STAGE" NEXT_PUBLIC_APP_VERSION="$RELEASE_ID" docker compose --project-name "$COMPOSE_PROJECT" --project-directory "$REMOTE_STAGE" --env-file "$REMOTE_STAGE/runtime.env" -f "$REMOTE_STAGE/docker-compose.yml" "$@"
}
compose_stage build web
CANDIDATE_TAG="krw-ontology-front-web:candidate-$RELEASE_ID"
CANDIDATE_IMAGE=$(docker image inspect --format '{{.Id}}' krw-ontology-front-web:local)
docker tag "$CANDIDATE_IMAGE" "$CANDIDATE_TAG"
# Prove the exact image, descriptor mount, tenant partition, and Supabase TLS
# route before any existing public container is replaced (legacy NODE block,
# heredoc form). This catches missing/renamed runtime settings at candidate
# time, not after cutover.
compose_stage run --rm --no-deps -T --entrypoint node web - <<'NODE'
@PREFLIGHT_JS@
NODE
docker tag "$OLD_TAG" krw-ontology-front-web:local
printf 'CANDIDATE_IMAGE=%s\n' "$CANDIDATE_IMAGE" > "$REMOTE_STAGE/forward-activation.env"
chmod 600 "$REMOTE_STAGE/forward-activation.env"
rm -f "$REMOTE_FRONT_ARCHIVE"
printf 'CANDIDATE_IMAGE=%s\n' "$CANDIDATE_IMAGE"
echo "Prepared fast VM candidate: $RELEASE_ID""#,
        &[
            ("RELEASE_ID", release_id),
            ("FRONT_COMMIT", front_commit),
            ("FRONT_DIR", &topology.remote_front_dir),
            ("STAGE", &remote_stage_dir(topology, release_id)),
            ("FRONT_ARCHIVE", &remote_front_archive_path(release_id)),
            ("AGENT_DESCRIPTOR", &remote_agent_descriptor_path(release_id)),
            ("PROJECT", &topology.compose_project),
            ("PROVIDER", provider),
            ("DESCRIPTOR_HASH", descriptor_hash),
            ("RELEASE_SET_HASH", release_set_hash),
            ("TENANT", tenant_id),
            ("HELPERS", REMOTE_ENV_HELPERS),
            ("PREFLIGHT_JS", candidate_preflight_script()),
        ],
    )
}

/// Node script of the legacy in-container candidate ABI check
/// (`run_remote_validate_agent_candidate_abi`): queue/outbox procedures,
/// daemon mcp_ready column, product outbox procedure + receipts table, all
/// over the production TLS pooler.
const CANDIDATE_ABI_CHECK_JS: &str = r#"const fs = require("fs");
const { Pool } = require("pg");
const url = process.env.AGENT_V1_DATABASE_URL;
const caPath = process.env.AGENT_V1_DATABASE_CA_FILE;
async function main() {
  if (!(typeof url === "string" && /^postgres(?:ql)?:\/\//.test(url)) || url.includes("sslmode=disable")) {
    throw new Error("gcp_agent_v1_database_tls_configuration_invalid");
  }
  if (!(typeof caPath === "string" && caPath.startsWith("/"))) {
    throw new Error("gcp_agent_v1_database_ca_path_missing");
  }
  const ca = fs.readFileSync(caPath, "utf8");
  const pool = new Pool({ connectionString: url, application_name: "krw-gcp-agent-v1-candidate-abi", max: 1, connectionTimeoutMillis: 5000, idleTimeoutMillis: 5000, ssl: { ca, rejectUnauthorized: true } });
  try {
    const { rows } = await pool.query(`
      select
        to_regprocedure('agent_v1.enqueue_run(jsonb)') is not null as enqueue_run,
        to_regprocedure('agent_v1.claim_outbox(jsonb)') is not null as claim_outbox,
        to_regprocedure('agent_v1.ack_outbox(jsonb)') is not null as ack_outbox,
        to_regprocedure('agent_v1.heartbeat_daemon(jsonb)') is not null as heartbeat_daemon,
        exists (
          select 1 from information_schema.columns
           where table_schema = 'public'
             and table_name = 'agent_v1_daemon_heartbeats'
             and column_name = 'mcp_ready'
        ) as daemon_mcp_readiness_column,
        to_regprocedure('public.apply_agent_v1_product_outbox(jsonb)') is not null as apply_product_outbox,
        to_regclass('public.agent_v1_product_outbox_receipts') is not null as product_outbox_receipts
    `);
    const checks = rows[0] || {};
    if (!Object.values(checks).every(Boolean)) {
      throw new Error("gcp_agent_v1_database_abi_missing");
    }
  } finally {
    await pool.end();
  }
  console.log(JSON.stringify({ status: "ok", check: "gcp_agent_v1_candidate_abi", queue_and_outbox_abi: "verified", supabase_tls: "verified" }));
}
main().catch((error) => { console.error(error instanceof Error ? error.message : "gcp_agent_v1_candidate_abi_failed"); process.exit(1); });"#;

/// Legacy `run_remote_validate_agent_candidate_abi`: the exact GCP runtime
/// proves the queue/outbox ABI it will use in production, after migrations
/// and before the Mac daemon or public web container is switched.
pub fn payload_remote_candidate_abi(topology: &RemoteTopology, release_id: &str) -> String {
    format!(
        "set -eu\nREMOTE_STAGE={stage}\nCOMPOSE_PROJECT={project}\ncompose_stage() {{ FRONT_DIR=\"$REMOTE_STAGE\" docker compose --project-name \"$COMPOSE_PROJECT\" --project-directory \"$REMOTE_STAGE\" --env-file \"$REMOTE_STAGE/runtime.env\" -f \"$REMOTE_STAGE/docker-compose.yml\" \"$@\"; }}\ncompose_stage run --rm --no-deps -T --entrypoint node web -e {script}",
        stage = shell_quote(&remote_stage_dir(topology, release_id)),
        project = shell_quote(&topology.compose_project),
        script = shell_quote(CANDIDATE_ABI_CHECK_JS),
    )
}

/// Node script of the legacy build-time candidate preflight
/// (`run_remote_prepare` NODE block): URL/TLS shape, tenant partition,
/// provider, CA, descriptor provider pin, and a live `select 1` through the
/// production TLS pooler — before any public container is replaced.
pub fn candidate_preflight_script() -> &'static str {
    r#"const fs = require("fs");
const { Pool } = require("pg");
const providerModels = { glm: "glm-5.3", deepseek: "deepseek-v4-flash" };
const url = process.env.AGENT_V1_DATABASE_URL;
const tenant = process.env.KRW_AGENT_TENANT_ID;
const provider = process.env.KRW_AGENT_PROVIDER;
const descriptorPath = process.env.KRW_AGENT_RELEASE_DESCRIPTOR_PATH;
const caPath = process.env.AGENT_V1_DATABASE_CA_FILE;
async function main() {
  if (!(typeof url === "string" && /^postgres(?:ql)?:\/\//.test(url))) {
    throw new Error("gcp_agent_v1_database_url_missing");
  }
  if (url.includes("sslmode=disable")) {
    throw new Error("gcp_agent_v1_database_tls_disabled");
  }
  if (!(typeof tenant === "string" && tenant.length > 0 && tenant.length <= 128)) {
    throw new Error("gcp_agent_v1_tenant_missing");
  }
  if (!Object.hasOwn(providerModels, provider)) {
    throw new Error("gcp_agent_v1_provider_invalid");
  }
  if (!(typeof caPath === "string" && caPath.startsWith("/"))) {
    throw new Error("gcp_agent_v1_ca_path_missing");
  }
  const ca = fs.readFileSync(caPath, "utf8");
  if (!ca.includes("BEGIN CERTIFICATE")) {
    throw new Error("gcp_agent_v1_ca_invalid");
  }
  const descriptor = JSON.parse(fs.readFileSync(descriptorPath, "utf8"));
  if (!Array.isArray(descriptor.entries) || descriptor.entries.length === 0 ||
      !descriptor.entries.every((entry) => entry?.execution?.resolved_model === providerModels[provider])) {
    throw new Error("gcp_agent_v1_descriptor_provider_mismatch");
  }
  const pool = new Pool({ connectionString: url, application_name: "krw-gcp-agent-v1-candidate-preflight", max: 1, connectionTimeoutMillis: 5000, idleTimeoutMillis: 5000, ssl: { ca, rejectUnauthorized: true } });
  try {
    await pool.query("select 1 as supabase_tls");
  } finally {
    await pool.end();
  }
  console.log(JSON.stringify({ status: "ok", check: "gcp_agent_v1_candidate_preflight", provider, tenant_configured: true, descriptor_pinned: true, supabase_tls: "verified" }));
}
main().catch((error) => { console.error(error instanceof Error ? error.message : "gcp_agent_v1_candidate_preflight_failed"); process.exit(1); });"#
}

/// Legacy `prune_remote_stale_deployment_artifacts`: keep the sole active
/// stage and its running local image; remove only inactive stage directories
/// and stale candidate/release/prev/rollback tags under this deployment root.
pub fn payload_prune_stale(topology: &RemoteTopology) -> String {
    render(
        r#"set -eu
REMOTE_FRONT_DIR='@FRONT_DIR@'
release_base="$REMOTE_FRONT_DIR/.simple-deploy/releases"
active_release=
if [ -L "$REMOTE_FRONT_DIR/.simple-deploy/current" ]; then
  active_release=$(cd "$REMOTE_FRONT_DIR/.simple-deploy/current" && pwd -P)
fi
if [ -d "$release_base" ]; then
  release_base=$(cd "$release_base" && pwd -P)
  for candidate in "$release_base"/*; do
    [ -d "$candidate" ] && [ ! -L "$candidate" ] || continue
    candidate=$(cd "$candidate" && pwd -P)
    case "$candidate" in
      "$release_base"/*) ;;
      *)
        echo "Refusing to prune an unexpected deployment path." >&2
        exit 1
        ;;
    esac
    [ "$candidate" = "$active_release" ] && continue
    rm -rf -- "$candidate"
  done
fi
docker images --format '{{.Repository}}|{{.Tag}}' \
  | while IFS='|' read -r repository tag; do
      [ "$repository" = "krw-ontology-front-web" ] || continue
      case "$tag" in
        candidate-*|release-*|prev-*|rollback-*) docker image rm "$repository:$tag" >/dev/null 2>&1 || true ;;
      esac
    done
docker images --format '{{.Repository}}|{{.Tag}}' \
  | while IFS='|' read -r repository tag; do
      [ "$repository" = "caddy" ] || continue
      case "$tag" in
        candidate-*|release-*|prev-*|rollback-*) docker image rm "$repository:$tag" >/dev/null 2>&1 || true ;;
      esac
    done
echo "Pruned inactive deployment stages and stale web candidate tags.""#,
        &[("FRONT_DIR", &topology.remote_front_dir)],
    )
}

/// Legacy `prune_remote_post_activation_artifacts`: after deep health passes,
/// drop the redundant candidate tag and bound unused BuildKit cache to 12GB.
pub fn payload_prune_post_activation(release_id: &str) -> String {
    render(
        r#"set -eu
RELEASE_ID='@RELEASE_ID@'
docker image rm "krw-ontology-front-web:candidate-$RELEASE_ID" >/dev/null 2>&1 || true
docker builder prune --all --force --max-used-space 12GB >/dev/null 2>&1 || true
echo "Pruned redundant candidate tag and bounded unused BuildKit cache to 12GB.""#,
        &[("RELEASE_ID", release_id)],
    )
}

/// One remote shell command: transport argv prefix (gcloud compute ssh or
/// plain ssh) + the shell payload as the final element. Same payload and
/// buffering/timeout semantics across transports; records never contain
/// secrets (payloads only reference operator-owned paths and env NAMES).
fn remote_shell_command(
    stage: &'static str,
    id: &str,
    topology: &RemoteTopology,
    ssh_ms: u64,
    payload: String,
    timeout_ms: u64,
) -> StageCommand {
    let mut argv = topology.transport.argv_prefix(ssh_ms);
    argv.push(payload);
    StageCommand::new(stage, id, argv, Path::new("/"), Vec::new(), timeout_ms)
}

// ---------------------------------------------------------------------------
// Command builders (pure; unit-tested for exact argv)
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct PipelineDeps<'a> {
    pub config: &'a DeployConfig,
    pub config_path: &'a Path,
    pub target: &'a TargetFile,
    pub context: &'a RunContext,
    /// `agent_head` recorded in the preflight receipt (drift guard target).
    pub preflight_agent_head: &'a str,
    /// `frontend_head` recorded in the preflight receipt; the archived source
    /// and local image build must come from exactly this commit.
    pub preflight_frontend_head: &'a str,
    pub now_unix_seconds: u64,
}

impl PipelineDeps<'_> {
    fn release_id(&self) -> String {
        format!("{}-{}", self.context.run_ts, self.context.run_id)
    }

    fn metrics_port_token(&self) -> String {
        if runtime_env_has_key(&self.config.runtime_env, METRICS_PORT_ENV_KEY) {
            env_placeholder(METRICS_PORT_ENV_KEY)
        } else {
            DEFAULT_METRICS_PORT.to_owned()
        }
    }

    /// Selected provider bundle root under the run's release output dir.
    fn provider_bundle(&self) -> PathBuf {
        self.context.output_dir.join(&self.config.provider)
    }

    /// Local ship directory holding the archives uploaded to the instance.
    /// Lives under the run's RECEIPT workspace, NOT the release root: the
    /// launchd installer requires the sealed release root to contain exactly
    /// {glm, deepseek, dual-release-index.json}.
    fn ship_dir(&self) -> PathBuf {
        self.context.receipt_dir.join(SHIP_DIR_NAME)
    }

    /// First runtime-env KEY NAME carrying a Postgres URL, in the legacy
    /// fallback order. Errors name the missing keys only.
    fn agent_db_url_handle(&self) -> Result<String, String> {
        for key in AGENT_DB_URL_ENV_KEYS {
            if runtime_env_has_key(&self.config.runtime_env, key) {
                return Ok(key.to_owned());
            }
        }
        Err(format!(
            "runtime env has none of the agent database URL keys ({}) required by the Rust agent release path",
            AGENT_DB_URL_ENV_KEYS.join(", ")
        ))
    }

    /// Tenant partition: the runtime env value when declared, else the legacy
    /// stable product default.
    fn tenant_id(&self) -> String {
        runtime_env_value(&self.config.runtime_env, TENANT_ID_ENV_KEY)
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_TENANT_ID.to_owned())
    }

    /// The env file the Rust daemon launches with: the operator-owned
    /// envelope pointer when declared, else the controller runtime env.
    fn agent_runtime_env_path(&self) -> String {
        if runtime_env_has_key(&self.config.runtime_env, RUNTIME_ENV_FILE_ENV_KEY) {
            env_placeholder(RUNTIME_ENV_FILE_ENV_KEY)
        } else {
            self.config.runtime_env.display().to_string()
        }
    }
}

pub fn build_stage3_command(deps: &PipelineDeps<'_>) -> StageCommand {
    StageCommand::new(
        crate::stages::STAGE_BUILD,
        "build.dual-provider-bundles",
        vec![
            "scripts/build_dual_provider_release.sh".to_owned(),
            "--output-root".to_owned(),
            deps.context.output_dir.display().to_string(),
        ],
        &deps.config.agent_source_root,
        vec![RUNTIME_ENV_ALL.to_owned()],
        BUILD_TIMEOUT_MS,
    )
}

pub fn build_stage4_commands(deps: &PipelineDeps<'_>, active_key_id: &str) -> Vec<StageCommand> {
    let output_dir = &deps.context.output_dir;
    let selected = &deps.config.provider;
    // The legacy seal loop (build_sealed_dual_provider_release.sh) prepares,
    // signs, and seals BOTH provider bundles before finalize, which verifies
    // the dual index. Activation still uses only the selected provider, and
    // per-provider live credentials are a runtime concern, not a seal-time
    // one. Mirror that exactly: the non-selected provider seals first, the
    // selected provider keeps the canonical (unsuffixed) command ids.
    let mut order = vec![selected.clone()];
    for provider in ["glm", "deepseek"] {
        if &provider != selected {
            order.insert(0, provider.to_owned());
        }
    }
    let mut commands = Vec::new();
    for provider in &order {
        let id_suffix = if provider == selected {
            String::new()
        } else {
            format!("-{provider}")
        };
        let bundle = output_dir.join(provider);
        let config_root = deps.config.operator_root.join("config").join(provider);
        let binding = config_root.join("deployment-binding.yaml");
        let endpoints = config_root.join("endpoint-registry.yaml");
        let registry = config_root.join("release-trust-registry.json");
        let authorization_root = deps.config.operator_root.join("signing").join("releases").join(deps.release_id());
        let authorization = authorization_root.join(format!("release-authorization-{provider}.json"));
        let now = deps.now_unix_seconds;
        commands.push(StageCommand::new(
            crate::stages::STAGE_SEAL,
            format!("seal.prepare-production-candidate{id_suffix}").as_str(),
            vec![
                "scripts/prepare_production_candidate.sh".to_owned(),
                "--provider".to_owned(),
                provider.clone(),
                "--bundle".to_owned(),
                bundle.display().to_string(),
                "--binding".to_owned(),
                binding.display().to_string(),
                "--endpoints".to_owned(),
                endpoints.display().to_string(),
            ],
            &deps.config.agent_source_root,
            vec![RUNTIME_ENV_ALL.to_owned()],
            SEAL_TIMEOUT_MS,
        ));
        commands.push(StageCommand::new(
            crate::stages::STAGE_SEAL,
            format!("seal.sign-release-authorization{id_suffix}").as_str(),
            vec![
                bundle.join("bin/krw-agent").display().to_string(),
                "release".to_owned(),
                "sign".to_owned(),
                "--descriptor".to_owned(),
                bundle.join("public-release.json").display().to_string(),
                "--private-key".to_owned(),
                deps.config.operator_root.join("signing/release-private.pk8").display().to_string(),
                "--key-id".to_owned(),
                active_key_id.to_owned(),
                "--sequence".to_owned(),
                now.to_string(),
                "--issued-at-unix-seconds".to_owned(),
                now.to_string(),
                "--expires-at-unix-seconds".to_owned(),
                (now + AUTHORIZATION_TTL_SECONDS).to_string(),
                "--runtime-version".to_owned(),
                RUNTIME_VERSION.to_owned(),
                "--kernel-version".to_owned(),
                KERNEL_VERSION.to_owned(),
                "--out".to_owned(),
                authorization.display().to_string(),
            ],
            &deps.config.agent_source_root,
            vec![RUNTIME_ENV_ALL.to_owned()],
            SEAL_TIMEOUT_MS,
        ));
        commands.push(StageCommand::new(
            crate::stages::STAGE_SEAL,
            format!("seal.seal-production-candidate{id_suffix}").as_str(),
            vec![
                "scripts/seal_production_candidate.sh".to_owned(),
                "--provider".to_owned(),
                provider.clone(),
                "--candidate".to_owned(),
                bundle.display().to_string(),
                "--authorization".to_owned(),
                authorization.display().to_string(),
                "--trust-registry".to_owned(),
                registry.display().to_string(),
            ],
            &deps.config.agent_source_root,
            vec![RUNTIME_ENV_ALL.to_owned()],
            SEAL_TIMEOUT_MS,
        ));
    }
    commands.push(StageCommand::new(
        crate::stages::STAGE_SEAL,
        "seal.finalize-dual-release",
        vec![
            "scripts/finalize_dual_provider_release.sh".to_owned(),
            "--release-root".to_owned(),
            output_dir.display().to_string(),
        ],
        &deps.config.agent_source_root,
        vec![RUNTIME_ENV_ALL.to_owned()],
        SEAL_TIMEOUT_MS,
    ));
    commands
}

/// Stage 5, step 1: re-resolve the frontend commit and pin it against the
/// preflight receipt (the shipped archive must come from exactly this clean
/// commit).
pub fn build_stage5_resolve_commit(deps: &PipelineDeps<'_>) -> StageCommand {
    StageCommand::new(
        crate::stages::STAGE_FRONTEND_IMAGE_PREPARE,
        "frontend-image.resolve-commit",
        vec![
            "git".to_owned(),
            "-C".to_owned(),
            deps.config.frontend_source_root.display().to_string(),
            "rev-parse".to_owned(),
            "HEAD".to_owned(),
        ],
        &deps.config.frontend_source_root,
        Vec::new(),
        SSH_QUICK_TIMEOUT_MS,
    )
}

/// Stage 5, step 2 — legacy `create_archives`: archive the exact clean commit
/// (`gzip -n` for determinism) into the run's ship directory. The remote
/// Docker builder recreates generated workers inside this immutable context;
/// NO image is ever built or shipped locally.
pub fn build_stage5_archive_source_command(deps: &PipelineDeps<'_>, commit: &str) -> StageCommand {
    let script = format!(
        "git -C {front} archive --format=tar {commit} | gzip -n > {archive}",
        front = shell_quote(&deps.config.frontend_source_root.display().to_string()),
        commit = shell_quote(commit),
        archive = shell_quote(&deps.ship_dir().join("front.tar.gz").display().to_string()),
    );
    StageCommand::new(
        crate::stages::STAGE_FRONTEND_IMAGE_PREPARE,
        "frontend-image.archive-source",
        vec!["/bin/sh".to_owned(), "-c".to_owned(), script],
        &deps.config.frontend_source_root,
        Vec::new(),
        SSH_QUICK_TIMEOUT_MS,
    )
}

/// Local ship copy of the public Rust release descriptor (the ONLY Rust
/// artifact the VM ever receives; the bundle, credentials, and local DB
/// runtime never leave the Mac). Mode 0644 like the legacy copy.
pub fn stage5_descriptor_copy_path(deps: &PipelineDeps<'_>) -> PathBuf {
    deps.ship_dir().join("public-release.json")
}

pub fn build_stage5_descriptor_copy_command(deps: &PipelineDeps<'_>) -> StageCommand {
    let source = deps.provider_bundle().join("public-release.json");
    let destination = stage5_descriptor_copy_path(deps);
    let script = format!(
        "cp {source} {destination} && chmod 0644 {destination}",
        source = shell_quote(&source.display().to_string()),
        destination = shell_quote(&destination.display().to_string()),
    );
    StageCommand::new(
        crate::stages::STAGE_FRONTEND_IMAGE_PREPARE,
        "frontend-image.descriptor-copy",
        vec!["/bin/sh".to_owned(), "-c".to_owned(), script],
        &deps.config.agent_source_root,
        Vec::new(),
        SSH_QUICK_TIMEOUT_MS,
    )
}

fn supabase_cli_path(deps: &PipelineDeps<'_>) -> String {
    deps.config.frontend_source_root.join("scripts/supabase-cli.sh").display().to_string()
}

/// Extract the shell-quoted absolute path ending in `/<file>` from a
/// controller-generated `-c` script (redirect target or copy destination).
fn extract_ship_file_path(script: &str, file: &str) -> Option<String> {
    let needle = format!("/{file}'");
    let start = script.rfind(&needle)?;
    let begin = script[..start].rfind('\'')? + 1;
    Some(format!("{}{}", &script[begin..start], needle.trim_matches('\'')))
}

/// Legacy `run_front_migration` wrapper as one shell command: capture the
/// CLI output, and on the operator's IPv6-only direct-connection failure
/// retry exactly once through the production TLS session pooler URL resolved
/// from the runtime env (`KRW_AGENT_DATABASE_URL` → `AGENT_V1_*` →
/// `AGENT_QUEUE_LISTEN_DATABASE_URL`). Every other failure propagates — a
/// fallback must never hide a SQL or migration-history failure. `cli_args`
/// are baked in (`db push --dry-run [--include-all]` / `db push ...`).
fn migration_wrapper_script(cli: &str, cli_args: &str) -> String {
    let cli = shell_quote(cli);
    format!(
        r#"out=$(mktemp "${{TMPDIR:-/tmp}}/krw-front-migration.XXXXXX") || exit 1
if {cli} {cli_args} >"$out" 2>&1; then cat "$out"; rm -f "$out"; exit 0; fi
status=$?
if grep -Eq 'IPv6 is not supported on your current network|no route to host' "$out"; then
  pooler=${{KRW_AGENT_DATABASE_URL:-${{AGENT_V1_DATABASE_URL:-${{AGENT_V1_OUTBOX_DATABASE_URL:-${{AGENT_QUEUE_LISTEN_DATABASE_URL:-}}}}}}}}
  case "$pooler" in
    postgres://*|postgresql://*) ;;
    *) cat "$out" >&2; rm -f "$out"; exit "$status" ;;
  esac
  echo "Direct Supabase database connection is IPv6-only; retrying this migration through the configured TLS session pooler." >&2
  rm -f "$out"
  exec {cli} db push --db-url "$pooler" {cli_tail}
fi
cat "$out" >&2; rm -f "$out"; exit "$status""#,
        cli = cli,
        cli_tail = cli_args.strip_prefix("db push ").unwrap_or_default(),
    )
}

fn migration_wrapper_command(
    stage: &'static str,
    id: &str,
    deps: &PipelineDeps<'_>,
    cli_args: &str,
    cwd: &Path,
    timeout_ms: u64,
) -> StageCommand {
    StageCommand::new(
        stage,
        id,
        vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            migration_wrapper_script(&supabase_cli_path(deps), cli_args),
        ],
        cwd,
        vec![RUNTIME_ENV_ALL.to_owned()],
        timeout_ms,
    )
}

/// Legacy RPC-contract gate that runs before the migration dry-run whenever
/// migrations apply (`verify-supabase-schema-smoke-rpc-contract.mjs`).
pub fn build_stage6_rpc_contract_command(deps: &PipelineDeps<'_>) -> StageCommand {
    StageCommand::new(
        crate::stages::STAGE_MIGRATIONS,
        "migrations.rpc-contract-verify",
        vec![
            "node".to_owned(),
            deps.config
                .frontend_source_root
                .join("scripts/verify-supabase-schema-smoke-rpc-contract.mjs")
                .display()
                .to_string(),
        ],
        &deps.config.frontend_source_root,
        vec![RUNTIME_ENV_ALL.to_owned()],
        DB_DRY_RUN_TIMEOUT_MS,
    )
}

pub fn build_stage6_dry_run_command(deps: &PipelineDeps<'_>, include_all: bool) -> StageCommand {
    let args = if include_all {
        "db push --include-all --dry-run"
    } else {
        "db push --dry-run"
    };
    migration_wrapper_command(
        crate::stages::STAGE_MIGRATIONS,
        if include_all {
            "migrations.db-push-dry-run-include-all"
        } else {
            "migrations.db-push-dry-run"
        },
        deps,
        args,
        &deps.config.frontend_source_root,
        DB_DRY_RUN_TIMEOUT_MS,
    )
}

/// Legacy `run_agent_release_migrations`: the sealed bundle's own migration
/// runner through the front wrapper, with the database URL/CA mapped from
/// the runtime env's agent DB handle (`--skip-front-migrations`; the front
/// plan is applied separately by the controller).
pub fn build_agent_release_migrations_command(deps: &PipelineDeps<'_>, _db_handle: &str) -> StageCommand {
    let apply_script = deps
        .config
        .frontend_source_root
        .join("scripts/apply-agent-v1-production-migrations.sh");
    let ca_path = deps
        .config
        .frontend_source_root
        .join("ops/certificates/supabase-root-2021-ca.crt");
    // The runtime env may compute the DB handle dynamically
    // (`export KRW_AGENT_DATABASE_URL="$(read_env_file ...)"`), so the URL
    // must be resolved INSIDE the sourced shell (the executor sources the
    // env file for RUNTIME_ENV_ALL commands), never through a static
    // `<env:...>` placeholder. Legacy fallback order preserved.
    let script = format!(
        "agent_database_url=${{KRW_AGENT_DATABASE_URL:-${{AGENT_V1_DATABASE_URL:-${{AGENT_V1_OUTBOX_DATABASE_URL:-${{AGENT_QUEUE_LISTEN_DATABASE_URL:-}}}}}}}}\ncase \"$agent_database_url\" in postgres://*|postgresql://*) ;; *) echo \"Rust Agent V1 migration requires a production Supabase PostgreSQL runtime URL.\" >&2; exit 1 ;; esac\nAGENT_V1_DATABASE_URL=\"$agent_database_url\" AGENT_V1_PSQL_URL=\"$agent_database_url\" AGENT_V1_DATABASE_CA_FILE={ca} exec \"$@\"",
        ca = shell_quote(&ca_path.display().to_string()),
    );
    StageCommand::new(
        crate::stages::STAGE_ADMISSION_CLOSE,
        "migrations.agent-release-apply",
        vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            script,
            "apply-agent-v1-migrations".to_owned(),
            apply_script.display().to_string(),
            "--agent-release".to_owned(),
            deps.provider_bundle().display().to_string(),
            "--skip-front-migrations".to_owned(),
        ],
        &deps.config.frontend_source_root,
        vec![RUNTIME_ENV_ALL.to_owned()],
        DB_PUSH_TIMEOUT_MS,
    )
}

/// Legacy `run_schema_smoke`: the Supabase schema smoke plus the Rust agent
/// V1 compatibility verifier, in one sourced-env shell exactly like the
/// legacy subshell.
pub fn build_schema_smoke_command(deps: &PipelineDeps<'_>) -> StageCommand {
    let front = &deps.config.frontend_source_root;
    let script = format!(
        "node {smoke} || exit 1\nagent_database_url=${{KRW_AGENT_DATABASE_URL:-${{AGENT_V1_DATABASE_URL:-${{AGENT_V1_OUTBOX_DATABASE_URL:-${{AGENT_QUEUE_LISTEN_DATABASE_URL:-}}}}}}}}\ncase \"$agent_database_url\" in postgres://*|postgresql://*) ;; *) echo \"Rust Agent V1 schema smoke requires a production Supabase PostgreSQL runtime URL.\" >&2; exit 1 ;; esac\nAGENT_V1_DATABASE_URL=\"$agent_database_url\" AGENT_V1_DATABASE_CA_FILE={ca} node {compat}",
        smoke = shell_quote(&front.join("scripts/supabase-schema-smoke.mjs").display().to_string()),
        ca = shell_quote(&front.join("ops/certificates/supabase-root-2021-ca.crt").display().to_string()),
        compat = shell_quote(&front.join("scripts/verify-agent-v1-compatibility.mjs").display().to_string()),
    );
    StageCommand::new(
        crate::stages::STAGE_ADMISSION_CLOSE,
        "migrations.schema-smoke",
        vec!["/bin/sh".to_owned(), "-c".to_owned(), script],
        front,
        vec![RUNTIME_ENV_ALL.to_owned()],
        SCHEMA_SMOKE_TIMEOUT_MS,
    )
}

/// One-time overlay for the sealed bundle's launchd launcher checks
/// (`verify_sealed_local_agent_database_abi` / `..._mcp_abi`): a temp tree
/// with a `current` symlink to the sealed bundle and a 0600 overlay env.
fn prepare_sealed_check_overlay(deps: &PipelineDeps<'_>, worker_id: &str) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let check_dir = std::env::temp_dir().join(format!(
        "krw-agent-sealed-check-{}-{worker_id}",
        deps.context.run_id
    ));
    std::fs::create_dir_all(&check_dir).map_err(|error| format!("{}: {error}", check_dir.display()))?;
    let current = check_dir.join("current");
    let _ = std::fs::remove_file(&current);
    #[cfg(unix)]
    std::os::unix::fs::symlink(deps.provider_bundle(), &current)
        .map_err(|error| format!("cannot link the sealed bundle for the {worker_id} check: {error}"))?;
    let overlay = check_dir.join("overlay.env");
    let contents = format!(
        "KRW_AGENT_CURRENT_DIR={current}\nKRW_AGENT_PROVIDER={provider}\nKRW_AGENT_ARTIFACT_ROOT={artifacts}\nKRW_AGENT_WORKER_ID={worker_id}\n",
        current = current.display(),
        provider = deps.config.provider,
        artifacts = check_dir.join("artifacts").display(),
    );
    std::fs::write(&overlay, contents).map_err(|error| format!("{}: {error}", overlay.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&overlay, std::fs::Permissions::from_mode(0o600));
    }
    Ok((check_dir, overlay))
}

/// Legacy sealed-bundle ABI checks: exercise the exact immutable bundle
/// through the same launcher launchd will use — `--database-check` after
/// migrations, `--mcp-check` after the gateways are active.
pub fn build_sealed_abi_command(deps: &PipelineDeps<'_>, mode: SealedAbiMode, overlay: &Path) -> Result<StageCommand, String> {
    let (id, flag, worker_id) = match mode {
        SealedAbiMode::Database => ("migrations.sealed-database-abi", "--database-check", "sealed-release-database-check"),
        SealedAbiMode::Mcp => ("activation.sealed-mcp-abi", "--mcp-check", "sealed-release-mcp-check"),
    };
    let _ = worker_id;
    let launcher = deps.provider_bundle().join("packaging/launchd/krw-agentd-start-local");
    Ok(StageCommand::new(
        if mode == SealedAbiMode::Database {
            crate::stages::STAGE_ADMISSION_CLOSE
        } else {
            crate::stages::STAGE_LOCAL_ACTIVATION
        },
        id,
        vec![
            launcher.display().to_string(),
            "--env-file".to_owned(),
            deps.agent_runtime_env_path(),
            "--overlay-file".to_owned(),
            overlay.display().to_string(),
            flag.to_owned(),
        ],
        &deps.config.agent_source_root,
        Vec::new(),
        SEALED_ABI_TIMEOUT_MS,
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealedAbiMode {
    Database,
    Mcp,
}

pub fn build_db_push_command(deps: &PipelineDeps<'_>, include_all: bool) -> StageCommand {
    let args = if include_all { "db push --include-all" } else { "db push" };
    let mut command = migration_wrapper_command(
        crate::stages::STAGE_ADMISSION_CLOSE,
        "migrations.db-push",
        deps,
        args,
        &deps.config.frontend_source_root,
        DB_PUSH_TIMEOUT_MS,
    );
    if include_all {
        command.id = "migrations.db-push-include-all".to_owned();
    }
    command
}

pub fn build_abi_command(deps: &PipelineDeps<'_>, id: &str, sql: &str) -> StageCommand {
    let handle = deps
        .target
        .db_url_env
        .clone()
        .unwrap_or_else(|| "KRW_AGENT_DB_URL".to_owned());
    StageCommand::new(
        crate::stages::STAGE_ADMISSION_CLOSE,
        id,
        vec![
            "psql".to_owned(),
            env_placeholder(&handle),
            "-tA".to_owned(),
            "-v".to_owned(),
            "ON_ERROR_STOP=1".to_owned(),
            "-c".to_owned(),
            sql.to_owned(),
        ],
        &deps.config.operator_root,
        vec![handle],
        PSQL_TIMEOUT_MS,
    )
}

pub fn build_admission_commands_remote(deps: &PipelineDeps<'_>, topology: &RemoteTopology) -> Vec<StageCommand> {
    let ssh_ms = deps.config.timeouts.ssh_ms;
    vec![
        remote_shell_command(
            crate::stages::STAGE_ADMISSION_CLOSE,
            "admission.read-previous",
            topology,
            ssh_ms,
            payload_read_previous_admission(topology),
            SSH_QUICK_TIMEOUT_MS,
        ),
        remote_shell_command(
            crate::stages::STAGE_ADMISSION_CLOSE,
            "admission.close",
            topology,
            ssh_ms,
            payload_close_admission(topology),
            SSH_COMPOSE_TIMEOUT_MS,
        ),
        remote_shell_command(
            crate::stages::STAGE_ADMISSION_CLOSE,
            "admission.verify-closed",
            topology,
            ssh_ms,
            payload_verify_healthz(topology),
            SSH_QUICK_TIMEOUT_MS,
        ),
    ]
}

fn local_compose_recreate(deps: &PipelineDeps<'_>, id: &str, stage: &'static str) -> StageCommand {
    StageCommand::new(
        stage,
        id,
        vec![
            "docker".to_owned(),
            "compose".to_owned(),
            "--project-directory".to_owned(),
            deps.config.frontend_source_root.display().to_string(),
            "--env-file".to_owned(),
            deps.config.runtime_env.display().to_string(),
            "up".to_owned(),
            "-d".to_owned(),
            "--no-deps".to_owned(),
            "--force-recreate".to_owned(),
            "--wait".to_owned(),
            "web".to_owned(),
            "agent-v1-outbox".to_owned(),
        ],
        &deps.config.frontend_source_root,
        Vec::new(),
        SSH_COMPOSE_TIMEOUT_MS,
    )
}

pub fn build_admission_commands_local(deps: &PipelineDeps<'_>) -> Vec<StageCommand> {
    vec![
        local_compose_recreate(deps, "admission.close-local", crate::stages::STAGE_ADMISSION_CLOSE),
        StageCommand::new(
            crate::stages::STAGE_ADMISSION_CLOSE,
            "admission.verify-closed-local",
            vec![
                "curl".to_owned(),
                "-fsS".to_owned(),
                "--max-time".to_owned(),
                seconds_from_ms(deps.config.timeouts.public_ready_ms),
                format!("{LOCAL_WEB_BASE_URL}/api/healthz"),
            ],
            &deps.config.operator_root,
            Vec::new(),
            deps.config.timeouts.public_ready_ms,
        ),
    ]
}

/// Stage 8 ports the legacy `WITH_AGENT_RELEASE` local activation block in
/// its exact order: stage the immutable bundle, (optionally) activate the
/// forward runtime env candidate, switch the daemon symlink WITHOUT starting
/// it, activate the capability runtime, materialize + activate the TLS MCP
/// gateways, prove the sealed MCP ABI through the active gateway, then start
/// the daemon.
pub fn build_stage8_commands(deps: &PipelineDeps<'_>, mcp_overlay: &Path) -> Vec<StageCommand> {
    let provider = &deps.config.provider;
    let launchd = deps.provider_bundle().join("packaging/launchd");
    let agentd = launchd.join("install-local-mac-agentd-release.sh");
    let capabilityd = launchd.join("install-local-mac-capabilityd-release.sh");
    let release_id = deps.release_id();
    // The installers default to ~/.local/share/krw-agent when the override
    // key is absent; only forward it when the operator env declares one.
    let install_root_env: Vec<String> = if runtime_env_has_key(&deps.config.runtime_env, LOCAL_INSTALL_ROOT_ENV_KEY) {
        vec![LOCAL_INSTALL_ROOT_ENV_KEY.to_owned()]
    } else {
        Vec::new()
    };
    let mut agentd_activate_argv = vec![
        agentd.display().to_string(),
        "--mode".to_owned(),
        "activate".to_owned(),
        "--release-id".to_owned(),
        release_id.clone(),
        "--provider".to_owned(),
        provider.clone(),
        "--env-file".to_owned(),
        deps.agent_runtime_env_path(),
        "--defer-start".to_owned(),
    ];
    if runtime_env_has_key(&deps.config.runtime_env, METRICS_PORT_ENV_KEY) {
        agentd_activate_argv.push("--metrics-port".to_owned());
        agentd_activate_argv.push(env_placeholder(METRICS_PORT_ENV_KEY));
    }
    let mut commands = vec![
        StageCommand::new(
            crate::stages::STAGE_LOCAL_ACTIVATION,
            "activation.agentd-stage",
            vec![
                agentd.display().to_string(),
                "--mode".to_owned(),
                "stage".to_owned(),
                "--release-root".to_owned(),
                deps.context.output_dir.display().to_string(),
                "--release-id".to_owned(),
                release_id.clone(),
                "--provider".to_owned(),
                provider.clone(),
            ],
            &deps.config.agent_source_root,
            install_root_env.clone(),
            INSTALL_TIMEOUT_MS,
        ),
    ];
    // Legacy `activate_agent_runtime_env_candidate`: only when the operator
    // staged a candidate envelope; the normal path has none and skips.
    if runtime_env_has_key(&deps.config.runtime_env, RUNTIME_ENV_CANDIDATE_ENV_KEY) {
        commands.push(StageCommand::new(
            crate::stages::STAGE_LOCAL_ACTIVATION,
            "activation.runtime-env-candidate",
            vec![
                "/bin/sh".to_owned(),
                deps.config
                    .frontend_source_root
                    .join("scripts/activate-forward-runtime-env.sh")
                    .display()
                    .to_string(),
                "--candidate".to_owned(),
                env_placeholder(RUNTIME_ENV_CANDIDATE_ENV_KEY),
                "--destination".to_owned(),
                env_placeholder(RUNTIME_ENV_FILE_ENV_KEY),
            ],
            &deps.config.agent_source_root,
            vec![
                RUNTIME_ENV_CANDIDATE_ENV_KEY.to_owned(),
                RUNTIME_ENV_FILE_ENV_KEY.to_owned(),
            ],
            INSTALL_TIMEOUT_MS,
        ));
    }
    commands.extend([
        StageCommand::new(
            crate::stages::STAGE_LOCAL_ACTIVATION,
            "activation.agentd-activate",
            agentd_activate_argv,
            &deps.config.agent_source_root,
            install_root_env.clone(),
            INSTALL_TIMEOUT_MS,
        ),
        StageCommand::new(
            crate::stages::STAGE_LOCAL_ACTIVATION,
            "activation.capabilityd-activate",
            vec![
                capabilityd.display().to_string(),
                "--mode".to_owned(),
                "activate".to_owned(),
                "--release-id".to_owned(),
                release_id.clone(),
                "--provider".to_owned(),
                provider.clone(),
                "--operator-root".to_owned(),
                deps.config.operator_root.display().to_string(),
            ],
            &deps.config.agent_source_root,
            install_root_env.clone(),
            INSTALL_TIMEOUT_MS,
        ),
    ]);
    // Legacy `prepare_local_agent_mcp_gateways` / `activate_local_agent_mcp_gateways`.
    let gateway_installer = deps
        .config
        .frontend_source_root
        .join("scripts/install-krw-agent-local-mcp-gateways.mjs");
    for (id, mode) in [
        ("activation.mcp-gateways-prepare", "prepare"),
        ("activation.mcp-gateways-activate", "activate"),
    ] {
        commands.push(StageCommand::new(
            crate::stages::STAGE_LOCAL_ACTIVATION,
            id,
            vec![
                "node".to_owned(),
                gateway_installer.display().to_string(),
                "--mode".to_owned(),
                mode.to_owned(),
                "--agent-bundle".to_owned(),
                deps.provider_bundle().display().to_string(),
                "--operator-root".to_owned(),
                deps.config.operator_root.display().to_string(),
            ],
            &deps.config.frontend_source_root,
            Vec::new(),
            INSTALL_TIMEOUT_MS,
        ));
    }
    // Legacy `verify_sealed_local_agent_mcp_abi` (built with the overlay).
    commands.push(build_sealed_abi_command(deps, SealedAbiMode::Mcp, mcp_overlay).expect("sealed mcp abi command"));
    // Legacy `start_local_agent_release`.
    let mut agentd_start_argv = vec![
        agentd.display().to_string(),
        "--mode".to_owned(),
        "start".to_owned(),
        "--release-id".to_owned(),
        release_id,
        "--provider".to_owned(),
        provider.clone(),
    ];
    if runtime_env_has_key(&deps.config.runtime_env, METRICS_PORT_ENV_KEY) {
        agentd_start_argv.push("--metrics-port".to_owned());
        agentd_start_argv.push(env_placeholder(METRICS_PORT_ENV_KEY));
    } else {
        agentd_start_argv.push("--metrics-port".to_owned());
        agentd_start_argv.push(DEFAULT_METRICS_PORT.to_owned());
    }
    commands.push(StageCommand::new(
        crate::stages::STAGE_LOCAL_ACTIVATION,
        "activation.agentd-start",
        agentd_start_argv,
        &deps.config.agent_source_root,
        install_root_env,
        INSTALL_TIMEOUT_MS,
    ));
    commands
}

/// Legacy `upload_archive`: `gcloud compute scp` (or plain scp) with the
/// keep-alive flags; the runner retries up to [`SCP_MAX_ATTEMPTS`] times.
/// Only the immutable SOURCE archive and the public descriptor are ever
/// uploaded — never an image.
pub fn build_ship_scp_command(
    deps: &PipelineDeps<'_>,
    id: &str,
    local_path: &Path,
    remote_path: &str,
    topology: &RemoteTopology,
) -> StageCommand {
    let mut argv = topology.transport.scp_argv_prefix(deps.config.timeouts.ssh_ms);
    argv.push(local_path.display().to_string());
    argv.push(topology.transport.scp_destination(remote_path));
    StageCommand::new(
        crate::stages::STAGE_REMOTE_ACTIVATION,
        id,
        argv,
        &deps.config.frontend_source_root,
        Vec::new(),
        SCP_TIMEOUT_MS,
    )
}

/// Legacy `run_remote_prepare` — the remote BUILDS the candidate image from
/// the shipped immutable source archive, tags it, and runs the in-container
/// candidate preflight inside the same payload. The hashes come from the
/// sealed release state captured during stage 4.
pub fn build_remote_prepare_command(
    deps: &PipelineDeps<'_>,
    topology: &RemoteTopology,
    descriptor_hash: &str,
    release_set_hash: &str,
) -> StageCommand {
    let payload = payload_remote_prepare(
        topology,
        &deps.release_id(),
        deps.preflight_frontend_head,
        descriptor_hash,
        release_set_hash,
        &deps.config.provider,
        &deps.tenant_id(),
    );
    remote_shell_command(
        crate::stages::STAGE_REMOTE_ACTIVATION,
        "ship.remote-prepare",
        topology,
        deps.config.timeouts.ssh_ms,
        payload,
        SSH_PREPARE_TIMEOUT_MS,
    )
}

pub fn build_stage9_commands(deps: &PipelineDeps<'_>, topology: &RemoteTopology) -> Vec<StageCommand> {
    let ssh_ms = deps.config.timeouts.ssh_ms;
    vec![
        remote_shell_command(
            crate::stages::STAGE_REMOTE_ACTIVATION,
            "remote.candidate-abi",
            topology,
            ssh_ms,
            payload_remote_candidate_abi(topology, &deps.release_id()),
            SSH_COMPOSE_TIMEOUT_MS,
        ),
        remote_shell_command(
            crate::stages::STAGE_REMOTE_ACTIVATION,
            "remote.web-up",
            topology,
            ssh_ms,
            payload_remote_web_up(topology, &deps.release_id(), &deps.target.site_origins),
            SSH_COMPOSE_TIMEOUT_MS,
        ),
        remote_shell_command(
            crate::stages::STAGE_REMOTE_ACTIVATION,
            "readiness.remote-web-healthz",
            topology,
            ssh_ms,
            payload_remote_web_healthz(topology, &deps.release_id()),
            SSH_QUICK_TIMEOUT_MS,
        ),
    ]
}

/// Stage 12 best-effort remote artifact cleanup (05 artifact-cleanup rules;
/// failures never fail the deploy — they are recorded as notes).
pub fn build_stage12_cleanup_commands(deps: &PipelineDeps<'_>, topology: &RemoteTopology) -> Vec<StageCommand> {
    let ssh_ms = deps.config.timeouts.ssh_ms;
    vec![
        remote_shell_command(
            crate::stages::STAGE_TERMINAL_SUCCESS_RECEIPT,
            "cleanup.remote-prune-stale",
            topology,
            ssh_ms,
            payload_prune_stale(topology),
            SSH_COMPOSE_TIMEOUT_MS,
        ),
        remote_shell_command(
            crate::stages::STAGE_TERMINAL_SUCCESS_RECEIPT,
            "cleanup.remote-prune-post-activation",
            topology,
            ssh_ms,
            payload_prune_post_activation(&deps.release_id()),
            SSH_COMPOSE_TIMEOUT_MS,
        ),
    ]
}

/// Public-origin healthz probe from the operator host (legacy
/// `run_remote_deep_verify` public loop): status `ok` and the new
/// deployment id, retried by the executor until `public_ready_ms`.
pub fn build_public_healthz_command(deps: &PipelineDeps<'_>, index: usize, origin: &str) -> StageCommand {
    StageCommand::new(
        crate::stages::STAGE_DEEP_READINESS,
        &format!("readiness.public-healthz-{index}"),
        vec![
            "curl".to_owned(),
            "-fsS".to_owned(),
            "--connect-timeout".to_owned(),
            "3".to_owned(),
            "--max-time".to_owned(),
            "10".to_owned(),
            format!("{origin}/api/healthz"),
        ],
        &deps.config.operator_root,
        Vec::new(),
        deps.config.timeouts.public_ready_ms,
    )
}

pub fn build_daemon_metrics_command(deps: &PipelineDeps<'_>) -> StageCommand {
    StageCommand::new(
        crate::stages::STAGE_DEEP_READINESS,
        "readiness.daemon-metrics",
        vec![
            "curl".to_owned(),
            "-fsS".to_owned(),
            "--max-time".to_owned(),
            seconds_from_ms(deps.config.timeouts.daemon_ready_ms),
            format!("http://127.0.0.1:{}/metrics", deps.metrics_port_token()),
        ],
        &deps.config.operator_root,
        Vec::new(),
        deps.config.timeouts.daemon_ready_ms,
    )
}

pub fn build_mcp_probe(index: usize, endpoint: &str, mcp_ms: u64) -> Result<TcpProbe, String> {
    let Some((host, port)) = parse_host_port(endpoint) else {
        return Err(format!("mcp endpoint `{endpoint}` must be `host:port`"));
    };
    Ok(TcpProbe {
        stage: crate::stages::STAGE_DEEP_READINESS,
        id: format!("readiness.mcp-tcp-{index}"),
        host,
        port,
        timeout_ms: mcp_ms,
    })
}

pub fn build_mcp_ready_command(deps: &PipelineDeps<'_>, index: usize, endpoint: &str) -> Result<StageCommand, String> {
    let Some((host, port)) = parse_host_port(endpoint) else {
        return Err(format!("mcp endpoint `{endpoint}` must be `host:port`"));
    };
    let scheme = if is_loopback_host(&host) { "http" } else { "https" };
    Ok(StageCommand::new(
        crate::stages::STAGE_DEEP_READINESS,
        &format!("readiness.mcp-ready-{index}"),
        vec![
            "curl".to_owned(),
            "-fsS".to_owned(),
            "--max-time".to_owned(),
            seconds_from_ms(deps.config.timeouts.mcp_ms),
            format!("{scheme}://{host}:{port}/readyz"),
        ],
        &deps.config.operator_root,
        Vec::new(),
        deps.config.timeouts.mcp_ms,
    ))
}

pub fn build_web_deep_command_remote(deps: &PipelineDeps<'_>, topology: &RemoteTopology) -> StageCommand {
    remote_shell_command(
        crate::stages::STAGE_DEEP_READINESS,
        "readiness.web-deep",
        topology,
        deps.config.timeouts.ssh_ms,
        payload_remote_web_deep(topology, &deps.release_id()),
        deps.config.timeouts.public_ready_ms.max(SSH_QUICK_TIMEOUT_MS),
    )
}

pub fn build_web_deep_command_local(deps: &PipelineDeps<'_>) -> StageCommand {
    StageCommand::new(
        crate::stages::STAGE_DEEP_READINESS,
        "readiness.web-deep",
        vec![
            "curl".to_owned(),
            "-fsS".to_owned(),
            "--max-time".to_owned(),
            seconds_from_ms(deps.config.timeouts.public_ready_ms),
            "-H".to_owned(),
            format!("x-internal-key: {}", env_placeholder(INTERNAL_API_KEY_ENV_KEY)),
            "-H".to_owned(),
            "x-krw-client-ip: 127.0.0.1".to_owned(),
            format!("{LOCAL_WEB_BASE_URL}/api/healthz/deep"),
        ],
        &deps.config.operator_root,
        vec![INTERNAL_API_KEY_ENV_KEY.to_owned()],
        deps.config.timeouts.public_ready_ms,
    )
}

pub fn build_db_heartbeat_command(deps: &PipelineDeps<'_>) -> Option<StageCommand> {
    let handle = deps.target.db_url_env.as_deref()?;
    Some(StageCommand::new(
        crate::stages::STAGE_DEEP_READINESS,
        "readiness.db-heartbeat",
        vec![
            "psql".to_owned(),
            env_placeholder(handle),
            "-tA".to_owned(),
            "-v".to_owned(),
            "ON_ERROR_STOP=1".to_owned(),
            "-c".to_owned(),
            "select provider, descriptor_artifact_hash, release_set_hash, mcp_ready from public.agent_v1_daemon_heartbeats order by heartbeat_expires_at desc, last_seen_at desc limit 1".to_owned(),
        ],
        &deps.config.operator_root,
        vec![handle.to_owned()],
        PSQL_TIMEOUT_MS,
    ))
}

pub fn build_admission_open_commands_remote(deps: &PipelineDeps<'_>, topology: &RemoteTopology) -> Vec<StageCommand> {
    let ssh_ms = deps.config.timeouts.ssh_ms;
    vec![
        remote_shell_command(
            crate::stages::STAGE_ADMISSION_OPEN,
            "admission.open",
            topology,
            ssh_ms,
            payload_open_admission(topology, &deps.release_id()),
            SSH_COMPOSE_TIMEOUT_MS,
        ),
        remote_shell_command(
            crate::stages::STAGE_ADMISSION_OPEN,
            "readiness.admission-open-verify",
            topology,
            ssh_ms,
            payload_verify_admission(topology, &deps.release_id(), "open"),
            SSH_QUICK_TIMEOUT_MS,
        ),
    ]
}

pub fn build_admission_open_commands_local(deps: &PipelineDeps<'_>) -> Vec<StageCommand> {
    vec![
        local_compose_recreate(deps, "admission.open-local", crate::stages::STAGE_ADMISSION_OPEN),
        StageCommand::new(
            crate::stages::STAGE_ADMISSION_OPEN,
            "readiness.admission-open-verify-local",
            vec![
                "curl".to_owned(),
                "-fsS".to_owned(),
                "--max-time".to_owned(),
                seconds_from_ms(deps.config.timeouts.public_ready_ms),
                "-H".to_owned(),
                format!("x-internal-key: {}", env_placeholder(INTERNAL_API_KEY_ENV_KEY)),
                "-H".to_owned(),
                "x-krw-client-ip: 127.0.0.1".to_owned(),
                format!("{LOCAL_WEB_BASE_URL}/api/healthz/deep"),
            ],
            &deps.config.operator_root,
            vec![INTERNAL_API_KEY_ENV_KEY.to_owned()],
            deps.config.timeouts.public_ready_ms,
        ),
    ]
}

fn seconds_from_ms(millis: u64) -> String {
    (millis / 1000).max(1).to_string()
}

fn parse_host_port(endpoint: &str) -> Option<(String, u16)> {
    let endpoint = endpoint.trim();
    let authority = endpoint
        .strip_prefix("https://")
        .or_else(|| endpoint.strip_prefix("http://"))
        .unwrap_or(endpoint);
    let authority = authority.split('/').next()?;
    let (host, port) = authority.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    if host.is_empty() {
        None
    } else {
        Some((host.to_owned(), port))
    }
}

fn is_loopback_host(host: &str) -> bool {
    host == "127.0.0.1" || host == "localhost" || host == "::1" || host == "[::1]"
}

// ---------------------------------------------------------------------------
// Pipeline driver
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct PipelineState {
    descriptor_sha256: Option<String>,
    release_set_hash: Option<String>,
    frontend_image_digest: Option<String>,
    applied_migrations: Vec<String>,
    previous_admission: Option<String>,
    /// Frontend commit resolved in stage 5 (pinned to the preflight receipt).
    front_commit: Option<String>,
    /// Supabase dry-run reported out-of-order local migrations; the apply
    /// must repeat with `--include-all` (legacy `MIGRATION_INCLUDE_ALL`).
    migration_include_all: bool,
    skipped: Vec<SkippedRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineOutcome {
    pub success: bool,
    pub failed_stage: Option<&'static str>,
    pub reason: Option<String>,
    pub admission: TerminalAdmission,
    pub artifacts: Option<DeployArtifacts>,
    pub skipped: Vec<SkippedRecord>,
}

struct StageCtx<'a> {
    deps: &'a PipelineDeps<'a>,
    state: &'a mut PipelineState,
    receipt: StageReceipt,
}

impl StageCtx<'_> {
    fn command(&mut self, executor: &dyn StageExecutor, spec: StageCommand) -> Result<String, String> {
        let result = executor.execute(&self.deps.config.runtime_env, &spec);
        let outcome = if result.is_ok() { "pass" } else { "fail" };
        self.receipt.commands.push(StageCommandRecord {
            id: spec.id.clone(),
            stage: spec.stage.to_owned(),
            argv: spec.argv,
            cwd: spec.cwd.display().to_string(),
            env_keys_used: spec.env_keys_used,
            timeout_ms: spec.timeout_ms,
            outcome: outcome.to_owned(),
        });
        result.map(|output| output.stdout).map_err(|error| format!("command `{}` failed: {error}", spec.id))
    }

    /// Legacy `upload_archive` retry envelope: up to [`SCP_MAX_ATTEMPTS`]
    /// attempts with the legacy backoff (only slept for the real executor).
    fn command_with_retries(
        &mut self,
        executor: &dyn StageExecutor,
        spec: StageCommand,
        max_attempts: u32,
    ) -> Result<String, String> {
        let mut last_error = String::new();
        for attempt in 1..=max_attempts {
            match self.command(executor, spec.clone()) {
                Ok(stdout) => {
                    if attempt > 1 {
                        self.note(format!("command `{}` succeeded on attempt {attempt}", spec.id));
                    }
                    return Ok(stdout);
                }
                Err(error) => {
                    last_error = error;
                    if attempt < max_attempts && executor.kind() == "real" {
                        std::thread::sleep(Duration::from_secs(u64::from(attempt) * 3));
                    }
                }
            }
        }
        Err(last_error)
    }

    /// Post-success cleanup commands (05 artifact-cleanup): recorded with
    /// their real outcome, but a failure NEVER fails the deploy — it becomes
    /// a receipt note for the operator.
    fn best_effort_command(&mut self, executor: &dyn StageExecutor, spec: StageCommand) {
        if let Err(error) = self.command(executor, spec.clone()) {
            self.note(format!("best-effort cleanup `{}` failed (ignored): {error}", spec.id));
        }
    }

    fn probe(&mut self, executor: &dyn StageExecutor, probe: TcpProbe) -> Result<(), String> {
        let result = executor.tcp_connect(&probe);
        let outcome = if result.is_ok() { "pass" } else { "fail" };
        self.receipt.probes.push(TcpProbeRecord {
            id: probe.id.clone(),
            stage: probe.stage.to_owned(),
            host: probe.host,
            port: probe.port,
            timeout_ms: probe.timeout_ms,
            outcome: outcome.to_owned(),
        });
        result.map_err(|error| format!("probe `{}` failed: {error}", probe.id))
    }

    fn note(&mut self, text: String) {
        self.receipt.notes.push(text);
    }
}

fn stage_table_index(id: &str) -> u8 {
    STAGE_TABLE
        .iter()
        .position(|entry| entry.id == id)
        .map(|position| (position + 1) as u8)
        .unwrap_or(0)
}

/// Re-verify the preflight receipt's agent HEAD and config hash before a
/// mutating stage (05: build/activate stages reject drift after the receipt).
fn verify_no_drift(deps: &PipelineDeps<'_>, executor: &dyn StageExecutor) -> Result<(), String> {
    let head = executor
        .git_head(&deps.config.agent_source_root)
        .map_err(|error| format!("agent HEAD re-read failed: {error}"))?;
    if head != deps.preflight_agent_head {
        return Err(format!(
            "agent HEAD drifted since the preflight receipt (preflight {preflight}, now {head}); refusing to mutate",
            preflight = deps.preflight_agent_head
        ));
    }
    let config_hash = sha256_file(deps.config_path)
        .map_err(|error| format!("config re-hash failed: {error}"))?;
    if config_hash != deps.context.config_sha256 {
        return Err(format!(
            "config sha256 drifted since the preflight receipt (preflight {preflight}, now {config_hash}); refusing to mutate",
            preflight = deps.context.config_sha256
        ));
    }
    Ok(())
}

/// Execute stages 3-12. Writes one immutable stage receipt per stage into
/// the run's receipt dir and returns the outcome for the terminal receipt.
pub fn run_deploy_pipeline(deps: &PipelineDeps<'_>, executor: &dyn StageExecutor) -> PipelineOutcome {
    let mut state = PipelineState::default();
    for entry in STAGE_TABLE.iter().skip(2) {
        let index = stage_table_index(entry.id);
        if (6..=11).contains(&index) {
            if let Err(reason) = verify_no_drift(deps, executor) {
                let mut receipt = new_receipt(deps, executor, index, entry.id, StageReceiptStatus::Fail);
                receipt.reason = Some(reason.clone());
                let _ = crate::receipts::write_stage_receipt(&deps.context.receipt_dir, &receipt);
                return failure_outcome(deps, &state, entry.id, index, reason);
            }
        }
        if entry.id == crate::stages::STAGE_REMOTE_ACTIVATION
            && deps.target.gcp.is_none()
            && deps.target.ssh_host.is_none()
        {
            state.skipped.push(SkippedRecord {
                stage: entry.id.to_owned(),
                operation: None,
                reason: LOCAL_ONLY_SKIP_REASON.to_owned(),
            });
            let mut receipt = new_receipt(deps, executor, index, entry.id, StageReceiptStatus::Skipped);
            receipt.reason = Some(LOCAL_ONLY_SKIP_REASON.to_owned());
            if let Err(error) = crate::receipts::write_stage_receipt(&deps.context.receipt_dir, &receipt) {
                return failure_outcome(deps, &state, entry.id, index, format!("stage receipt write failed: {error}"));
            }
            continue;
        }
        let mut ctx = StageCtx {
            deps,
            state: &mut state,
            receipt: new_receipt(deps, executor, index, entry.id, StageReceiptStatus::Pass),
        };
        let result = match entry.id {
            crate::stages::STAGE_BUILD => stage_build(&mut ctx, executor),
            crate::stages::STAGE_SEAL => stage_seal(&mut ctx, executor),
            crate::stages::STAGE_FRONTEND_IMAGE_PREPARE => stage_frontend_image(&mut ctx, executor),
            crate::stages::STAGE_MIGRATIONS => stage_migrations_dry_run(&mut ctx, executor),
            crate::stages::STAGE_ADMISSION_CLOSE => stage_admission_close(&mut ctx, executor),
            crate::stages::STAGE_LOCAL_ACTIVATION => stage_local_activation(&mut ctx, executor),
            crate::stages::STAGE_REMOTE_ACTIVATION => stage_remote_activation(&mut ctx, executor),
            crate::stages::STAGE_DEEP_READINESS => stage_deep_readiness(&mut ctx, executor),
            crate::stages::STAGE_ADMISSION_OPEN => stage_admission_open(&mut ctx, executor),
            crate::stages::STAGE_TERMINAL_SUCCESS_RECEIPT => stage_terminal_cleanup(&mut ctx, executor),
            other => unreachable!("stage table declared unknown stage `{other}`"),
        };
        match result {
            Ok(()) => {
                if let Err(error) = crate::receipts::write_stage_receipt(&deps.context.receipt_dir, &ctx.receipt) {
                    let StageCtx { .. } = ctx;
                    return failure_outcome(deps, &state, entry.id, index, format!("stage receipt write failed: {error}"));
                }
            }
            Err(reason) => {
                // End the ctx borrow, then record the failure with the
                // stage's executed commands (status `fail`, write-once).
                let StageCtx { receipt, .. } = ctx;
                let mut receipt = receipt;
                receipt.status = StageReceiptStatus::Fail.as_str().to_owned();
                receipt.reason = Some(reason.clone());
                let _ = crate::receipts::write_stage_receipt(&deps.context.receipt_dir, &receipt);
                return failure_outcome(deps, &state, entry.id, index, reason);
            }
        }
    }
    PipelineOutcome {
        success: true,
        failed_stage: None,
        reason: None,
        admission: TerminalAdmission::Open,
        artifacts: Some(DeployArtifacts {
            release_dir: deps.context.output_dir.display().to_string(),
            release_id: deps.release_id(),
            descriptor_sha256: state.descriptor_sha256.clone().unwrap_or_else(|| UNAVAILABLE_HASH.to_owned()),
            frontend_image_digest: state
                .frontend_image_digest
                .clone()
                .unwrap_or_else(|| "<unavailable>".to_owned()),
            applied_migrations: state.applied_migrations.clone(),
            previous_admission: state.previous_admission.clone().unwrap_or_else(|| "unknown".to_owned()),
        }),
        skipped: state.skipped.clone(),
    }
}

fn new_receipt(
    deps: &PipelineDeps<'_>,
    executor: &dyn StageExecutor,
    index: u8,
    stage: &str,
    status: StageReceiptStatus,
) -> StageReceipt {
    StageReceipt::new(
        &deps.context.run_id,
        &deps.context.created_at_utc,
        deps.context.mode.as_str(),
        executor.kind(),
        index,
        stage,
        status,
    )
}

fn failure_outcome(
    deps: &PipelineDeps<'_>,
    state: &PipelineState,
    stage: &'static str,
    index: u8,
    reason: String,
) -> PipelineOutcome {
    // Admission posture: not-touched before `admission_close` (nothing
    // mutated), closed from `admission_close` onward (05 failure policy).
    let admission = if index >= stage_table_index(crate::stages::STAGE_ADMISSION_CLOSE) {
        TerminalAdmission::Closed
    } else {
        TerminalAdmission::NotTouched
    };
    PipelineOutcome {
        success: false,
        failed_stage: Some(stage),
        reason: Some(reason),
        admission,
        artifacts: partial_artifacts(deps, state),
        skipped: state.skipped.clone(),
    }
}

fn partial_artifacts(deps: &PipelineDeps<'_>, state: &PipelineState) -> Option<DeployArtifacts> {
    if state.descriptor_sha256.is_none()
        && state.frontend_image_digest.is_none()
        && state.previous_admission.is_none()
        && state.applied_migrations.is_empty()
    {
        return None;
    }
    Some(DeployArtifacts {
        release_dir: deps.context.output_dir.display().to_string(),
        release_id: deps.release_id(),
        descriptor_sha256: state.descriptor_sha256.clone().unwrap_or_else(|| UNAVAILABLE_HASH.to_owned()),
        frontend_image_digest: state
            .frontend_image_digest
            .clone()
            .unwrap_or_else(|| "<unavailable>".to_owned()),
        applied_migrations: state.applied_migrations.clone(),
        previous_admission: state.previous_admission.clone().unwrap_or_else(|| "unknown".to_owned()),
    })
}

// --- individual stage runners ----------------------------------------------

fn stage_build(ctx: &mut StageCtx<'_>, executor: &dyn StageExecutor) -> Result<(), String> {
    ctx.command(executor, build_stage3_command(ctx.deps))?;
    // The raw dual-provider build produces per-bundle release manifests and
    // the dual index. The sealed `public-release.json` descriptor only comes
    // into existence when stage 4's `prepare_production_candidate.sh` applies
    // the operator endpoint bindings to the SELECTED provider bundle.
    for provider in ["glm", "deepseek"] {
        let manifest = ctx.deps.context.output_dir.join(provider).join("release-manifest.json");
        if !manifest.is_file() {
            return Err(format!(
                "build did not produce the {provider} bundle manifest at {}",
                manifest.display()
            ));
        }
    }
    let index = ctx.deps.context.output_dir.join("dual-release-index.json");
    if !index.is_file() {
        return Err(format!(
            "build did not produce the dual release index at {}",
            index.display()
        ));
    }
    ctx.note(format!(
        "dual-provider bundles built under {}",
        ctx.deps.context.output_dir.display()
    ));
    Ok(())
}

fn stage_seal(ctx: &mut StageCtx<'_>, executor: &dyn StageExecutor) -> Result<(), String> {
    let provider = ctx.deps.config.provider.clone();
    // Both provider bundles are sealed (the finalize verifier requires the
    // dual index); both operators' config roots must therefore exist.
    for seal_provider in ["glm", "deepseek"] {
        let config_root = ctx.deps.config.operator_root.join("config").join(seal_provider);
        for required in [
            config_root.join("deployment-binding.yaml"),
            config_root.join("endpoint-registry.yaml"),
            config_root.join("release-trust-registry.json"),
        ] {
            if !required.is_file() {
                return Err(format!(
                    "operator configuration for provider `{seal_provider}` is missing or unsafe: {}",
                    required.display()
                ));
            }
        }
    }
    let config_root = ctx.deps.config.operator_root.join("config").join(&provider);
    let registry = config_root.join("release-trust-registry.json");
    let registry_bytes = std::fs::read(&registry).map_err(|error| format!("{}: {error}", registry.display()))?;
    let active_key_id = resolve_active_key_id(&registry_bytes, ctx.deps.now_unix_seconds)?;
    ctx.note(format!("resolved active signing key `{active_key_id}` for provider `{provider}`"));

    let authorization_root = ctx
        .deps
        .config
        .operator_root
        .join("signing")
        .join("releases")
        .join(ctx.deps.release_id());
    std::fs::create_dir_all(&authorization_root)
        .map_err(|error| format!("{}: {error}", authorization_root.display()))?;

    for spec in build_stage4_commands(ctx.deps, &active_key_id) {
        ctx.command(executor, spec)?;
    }
    let descriptor = ctx
        .deps
        .context
        .output_dir
        .join(&provider)
        .join("public-release.json");
    let descriptor_hash = sha256_file(&descriptor).map_err(|error| format!("descriptor hash failed: {error}"))?;
    let release_set_hash = read_descriptor_release_set_hash(&descriptor)?;
    ctx.state.descriptor_sha256 = Some(descriptor_hash.clone());
    ctx.state.release_set_hash = Some(release_set_hash.clone());
    ctx.note(format!("sealed provider `{provider}`; descriptor {descriptor_hash}; release set {release_set_hash}"));
    Ok(())
}

/// `selected_release_set_hash` from the sealed public descriptor (the legacy
/// `verify-krw-agent-production-release.mjs` field), fail-closed when absent.
fn read_descriptor_release_set_hash(descriptor: &Path) -> Result<String, String> {
    let bytes = std::fs::read(descriptor).map_err(|error| format!("{}: {error}", descriptor.display()))?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| format!("{} is not valid JSON: {error}", descriptor.display()))?;
    value
        .get("release_set_hash")
        .and_then(serde_json::Value::as_str)
        .filter(|hash| !hash.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("sealed descriptor {} has no release_set_hash", descriptor.display()))
}

fn stage_frontend_image(ctx: &mut StageCtx<'_>, executor: &dyn StageExecutor) -> Result<(), String> {
    // Pin the exact clean commit the preflight receipt recorded.
    let commit = ctx.command(executor, build_stage5_resolve_commit(ctx.deps))?;
    let commit = commit.trim().to_owned();
    if commit.is_empty() {
        return Err("frontend commit resolution returned an empty HEAD".to_owned());
    }
    if commit != ctx.deps.preflight_frontend_head {
        return Err(format!(
            "frontend HEAD drifted since the preflight receipt (preflight {preflight}, now {commit}); refusing to ship the release source",
            preflight = ctx.deps.preflight_frontend_head
        ));
    }
    ctx.state.front_commit = Some(commit.clone());
    ctx.note(format!("frontend release source pinned to commit {commit}"));

    // LOCAL SOURCE PREPARATION ONLY (legacy `create_archives`): the exact
    // clean commit is archived and the public descriptor copied. No image is
    // built or shipped locally — the VM builds the image inside the shipped
    // immutable context during stage 9's remote prepare.
    let ship_dir = ctx.deps.ship_dir();
    std::fs::create_dir_all(&ship_dir).map_err(|error| format!("{}: {error}", ship_dir.display()))?;
    ctx.command(executor, build_stage5_archive_source_command(ctx.deps, &commit))?;
    let archive = ship_dir.join("front.tar.gz");
    if !archive.is_file() {
        return Err(format!(
            "source archive was not created at {} (legacy create_archives)",
            archive.display()
        ));
    }

    // Legacy `create_agent_descriptor_upload`: copy the public descriptor
    // (the only Rust artifact that ever leaves this Mac) and gate its hash
    // against the sealed descriptor hash captured in stage 4.
    ctx.command(executor, build_stage5_descriptor_copy_command(ctx.deps))?;
    let descriptor_copy = stage5_descriptor_copy_path(ctx.deps);
    let observed = sha256_file(&descriptor_copy)
        .map_err(|error| format!("copied public descriptor hash failed: {error}"))?;
    let sealed = ctx
        .state
        .descriptor_sha256
        .clone()
        .ok_or("sealed descriptor hash is missing before the descriptor copy")?;
    if observed != sealed {
        return Err("copied public descriptor hash does not match the verified release".to_owned());
    }
    ctx.note(format!(
        "shipped source archive + public descriptor prepared under {} (image built remotely)",
        ship_dir.display()
    ));
    Ok(())
}

/// The legacy in-container checks print one compact JSON status object;
/// docker/compose noise may surround it, so scan for the marker line.
fn verify_candidate_check_output(stdout: &str, check_name: &str) -> Result<(), String> {
    let marker = format!("\"check\":\"{check_name}\"");
    if stdout.lines().any(|line| line.contains("\"status\":\"ok\"") && line.contains(&marker)) {
        Ok(())
    } else {
        Err(format!("in-container check `{check_name}` did not report status ok"))
    }
}

/// Parse the `CANDIDATE_IMAGE=<docker-image-id>` line the remote prepare
/// reports after building + validating the candidate on the VM (an image id
/// only; never secret material).
pub fn parse_candidate_image_id(prepare_output: &str) -> Option<String> {
    prepare_output
        .lines()
        .find_map(|line| line.trim().strip_prefix("CANDIDATE_IMAGE="))
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

fn stage_migrations_dry_run(ctx: &mut StageCtx<'_>, executor: &dyn StageExecutor) -> Result<(), String> {
    // Legacy order: the RPC-contract gate runs before the migration plan.
    ctx.command(executor, build_stage6_rpc_contract_command(ctx.deps))?;
    let stdout = ctx.command(executor, build_stage6_dry_run_command(ctx.deps, false))?;
    // Supabase reports out-of-order migrations as a successful dry run with
    // an instruction to rerun with --include-all; detect the advisory or the
    // real apply silently omits lifecycle migrations.
    if stdout.contains("--include-all") {
        ctx.note("migration plan reported out-of-order local migrations; retrying with --include-all".to_owned());
        ctx.command(executor, build_stage6_dry_run_command(ctx.deps, true))?;
        ctx.state.migration_include_all = true;
    }
    let planned = ctx.deps.target.supabase_migration_plan.len();
    ctx.note(format!(
        "forward-only migration dry-run passed with {planned} planned entr{}; nothing applied before admission close",
        if planned == 1 { "y" } else { "ies" }
    ));
    Ok(())
}

fn normalize_previous_admission(raw: &str) -> String {
    match raw.trim() {
        "open" | "closed" | "drain" | "canary" => raw.trim().to_owned(),
        _ => "closed".to_owned(),
    }
}

fn verify_deep_observation(observation: Option<DeepHealthObservation>, release_id: &str) -> Result<(), String> {
    if let Some(observed) = observation {
        if observed.status.as_deref() != Some("ok") {
            return Err(format!("deep health reported status `{}`", observed.status.unwrap_or_default()));
        }
        if observed.deployment_id.as_deref() != Some(release_id) {
            return Err(format!(
                "deep health deployment_id `{}` does not match release `{release_id}`",
                observed.deployment_id.unwrap_or_default()
            ));
        }
    }
    Ok(())
}

fn stage_admission_close(ctx: &mut StageCtx<'_>, executor: &dyn StageExecutor) -> Result<(), String> {
    let topology = resolve_remote_topology(ctx.deps.target)?;
    let previous = match &topology {
        Some(remote) => {
            ctx.note(format!("remote admission transport: {}", remote.transport.label()));
            let mut commands = build_admission_commands_remote(ctx.deps, remote).into_iter();
            let read_previous = commands.next().expect("admission.read-previous command");
            let raw = ctx.command(executor, read_previous)?;
            let close = commands.next().expect("admission.close command");
            ctx.command(executor, close)?;
            let verify = commands.next().expect("admission.verify-closed command");
            ctx.command(executor, verify)?;
            normalize_previous_admission(&raw)
        }
        None => {
            ctx.state.skipped.push(SkippedRecord {
                stage: crate::stages::STAGE_ADMISSION_CLOSE.to_owned(),
                operation: Some("admission.read-previous".to_owned()),
                reason: LOCAL_ONLY_SKIP_REASON.to_owned(),
            });
            let value = runtime_env_value(&ctx.deps.config.runtime_env, ADMISSION_ENV_KEY)
                .unwrap_or_else(|_| "closed".to_owned());
            upsert_env_value(&ctx.deps.config.runtime_env, ADMISSION_ENV_KEY, "closed")
                .map_err(|error| format!("local admission upsert failed: {error}"))?;
            let mut commands = build_admission_commands_local(ctx.deps).into_iter();
            let close = commands.next().expect("admission.close-local command");
            ctx.command(executor, close)?;
            let verify = commands.next().expect("admission.verify-closed-local command");
            let stdout = ctx.command(executor, verify)?;
            let status_ok = serde_json::from_str::<serde_json::Value>(&stdout)
                .ok()
                .and_then(|value| value.get("status").and_then(serde_json::Value::as_str).map(str::to_owned))
                == Some("ok".to_owned());
            if !status_ok {
                return Err("local gateway stack did not report healthz status ok after admission close".to_owned());
            }
            normalize_previous_admission(&value)
        }
    };
    ctx.state.previous_admission = Some(previous.clone());
    ctx.note(format!("captured previous {ADMISSION_ENV_KEY} `{previous}`"));

    // 05 migration ordering rules 3-4 + the legacy apply order: with the
    // gate closed, first the sealed bundle's own migrations, then the front
    // plan, then the schema smoke, then the sealed DB ABI — and only then
    // the contract's exact ABI entries.
    let db_handle = ctx.deps.agent_db_url_handle()?;
    ctx.command(executor, build_agent_release_migrations_command(ctx.deps, &db_handle))
        .map_err(|error| format!("sealed agent release migrations failed: {error}"))?;
    let include_all = ctx.state.migration_include_all;
    let stdout = ctx.command(executor, build_db_push_command(ctx.deps, include_all))?;
    let applied = parse_applied_migrations(&stdout);
    ctx.note(format!(
        "forward-only db push applied {} migration(s): {}",
        applied.len(),
        if applied.is_empty() { "(none pending)".to_owned() } else { applied.join(", ") }
    ));
    ctx.state.applied_migrations = applied;
    ctx.command(executor, build_schema_smoke_command(ctx.deps))
        .map_err(|error| format!("supabase schema smoke failed: {error}"))?;
    let (check_dir, overlay) = prepare_sealed_check_overlay(ctx.deps, "sealed-release-database-check")?;
    let abi_result = ctx.command(executor, build_sealed_abi_command(ctx.deps, SealedAbiMode::Database, &overlay)?);
    let _ = std::fs::remove_dir_all(&check_dir);
    abi_result
        .map_err(|error| format!("sealed local database ABI verification failed: {error}"))?;
    ctx.note("sealed local Rust daemon database ABI verified through the launchd entrypoint".to_owned());

    let procedures = ctx.deps.contract_required_procedures();
    let columns = ctx.deps.contract_required_columns();
    if procedures.is_empty() && columns.is_empty() {
        ctx.note("frontend contract pins no DB ABI lists; ABI verification recorded none".to_owned());
        return Ok(());
    }
    if ctx.deps.target.db_url_env.is_none() {
        return Err("DB ABI verification requires a db_url_env handle in the target file".to_owned());
    }
    for (index, entry) in procedures.iter().enumerate() {
        let abi = parse_procedure_entry(entry)?;
        let spec = build_abi_command(ctx.deps, &format!("{PROCEDURE_ABI_PREFIX}{index}"), &abi.sql());
        let stdout = ctx.command(executor, spec)?;
        if stdout != abi.expected_output() {
            return Err(format!(
                "procedure ABI verification failed for `{entry}`: expected `{}`, got `{stdout}`",
                abi.expected_output()
            ));
        }
        ctx.note(format!("procedure ABI verified: {entry}"));
    }
    for (index, entry) in columns.iter().enumerate() {
        let abi = parse_column_entry(entry)?;
        let spec = build_abi_command(ctx.deps, &format!("{COLUMN_ABI_PREFIX}{index}"), &abi.sql());
        let stdout = ctx.command(executor, spec)?;
        if stdout != abi.expected_output() {
            return Err(format!(
                "column ABI verification failed for `{entry}`: expected `{}`, got `{stdout}`",
                abi.expected_output()
            ));
        }
        ctx.note(format!("column ABI verified: {entry}"));
    }
    Ok(())
}

fn stage_local_activation(ctx: &mut StageCtx<'_>, executor: &dyn StageExecutor) -> Result<(), String> {
    // Legacy `require_file` gates on the sealed bundle packaging surface.
    let bundle = ctx.deps.provider_bundle();
    for required in [
        bundle.join("packaging/launchd/install-local-mac-agentd-release.sh"),
        bundle.join("packaging/launchd/install-local-mac-capabilityd-release.sh"),
        bundle.join("packaging/launchd/krw-agentd-start-local"),
        bundle.join("apply_migrations.sh"),
    ] {
        if !required.is_file() {
            return Err(format!(
                "sealed bundle packaging is incomplete for local activation: {}",
                required.display()
            ));
        }
    }
    let (mcp_check_dir, mcp_overlay) = prepare_sealed_check_overlay(ctx.deps, "sealed-release-mcp-check")?;
    let result: Result<(), String> = (|| {
        for spec in build_stage8_commands(ctx.deps, &mcp_overlay) {
            ctx.command(executor, spec)?;
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&mcp_check_dir);
    result?;
    if runtime_env_has_key(&ctx.deps.config.runtime_env, RUNTIME_ENV_CANDIDATE_ENV_KEY) {
        ctx.note(format!(
            "forward runtime env candidate activated from {RUNTIME_ENV_CANDIDATE_ENV_KEY} before the daemon switch"
        ));
    }
    ctx.note(format!(
        "local agentd + capabilityd + MCP gateways activated and daemon started for release `{}` (provider `{}`)",
        ctx.deps.release_id(),
        ctx.deps.config.provider
    ));
    Ok(())
}

fn stage_remote_activation(ctx: &mut StageCtx<'_>, executor: &dyn StageExecutor) -> Result<(), String> {
    let Some(topology) = resolve_remote_topology(ctx.deps.target)? else {
        return Err("remote activation stage reached without a remote topology".to_owned());
    };
    // The legacy target pins two public origins; the activation payload and
    // the deep readiness product layer both depend on them.
    if ctx.deps.target.site_origins.len() < 2 {
        return Err(format!(
            "remote target must record at least two site_origins (legacy ops/production-target.json siteOrigins); found {}",
            ctx.deps.target.site_origins.len()
        ));
    }
    let release_id = ctx.deps.release_id();
    let descriptor_hash = ctx
        .state
        .descriptor_sha256
        .clone()
        .ok_or("sealed descriptor hash is missing for the remote prepare")?;
    let release_set_hash = ctx
        .state
        .release_set_hash
        .clone()
        .ok_or("sealed release set hash is missing for the remote prepare")?;

    // Ship sequence (legacy prune-before-ship lives in stage 12 as
    // post-success cleanup per the 05 artifact rules): upload ONLY the
    // immutable source archive and the public descriptor. The remote prepare
    // then builds, tags, and candidate-validates the image ON THE VM, inside
    // the shipped immutable context.
    let ship_dir = ctx.deps.ship_dir();
    let archive = ship_dir.join("front.tar.gz");
    if !archive.is_file() {
        return Err(format!(
            "source archive {} from stage 5 is missing; refusing to ship",
            archive.display()
        ));
    }
    let descriptor_copy = stage5_descriptor_copy_path(ctx.deps);
    if !descriptor_copy.is_file() {
        return Err(format!(
            "public descriptor copy {} from stage 5 is missing; refusing to ship",
            descriptor_copy.display()
        ));
    }
    ctx.command_with_retries(
        executor,
        build_ship_scp_command(ctx.deps, "ship.scp-front-archive", &archive, &remote_front_archive_path(&release_id), &topology),
        SCP_MAX_ATTEMPTS,
    )?;
    ctx.command_with_retries(
        executor,
        build_ship_scp_command(ctx.deps, "ship.scp-agent-descriptor", &descriptor_copy, &remote_agent_descriptor_path(&release_id), &topology),
        SCP_MAX_ATTEMPTS,
    )?;
    let prepare_output = ctx.command(executor, build_remote_prepare_command(ctx.deps, &topology, &descriptor_hash, &release_set_hash))?;
    // The remote build reports the candidate image id; surface it as the
    // run's frontend image digest artifact (never a secret).
    if let Some(image_id) = parse_candidate_image_id(&prepare_output) {
        ctx.state.frontend_image_digest = Some(image_id.clone());
        ctx.note(format!("remote candidate image built and validated: {image_id}"));
    } else {
        ctx.note("remote prepare completed without a reported CANDIDATE_IMAGE; frontend_image_digest left unavailable".to_owned());
    }
    ctx.note(format!("remote candidate prepared at {}", remote_stage_dir(&topology, &release_id)));

    // Post-migration candidate ABI proof, then the public activation.
    for spec in build_stage9_commands(ctx.deps, &topology) {
        let id = spec.id.clone();
        let stdout = ctx.command(executor, spec)?;
        match id.as_str() {
            "remote.candidate-abi" => {
                verify_candidate_check_output(&stdout, "gcp_agent_v1_candidate_abi")
                    .map_err(|error| format!("remote candidate ABI verification failed: {error}"))?;
                ctx.note("remote candidate queue/outbox ABI verified in-container".to_owned());
            }
            "readiness.remote-web-healthz" => {
                verify_deep_observation(parse_deep_health(&stdout), &release_id)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn stage_deep_readiness(ctx: &mut StageCtx<'_>, executor: &dyn StageExecutor) -> Result<(), String> {
    // Process layer: the local daemon's metrics endpoint.
    ctx.command(executor, build_daemon_metrics_command(ctx.deps))?;

    // Dependency layer: every MCP endpoint — TCP + readiness endpoint, no
    // LLM calls.
    for (index, endpoint) in ctx.deps.target.mcp_endpoints.iter().enumerate() {
        ctx.probe(executor, build_mcp_probe(index, endpoint, ctx.deps.config.timeouts.mcp_ms)?)?;
        ctx.command(executor, build_mcp_ready_command(ctx.deps, index, endpoint)?)?;
    }

    // Product layer 1: web /api/healthz/deep with internal-key headers.
    let topology = resolve_remote_topology(ctx.deps.target)?;
    let deep = match &topology {
        Some(remote) => ctx.command(executor, build_web_deep_command_remote(ctx.deps, remote))?,
        None => ctx.command(executor, build_web_deep_command_local(ctx.deps))?,
    };
    verify_deep_observation(parse_deep_health(&deep), &ctx.deps.release_id())?;
    if let Some(observed) = parse_deep_health(&deep) {
        ctx.note(format!(
            "web deep health: deployment `{}`, agent_v1 admission `{}`",
            observed.deployment_id.unwrap_or_default(),
            observed.admission.unwrap_or_default()
        ));
    }

    // Product layer 1b: every public origin serves the new deployment id
    // (legacy `run_remote_deep_verify` public loop, from the operator host).
    for (index, origin) in ctx.deps.target.site_origins.iter().enumerate() {
        if topology.is_none() {
            break;
        }
        let stdout = ctx.command(executor, build_public_healthz_command(ctx.deps, index, origin))?;
        verify_deep_observation(parse_deep_health(&stdout), &ctx.deps.release_id())?;
        ctx.note(format!("public origin {origin} serves the new deployment id"));
    }

    // Product layer 2: the daemon's release-bound DB heartbeat.
    if let Some(spec) = build_db_heartbeat_command(ctx.deps) {
        let stdout = ctx.command(executor, spec)?;
        let parts: Vec<&str> = stdout.split('|').collect();
        if parts.len() != 4 {
            return Err(format!("db heartbeat row has the wrong shape: `{stdout}`"));
        }
        if parts[0] != ctx.deps.config.provider {
            return Err(format!(
                "db heartbeat provider `{}` does not match config provider `{}`",
                parts[0], ctx.deps.config.provider
            ));
        }
        if let Some(descriptor) = &ctx.state.descriptor_sha256
            && parts[1] != *descriptor
        {
            return Err(format!(
                "db heartbeat descriptor `{}` does not match the sealed descriptor `{descriptor}`",
                parts[1]
            ));
        }
        if parts[3] != "t" {
            return Err(format!("db heartbeat mcp_ready is `{}`, expected `t`", parts[3]));
        }
        ctx.note(format!("db heartbeat verified (release_set_hash {})", parts[2]));
    } else {
        ctx.note("target records no db_url_env handle; db heartbeat verification recorded none".to_owned());
    }
    Ok(())
}

fn stage_admission_open(ctx: &mut StageCtx<'_>, executor: &dyn StageExecutor) -> Result<(), String> {
    let topology = resolve_remote_topology(ctx.deps.target)?;
    let verify_id_remote = "readiness.admission-open-verify";
    let verify_id_local = "readiness.admission-open-verify-local";
    match &topology {
        Some(remote) => {
            for spec in build_admission_open_commands_remote(ctx.deps, remote) {
                let is_verify = spec.id == verify_id_remote;
                let stdout = ctx.command(executor, spec)?;
                if is_verify {
                    verify_deep_observation(parse_deep_health(&stdout), &ctx.deps.release_id())?;
                    if parse_deep_health(&stdout).and_then(|observation| observation.admission) != Some("open".to_owned()) {
                        return Err("deep health did not report agent_v1 admission `open`".to_owned());
                    }
                }
            }
        }
        None => {
            ctx.state.skipped.push(SkippedRecord {
                stage: crate::stages::STAGE_ADMISSION_OPEN.to_owned(),
                operation: Some("admission.open".to_owned()),
                reason: LOCAL_ONLY_SKIP_REASON.to_owned(),
            });
            upsert_env_value(&ctx.deps.config.runtime_env, ADMISSION_ENV_KEY, "open")
                .map_err(|error| format!("local admission upsert failed: {error}"))?;
            for spec in build_admission_open_commands_local(ctx.deps) {
                let is_verify = spec.id == verify_id_local;
                let stdout = ctx.command(executor, spec)?;
                if is_verify {
                    verify_deep_observation(parse_deep_health(&stdout), &ctx.deps.release_id())?;
                    if parse_deep_health(&stdout).and_then(|observation| observation.admission) != Some("open".to_owned()) {
                        return Err("local deep health did not report agent_v1 admission `open`".to_owned());
                    }
                }
            }
        }
    }
    ctx.note(format!("{ADMISSION_ENV_KEY} set to open and verified through deep health"));
    Ok(())
}

/// Stage 12: best-effort remote artifact cleanup BEFORE the terminal
/// receipt (05 artifact-cleanup rules). Cleanup failures never fail the
/// deploy — they are recorded in the stage receipt as notes.
fn stage_terminal_cleanup(ctx: &mut StageCtx<'_>, executor: &dyn StageExecutor) -> Result<(), String> {
    let topology = resolve_remote_topology(ctx.deps.target)?;
    match topology {
        Some(remote) => {
            for spec in build_stage12_cleanup_commands(ctx.deps, &remote) {
                ctx.best_effort_command(executor, spec);
            }
            ctx.note("remote artifact cleanup recorded (best-effort; bounded by exact project labels)".to_owned());
        }
        None => {
            ctx.state.skipped.push(SkippedRecord {
                stage: crate::stages::STAGE_TERMINAL_SUCCESS_RECEIPT.to_owned(),
                operation: Some("cleanup.remote-prune-stale".to_owned()),
                reason: LOCAL_ONLY_SKIP_REASON.to_owned(),
            });
        }
    }
    Ok(())
}

impl PipelineDeps<'_> {
    fn contract_required_procedures(&self) -> Vec<String> {
        crate::contract::load_contract(&self.config.frontend_contract)
            .map(|contract| contract.required_procedures)
            .unwrap_or_default()
    }

    fn contract_required_columns(&self) -> Vec<String> {
        crate::contract::load_contract(&self.config.frontend_contract)
            .map(|contract| contract.required_columns)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DeployTimeouts;
    use crate::stages::{CommandMode, RunContext};

    const FIXED_NOW: u64 = 1_786_843_200; // 2026-08-16T01:20:00Z

    fn trust_registry(active_key_id: &str) -> Vec<u8> {
        serde_jcs::to_vec(&krw_agent_release_authorization::ReleaseTrustRegistryV1 {
            schema_version: 1,
            registry_id: "krw.deploy.pipeline-test".to_owned(),
            minimum_sequence: 1,
            keys: vec![krw_agent_release_authorization::ReleaseTrustKeyV1 {
                key_id: active_key_id.to_owned(),
                ed25519_public_key_hex: "33".repeat(32),
                not_before_unix_seconds: 0,
                not_after_unix_seconds: 4_102_444_800,
                revoked: false,
            }],
        })
        .unwrap()
    }

    #[test]
    fn resolve_active_key_requires_exactly_one_valid_key() {
        let bytes = trust_registry("pipeline-active-key");
        assert_eq!(resolve_active_key_id(&bytes, FIXED_NOW).unwrap(), "pipeline-active-key");

        let mut revoked = krw_agent_release_authorization::parse_canonical_trust_registry(&bytes).unwrap();
        revoked.keys[0].revoked = true;
        let error = resolve_active_key_id(&serde_jcs::to_vec(&revoked).unwrap(), FIXED_NOW).unwrap_err();
        assert!(error.contains("no active signing key"), "error: {error}");

        let mut expired = krw_agent_release_authorization::parse_canonical_trust_registry(&bytes).unwrap();
        expired.keys[0].not_after_unix_seconds = FIXED_NOW - 1;
        let error = resolve_active_key_id(&serde_jcs::to_vec(&expired).unwrap(), FIXED_NOW).unwrap_err();
        assert!(error.contains("no active signing key"), "error: {error}");

        let mut dual = krw_agent_release_authorization::parse_canonical_trust_registry(&bytes).unwrap();
        dual.keys.push(krw_agent_release_authorization::ReleaseTrustKeyV1 {
            key_id: "second-key".to_owned(),
            ed25519_public_key_hex: "44".repeat(32),
            not_before_unix_seconds: 0,
            not_after_unix_seconds: 4_102_444_800,
            revoked: false,
        });
        let error = resolve_active_key_id(&serde_jcs::to_vec(&dual).unwrap(), FIXED_NOW).unwrap_err();
        assert!(error.contains("2 active signing keys"), "error: {error}");
    }

    #[test]
    fn env_value_and_upsert_follow_the_legacy_awk_semantics() {
        let dir = std::env::temp_dir().join(format!("krw-pipeline-env-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let env_file = dir.join("runtime.env");
        std::fs::write(&env_file, "# comment\nA=first\nB=\"quoted\"\nA=second\n").unwrap();
        assert_eq!(runtime_env_value(&env_file, "A").unwrap(), "second");
        assert_eq!(runtime_env_value(&env_file, "B").unwrap(), "quoted");
        assert!(runtime_env_value(&env_file, "MISSING").is_err());
        assert!(runtime_env_has_key(&env_file, "A"));
        assert!(!runtime_env_has_key(&env_file, "MISSING"));

        upsert_env_value(&env_file, "KRW_AGENT_ADMISSION_MODE", "closed").unwrap();
        let text = std::fs::read_to_string(&env_file).unwrap();
        assert!(text.contains("KRW_AGENT_ADMISSION_MODE=closed\n"), "text: {text}");
        assert_eq!(runtime_env_value(&env_file, "A").unwrap(), "second");

        // First occurrence replaced in place, duplicates dropped, order kept.
        upsert_env_value(&env_file, "A", "replaced").unwrap();
        let text = std::fs::read_to_string(&env_file).unwrap();
        assert_eq!(text.matches("A=").count(), 1, "text: {text}");
        assert!(text.contains("A=replaced"), "text: {text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn applied_migrations_parse_from_supabase_output() {
        let stdout = "Searching for migration files...\nApplying migration 0001_agent_v1...\nApplying migration 0022_daemon_mcp_readiness ...\n";
        assert_eq!(
            parse_applied_migrations(stdout),
            ["0001_agent_v1".to_owned(), "0022_daemon_mcp_readiness".to_owned()]
        );
        assert!(parse_applied_migrations("no migrations to apply").is_empty());
    }

    #[test]
    fn procedure_abi_parses_and_builds_the_verification_query() {
        let abi = parse_procedure_entry("agent_v1.enqueue_run(jsonb)").unwrap();
        assert_eq!(abi.schema, "agent_v1");
        assert_eq!(abi.name, "enqueue_run");
        assert_eq!(abi.arg_list, "jsonb");
        assert_eq!(abi.expected_output(), "1|jsonb");
        let sql = abi.sql();
        assert!(sql.contains("information_schema.routines"), "sql: {sql}");
        assert!(sql.contains("routine_schema = 'agent_v1'"), "sql: {sql}");
        assert!(sql.contains("routine_name = 'enqueue_run'"), "sql: {sql}");
        assert!(sql.contains("information_schema.parameters"), "sql: {sql}");

        let no_args = parse_procedure_entry("agent_v1.heartbeat_daemon()").unwrap();
        assert_eq!(no_args.expected_output(), "1|");

        assert!(parse_procedure_entry("enqueue_run(jsonb)").is_err());
        assert!(parse_procedure_entry("agent_v1.enqueue_run(jsonb").is_err());
        assert!(parse_procedure_entry("agent_v1.enq'ueue(jsonb)").is_err());
    }

    #[test]
    fn column_abi_defaults_to_public_schema() {
        let abi = parse_column_entry("agent_v1_daemon_heartbeats.mcp_ready").unwrap();
        assert_eq!(abi.schema, "public");
        assert_eq!(abi.table, "agent_v1_daemon_heartbeats");
        assert_eq!(abi.column, "mcp_ready");
        assert_eq!(abi.expected_output(), "1");
        assert!(abi.sql().contains("table_schema = 'public'"), "sql: {}", abi.sql());

        let explicit = parse_column_entry("agent_store.mutations.id").unwrap();
        assert_eq!(explicit.schema, "agent_store");
        assert!(parse_column_entry("single").is_err());
        assert!(parse_column_entry("a.b.c.d").is_err());
    }

    #[test]
    fn deep_health_observation_parses_and_admission_is_read() {
        let observed = parse_deep_health(
            r#"{"status":"ok","deployment_id":"rid","checks":{"agent_v1":{"admission":"closed"}}}"#,
        )
        .unwrap();
        assert_eq!(observed.status.as_deref(), Some("ok"));
        assert_eq!(observed.deployment_id.as_deref(), Some("rid"));
        assert_eq!(observed.admission.as_deref(), Some("closed"));
        assert!(parse_deep_health("not json").is_none());
    }

    // A tiny guard so the helper below cleans its tempdir up.
    struct TempDirGuard(PathBuf);
    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn deps_for(provider_cfg: bool) -> (TempDirGuard, Box<PipelineDeps<'static>>) {
        deps_for_transport(provider_cfg, false)
    }

    /// `gcp_transport = true` selects the production gcloud transport (gcp
    /// block); otherwise the target declares plain ssh without a gcp block.
    fn deps_for_transport(provider_cfg: bool, gcp_transport: bool) -> (TempDirGuard, Box<PipelineDeps<'static>>) {
        let root = std::env::temp_dir().join(format!(
            "krw-pipeline-deps-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(root.join("agent")).unwrap();
        std::fs::create_dir_all(root.join("front")).unwrap();
        std::fs::create_dir_all(root.join("operator/signing")).unwrap();
        std::fs::create_dir_all(root.join("operator/runtime")).unwrap();
        std::fs::create_dir_all(root.join("operator/ops")).unwrap();
        std::fs::write(
            root.join("operator/runtime/krw-agent-deploy.env"),
            "KRW_AGENT_DB_URL=postgresql://dummy\nKRW_AGENT_DATABASE_URL=postgresql://dummy-agent\n",
        )
        .unwrap();
        if provider_cfg {
            let config_dir = root.join("operator/config/deepseek");
            std::fs::create_dir_all(&config_dir).unwrap();
            std::fs::write(config_dir.join("deployment-binding.yaml"), "b: 1\n").unwrap();
            std::fs::write(config_dir.join("endpoint-registry.yaml"), "e: 1\n").unwrap();
            std::fs::write(config_dir.join("release-trust-registry.json"), trust_registry("deps-key")).unwrap();
        }
        std::fs::write(
            root.join("operator/ops/agent-v1-deployment-contract.json"),
            crate::contract::canonical_fixture_contract_json(),
        )
        .unwrap();

        let config = Box::leak(Box::new(DeployConfig {
            schema_version: 1,
            provider: "deepseek".to_owned(),
            agent_source_root: root.join("agent"),
            frontend_source_root: root.join("front"),
            operator_root: root.join("operator"),
            runtime_env: root.join("operator/runtime/krw-agent-deploy.env"),
            target_file: root.join("operator/ops/production-target.json"),
            frontend_contract: root.join("operator/ops/agent-v1-deployment-contract.json"),
            timeouts: DeployTimeouts {
                ssh_ms: 5_000,
                mcp_ms: 7_000,
                daemon_ready_ms: 90_000,
                public_ready_ms: 120_000,
            },
        }));
        let target = Box::leak(Box::new(TargetFile {
            provider: Some("deepseek".to_owned()),
            gcp: gcp_transport.then(|| crate::target::GcpTarget {
                project: "krw-prod-dummy".to_owned(),
                zone: "asia-northeast3-a".to_owned(),
                instance: "krw-agent-prod-dummy".to_owned(),
                instance_id: Some("0000000000000000000".to_owned()),
            }),
            ssh_host: (!gcp_transport).then(|| "deploy@127.0.0.1".to_owned()),
            remote_front_dir: Some("/home/deploy/krw-ontology-front".to_owned()),
            compose_project: Some("krw-ontology-front".to_owned()),
            local_ports: Vec::new(),
            required_env_keys: vec!["KRW_AGENT_DB_URL".to_owned()],
            db_url_env: Some("KRW_AGENT_DB_URL".to_owned()),
            db_abi: Some("agent_v1_v7".to_owned()),
            mcp_endpoints: vec!["127.0.0.1:18081".to_owned()],
            supabase_migration_plan: vec!["0001_agent_v1.sql".to_owned()],
            site_origins: vec![
                "https://one.example.com".to_owned(),
                "https://two.example.com".to_owned(),
            ],
            frontend_contract_sha256: None,
        }));
        let context = Box::leak(Box::new(RunContext {
            mode: CommandMode::Deploy,
            run_ts: "20260816T012000Z".to_owned(),
            run_id: "abcd1234".to_owned(),
            created_at_utc: "2026-08-16T01:20:00Z".to_owned(),
            config_sha256: "sha256:aa".to_owned(),
            output_dir: root.join("operator/releases/20260816T012000Z-abcd1234"),
            receipt_dir: root.join("operator/deploy-receipts/20260816T012000Z-abcd1234"),
        }));
        let deps = Box::new(PipelineDeps {
            config,
            config_path: Path::new("/nonexistent-config-for-argv-tests.json"),
            target,
            context,
            preflight_agent_head: "0123456789abcdef0123456789abcdef01234567",
            preflight_frontend_head: "0123456789abcdef0123456789abcdef01234567",
            now_unix_seconds: FIXED_NOW,
        });
        (TempDirGuard(root.clone()), deps)
    }

    #[test]
    fn stage3_build_argv_matches_the_legacy_script_surface() {
        let (_guard, deps) = deps_for(false);
        let spec = build_stage3_command(&deps);
        assert_eq!(spec.stage, "build");
        assert_eq!(spec.id, "build.dual-provider-bundles");
        assert_eq!(
            spec.argv,
            vec![
                "scripts/build_dual_provider_release.sh".to_owned(),
                "--output-root".to_owned(),
                deps.context.output_dir.display().to_string(),
            ]
        );
        assert_eq!(spec.cwd, deps.config.agent_source_root);
        assert_eq!(spec.env_keys_used, [RUNTIME_ENV_ALL.to_owned()]);
        assert_eq!(spec.timeout_ms, BUILD_TIMEOUT_MS);
    }

    #[test]
    fn stage4_seal_argv_ports_the_legacy_sign_sequence() {
        let (_guard, deps) = deps_for(true);
        let specs = build_stage4_commands(&deps, "the-active-key");
        let ids: Vec<&str> = specs.iter().map(|spec| spec.id.as_str()).collect();
        // Both provider bundles seal (legacy dual loop; the finalize verifier
        // requires the dual index). The non-selected provider goes first with
        // a suffixed id; the selected provider keeps the canonical ids.
        assert_eq!(
            ids,
            [
                "seal.prepare-production-candidate-glm",
                "seal.sign-release-authorization-glm",
                "seal.seal-production-candidate-glm",
                "seal.prepare-production-candidate",
                "seal.sign-release-authorization",
                "seal.seal-production-candidate",
                "seal.finalize-dual-release"
            ]
        );
        assert!(specs.iter().all(|spec| spec.stage == "seal"));
        assert!(specs.iter().all(|spec| spec.env_keys_used == [RUNTIME_ENV_ALL.to_owned()]));

        let prepare = &specs[3];
        assert_eq!(
            prepare.argv,
            vec![
                "scripts/prepare_production_candidate.sh".to_owned(),
                "--provider".to_owned(),
                "deepseek".to_owned(),
                "--bundle".to_owned(),
                deps.context.output_dir.join("deepseek").display().to_string(),
                "--binding".to_owned(),
                deps.config.operator_root.join("config/deepseek/deployment-binding.yaml").display().to_string(),
                "--endpoints".to_owned(),
                deps.config.operator_root.join("config/deepseek/endpoint-registry.yaml").display().to_string(),
            ]
        );

        let sign = &specs[4];
        assert_eq!(sign.argv[0], deps.context.output_dir.join("deepseek/bin/krw-agent").display().to_string());
        assert_eq!(&sign.argv[1..5], &["release", "sign", "--descriptor", &deps.context.output_dir.join("deepseek/public-release.json").display().to_string()]);
        // [5..7] --private-key <operator key>; [7..9] --key-id <active key>.
        assert_eq!(sign.argv[8], "the-active-key");
        assert_eq!(sign.argv[9], "--sequence");
        assert_eq!(sign.argv[10], FIXED_NOW.to_string());
        assert_eq!(sign.argv[12], FIXED_NOW.to_string());
        assert_eq!(sign.argv[14], (FIXED_NOW + AUTHORIZATION_TTL_SECONDS).to_string());
        assert_eq!(sign.argv[15], "--runtime-version");
        assert_eq!(sign.argv[16], RUNTIME_VERSION);
        assert_eq!(sign.argv[17], "--kernel-version");
        assert_eq!(sign.argv[18], KERNEL_VERSION);
        assert_eq!(sign.argv[19], "--out");
        assert_eq!(sign.argv[20], deps.config.operator_root.join("signing/releases/20260816T012000Z-abcd1234/release-authorization-deepseek.json").display().to_string());

        let seal = &specs[5];
        assert_eq!(
            seal.argv[1..3],
            [
                "--provider".to_owned(),
                "deepseek".to_owned(),
            ]
        );
        assert_eq!(seal.argv[seal.argv.len() - 1], deps.config.operator_root.join("config/deepseek/release-trust-registry.json").display().to_string());
        assert_eq!(&specs[6].argv[1..], &["--release-root", &deps.context.output_dir.display().to_string()]);
    }

    #[test]
    fn stage5_prepares_only_the_source_archive_and_descriptor_copy() {
        let (_guard, deps) = deps_for(false);
        let ship_dir = deps.ship_dir();

        let resolve = build_stage5_resolve_commit(&deps);
        assert_eq!(resolve.id, "frontend-image.resolve-commit");
        assert_eq!(resolve.argv, vec!["git".to_owned(), "-C".to_owned(), deps.config.frontend_source_root.display().to_string(), "rev-parse".to_owned(), "HEAD".to_owned()]);

        let archive = build_stage5_archive_source_command(&deps, "0123456789abcdef0123456789abcdef01234567");
        assert_eq!(archive.id, "frontend-image.archive-source");
        assert_eq!(archive.stage, "frontend_image_prepare");
        assert_eq!(&archive.argv[0..2], &["/bin/sh", "-c"]);
        let archive_script = &archive.argv[2];
        assert!(
            archive_script.contains(&format!(
                "git -C '{}' archive --format=tar '0123456789abcdef0123456789abcdef01234567' | gzip -n > '{}/front.tar.gz'",
                deps.config.frontend_source_root.display(),
                ship_dir.display()
            )),
            "script: {archive_script}"
        );
        assert!(archive.env_keys_used.is_empty());

        let descriptor = build_stage5_descriptor_copy_command(&deps);
        assert_eq!(descriptor.id, "frontend-image.descriptor-copy");
        let descriptor_script = &descriptor.argv[2];
        assert!(descriptor_script.contains(&format!("cp '{}' '{}/public-release.json'", deps.provider_bundle().join("public-release.json").display(), ship_dir.display())), "script: {descriptor_script}");
        assert!(descriptor_script.contains("chmod 0644"), "script: {descriptor_script}");
    }

    #[test]
    fn parse_candidate_image_id_reads_the_remote_prepare_report() {
        assert_eq!(
            parse_candidate_image_id("building...\nCANDIDATE_IMAGE=sha256:abc\nPrepared fast VM candidate: r1\n"),
            Some("sha256:abc".to_owned())
        );
        assert_eq!(parse_candidate_image_id("CANDIDATE_IMAGE=  "), None);
        assert_eq!(parse_candidate_image_id("no report"), None);
    }

    #[test]
    fn migration_and_abi_argv_use_the_front_cli_and_env_handle_placeholders() {
        let (_guard, deps) = deps_for(false);
        let rpc = build_stage6_rpc_contract_command(&deps);
        assert_eq!(rpc.id, "migrations.rpc-contract-verify");
        assert_eq!(rpc.argv[0], "node");
        assert!(rpc.argv[1].ends_with("scripts/verify-supabase-schema-smoke-rpc-contract.mjs"));

        let dry_run = build_stage6_dry_run_command(&deps, false);
        assert_eq!(dry_run.stage, "migrations");
        assert_eq!(dry_run.id, "migrations.db-push-dry-run");
        assert_eq!(&dry_run.argv[0..2], &["/bin/sh", "-c"]);
        let dry_run_script = &dry_run.argv[2];
        assert!(dry_run_script.contains("db push --dry-run"), "script: {dry_run_script}");
        // Legacy IPv6 pooler fallback is baked into the wrapper.
        assert!(dry_run_script.contains("IPv6 is not supported on your current network"), "script: {dry_run_script}");
        assert!(dry_run_script.contains("KRW_AGENT_DATABASE_URL:-${AGENT_V1_DATABASE_URL"), "script: {dry_run_script}");
        assert!(dry_run_script.contains("db push --db-url \"$pooler\""), "script: {dry_run_script}");
        assert_eq!(dry_run.env_keys_used, [RUNTIME_ENV_ALL.to_owned()]);

        let include_all = build_stage6_dry_run_command(&deps, true);
        assert_eq!(include_all.id, "migrations.db-push-dry-run-include-all");
        assert!(include_all.argv[2].contains("db push --include-all --dry-run"), "script: {}", include_all.argv[2]);

        let push = build_db_push_command(&deps, false);
        assert_eq!(push.stage, "admission_close"); // applied after admission close per 05 ordering
        assert_eq!(push.id, "migrations.db-push");
        assert!(push.argv[2].contains("db push"), "script: {}", push.argv[2]);
        let push_all = build_db_push_command(&deps, true);
        assert_eq!(push_all.id, "migrations.db-push-include-all");
        assert!(push_all.argv[2].contains("db push --include-all"), "script: {}", push_all.argv[2]);

        let abi = build_abi_command(&deps, "migrations.abi-verify-procedure-0", "select 1");
        assert_eq!(abi.stage, "admission_close");
        assert_eq!(abi.id, "migrations.abi-verify-procedure-0");
        assert_eq!(abi.argv[0], "psql");
        assert_eq!(abi.argv[1], "<env:KRW_AGENT_DB_URL>");
        assert_eq!(abi.argv[2], "-tA");
        assert_eq!(&abi.argv[3..5], &["-v", "ON_ERROR_STOP=1"]);
        assert_eq!(abi.argv[6], "select 1");
        assert_eq!(abi.env_keys_used, ["KRW_AGENT_DB_URL"]);
    }

    #[test]
    fn sealed_release_migration_and_verification_argv_match_the_legacy_surface() {
        let (_guard, deps) = deps_for(false);
        let handle = deps.agent_db_url_handle().unwrap();
        assert_eq!(handle, "KRW_AGENT_DATABASE_URL");

        let apply = build_agent_release_migrations_command(&deps, &handle);
        assert_eq!(apply.id, "migrations.agent-release-apply");
        assert_eq!(apply.stage, "admission_close");
        let apply_script = &apply.argv[2];
        // The URL is resolved INSIDE the sourced shell (the runtime env may
        // compute it dynamically), so no static `<env:...>` placeholder may
        // appear anywhere in the script.
        assert!(apply_script.contains("KRW_AGENT_DATABASE_URL:-${AGENT_V1_DATABASE_URL"), "script: {apply_script}");
        assert!(apply_script.contains("AGENT_V1_DATABASE_URL=\"$agent_database_url\""), "script: {apply_script}");
        assert!(apply_script.contains("AGENT_V1_PSQL_URL=\"$agent_database_url\""), "script: {apply_script}");
        assert!(!apply_script.contains("<env:"), "script: {apply_script}");
        assert!(apply_script.contains("supabase-root-2021-ca.crt"), "script: {apply_script}");
        assert_eq!(apply.argv[3], "apply-agent-v1-migrations");
        assert!(apply.argv[4].ends_with("scripts/apply-agent-v1-production-migrations.sh"));
        assert_eq!(apply.argv[5], "--agent-release");
        assert_eq!(apply.argv[6], deps.provider_bundle().display().to_string());
        assert_eq!(apply.argv[7], "--skip-front-migrations");
        assert_eq!(apply.env_keys_used, [RUNTIME_ENV_ALL.to_owned()]);

        let smoke = build_schema_smoke_command(&deps);
        assert_eq!(smoke.id, "migrations.schema-smoke");
        let smoke_script = &smoke.argv[2];
        assert!(smoke_script.contains("supabase-schema-smoke.mjs"), "script: {smoke_script}");
        assert!(smoke_script.contains("verify-agent-v1-compatibility.mjs"), "script: {smoke_script}");
        assert!(smoke_script.contains("AGENT_V1_DATABASE_CA_FILE="), "script: {smoke_script}");

        let (check_dir, overlay) = prepare_sealed_check_overlay(&deps, "sealed-release-database-check").unwrap();
        assert!(overlay.starts_with(check_dir.clone()), "overlay {overlay:?} under {check_dir:?}");
        let overlay_text = std::fs::read_to_string(&overlay).unwrap();
        assert!(overlay_text.contains("KRW_AGENT_WORKER_ID=sealed-release-database-check"), "overlay: {overlay_text}");
        assert!(overlay_text.contains("KRW_AGENT_PROVIDER=deepseek"), "overlay: {overlay_text}");
        let database_check = build_sealed_abi_command(&deps, SealedAbiMode::Database, &overlay).unwrap();
        assert_eq!(database_check.id, "migrations.sealed-database-abi");
        assert_eq!(database_check.stage, "admission_close");
        assert!(database_check.argv[0].ends_with("packaging/launchd/krw-agentd-start-local"));
        assert!(database_check.argv.contains(&"--database-check".to_owned()));
        assert!(database_check.argv.contains(&overlay.display().to_string()));
        let mcp_check = build_sealed_abi_command(&deps, SealedAbiMode::Mcp, &overlay).unwrap();
        assert_eq!(mcp_check.id, "activation.sealed-mcp-abi");
        assert_eq!(mcp_check.stage, "local_activation");
        assert!(mcp_check.argv.contains(&"--mcp-check".to_owned()));
        let _ = std::fs::remove_dir_all(&check_dir);
    }

    #[test]
    fn remote_admission_commands_wrap_the_legacy_payloads_over_batchmode_ssh() {
        let (_guard, deps) = deps_for(false);
        let topology = resolve_remote_topology(deps.target).unwrap().unwrap();
        let commands = build_admission_commands_remote(&deps, &topology);
        let ids: Vec<&str> = commands.iter().map(|command| command.id.as_str()).collect();
        assert_eq!(ids, ["admission.read-previous", "admission.close", "admission.verify-closed"]);
        for command in &commands {
            assert_eq!(&command.argv[0..2], &["ssh", "-o"]);
            assert_eq!(command.argv[2], "BatchMode=yes");
            assert_eq!(command.argv[4], format!("ConnectTimeout={}", (deps.config.timeouts.ssh_ms / 1000).max(1)));
            assert_eq!(command.argv[5], "deploy@127.0.0.1");
            assert!(command.env_keys_used.is_empty());
        }
        let read_previous = &commands[0];
        assert!(read_previous.argv[6].contains("KRW_AGENT_ADMISSION_MODE"), "payload: {}", read_previous.argv[6]);
        assert!(read_previous.argv[6].contains(".simple-deploy/current/runtime.env"));
        assert!(read_previous.argv[6].contains("tail -n 1"));
        let close = &commands[1];
        assert!(close.argv[6].contains("-v value=closed"), "payload: {}", close.argv[6]);
        assert!(close.argv[6].contains("up -d --no-deps --force-recreate --wait web agent-v1-outbox"));
        let verify = &commands[2];
        assert!(verify.argv[6].contains("/api/healthz"));
    }

    #[test]
    fn local_admission_commands_use_the_runtime_env_compose_stack() {
        let (_guard, deps) = deps_for(false);
        let commands = build_admission_commands_local(&deps);
        assert_eq!(commands[0].id, "admission.close-local");
        assert_eq!(
            commands[0].argv,
            vec![
                "docker".to_owned(),
                "compose".to_owned(),
                "--project-directory".to_owned(),
                deps.config.frontend_source_root.display().to_string(),
                "--env-file".to_owned(),
                deps.config.runtime_env.display().to_string(),
                "up".to_owned(),
                "-d".to_owned(),
                "--no-deps".to_owned(),
                "--force-recreate".to_owned(),
                "--wait".to_owned(),
                "web".to_owned(),
                "agent-v1-outbox".to_owned(),
            ]
        );
        assert_eq!(commands[1].id, "admission.verify-closed-local");
        assert_eq!(commands[1].argv[0], "curl");
        assert!(commands[1].argv.iter().any(|element| element.contains("/api/healthz")));
    }

    #[test]
    fn local_activation_ports_the_legacy_install_and_gateway_sequence() {
        let (_guard, deps) = deps_for(false);
        let overlay = deps.context.output_dir.join("overlay-mcp.env");
        let specs = build_stage8_commands(&deps, &overlay);
        let ids: Vec<&str> = specs.iter().map(|spec| spec.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "activation.agentd-stage",
                "activation.agentd-activate",
                "activation.capabilityd-activate",
                "activation.mcp-gateways-prepare",
                "activation.mcp-gateways-activate",
                "activation.sealed-mcp-abi",
                "activation.agentd-start",
            ]
        );
        for spec in &specs {
            if spec.id == "activation.sealed-mcp-abi" {
                assert_eq!(spec.timeout_ms, SEALED_ABI_TIMEOUT_MS, "spec {}", spec.id);
            } else {
                assert_eq!(spec.timeout_ms, INSTALL_TIMEOUT_MS, "spec {}", spec.id);
            }
        }
        for id in [
            "activation.agentd-stage",
            "activation.agentd-activate",
            "activation.capabilityd-activate",
            "activation.agentd-start",
        ] {
            assert!(
                specs.iter().any(|spec| spec.id == id && spec.env_keys_used.is_empty()),
                "{id} must not require KRW_AGENT_LOCAL_INSTALL_ROOT when the operator env omits it (installer default)"
            );
        }
        let agentd_stage = &specs[0];
        assert_eq!(
            agentd_stage.argv,
            vec![
                deps.provider_bundle().join("packaging/launchd/install-local-mac-agentd-release.sh").display().to_string(),
                "--mode".to_owned(),
                "stage".to_owned(),
                "--release-root".to_owned(),
                deps.context.output_dir.display().to_string(),
                "--release-id".to_owned(),
                "20260816T012000Z-abcd1234".to_owned(),
                "--provider".to_owned(),
                "deepseek".to_owned(),
            ]
        );
        // The daemon activation defers its start (legacy --defer-start) until
        // every MCP dependency is ready.
        let agentd_activate = &specs[1];
        assert!(agentd_activate.argv.contains(&"--defer-start".to_owned()));
        assert!(agentd_activate.argv.contains(&deps.config.runtime_env.display().to_string()));
        // No KRW_AGENT_METRICS_PORT in the fixture runtime env: no placeholder
        // on activate, literal default on start.
        assert!(!agentd_activate.argv.contains(&"--metrics-port".to_owned()));
        let gateways_prepare = &specs[3];
        assert_eq!(gateways_prepare.argv[0], "node");
        assert!(gateways_prepare.argv[1].ends_with("scripts/install-krw-agent-local-mcp-gateways.mjs"));
        assert_eq!(&gateways_prepare.argv[2..4], &["--mode".to_owned(), "prepare".to_owned()]);
        assert_eq!(gateways_prepare.argv[4], "--agent-bundle");
        assert_eq!(gateways_prepare.argv[5], deps.provider_bundle().display().to_string());
        assert_eq!(gateways_prepare.argv[6], "--operator-root");
        assert_eq!(gateways_prepare.argv[7], deps.config.operator_root.display().to_string());
        assert_eq!(&specs[4].argv[3], &"activate");
        let sealed_mcp = &specs[5];
        assert!(sealed_mcp.argv.contains(&overlay.display().to_string()));
        assert!(sealed_mcp.argv.contains(&"--mcp-check".to_owned()));
        let start = &specs[6];
        assert_eq!(&start.argv[1..3], &["--mode", "start"]);
        assert!(start.argv.contains(&"--metrics-port".to_owned()));
        assert!(start.argv.contains(&DEFAULT_METRICS_PORT.to_owned()));
    }

    #[test]
    fn runtime_env_candidate_activation_is_gated_on_the_staged_key() {
        // Default fixture env has no candidate: the command must NOT appear.
        let (_guard, deps) = deps_for(false);
        let overlay = deps.context.output_dir.join("overlay.env");
        let ids = build_stage8_commands(&deps, &overlay)
            .into_iter()
            .map(|spec| spec.id)
            .collect::<Vec<_>>();
        assert!(!ids.contains(&"activation.runtime-env-candidate".to_owned()), "ids: {ids:?}");

        // Declaring both keys materializes the legacy forward activation.
        let (guard, deps) = deps_for(false);
        std::fs::write(
            &deps.config.runtime_env,
            "KRW_AGENT_DATABASE_URL=postgresql://dummy-agent\nKRW_AGENT_RUNTIME_ENV_CANDIDATE=/abs/candidate.env\nKRW_AGENT_RUNTIME_ENV_FILE=/abs/runtime.env\n",
        )
        .unwrap();
        let specs = build_stage8_commands(&deps, &overlay);
        let candidate = specs
            .iter()
            .find(|spec| spec.id == "activation.runtime-env-candidate")
            .expect("runtime env candidate command");
        assert!(candidate.argv[1].ends_with("scripts/activate-forward-runtime-env.sh"));
        assert_eq!(&candidate.argv[2..], &["--candidate", "<env:KRW_AGENT_RUNTIME_ENV_CANDIDATE>", "--destination", "<env:KRW_AGENT_RUNTIME_ENV_FILE>"]);
        // The daemon env file follows the operator envelope pointer.
        let agentd_activate = specs.iter().find(|spec| spec.id == "activation.agentd-activate").unwrap();
        let env_position = agentd_activate.argv.iter().position(|flag| flag == "--env-file").unwrap();
        assert_eq!(agentd_activate.argv[env_position + 1], "<env:KRW_AGENT_RUNTIME_ENV_FILE>");
        drop(guard);
    }

    #[test]
    fn metrics_port_placeholder_replaces_the_default_when_declared() {
        let (guard, deps) = deps_for(false);
        std::fs::write(
            &deps.config.runtime_env,
            "KRW_AGENT_DATABASE_URL=postgresql://dummy\nKRW_AGENT_METRICS_PORT=15599\n",
        )
        .unwrap();
        let overlay = deps.context.output_dir.join("overlay.env");
        let specs = build_stage8_commands(&deps, &overlay);
        let activate = specs.iter().find(|spec| spec.id == "activation.agentd-activate").unwrap();
        let position = activate.argv.iter().position(|flag| flag == "--metrics-port").unwrap();
        assert_eq!(activate.argv[position + 1], "<env:KRW_AGENT_METRICS_PORT>");

        let metrics = build_daemon_metrics_command(&deps);
        assert!(metrics.argv[4].contains("http://127.0.0.1:<env:KRW_AGENT_METRICS_PORT>/metrics"));
        drop(guard);
    }

    #[test]
    fn stage9_ships_source_archive_and_descriptor_only_and_remote_builds() {
        let (_guard, deps) = deps_for(false);
        let topology = resolve_remote_topology(deps.target).unwrap().unwrap();
        let release_id = deps.release_id();

        let scp = build_ship_scp_command(
            &deps,
            "ship.scp-front-archive",
            &deps.ship_dir().join("front.tar.gz"),
            &remote_front_archive_path(&release_id),
            &topology,
        );
        assert_eq!(scp.id, "ship.scp-front-archive");
        assert_eq!(
            scp.argv,
            vec![
                "scp".to_owned(),
                "-o".to_owned(),
                "BatchMode=yes".to_owned(),
                "-o".to_owned(),
                "ConnectTimeout=5".to_owned(),
                deps.ship_dir().join("front.tar.gz").display().to_string(),
                format!("deploy@127.0.0.1:/tmp/krw-front-{release_id}.tar.gz"),
            ]
        );
        let descriptor_scp = build_ship_scp_command(
            &deps,
            "ship.scp-agent-descriptor",
            &stage5_descriptor_copy_path(&deps),
            &remote_agent_descriptor_path(&release_id),
            &topology,
        );
        assert_eq!(descriptor_scp.id, "ship.scp-agent-descriptor");
        assert!(descriptor_scp.argv[5].ends_with("ship/public-release.json"), "argv: {:?}", descriptor_scp.argv);

        let prepare = build_remote_prepare_command(
            &deps,
            &topology,
            "sha256:abcd",
            "sha256:eff0",
        );
        assert_eq!(prepare.id, "ship.remote-prepare");
        let payload = &prepare.argv[6];
        assert!(payload.contains(&format!("REMOTE_STAGE='{stage}'", stage = remote_stage_dir(&topology, &release_id))), "payload: {payload}");
        assert!(payload.contains("test ! -e \"$REMOTE_STAGE\""), "payload: {payload}");
        // NO image is shipped or loaded: the VM builds inside the shipped
        // immutable context (legacy run_remote_prepare verbatim).
        assert!(!payload.contains("docker load"), "payload must not load an image: {payload}");
        assert!(payload.contains("install -m 0644 \"$REMOTE_AGENT_DESCRIPTOR\" \"$REMOTE_STAGE/agent-release/public-release.json\""), "payload: {payload}");
        assert!(payload.contains("test \"$observed_descriptor_hash\" = \"$AGENT_DESCRIPTOR_HASH\""), "payload: {payload}");
        assert!(payload.contains("KRW_AGENT_RELEASE_SET_HASH"), "payload: {payload}");
        assert!(payload.contains("KRW_AGENT_BACKEND_MODE rust"), "payload: {payload}");
        assert!(payload.contains("KRW_AGENT_ADMISSION_MODE closed"), "payload: {payload}");
        // The remote build sequence: OLD_IMAGE capture is present, the stage
        // compose builds the web image, the candidate is tagged from :local,
        // and the in-container candidate preflight runs in the same payload.
        assert!(payload.contains("OLD_WEB=$(docker compose --project-name \"$COMPOSE_PROJECT\" --project-directory \"$REMOTE_FRONT_DIR\""), "payload: {payload}");
        assert!(payload.contains("compose_stage build web"), "payload: {payload}");
        assert!(payload.contains("CANDIDATE_TAG=\"krw-ontology-front-web:candidate-$RELEASE_ID\""), "payload: {payload}");
        assert!(payload.contains("CANDIDATE_IMAGE=$(docker image inspect --format '{{.Id}}' krw-ontology-front-web:local)"), "payload: {payload}");
        assert!(payload.contains("docker tag \"$CANDIDATE_IMAGE\" \"$CANDIDATE_TAG\""), "payload: {payload}");
        assert!(payload.contains("compose_stage run --rm --no-deps -T --entrypoint node web - <<'NODE'"), "payload: {payload}");
        assert!(payload.contains("gcp_agent_v1_candidate_preflight"), "payload: {payload}");
        assert!(payload.contains("select 1 as supabase_tls"), "payload: {payload}");
        assert!(payload.contains("descriptor.entries.every"), "payload: {payload}");
        assert!(payload.contains("NODE\n"), "payload: {payload}");
        // The old image is pinned under prev-<release> before the stage
        // build (BuildKit GC) and restored to the stable local tag after.
        assert!(payload.contains("OLD_TAG=\"krw-ontology-front-web:prev-$RELEASE_ID\""), "payload: {payload}");
        assert!(payload.contains("docker tag \"$OLD_IMAGE\" \"$OLD_TAG\""), "payload: {payload}");
        assert!(payload.contains("docker tag \"$OLD_TAG\" krw-ontology-front-web:local"), "payload: {payload}");
        assert!(payload.contains("forward-activation.env"), "payload: {payload}");
        assert!(payload.contains("printf 'CANDIDATE_IMAGE=%s\\n' \"$CANDIDATE_IMAGE\""), "payload: {payload}");
        // No unresolved render tokens.
        for token in ["@IMAGE_ARCHIVE@", "@PREFLIGHT_JS@", "@HELPERS@", "@TENANT@"] {
            assert!(!payload.contains(token), "unresolved token {token}: {payload}");
        }
    }

    #[test]
    fn remote_compose_probes_read_the_stage_runtime_env_file() {
        // Regression: the in-container probes once rendered the stage
        // DIRECTORY as the compose --env-file (docker compose then fails
        // with "X is a directory"), and before that a literal 'unused-env'.
        // Every probe must resolve the same stage runtime.env the web-up
        // compose_stage uses, under a bounded retry budget.
        let (_guard, deps) = deps_for(false);
        let topology = resolve_remote_topology(deps.target).unwrap().unwrap();
        let release_id = deps.release_id();
        for (name, payload) in [
            ("healthz", payload_remote_web_healthz(&topology, &release_id)),
            ("deep", payload_remote_web_deep(&topology, &release_id)),
            ("verify_admission", payload_verify_admission(&topology, &release_id, "open")),
        ] {
            assert!(
                payload.contains("--env-file \"$REMOTE_STAGE/runtime.env\""),
                "{name} probe must read the stage runtime.env: {payload}"
            );
            assert!(
                payload.contains("while [ \"$attempt\" -le 20 ]"),
                "{name} probe must retry on a bounded budget: {payload}"
            );
        }
    }

    #[test]
    fn remote_prepare_payload_takes_the_declared_tenant_over_the_default() {
        let (_guard, deps) = deps_for(false);
        let topology = resolve_remote_topology(deps.target).unwrap().unwrap();
        let default_prepare = build_remote_prepare_command(&deps, &topology, "sha256:abcd", "sha256:eff0");
        assert!(default_prepare.argv[6].contains("agent_tenant_id='krw-ontology-prod'"), "payload: {}", default_prepare.argv[6]);

        let (guard, deps) = deps_for(false);
        std::fs::write(
            &deps.config.runtime_env,
            "KRW_AGENT_DATABASE_URL=postgresql://dummy-agent\nKRW_AGENT_TENANT_ID=custom-tenant\n",
        )
        .unwrap();
        let custom = build_remote_prepare_command(&deps, &topology, "sha256:abcd", "sha256:eff0");
        assert!(custom.argv[6].contains("agent_tenant_id='custom-tenant'"), "payload: {}", custom.argv[6]);
        drop(guard);
    }

    #[test]
    fn stage9_candidate_abi_and_web_up_payloads_match_the_legacy_activate() {
        let (_guard, deps) = deps_for(false);
        let topology = resolve_remote_topology(deps.target).unwrap().unwrap();
        let specs = build_stage9_commands(&deps, &topology);
        let ids: Vec<&str> = specs.iter().map(|spec| spec.id.as_str()).collect();
        assert_eq!(ids, ["remote.candidate-abi", "remote.web-up", "readiness.remote-web-healthz"]);

        let candidate_abi = &specs[0];
        let abi_payload = &candidate_abi.argv[6];
        assert!(abi_payload.contains("compose_stage run --rm --no-deps -T --entrypoint node web -e"), "payload: {abi_payload}");
        assert!(abi_payload.contains("agent_v1.enqueue_run(jsonb)"), "payload: {abi_payload}");
        assert!(abi_payload.contains("agent_v1_daemon_heartbeats"), "payload: {abi_payload}");
        assert!(abi_payload.contains("agent_v1_product_outbox_receipts"), "payload: {abi_payload}");
        assert!(abi_payload.contains("gcp_agent_v1_candidate_abi"), "payload: {abi_payload}");

        let web_up = &specs[1];
        let up_payload = &web_up.argv[6];
        assert!(up_payload.contains("forward-activation.env"), "payload: {up_payload}");
        assert!(up_payload.contains("SERVICES=\"web reverse-proxy agent-v1-outbox llm-gateway filings-mcp document-worker document-outbox-dispatcher filing-notification-worker filing-brief-worker billing-worker\""), "payload: {up_payload}");
        assert!(up_payload.contains("docker tag \"$CANDIDATE_IMAGE\" krw-ontology-front-web:local"), "payload: {up_payload}");
        assert!(up_payload.contains("label=com.docker.compose.service=ontology-mcp"), "payload: {up_payload}");
        assert!(!up_payload.contains("@ORIGIN_ONE@") && !up_payload.contains("@RELEASE_ID@"), "unresolved tokens: {up_payload}");
        assert!(up_payload.contains("'https://one.example.com' 'https://two.example.com'"), "payload: {up_payload}");
        assert!(up_payload.contains("market-web-source-worker"), "payload: {up_payload}");
        assert!(up_payload.contains("MARKET_ISSUE_X_INGESTION_ENABLED"), "payload: {up_payload}");
        assert!(up_payload.contains("ln -s \"$REMOTE_STAGE\" \"$REMOTE_FRONT_DIR/.simple-deploy/current\""), "payload: {up_payload}");

        let healthz = &specs[2];
        assert_eq!(healthz.id, "readiness.remote-web-healthz");
        assert!(healthz.argv[6].contains("RELEASE_ID='20260816T012000Z-abcd1234'"));
        assert!(healthz.argv[6].contains("-e EXPECTED_RELEASE_ID=\"$RELEASE_ID\""));
        assert!(healthz.argv[6].contains("deployment_id"));
        assert!(healthz.argv[6].contains(&remote_stage_dir(&topology, "20260816T012000Z-abcd1234")), "payload: {}", healthz.argv[6]);
    }

    #[test]
    fn readiness_layer_commands_carry_the_configured_timeouts() {
        let (_guard, deps) = deps_for(false);
        let metrics = build_daemon_metrics_command(&deps);
        assert_eq!(metrics.id, "readiness.daemon-metrics");
        assert_eq!(metrics.argv[3], "90"); // daemon_ready_ms -> seconds
        assert_eq!(metrics.timeout_ms, deps.config.timeouts.daemon_ready_ms);
        assert!(metrics.argv[4].ends_with("/metrics"));

        let probe = build_mcp_probe(0, "127.0.0.1:18081", deps.config.timeouts.mcp_ms).unwrap();
        assert_eq!(probe.id, "readiness.mcp-tcp-0");
        assert_eq!(probe.host, "127.0.0.1");
        assert_eq!(probe.port, 18081);
        assert_eq!(probe.timeout_ms, 7_000);
        assert!(build_mcp_probe(0, "no-port", 1_000).is_err());

        let ready = build_mcp_ready_command(&deps, 0, "127.0.0.1:18081").unwrap();
        assert_eq!(ready.id, "readiness.mcp-ready-0");
        assert_eq!(ready.argv[4], "http://127.0.0.1:18081/readyz"); // loopback -> http
        let tls_ready = build_mcp_ready_command(&deps, 1, "mcp.example.com:9443").unwrap();
        assert_eq!(tls_ready.argv[4], "https://mcp.example.com:9443/readyz");

        let topology = resolve_remote_topology(deps.target).unwrap().unwrap();
        let deep = build_web_deep_command_remote(&deps, &topology);
        assert_eq!(deep.id, "readiness.web-deep");
        assert!(deep.argv[6].contains("healthz/deep"));
        assert!(deep.argv[6].contains("INTERNAL_API_KEY"));
        assert!(deep.argv[6].contains("NEXT_PUBLIC_APP_VERSION"));

        let local_deep = build_web_deep_command_local(&deps);
        assert_eq!(local_deep.env_keys_used, [INTERNAL_API_KEY_ENV_KEY.to_owned()]);
        assert_eq!(local_deep.argv[3], "120"); // public_ready_ms -> seconds
        assert!(local_deep.argv.iter().any(|element| element.starts_with("x-internal-key: <env:")));

        let heartbeat = build_db_heartbeat_command(&deps).unwrap();
        assert_eq!(heartbeat.id, "readiness.db-heartbeat");
        assert_eq!(heartbeat.argv[1], "<env:KRW_AGENT_DB_URL>");
        assert!(heartbeat.argv[6].contains("agent_v1_daemon_heartbeats"));
        assert!(heartbeat.argv[6].contains("mcp_ready"));
    }

    #[test]
    fn admission_open_payload_verifies_closed_first_and_re_closes_on_failure() {
        let (_guard, deps) = deps_for(false);
        let topology = resolve_remote_topology(deps.target).unwrap().unwrap();
        let commands = build_admission_open_commands_remote(&deps, &topology);
        assert_eq!(commands[0].id, "admission.open");
        let open_payload = &commands[0].argv[6];
        assert!(open_payload.contains("test \"$(value_of \"$e\" KRW_AGENT_ADMISSION_MODE)\" = \"closed\""), "payload: {open_payload}");
        assert!(open_payload.contains("verify_deep_admission closed"), "payload: {open_payload}");
        assert!(open_payload.contains("upsert_env_value \"$e\" KRW_AGENT_ADMISSION_MODE open"), "payload: {open_payload}");
        assert!(open_payload.contains("re_close"), "payload: {open_payload}");
        assert!(open_payload.contains("verify_deep_admission open"), "payload: {open_payload}");
        // The opener works on the STAGE runtime env, not the previous release.
        assert!(open_payload.contains(&remote_stage_dir(&topology, &deps.release_id())), "payload: {open_payload}");
        assert_eq!(commands[1].id, "readiness.admission-open-verify");
        assert!(commands[1].argv[6].contains("'open'"), "payload: {}", commands[1].argv[6]);

        let local = build_admission_open_commands_local(&deps);
        assert_eq!(local[0].id, "admission.open-local");
        assert_eq!(local[1].id, "readiness.admission-open-verify-local");
        assert_eq!(local[1].env_keys_used, [INTERNAL_API_KEY_ENV_KEY.to_owned()]);
    }

    #[test]
    fn stage12_cleanup_payloads_prune_with_exact_labels_and_bounds() {
        let (_guard, deps) = deps_for(false);
        let topology = resolve_remote_topology(deps.target).unwrap().unwrap();
        let commands = build_stage12_cleanup_commands(&deps, &topology);
        assert_eq!(
            commands.iter().map(|command| command.id.as_str()).collect::<Vec<_>>(),
            ["cleanup.remote-prune-stale", "cleanup.remote-prune-post-activation"]
        );
        assert!(commands.iter().all(|command| command.stage == "terminal_success_receipt"));
        let stale = &commands[0].argv[6];
        assert!(stale.contains("Refusing to prune an unexpected deployment path"), "payload: {stale}");
        assert!(stale.contains("[ \"$candidate\" = \"$active_release\" ] && continue"), "payload: {stale}");
        assert!(stale.contains("rm -rf -- \"$candidate\""), "payload: {stale}");
        assert!(stale.contains("candidate-*|release-*|prev-*|rollback-*"), "payload: {stale}");
        assert!(stale.contains("[ \"$repository\" = \"krw-ontology-front-web\" ]"), "payload: {stale}");
        assert!(stale.contains("[ \"$repository\" = \"caddy\" ]"), "payload: {stale}");
        let post = &commands[1].argv[6];
        assert!(post.contains("docker image rm \"krw-ontology-front-web:candidate-$RELEASE_ID\""), "payload: {post}");
        assert!(post.contains("docker builder prune --all --force --max-used-space 12GB"), "payload: {post}");
    }

    #[test]
    fn remote_topology_selects_transport_and_defaults_the_compose_project() {
        let (_guard, deps) = deps_for(false);
        let topology = resolve_remote_topology(deps.target).unwrap().unwrap();
        assert_eq!(
            topology.transport,
            RemoteTransport::Ssh { host: "deploy@127.0.0.1".to_owned() }
        );
        assert_eq!(topology.compose_project, "krw-ontology-front");

        let (_guard, deps) = deps_for_transport(false, true);
        let topology = resolve_remote_topology(deps.target).unwrap().unwrap();
        assert_eq!(
            topology.transport,
            RemoteTransport::Gcloud {
                instance: "krw-agent-prod-dummy".to_owned(),
                project: "krw-prod-dummy".to_owned(),
                zone: "asia-northeast3-a".to_owned(),
            }
        );

        let mut local = TargetFile {
            provider: Some("deepseek".to_owned()),
            gcp: None,
            ssh_host: None,
            remote_front_dir: None,
            compose_project: None,
            local_ports: Vec::new(),
            required_env_keys: Vec::new(),
            db_url_env: None,
            db_abi: None,
            mcp_endpoints: Vec::new(),
            supabase_migration_plan: Vec::new(),
            site_origins: Vec::new(),
            frontend_contract_sha256: None,
        };
        assert!(resolve_remote_topology(&local).unwrap().is_none());

        // ssh_host without a gcp block and without remote_front_dir fails.
        local.ssh_host = Some("deploy@host".to_owned());
        let error = resolve_remote_topology(&local).unwrap_err();
        assert!(error.contains("remote_front_dir"), "error: {error}");

        // A gcp block with empty fields fails closed before any transport.
        local.gcp = Some(crate::target::GcpTarget {
            project: String::new(),
            zone: "asia-northeast3-a".to_owned(),
            instance: "krw-agent-prod-dummy".to_owned(),
            instance_id: None,
        });
        let error = resolve_remote_topology(&local).unwrap_err();
        assert!(error.contains("non-empty project, zone, and instance"), "error: {error}");

        local.gcp.as_mut().unwrap().project = "krw-prod".to_owned();
        local.remote_front_dir = Some("/srv/some-front".to_owned());
        let topology = resolve_remote_topology(&local).unwrap().unwrap();
        assert_eq!(topology.compose_project, "some-front");
        assert!(matches!(topology.transport, RemoteTransport::Gcloud { .. }));
    }

    #[test]
    fn gcp_transport_targets_issue_gcloud_compute_ssh_and_scp_argv() {
        let (_guard, deps) = deps_for_transport(false, true);
        let topology = resolve_remote_topology(deps.target).unwrap().unwrap();
        let expected_prefix = vec![
            "gcloud".to_owned(),
            "compute".to_owned(),
            "ssh".to_owned(),
            "krw-agent-prod-dummy".to_owned(),
            "--project".to_owned(),
            "krw-prod-dummy".to_owned(),
            "--zone".to_owned(),
            "asia-northeast3-a".to_owned(),
            "--command".to_owned(),
        ];
        let commands = build_admission_commands_remote(&deps, &topology);
        assert_eq!(&commands[0].argv[..9], &expected_prefix[..]);
        assert!(commands[0].argv[9].contains("KRW_AGENT_ADMISSION_MODE"), "payload: {}", commands[0].argv[9]);
        assert!(commands[0].argv[9].contains(".simple-deploy/current/runtime.env"));
        assert!(commands[1].argv[9].contains("-v value=closed"), "payload: {}", commands[1].argv[9]);
        assert_eq!(commands[1].argv.len(), 10, "one payload element after the gcloud prefix");

        let stage9 = build_stage9_commands(&deps, &topology);
        for command in &stage9 {
            assert_eq!(&command.argv[..9], &expected_prefix[..], "command {}", command.id);
            assert_eq!(command.argv.len(), 10, "one payload element: {}", command.id);
        }
        assert!(stage9[1].argv[9].contains("up -d --force-recreate --wait $SERVICES"));
        let web_up = stage9.iter().find(|command| command.id == "remote.web-up").unwrap();
        assert!(web_up.argv[9].contains("docker tag \"$CANDIDATE_IMAGE\" krw-ontology-front-web:local"));

        // Uploads ride the legacy gcloud scp flags.
        let scp = build_ship_scp_command(
            &deps,
            "ship.scp-front-archive",
            &deps.ship_dir().join("front.tar.gz"),
            &remote_front_archive_path(&deps.release_id()),
            &topology,
        );
        assert_eq!(
            scp.argv,
            vec![
                "gcloud".to_owned(),
                "compute".to_owned(),
                "scp".to_owned(),
                "--project".to_owned(),
                "krw-prod-dummy".to_owned(),
                "--zone".to_owned(),
                "asia-northeast3-a".to_owned(),
                "--scp-flag=-oServerAliveInterval=15".to_owned(),
                "--scp-flag=-oServerAliveCountMax=8".to_owned(),
                deps.ship_dir().join("front.tar.gz").display().to_string(),
                format!("krw-agent-prod-dummy:/tmp/krw-front-{}.tar.gz", deps.release_id()),
            ]
        );

        let deep = build_web_deep_command_remote(&deps, &topology);
        assert_eq!(&deep.argv[..9], &expected_prefix[..]);
        assert!(deep.argv[9].contains("healthz/deep"));

        let open = build_admission_open_commands_remote(&deps, &topology);
        assert_eq!(&open[1].argv[..9], &expected_prefix[..]);
        assert!(open[1].argv[9].contains("'open'"), "payload: {}", open[1].argv[9]);
        // No ssh-style options leak into the gcloud transport.
        for command in commands.iter().chain(stage9.iter()).chain(open.iter()) {
            assert!(!command.argv.contains(&"BatchMode=yes".to_owned()));
            assert!(!command.argv.iter().any(|element| element.starts_with("ConnectTimeout=")));
        }
    }

    #[test]
    fn gcp_block_wins_over_ssh_host_when_both_are_declared() {
        let (_guard, deps) = deps_for(false);
        // Rebuild the target with BOTH a gcp block and an ssh_host: the
        // production transport selection must still be gcloud.
        let both = TargetFile {
            gcp: Some(crate::target::GcpTarget {
                project: "krw-prod-dummy".to_owned(),
                zone: "asia-northeast3-a".to_owned(),
                instance: "krw-agent-prod-dummy".to_owned(),
                instance_id: None,
            }),
            ssh_host: Some("deploy@ignored".to_owned()),
            remote_front_dir: deps.target.remote_front_dir.clone(),
            compose_project: deps.target.compose_project.clone(),
            ..(*deps.target).clone()
        };
        let topology = resolve_remote_topology(&both).unwrap().unwrap();
        assert!(matches!(topology.transport, RemoteTransport::Gcloud { .. }));
    }

    #[test]
    fn previous_admission_normalizes_unknown_values_to_closed() {
        assert_eq!(normalize_previous_admission("open"), "open");
        assert_eq!(normalize_previous_admission(" canary "), "canary");
        assert_eq!(normalize_previous_admission("bogus"), "closed");
        assert_eq!(normalize_previous_admission(""), "closed");
    }

    #[test]
    fn fixture_executor_records_specs_and_simulates_build_side_effects() {
        let (guard, deps) = deps_for(false);
        let mut fixture = FixtureStageExecutor::passing();
        fixture.failures.insert("seal.prepare-production-candidate".to_owned(), "injected".to_owned());
        let build = build_stage3_command(&deps);
        let outcome = fixture.execute(&deps.config.runtime_env, &build).unwrap();
        assert!(outcome.stdout.is_empty());
        assert!(
            deps.context.output_dir.join("glm/release-manifest.json").is_file(),
            "fixture must simulate the built bundles"
        );
        assert!(
            deps.context.output_dir.join("deepseek/release-manifest.json").is_file(),
            "fixture must simulate the built bundles"
        );
        assert!(
            deps.context.output_dir.join("dual-release-index.json").is_file(),
            "fixture must simulate the dual release index"
        );
        let topology = resolve_remote_topology(deps.target).unwrap().unwrap();
        let prepare = build_stage4_commands(&deps, "k")
            .into_iter()
            .find(|spec| spec.id == "seal.prepare-production-candidate")
            .expect("selected provider prepare command");
        let error = fixture.execute(&deps.config.runtime_env, &prepare).unwrap_err();
        assert_eq!(error, "injected");
        let read_previous = build_admission_commands_remote(&deps, &topology).remove(0);
        let outcome = fixture.execute(&deps.config.runtime_env, &read_previous).unwrap();
        assert_eq!(outcome.stdout, "open");
        assert_eq!(fixture.command_ids(), vec!["build.dual-provider-bundles", "seal.prepare-production-candidate", "admission.read-previous"]);
        drop(guard);
    }
}
