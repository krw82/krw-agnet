//! Ordered read-only preflight check registry (doc/refetoring/05).
//!
//! The registry runs the 05-doc preflight list in a fixed order with stable
//! check ids. Filesystem checks read directly (testable against tempdir
//! fixtures); world-observing checks go through [`PreflightExecutor`]. No
//! check ever mutates files, env, gateway, launchd, DB, or a remote host.

use std::collections::{BTreeMap, BTreeSet};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

use crate::config::{DeployConfig, REQUIRED_COMMANDS};
use crate::contract::{self, ContractError};
use crate::executor::PreflightExecutor;
use crate::hashing::{UNAVAILABLE_HASH, sha256_file_or_unavailable};
use crate::target::{TargetError, TargetFile};

pub const CHECK_CONFIG_SCHEMA: &str = "config-schema";
pub const CHECK_TARGET_EXPLICIT_PROVIDER: &str = "target-explicit-provider";
pub const CHECK_AGENT_SOURCE_CLEAN_COMMIT: &str = "agent-source-clean-commit";
pub const CHECK_FRONTEND_SOURCE_CLEAN_COMMIT: &str = "frontend-source-clean-commit";
pub const CHECK_OUTPUT_DIR_ABSENT: &str = "output-dir-absent";
pub const CHECK_REQUIRED_COMMANDS: &str = "required-commands";
pub const CHECK_FRONTEND_CONTRACT_HASH: &str = "frontend-contract-hash";
pub const CHECK_SIGNING_TRUST_VALIDITY: &str = "signing-trust-validity";
pub const CHECK_PORT_OWNERSHIP: &str = "port-ownership";
pub const CHECK_GCP_TARGET_DESCRIBABLE: &str = "gcp-target-describable";
pub const CHECK_SSH_REMOTE_REACHABLE: &str = "ssh-remote-reachable";
pub const CHECK_REMOTE_ENV_REQUIRED_KEYS: &str = "remote-env-required-keys";
pub const CHECK_SUPABASE_MIGRATION_PLAN: &str = "supabase-migration-plan";
pub const CHECK_DB_AGENT_V1_ABI: &str = "db-agent-v1-abi";
pub const CHECK_MCP_ENDPOINT_REACHABLE: &str = "mcp-endpoint-reachable";

/// All check ids in execution order (receipts record exactly this order).
pub const CHECK_ORDER: [&str; 15] = [
    CHECK_CONFIG_SCHEMA,
    CHECK_TARGET_EXPLICIT_PROVIDER,
    CHECK_AGENT_SOURCE_CLEAN_COMMIT,
    CHECK_FRONTEND_SOURCE_CLEAN_COMMIT,
    CHECK_OUTPUT_DIR_ABSENT,
    CHECK_REQUIRED_COMMANDS,
    CHECK_FRONTEND_CONTRACT_HASH,
    CHECK_SIGNING_TRUST_VALIDITY,
    CHECK_PORT_OWNERSHIP,
    CHECK_GCP_TARGET_DESCRIBABLE,
    CHECK_SSH_REMOTE_REACHABLE,
    CHECK_REMOTE_ENV_REQUIRED_KEYS,
    CHECK_SUPABASE_MIGRATION_PLAN,
    CHECK_DB_AGENT_V1_ABI,
    CHECK_MCP_ENDPOINT_REACHABLE,
];

pub const SIGNING_KEY_RELATIVE_PATH: &str = "signing/release-private.pk8";
pub const TRUST_REGISTRY_RELATIVE_PATH: &str = "signing/release-trust-registry.json";

/// The REAL operator layout publishes per-provider trust registries at
/// `<operator_root>/config/<provider>/release-trust-registry.json` (the
/// same file the seal stage signs against).
pub fn per_provider_trust_registry_path(operator_root: &Path, provider: &str) -> PathBuf {
    operator_root
        .join("config")
        .join(provider)
        .join("release-trust-registry.json")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Pass,
    Fail,
    Skipped,
}

impl CheckStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    pub id: &'static str,
    pub status: CheckStatus,
    /// Human-readable outcome; for `Skipped` this is the skip reason.
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightOutcome {
    pub checks: Vec<CheckResult>,
    /// Input hashes per 05: config sha256, agent/frontend HEAD, contract
    /// sha256, target file sha256. Unresolvable inputs record
    /// [`UNAVAILABLE_HASH`] rather than blocking the receipt.
    pub input_hashes: BTreeMap<String, String>,
    pub target_identity: BTreeMap<String, String>,
    pub verdict: CheckStatus,
}

impl PreflightOutcome {
    pub fn failed_checks(&self) -> Vec<&CheckResult> {
        self.checks
            .iter()
            .filter(|check| check.status == CheckStatus::Fail)
            .collect()
    }
}

/// Everything the registry needs besides the executor.
#[derive(Debug, Clone)]
pub struct PreflightPlan<'a> {
    pub config: &'a DeployConfig,
    pub config_path: &'a Path,
    pub config_sha256: String,
    /// Run-scoped output directory; must not exist before build.
    pub output_dir: &'a Path,
    /// Clock used by the "exactly one currently-valid signing key" rule
    /// (mirrors the seal stage's active-key resolution).
    pub now_unix_seconds: u64,
}

/// Run all preflight checks in 05-doc order. Pure read: the only system
/// effects are executor probes (read-only shell-outs / TCP connects).
pub fn run_preflight(
    plan: &PreflightPlan<'_>,
    executor: &dyn PreflightExecutor,
) -> PreflightOutcome {
    let mut checks = Vec::new();
    let config = plan.config;

    // 1. config schema and absolute paths (already enforced by the parser;
    //    record the validated shape for the receipt).
    checks.push(pass(
        CHECK_CONFIG_SCHEMA,
        format!(
            "schema_version {} validated; provider `{}` explicit; all paths absolute; timeouts ssh={}ms mcp={}ms daemon_ready={}ms public_ready={}ms",
            config.schema_version,
            config.provider,
            config.timeouts.ssh_ms,
            config.timeouts.mcp_ms,
            config.timeouts.daemon_ready_ms,
            config.timeouts.public_ready_ms,
        ),
    ));

    // 2. explicit provider/model/registry from the target file.
    let target = match load_target(&config.target_file) {
        Ok(target) => {
            let mut detail = format!(
                "target provider `{}` explicit",
                target.provider.as_deref().unwrap_or_default()
            );
            match (&target.provider, config.provider.as_str()) {
                (Some(target_provider), config_provider) if target_provider == config_provider => {
                    checks.push(pass(CHECK_TARGET_EXPLICIT_PROVIDER, detail));
                }
                (Some(target_provider), config_provider) => {
                    detail = format!(
                        "target provider `{target_provider}` does not match config provider `{config_provider}`"
                    );
                    checks.push(fail(CHECK_TARGET_EXPLICIT_PROVIDER, detail));
                }
                (None, _) => {
                    checks.push(fail(
                        CHECK_TARGET_EXPLICIT_PROVIDER,
                        "target file has no explicit `provider` field".to_owned(),
                    ));
                }
            }
            target
        }
        Err(error) => {
            checks.push(fail(
                CHECK_TARGET_EXPLICIT_PROVIDER,
                format!("target file unusable: {error}"),
            ));
            TargetFile {
                provider: None,
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
            }
        }
    };

    // 3. agent source clean commit.
    let agent_head = match (
        executor.git_porcelain(&config.agent_source_root),
        executor.git_head(&config.agent_source_root),
    ) {
        (Ok(porcelain), Ok(head)) if porcelain.trim().is_empty() => {
            checks.push(pass(
                CHECK_AGENT_SOURCE_CLEAN_COMMIT,
                format!("clean tree at HEAD {head}"),
            ));
            head
        }
        (Ok(porcelain), Ok(head)) => {
            checks.push(fail(
                CHECK_AGENT_SOURCE_CLEAN_COMMIT,
                format!(
                    "agent source dirty at HEAD {head}: {} uncommitted entries",
                    porcelain.lines().count()
                ),
            ));
            head
        }
        (Err(error), _) | (_, Err(error)) => {
            checks.push(fail(CHECK_AGENT_SOURCE_CLEAN_COMMIT, error));
            UNAVAILABLE_HASH.to_owned()
        }
    };

    // 4. frontend source clean commit.
    let frontend_head = match (
        executor.git_porcelain(&config.frontend_source_root),
        executor.git_head(&config.frontend_source_root),
    ) {
        (Ok(porcelain), Ok(head)) if porcelain.trim().is_empty() => {
            checks.push(pass(
                CHECK_FRONTEND_SOURCE_CLEAN_COMMIT,
                format!("clean tree at HEAD {head}"),
            ));
            head
        }
        (Ok(porcelain), Ok(head)) => {
            checks.push(fail(
                CHECK_FRONTEND_SOURCE_CLEAN_COMMIT,
                format!(
                    "frontend source dirty at HEAD {head}: {} uncommitted entries",
                    porcelain.lines().count()
                ),
            ));
            head
        }
        (Err(error), _) | (_, Err(error)) => {
            checks.push(fail(CHECK_FRONTEND_SOURCE_CLEAN_COMMIT, error));
            UNAVAILABLE_HASH.to_owned()
        }
    };

    // 5. output directory must not exist (never build into existing output).
    if plan.output_dir.exists() {
        checks.push(fail(
            CHECK_OUTPUT_DIR_ABSENT,
            format!(
                "output directory already exists: {}",
                plan.output_dir.display()
            ),
        ));
    } else {
        checks.push(pass(
            CHECK_OUTPUT_DIR_ABSENT,
            format!("output directory absent: {}", plan.output_dir.display()),
        ));
    }

    // 6. required local commands.
    let mut missing_commands = Vec::new();
    for command in REQUIRED_COMMANDS {
        match executor.command_available(command) {
            Ok(true) => {}
            Ok(false) => missing_commands.push(command.to_owned()),
            Err(error) => {
                missing_commands.push(format!("{command} (probe error: {error})"));
            }
        }
    }
    if missing_commands.is_empty() {
        checks.push(pass(
            CHECK_REQUIRED_COMMANDS,
            format!(
                "all {} required commands resolve on PATH",
                REQUIRED_COMMANDS.len()
            ),
        ));
    } else {
        checks.push(fail(
            CHECK_REQUIRED_COMMANDS,
            format!("missing required commands: {}", missing_commands.join(", ")),
        ));
    }

    // 7. frontend deployment contract file exists + canonical hash check.
    let contract_sha256 = match contract::load_contract(&config.frontend_contract) {
        Ok(contract) => {
            let hash = contract.canonical_sha256.clone();
            match &target.frontend_contract_sha256 {
                Some(pinned) if *pinned == hash => checks.push(pass(
                    CHECK_FRONTEND_CONTRACT_HASH,
                    format!("contract valid; canonical {hash} matches target pin"),
                )),
                Some(pinned) => checks.push(fail(
                    CHECK_FRONTEND_CONTRACT_HASH,
                    format!("contract canonical hash {hash} does not match target pin {pinned}"),
                )),
                None => checks.push(CheckResult {
                    id: CHECK_FRONTEND_CONTRACT_HASH,
                    status: CheckStatus::Skipped,
                    detail: format!(
                        "contract valid; canonical {hash} recorded; target pins no contract hash"
                    ),
                }),
            }
            hash
        }
        Err(ContractError::MissingOrUnsafe(detail)) => {
            checks.push(fail(
                CHECK_FRONTEND_CONTRACT_HASH,
                format!("contract missing or unsafe: {detail}"),
            ));
            UNAVAILABLE_HASH.to_owned()
        }
        Err(error) => {
            checks.push(fail(CHECK_FRONTEND_CONTRACT_HASH, error.to_string()));
            UNAVAILABLE_HASH.to_owned()
        }
    };

    // 8. signing key / trust validity.
    let signing_key = config.operator_root.join(SIGNING_KEY_RELATIVE_PATH);
    let per_provider_registry =
        per_provider_trust_registry_path(&config.operator_root, &config.provider);
    let trust_registry = config.operator_root.join(TRUST_REGISTRY_RELATIVE_PATH);
    let key_bytes = std::fs::read(&signing_key);
    match &key_bytes {
        Ok(bytes) if !bytes.is_empty() => {}
        Ok(_) => checks.push(fail(
            CHECK_SIGNING_TRUST_VALIDITY,
            format!("signing key is empty: {}", signing_key.display()),
        )),
        Err(error) => checks.push(fail(
            CHECK_SIGNING_TRUST_VALIDITY,
            format!("signing key unreadable: {}: {error}", signing_key.display()),
        )),
    }
    if key_bytes.as_ref().is_ok_and(|bytes| !bytes.is_empty()) {
        // Preferred: the per-provider registry the seal stage actually
        // signs against. It must parse AND carry EXACTLY ONE non-revoked,
        // currently-valid signing key (same rule as seal; zero or multiple
        // active keys fail closed).
        if per_provider_registry.exists() {
            match std::fs::read(&per_provider_registry) {
                Ok(bytes) => match crate::pipeline::resolve_active_key_id(&bytes, plan.now_unix_seconds) {
                    Ok(active_key_id) => checks.push(pass(
                        CHECK_SIGNING_TRUST_VALIDITY,
                        format!(
                            "signing key readable; provider registry `{}` has exactly one active signing key `{}`",
                            per_provider_registry.display(),
                            active_key_id
                        ),
                    )),
                    Err(error) => checks.push(fail(
                        CHECK_SIGNING_TRUST_VALIDITY,
                        format!("provider trust registry invalid: {}: {error}", per_provider_registry.display()),
                    )),
                },
                Err(error) => checks.push(fail(
                    CHECK_SIGNING_TRUST_VALIDITY,
                    format!("provider trust registry unreadable: {}: {error}", per_provider_registry.display()),
                )),
            }
        } else if trust_registry.exists() {
            // Fallback: legacy shared registry under signing/ (parse
            // validity only, as before).
            match std::fs::read(&trust_registry) {
                Ok(bytes) => {
                    match krw_agent_release_authorization::parse_canonical_trust_registry(&bytes) {
                        Ok(registry) => checks.push(pass(
                            CHECK_SIGNING_TRUST_VALIDITY,
                            format!(
                                "signing key readable; trust registry `{}` parses with {} key(s)",
                                registry.registry_id,
                                registry.keys.len()
                            ),
                        )),
                        Err(error) => checks.push(fail(
                            CHECK_SIGNING_TRUST_VALIDITY,
                            format!(
                                "trust registry invalid: {}: {error}",
                                trust_registry.display()
                            ),
                        )),
                    }
                }
                Err(error) => checks.push(fail(
                    CHECK_SIGNING_TRUST_VALIDITY,
                    format!(
                        "trust registry unreadable: {}: {error}",
                        trust_registry.display()
                    ),
                )),
            }
        } else {
            checks.push(CheckResult {
                id: CHECK_SIGNING_TRUST_VALIDITY,
                status: CheckStatus::Skipped,
                detail: format!(
                    "signing key readable; no trust registry published at {} or {}",
                    per_provider_registry.display(),
                    trust_registry.display()
                ),
            });
        }
    }

    // 9. port ownership (read-only bind probe on loopback).
    if target.local_ports.is_empty() {
        checks.push(CheckResult {
            id: CHECK_PORT_OWNERSHIP,
            status: CheckStatus::Skipped,
            detail: "target file declares no local ports".to_owned(),
        });
    } else {
        let mut owned = Vec::new();
        let mut conflicts = Vec::new();
        for port in &target.local_ports {
            match TcpListener::bind(("127.0.0.1", *port)) {
                Ok(listener) => {
                    drop(listener);
                    owned.push(port.to_string());
                }
                Err(error) => conflicts.push(format!("port {port}: {error}")),
            }
        }
        if conflicts.is_empty() {
            checks.push(pass(
                CHECK_PORT_OWNERSHIP,
                format!("all declared ports free on 127.0.0.1: {}", owned.join(", ")),
            ));
        } else {
            checks.push(fail(
                CHECK_PORT_OWNERSHIP,
                format!("declared ports already in use: {}", conflicts.join("; ")),
            ));
        }
    }

    // 10. GCP project/instance/zone describable (read-only describe).
    match &target.gcp {
        Some(gcp) => match executor.gcp_describe_instance(gcp) {
            Ok(_) => checks.push(pass(
                CHECK_GCP_TARGET_DESCRIBABLE,
                format!(
                    "instance `{}` describable in zone `{}` of project `{}`",
                    gcp.instance, gcp.zone, gcp.project
                ),
            )),
            Err(error) => checks.push(fail(CHECK_GCP_TARGET_DESCRIBABLE, error)),
        },
        None => checks.push(CheckResult {
            id: CHECK_GCP_TARGET_DESCRIBABLE,
            status: CheckStatus::Skipped,
            detail: "target file records no gcp project/instance/zone".to_owned(),
        }),
    }

    // 11. Remote shell reachability. The production transport is
    // `gcloud compute ssh` whenever the target declares a gcp block; plain
    // ssh survives only for ssh_host-only targets. Write-free echo probe.
    match (&target.gcp, &target.ssh_host) {
        (Some(gcp), _) => match executor.gcloud_ssh_echo_ok(gcp) {
            Ok(()) => checks.push(pass(
                CHECK_SSH_REMOTE_REACHABLE,
                format!(
                    "gcloud compute ssh `{}` reachable in zone `{}` of project `{}` (echo probe, no writes)",
                    gcp.instance, gcp.zone, gcp.project
                ),
            )),
            Err(error) => checks.push(fail(CHECK_SSH_REMOTE_REACHABLE, error)),
        },
        (None, Some(host)) => match executor.ssh_batch_true(host, config.timeouts.ssh_ms) {
            Ok(()) => checks.push(pass(
                CHECK_SSH_REMOTE_REACHABLE,
                format!("ssh `{host}` reachable (BatchMode, no writes)"),
            )),
            Err(error) => checks.push(fail(CHECK_SSH_REMOTE_REACHABLE, error)),
        },
        (None, None) => checks.push(CheckResult {
            id: CHECK_SSH_REMOTE_REACHABLE,
            status: CheckStatus::Skipped,
            detail: "target file records no gcp block and no ssh_host".to_owned(),
        }),
    }

    // 12. remote env required keys (names only; values never recorded).
    if target.required_env_keys.is_empty() {
        checks.push(CheckResult {
            id: CHECK_REMOTE_ENV_REQUIRED_KEYS,
            status: CheckStatus::Skipped,
            detail: "target file declares no required env keys".to_owned(),
        });
    } else {
        match env_key_names(&config.runtime_env) {
            Ok(present) => {
                let missing = target
                    .required_env_keys
                    .iter()
                    .filter(|key| !present.contains(*key))
                    .cloned()
                    .collect::<Vec<_>>();
                if missing.is_empty() {
                    checks.push(pass(
                        CHECK_REMOTE_ENV_REQUIRED_KEYS,
                        format!(
                            "all {} required env key(s) present in {} (names only recorded)",
                            target.required_env_keys.len(),
                            config.runtime_env.display()
                        ),
                    ));
                } else {
                    checks.push(fail(
                        CHECK_REMOTE_ENV_REQUIRED_KEYS,
                        format!("missing required env keys: {}", missing.join(", ")),
                    ));
                }
            }
            Err(error) => checks.push(fail(CHECK_REMOTE_ENV_REQUIRED_KEYS, error)),
        }
    }

    // 13. Supabase target/TLS/migration plan (structural, read-only).
    if target.supabase_migration_plan.is_empty() {
        checks.push(CheckResult {
            id: CHECK_SUPABASE_MIGRATION_PLAN,
            status: CheckStatus::Skipped,
            detail: "target file records no supabase migration plan".to_owned(),
        });
    } else {
        checks.push(pass(
            CHECK_SUPABASE_MIGRATION_PLAN,
            format!(
                "migration plan recorded with {} ordered entr{} (read-only verification; applied only after admission close)",
                target.supabase_migration_plan.len(),
                if target.supabase_migration_plan.len() == 1 { "y" } else { "ies" }
            ),
        ));
    }

    // 14. current DB Agent V1 ABI (read-only `select 1` reachability; the
    // db_url_env handle resolves from the RUNTIME ENV FILE, never the
    // controller's process env, and the URL only ever reaches the psql
    // child's environment).
    match &target.db_url_env {
        Some(handle) => {
            let abi = target.db_abi.as_deref().unwrap_or("<unspecified>");
            match executor.db_select_one(&config.runtime_env, handle) {
                Ok(()) => checks.push(pass(
                    CHECK_DB_AGENT_V1_ABI,
                    format!("db reachable via runtime-env handle `{handle}`; declared ABI `{abi}`"),
                )),
                Err(error) => checks.push(fail(
                    CHECK_DB_AGENT_V1_ABI,
                    format!("db probe failed (declared ABI `{abi}`): {error}"),
                )),
            }
        }
        None => checks.push(CheckResult {
            id: CHECK_DB_AGENT_V1_ABI,
            status: CheckStatus::Skipped,
            detail: "target file records no db_url_env handle".to_owned(),
        }),
    }

    // 15. local MCP endpoint readiness (connectivity only; never LLM calls).
    if target.mcp_endpoints.is_empty() {
        checks.push(CheckResult {
            id: CHECK_MCP_ENDPOINT_REACHABLE,
            status: CheckStatus::Skipped,
            detail: "target file records no mcp endpoints".to_owned(),
        });
    } else {
        let mut failures = Vec::new();
        for endpoint in &target.mcp_endpoints {
            if let Some((host, port)) = parse_host_port(endpoint) {
                if let Err(error) = executor.tcp_connect(&host, port, config.timeouts.mcp_ms) {
                    failures.push(format!("{endpoint}: {error}"));
                }
            } else {
                failures.push(format!("{endpoint}: endpoint must be `host:port`"));
            }
        }
        if failures.is_empty() {
            checks.push(pass(
                CHECK_MCP_ENDPOINT_REACHABLE,
                format!(
                    "{} mcp endpoint(s) connectable (no LLM calls)",
                    target.mcp_endpoints.len()
                ),
            ));
        } else {
            checks.push(fail(
                CHECK_MCP_ENDPOINT_REACHABLE,
                format!("mcp endpoints unreachable: {}", failures.join("; ")),
            ));
        }
    }

    let verdict = if checks.iter().any(|check| check.status == CheckStatus::Fail) {
        CheckStatus::Fail
    } else {
        CheckStatus::Pass
    };

    let mut input_hashes = BTreeMap::new();
    input_hashes.insert("config_sha256".to_owned(), plan.config_sha256.clone());
    input_hashes.insert("agent_head".to_owned(), agent_head);
    input_hashes.insert("frontend_head".to_owned(), frontend_head);
    input_hashes.insert("frontend_contract_sha256".to_owned(), contract_sha256);
    input_hashes.insert(
        "target_file_sha256".to_owned(),
        sha256_file_or_unavailable(&config.target_file),
    );

    PreflightOutcome {
        checks,
        input_hashes,
        target_identity: target.identity_map(),
        verdict,
    }
}

fn pass(id: &'static str, detail: String) -> CheckResult {
    CheckResult {
        id,
        status: CheckStatus::Pass,
        detail,
    }
}

fn fail(id: &'static str, detail: String) -> CheckResult {
    CheckResult {
        id,
        status: CheckStatus::Fail,
        detail,
    }
}

fn load_target(path: &Path) -> Result<TargetFile, TargetError> {
    TargetFile::from_path(path)
}

/// KEY=NAME parsing of an env file: returns key NAMES only. Comment lines
/// and blanks are ignored; `export ` prefixes are tolerated.
fn env_key_names(path: &Path) -> Result<BTreeSet<String>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut names = BTreeSet::new();
    for line in text.lines() {
        let line = line.trim().strip_prefix("export ").unwrap_or(line.trim());
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, _value)) = line.split_once('=') {
            names.insert(key.trim().to_owned());
        }
    }
    Ok(names)
}

/// Parse `host:port` (tolerating a trailing path or scheme-free URL forms).
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::FixtureExecutor;
    use crate::hashing::sha256_file;
    use std::sync::atomic::{AtomicU32, Ordering};

    static DIR_SEQUENCE: AtomicU32 = AtomicU32::new(0);
    /// 2026-08-16T01:20:00Z; every key in the fixture registries is valid at
    /// this instant (or not, when a test expires one on purpose).
    const FIXED_NOW: u64 = 1_786_843_200;

    struct Fixture {
        root: PathBuf,
        config: DeployConfig,
        config_path: PathBuf,
    }

    impl Fixture {
        fn output_dir(&self) -> PathBuf {
            self.root.join("out").join("release-under-test")
        }

        fn target_path(&self) -> PathBuf {
            self.config.target_file.clone()
        }

        fn rewrite_target(&self, transform: impl Fn(String) -> String) {
            let text = std::fs::read_to_string(self.target_path()).unwrap();
            std::fs::write(self.target_path(), transform(text)).unwrap();
        }
    }

    fn write_fixture(tag: &str) -> Fixture {
        write_fixture_with(tag, |_| {})
    }

    fn write_fixture_with(tag: &str, adjust: impl Fn(&Fixture)) -> Fixture {
        let root = std::env::temp_dir().join(format!(
            "krw-agent-deploy-checks-{}-{tag}-{}",
            std::process::id(),
            DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let agent_root = root.join("agent");
        let frontend_root = root.join("front");
        let operator_root = root.join("operator");
        for dir in [&agent_root, &frontend_root] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::create_dir_all(operator_root.join("signing")).unwrap();
        std::fs::create_dir_all(operator_root.join("ops")).unwrap();
        std::fs::create_dir_all(operator_root.join("runtime")).unwrap();
        std::fs::write(
            operator_root.join("signing/release-private.pk8"),
            b"dummy-pkcs8-bytes",
        )
        .unwrap();
        std::fs::write(
            operator_root.join("runtime/krw-agent-deploy.env"),
            "KRW_AGENT_DB_URL=postgresql://dummy\n# comment\nKRW_AGENT_PROVIDER_KEY_HANDLE=dummy\n",
        )
        .unwrap();
        std::fs::write(
            operator_root.join("ops/agent-v1-deployment-contract.json"),
            contract::canonical_fixture_contract_json(),
        )
        .unwrap();
        let fixture = Fixture {
            config_path: operator_root.join("ops/deploy-config.json"),
            config: DeployConfig {
                schema_version: 1,
                provider: "deepseek".to_owned(),
                agent_source_root: agent_root,
                frontend_source_root: frontend_root,
                operator_root: operator_root.clone(),
                runtime_env: operator_root.join("runtime/krw-agent-deploy.env"),
                target_file: operator_root.join("ops/production-target.json"),
                frontend_contract: operator_root.join("ops/agent-v1-deployment-contract.json"),
                timeouts: crate::config::DeployTimeouts {
                    ssh_ms: 5_000,
                    mcp_ms: 5_000,
                    daemon_ready_ms: 90_000,
                    public_ready_ms: 90_000,
                },
            },
            root,
        };
        std::fs::write(
            fixture.target_path(),
            r#"{
              "provider": "deepseek",
              "local_ports": [],
              "required_env_keys": [],
              "supabase_migration_plan": [],
              "mcp_endpoints": []
            }"#,
        )
        .unwrap();
        let text = serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 1,
            "provider": "deepseek",
            "agent_source_root": fixture.config.agent_source_root,
            "frontend_source_root": fixture.config.frontend_source_root,
            "operator_root": fixture.config.operator_root,
            "runtime_env": fixture.config.runtime_env,
            "target_file": fixture.config.target_file,
            "frontend_contract": fixture.config.frontend_contract,
            "timeouts": {
                "ssh_ms": 5000,
                "mcp_ms": 5000,
                "daemon_ready_ms": 90000,
                "public_ready_ms": 90000
            }
        }))
        .unwrap();
        std::fs::write(&fixture.config_path, text).unwrap();
        adjust(&fixture);
        fixture
    }

    fn run(fixture: &Fixture, executor: &dyn PreflightExecutor) -> PreflightOutcome {
        let config_sha256 = sha256_file(&fixture.config_path).unwrap();
        let output_dir = fixture.output_dir();
        let plan = PreflightPlan {
            config: &fixture.config,
            config_path: &fixture.config_path,
            config_sha256,
            output_dir: &output_dir,
            now_unix_seconds: FIXED_NOW,
        };
        run_preflight(&plan, executor)
    }

    fn result_of<'a>(outcome: &'a PreflightOutcome, id: &str) -> &'a CheckResult {
        outcome
            .checks
            .iter()
            .find(|check| check.id == id)
            .unwrap_or_else(|| panic!("check `{id}` missing from outcome"))
    }

    #[test]
    fn happy_path_passes_with_fixture_executor() {
        let fixture = write_fixture("happy");
        let outcome = run(&fixture, &FixtureExecutor::passing());
        assert_eq!(
            outcome.verdict,
            CheckStatus::Pass,
            "details: {:#?}",
            outcome.failed_checks()
        );
        assert_eq!(outcome.checks.len(), CHECK_ORDER.len());
        assert_eq!(
            outcome
                .checks
                .iter()
                .map(|check| check.id)
                .collect::<Vec<_>>(),
            CHECK_ORDER
        );
        assert_eq!(
            outcome.input_hashes.get("agent_head").map(String::as_str),
            Some("0123456789abcdef0123456789abcdef01234567")
        );
        assert!(
            outcome
                .input_hashes
                .get("config_sha256")
                .unwrap()
                .starts_with("sha256:")
        );
        assert_eq!(
            outcome.target_identity.get("provider").map(String::as_str),
            Some("deepseek")
        );
    }

    #[test]
    fn existing_output_directory_fails() {
        let fixture = write_fixture("output-exists");
        std::fs::create_dir_all(fixture.output_dir()).unwrap();
        let outcome = run(&fixture, &FixtureExecutor::passing());
        assert_eq!(outcome.verdict, CheckStatus::Fail);
        assert_eq!(
            result_of(&outcome, CHECK_OUTPUT_DIR_ABSENT).status,
            CheckStatus::Fail
        );
    }

    #[test]
    fn target_without_explicit_provider_fails() {
        let fixture = write_fixture("provider-missing");
        fixture.rewrite_target(|text| text.replace("\"provider\": \"deepseek\",", ""));
        let outcome = run(&fixture, &FixtureExecutor::passing());
        assert_eq!(
            result_of(&outcome, CHECK_TARGET_EXPLICIT_PROVIDER).status,
            CheckStatus::Fail
        );
        assert_eq!(outcome.verdict, CheckStatus::Fail);
    }

    #[test]
    fn provider_mismatch_between_config_and_target_fails() {
        let fixture = write_fixture("provider-mismatch");
        fixture.rewrite_target(|text| {
            text.replace("\"provider\": \"deepseek\"", "\"provider\": \"glm\"")
        });
        let outcome = run(&fixture, &FixtureExecutor::passing());
        let check = result_of(&outcome, CHECK_TARGET_EXPLICIT_PROVIDER);
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(
            check.detail.contains("does not match"),
            "detail: {}",
            check.detail
        );
    }

    #[test]
    fn contract_hash_mismatch_fails() {
        let fixture = write_fixture("contract-mismatch");
        let pinned = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
        fixture.rewrite_target(|text| {
            text.replace(
                "\"local_ports\": []",
                &format!("\"frontend_contract_sha256\": \"{pinned}\",\n \"local_ports\": []"),
            )
        });
        let outcome = run(&fixture, &FixtureExecutor::passing());
        let check = result_of(&outcome, CHECK_FRONTEND_CONTRACT_HASH);
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(
            check.detail.contains("does not match target pin"),
            "detail: {}",
            check.detail
        );
    }

    #[test]
    fn contract_hash_pin_match_passes() {
        let fixture = write_fixture("contract-pin");
        let contract_path = fixture.config.frontend_contract.clone();
        let contract_bytes = std::fs::read(&contract_path).unwrap();
        let pinned = crate::contract::parse_contract(&contract_bytes)
            .unwrap()
            .canonical_sha256;
        fixture.rewrite_target(|text| {
            text.replace(
                "\"local_ports\": []",
                &format!("\"frontend_contract_sha256\": \"{pinned}\",\n \"local_ports\": []"),
            )
        });
        let outcome = run(&fixture, &FixtureExecutor::passing());
        assert_eq!(
            result_of(&outcome, CHECK_FRONTEND_CONTRACT_HASH).status,
            CheckStatus::Pass
        );
    }

    #[test]
    fn missing_contract_file_fails() {
        let fixture = write_fixture("contract-missing");
        std::fs::remove_file(fixture.config.frontend_contract.clone()).unwrap();
        let outcome = run(&fixture, &FixtureExecutor::passing());
        assert_eq!(
            result_of(&outcome, CHECK_FRONTEND_CONTRACT_HASH).status,
            CheckStatus::Fail
        );
        assert_eq!(
            outcome
                .input_hashes
                .get("frontend_contract_sha256")
                .map(String::as_str),
            Some(UNAVAILABLE_HASH)
        );
    }

    #[test]
    fn port_in_use_fails() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let fixture = write_fixture("port-in-use");
        fixture.rewrite_target(|text| {
            text.replace("\"local_ports\": []", &format!("\"local_ports\": [{port}]"))
        });
        let outcome = run(&fixture, &FixtureExecutor::passing());
        let check = result_of(&outcome, CHECK_PORT_OWNERSHIP);
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(
            check.detail.contains(&port.to_string()),
            "detail: {}",
            check.detail
        );
    }

    #[test]
    fn free_port_passes() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let fixture = write_fixture("port-free");
        fixture.rewrite_target(|text| {
            text.replace("\"local_ports\": []", &format!("\"local_ports\": [{port}]"))
        });
        let outcome = run(&fixture, &FixtureExecutor::passing());
        assert_eq!(
            result_of(&outcome, CHECK_PORT_OWNERSHIP).status,
            CheckStatus::Pass
        );
    }

    #[test]
    fn missing_required_commands_fail() {
        let fixture = write_fixture("commands-missing");
        let mut executor = FixtureExecutor::passing();
        executor.missing_commands = REQUIRED_COMMANDS
            .iter()
            .map(|command| (*command).to_owned())
            .collect();
        let outcome = run(&fixture, &executor);
        let check = result_of(&outcome, CHECK_REQUIRED_COMMANDS);
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.detail.contains("docker"), "detail: {}", check.detail);
    }

    #[test]
    fn dirty_agent_source_fails() {
        let fixture = write_fixture("dirty-agent");
        let mut executor = FixtureExecutor::passing();
        executor.porcelain = Ok(" M crates/foo/src/lib.rs\n?? scratch.txt\n".to_owned());
        let outcome = run(&fixture, &executor);
        assert_eq!(
            result_of(&outcome, CHECK_AGENT_SOURCE_CLEAN_COMMIT).status,
            CheckStatus::Fail
        );
    }

    #[test]
    fn missing_signing_key_fails() {
        let fixture = write_fixture("key-missing");
        std::fs::remove_file(fixture.config.operator_root.join(SIGNING_KEY_RELATIVE_PATH)).unwrap();
        let outcome = run(&fixture, &FixtureExecutor::passing());
        assert_eq!(
            result_of(&outcome, CHECK_SIGNING_TRUST_VALIDITY).status,
            CheckStatus::Fail
        );
    }

    #[test]
    fn invalid_trust_registry_fails_when_published() {
        let fixture = write_fixture("trust-invalid");
        std::fs::write(
            fixture
                .config
                .operator_root
                .join(TRUST_REGISTRY_RELATIVE_PATH),
            b"{\"schema_version\": 99}",
        )
        .unwrap();
        let outcome = run(&fixture, &FixtureExecutor::passing());
        assert_eq!(
            result_of(&outcome, CHECK_SIGNING_TRUST_VALIDITY).status,
            CheckStatus::Fail
        );
    }

    #[test]
    fn valid_trust_registry_passes_and_absent_registry_skips() {
        let fixture = write_fixture("trust-valid");
        // Trust registries are stored as JCS-canonical JSON; build the
        // fixture through the same serializer the release tooling uses.
        let registry = krw_agent_release_authorization::ReleaseTrustRegistryV1 {
            schema_version: 1,
            registry_id: "krw.deploy-test".to_owned(),
            minimum_sequence: 1,
            keys: vec![krw_agent_release_authorization::ReleaseTrustKeyV1 {
                key_id: "deploy-test-key".to_owned(),
                ed25519_public_key_hex: "11".repeat(32),
                not_before_unix_seconds: 0,
                not_after_unix_seconds: 4_102_444_800,
                revoked: false,
            }],
        };
        std::fs::write(
            fixture
                .config
                .operator_root
                .join(TRUST_REGISTRY_RELATIVE_PATH),
            serde_jcs::to_vec(&registry).unwrap(),
        )
        .unwrap();
        let outcome = run(&fixture, &FixtureExecutor::passing());
        assert_eq!(
            result_of(&outcome, CHECK_SIGNING_TRUST_VALIDITY).status,
            CheckStatus::Pass
        );

        let fixture = write_fixture("trust-absent");
        let outcome = run(&fixture, &FixtureExecutor::passing());
        let check = result_of(&outcome, CHECK_SIGNING_TRUST_VALIDITY);
        assert_eq!(check.status, CheckStatus::Skipped);
        assert!(
            check
                .detail
                .contains("config/deepseek/release-trust-registry.json"),
            "skip detail names both registry locations: {}",
            check.detail
        );
    }

    fn active_key(key_id: &str) -> krw_agent_release_authorization::ReleaseTrustKeyV1 {
        krw_agent_release_authorization::ReleaseTrustKeyV1 {
            key_id: key_id.to_owned(),
            ed25519_public_key_hex: "77".repeat(32),
            not_before_unix_seconds: 0,
            not_after_unix_seconds: 4_102_444_800,
            revoked: false,
        }
    }

    fn write_per_provider_registry(
        fixture: &Fixture,
        keys: Vec<krw_agent_release_authorization::ReleaseTrustKeyV1>,
    ) -> PathBuf {
        let registry = krw_agent_release_authorization::ReleaseTrustRegistryV1 {
            schema_version: 1,
            registry_id: "krw.deploy-checks-per-provider".to_owned(),
            minimum_sequence: 1,
            keys,
        };
        let path = per_provider_trust_registry_path(
            &fixture.config.operator_root,
            &fixture.config.provider,
        );
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_jcs::to_vec(&registry).unwrap()).unwrap();
        path
    }

    #[test]
    fn per_provider_registry_with_one_active_key_passes_and_wins_over_fallback() {
        let fixture = write_fixture("trust-per-provider");
        write_per_provider_registry(&fixture, vec![active_key("checks-active-key")]);
        // An INVALID legacy registry sits next to it: the per-provider file
        // must take precedence, so the check still passes.
        std::fs::write(
            fixture
                .config
                .operator_root
                .join(TRUST_REGISTRY_RELATIVE_PATH),
            b"{\"schema_version\": 99}",
        )
        .unwrap();
        let outcome = run(&fixture, &FixtureExecutor::passing());
        let check = result_of(&outcome, CHECK_SIGNING_TRUST_VALIDITY);
        assert_eq!(check.status, CheckStatus::Pass);
        assert!(
            check
                .detail
                .contains("exactly one active signing key `checks-active-key`"),
            "detail: {}",
            check.detail
        );
        assert!(
            check
                .detail
                .contains("config/deepseek/release-trust-registry.json"),
            "detail: {}",
            check.detail
        );
    }

    #[test]
    fn per_provider_registry_with_zero_active_keys_fails() {
        let fixture = write_fixture("trust-per-provider-expired");
        let mut expired = active_key("checks-expired-key");
        expired.not_after_unix_seconds = FIXED_NOW - 1;
        write_per_provider_registry(&fixture, vec![expired]);
        let outcome = run(&fixture, &FixtureExecutor::passing());
        let check = result_of(&outcome, CHECK_SIGNING_TRUST_VALIDITY);
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(
            check.detail.contains("no active signing key"),
            "detail: {}",
            check.detail
        );

        // A revoked-only registry is the same zero-active failure.
        let fixture = write_fixture("trust-per-provider-revoked");
        let mut revoked = active_key("checks-revoked-key");
        revoked.revoked = true;
        write_per_provider_registry(&fixture, vec![revoked]);
        let outcome = run(&fixture, &FixtureExecutor::passing());
        let check = result_of(&outcome, CHECK_SIGNING_TRUST_VALIDITY);
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(
            check.detail.contains("no active signing key"),
            "detail: {}",
            check.detail
        );
    }

    #[test]
    fn per_provider_registry_with_two_active_keys_fails() {
        let fixture = write_fixture("trust-per-provider-dual");
        write_per_provider_registry(
            &fixture,
            vec![active_key("checks-active-a"), active_key("checks-active-b")],
        );
        let outcome = run(&fixture, &FixtureExecutor::passing());
        let check = result_of(&outcome, CHECK_SIGNING_TRUST_VALIDITY);
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(
            check.detail.contains("2 active signing keys"),
            "detail: {}",
            check.detail
        );
    }

    #[test]
    fn db_handle_resolves_from_runtime_env_file_not_process_env() {
        // The handle exists ONLY in the runtime env file — it is never set
        // in the controller's process env (unique name, never exported).
        let handle = "KRW_CHECKS_FILE_ONLY_DB_HANDLE_7C";
        let fixture = write_fixture_with("db-file-handle", |fixture| {
            std::fs::write(
                fixture.config.runtime_env.clone(),
                format!("# names only\nKRW_AGENT_DB_URL=postgresql://dummy\n{handle}=postgresql://fixture-file-only\n"),
            )
            .unwrap();
            std::fs::write(
                fixture.config.target_file.clone(),
                format!(
                    r#"{{"provider": "deepseek", "local_ports": [], "required_env_keys": [], "supabase_migration_plan": [], "mcp_endpoints": [], "db_url_env": "{handle}", "db_abi": "agent_v1_v7"}}"#
                ),
            )
            .unwrap();
        });
        assert!(
            std::env::var_os(handle).is_none(),
            "test handle must not leak into the process env"
        );
        let outcome = run(&fixture, &FixtureExecutor::passing());
        let check = result_of(&outcome, CHECK_DB_AGENT_V1_ABI);
        assert_eq!(check.status, CheckStatus::Pass, "detail: {}", check.detail);
        assert!(
            check.detail.contains("runtime-env handle"),
            "detail: {}",
            check.detail
        );
        assert!(
            !check.detail.contains("postgresql://"),
            "detail: {}",
            check.detail
        );

        // Same target, but the handle is absent from the runtime env file:
        // clear key-named failure (the old bug consulted process env).
        std::fs::write(
            fixture.config.runtime_env.clone(),
            "# names only\nKRW_AGENT_DB_URL=postgresql://dummy\n",
        )
        .unwrap();
        let outcome = run(&fixture, &FixtureExecutor::passing());
        let check = result_of(&outcome, CHECK_DB_AGENT_V1_ABI);
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.detail.contains(handle), "detail: {}", check.detail);
        assert!(
            check.detail.contains("not present"),
            "detail: {}",
            check.detail
        );
    }

    #[test]
    fn missing_required_env_keys_fail() {
        let fixture = write_fixture("env-missing");
        fixture.rewrite_target(|text| {
            text.replace(
                "\"required_env_keys\": []",
                "\"required_env_keys\": [\"KRW_AGENT_DB_URL\", \"KRW_AGENT_MISSING_KEY\"]",
            )
        });
        let outcome = run(&fixture, &FixtureExecutor::passing());
        let check = result_of(&outcome, CHECK_REMOTE_ENV_REQUIRED_KEYS);
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(
            check.detail.contains("KRW_AGENT_MISSING_KEY"),
            "detail: {}",
            check.detail
        );
    }

    #[test]
    fn remote_checks_run_and_can_fail_via_executor() {
        let fixture = write_fixture_with("remote", |fixture| {
            std::fs::write(
                fixture.config.target_file.clone(),
                r#"{
                  "provider": "deepseek",
                  "gcp": {"project": "krw-prod-dummy", "zone": "asia-northeast3-a", "instance": "krw-agent-prod-dummy"},
                  "ssh_host": "deploy@127.0.0.1",
                  "db_url_env": "KRW_AGENT_DB_URL",
                  "db_abi": "agent_v1_v7",
                  "mcp_endpoints": ["127.0.0.1:18081"],
                  "local_ports": [],
                  "required_env_keys": [],
                  "supabase_migration_plan": ["0001_agent_v1.sql"]
                }"#,
            )
            .unwrap();
        });
        let mut executor = FixtureExecutor::passing();
        executor.gcp_describe = Err("gcloud credentials expired".to_owned());
        // gcp block present => the reachability check uses the gcloud
        // transport; the plain-ssh probe must not be consulted at all.
        executor.gcloud_ssh = Err("gcloud compute ssh: connection refused".to_owned());
        executor.ssh = Ok(());
        executor.db = Err("env handle `KRW_AGENT_DB_URL` is not set".to_owned());
        executor.tcp = Err("connect refused".to_owned());
        let outcome = run(&fixture, &executor);
        assert_eq!(
            result_of(&outcome, CHECK_GCP_TARGET_DESCRIBABLE).status,
            CheckStatus::Fail
        );
        let reachability = result_of(&outcome, CHECK_SSH_REMOTE_REACHABLE);
        assert_eq!(reachability.status, CheckStatus::Fail);
        assert!(
            reachability.detail.contains("gcloud compute ssh"),
            "detail: {}",
            reachability.detail
        );
        assert_eq!(
            result_of(&outcome, CHECK_DB_AGENT_V1_ABI).status,
            CheckStatus::Fail
        );
        assert_eq!(
            result_of(&outcome, CHECK_MCP_ENDPOINT_REACHABLE).status,
            CheckStatus::Fail
        );
        assert_eq!(
            result_of(&outcome, CHECK_SUPABASE_MIGRATION_PLAN).status,
            CheckStatus::Pass
        );
    }

    #[test]
    fn ssh_only_target_uses_the_plain_ssh_reachability_probe() {
        let fixture = write_fixture_with("ssh-only", |fixture| {
            std::fs::write(
                fixture.config.target_file.clone(),
                r#"{
                  "provider": "deepseek",
                  "ssh_host": "deploy@127.0.0.1",
                  "remote_front_dir": "/home/deploy/krw-ontology-front",
                  "local_ports": [],
                  "required_env_keys": [],
                  "supabase_migration_plan": [],
                  "mcp_endpoints": []
                }"#,
            )
            .unwrap();
        });
        let mut executor = FixtureExecutor::passing();
        executor.ssh = Err("ssh: connection refused".to_owned());
        executor.gcloud_ssh = Ok(());
        let outcome = run(&fixture, &executor);
        let reachability = result_of(&outcome, CHECK_SSH_REMOTE_REACHABLE);
        assert_eq!(reachability.status, CheckStatus::Fail);
        assert!(
            reachability.detail.contains("connection refused"),
            "detail: {}",
            reachability.detail
        );

        // The passing form names the plain-ssh transport.
        let outcome = run(&fixture, &FixtureExecutor::passing());
        let reachability = result_of(&outcome, CHECK_SSH_REMOTE_REACHABLE);
        assert_eq!(reachability.status, CheckStatus::Pass);
        assert!(
            reachability
                .detail
                .contains("ssh `deploy@127.0.0.1` reachable"),
            "detail: {}",
            reachability.detail
        );
    }

    #[test]
    fn minimal_target_records_skips_not_failures() {
        let fixture = write_fixture("skips");
        let outcome = run(&fixture, &FixtureExecutor::passing());
        for id in [
            CHECK_PORT_OWNERSHIP,
            CHECK_GCP_TARGET_DESCRIBABLE,
            CHECK_SSH_REMOTE_REACHABLE,
            CHECK_REMOTE_ENV_REQUIRED_KEYS,
            CHECK_SUPABASE_MIGRATION_PLAN,
            CHECK_DB_AGENT_V1_ABI,
            CHECK_MCP_ENDPOINT_REACHABLE,
        ] {
            assert_eq!(
                result_of(&outcome, id).status,
                CheckStatus::Skipped,
                "check {id}"
            );
        }
        assert_eq!(outcome.verdict, CheckStatus::Pass);
    }

    #[test]
    fn unparseable_target_file_fails_provider_check_and_keeps_going() {
        let fixture = write_fixture("target-broken");
        std::fs::write(fixture.target_path(), b"{ not json").unwrap();
        let outcome = run(&fixture, &FixtureExecutor::passing());
        assert_eq!(
            result_of(&outcome, CHECK_TARGET_EXPLICIT_PROVIDER).status,
            CheckStatus::Fail
        );
        // The unparseable-but-present file still hashes: drift evidence.
        assert!(
            outcome
                .input_hashes
                .get("target_file_sha256")
                .unwrap()
                .starts_with("sha256:")
        );
    }

    #[test]
    fn parse_host_port_accepts_known_forms() {
        assert_eq!(
            parse_host_port("127.0.0.1:18081"),
            Some(("127.0.0.1".to_owned(), 18081))
        );
        assert_eq!(
            parse_host_port("https://mcp.local:9443/base"),
            Some(("mcp.local".to_owned(), 9443))
        );
        assert_eq!(parse_host_port("no-port"), None);
        assert_eq!(parse_host_port("host:notaport"), None);
    }
}
