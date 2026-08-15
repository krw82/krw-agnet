//! End-to-end integration tests through the controller library API.
//!
//! These tests drive `krw_agent_deploy::stages::run_command` (the exact code
//! the thin `main.rs` CLI calls) against a tempdir fixture tree assembled
//! from the checked-in fixtures under `tests/fixtures/`. Nothing touches a
//! real operator root; the fixture executor stands in for world-observing
//! probes.

use std::collections::BTreeSet;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use krw_agent_deploy::executor::FixtureExecutor;
use krw_agent_deploy::stages::{run_command, CommandMode, ControllerError};

const CONTRACT_FIXTURE: &str = include_str!("fixtures/agent-v1-deployment-contract.json");
const TARGET_FIXTURE: &str = include_str!("fixtures/production-target.json");

static DIR_SEQUENCE: AtomicU32 = AtomicU32::new(0);
/// 2026-08-16T01:20:00Z (fixed clock keeps run ids deterministic).
const FIXED_NOW: u64 = 1_786_843_200;

struct Tree {
    root: PathBuf,
    config_path: PathBuf,
}

impl Tree {
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
            .expect("exactly one run receipt directory")
            .unwrap()
            .path()
    }

    fn rewrite_target(&self, transform: impl Fn(String) -> String) {
        let path = self.operator_root().join("ops/production-target.json");
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, transform(text)).unwrap();
    }
}

fn build_tree(tag: &str) -> Tree {
    let root = std::env::temp_dir().join(format!(
        "krw-agent-deploy-integration-{}-{tag}-{}",
        std::process::id(),
        DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    let agent_root = root.join("agent");
    let frontend_root = root.join("front");
    let operator_root = root.join("operator");
    std::fs::create_dir_all(agent_root.join("src")).unwrap();
    std::fs::write(agent_root.join("Cargo.toml"), "[package]\nname = \"fixture-agent\"\n").unwrap();
    std::fs::create_dir_all(&frontend_root).unwrap();
    std::fs::create_dir_all(operator_root.join("signing")).unwrap();
    std::fs::create_dir_all(operator_root.join("ops")).unwrap();
    std::fs::write(operator_root.join("signing/release-private.pk8"), b"fixture-pkcs8-material").unwrap();
    std::fs::create_dir_all(operator_root.join("runtime")).unwrap();
    std::fs::write(operator_root.join("ops/agent-v1-deployment-contract.json"), CONTRACT_FIXTURE).unwrap();
    std::fs::write(operator_root.join("ops/production-target.json"), TARGET_FIXTURE).unwrap();
    std::fs::write(
        operator_root.join("runtime/krw-agent-deploy.env"),
        "# fixture env: names only, dummy values\nKRW_AGENT_DB_URL=postgresql://fixture-dummy\n",
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
    Tree { root, config_path }
}

fn cleanup(tree: &Tree) {
    let _ = std::fs::remove_dir_all(&tree.root);
}

#[test]
fn preflight_then_dry_run_produce_read_only_receipts() {
    let tree = build_tree("happy");

    // Preflight: read-only, receipt verdict pass, no terminal receipt.
    let preflight = run_command(CommandMode::Preflight, &tree.config_path, FIXED_NOW, &FixtureExecutor::passing())
        .expect("preflight run");
    assert_eq!(preflight.exit_code, 0, "message: {}", preflight.message);
    let run_dir = tree.single_run_dir();
    assert!(run_dir.join("preflight.json").is_file());
    assert!(!run_dir.join("terminal.json").exists());
    let preflight_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("preflight.json")).unwrap()).unwrap();
    assert_eq!(preflight_json["verdict"], "pass");
    assert_eq!(preflight_json["executor_kind"], "fixture");
    let checks = preflight_json["checks"].as_array().unwrap();
    assert_eq!(checks.len(), 15);
    // Remote fixtures from production-target.json are exercised, not skipped.
    let ids = checks.iter().map(|check| check["id"].as_str().unwrap().to_owned()).collect::<BTreeSet<_>>();
    for expected in [
        "gcp-target-describable",
        "ssh-remote-reachable",
        "db-agent-v1-abi",
        "mcp-endpoint-reachable",
        "supabase-migration-plan",
        "remote-env-required-keys",
    ] {
        assert!(ids.contains(expected), "missing check {expected} in {ids:?}");
    }
    assert_eq!(
        preflight_json["target_identity"]["provider"],
        "deepseek",
        "target identity recorded: {}",
        preflight_json["target_identity"]
    );

    // Dry-run at a different timestamp: full receipt chain, dry-run-ok.
    let dry_run = run_command(CommandMode::DryRun, &tree.config_path, FIXED_NOW + 60, &FixtureExecutor::passing())
        .expect("dry-run run");
    assert_eq!(dry_run.exit_code, 0, "message: {}", dry_run.message);
    assert_eq!(dry_run.outcome, "dry-run-ok");
    let dry_run_dir = dry_run.terminal_receipt_path.expect("terminal receipt").parent().unwrap().to_path_buf();
    let terminal: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dry_run_dir.join("terminal.json")).unwrap()).unwrap();
    assert_eq!(terminal["outcome"], "dry-run-ok");
    assert_eq!(terminal["admission"], "not-touched");
    assert_eq!(terminal["reached_stage"], "dry_run");
    assert!(terminal["receipt_path"].as_str().unwrap().ends_with("preflight.json"));

    // Read-only invariant: no release output tree, only receipts under
    // deploy-receipts, and the source fixtures are untouched.
    assert!(!tree.operator_root().join("releases").exists());
    let receipt_entries =
        std::fs::read_dir(tree.receipt_root()).unwrap().count();
    assert_eq!(receipt_entries, 2, "two runs, two receipt directories");
    cleanup(&tree);
}

#[test]
fn deploy_is_fail_closed_at_build_with_admission_closed() {
    let tree = build_tree("deploy");
    let outcome = run_command(CommandMode::Deploy, &tree.config_path, FIXED_NOW, &FixtureExecutor::passing())
        .expect("deploy run completes (fail-closed)");
    assert_eq!(outcome.exit_code, 1, "message: {}", outcome.message);
    let terminal_path = outcome.terminal_receipt_path.expect("terminal receipt");
    let terminal: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&terminal_path).unwrap()).unwrap();
    assert_eq!(terminal["outcome"], "failure");
    assert_eq!(terminal["failed_stage"], "build");
    assert_eq!(terminal["admission"], "closed");
    assert_eq!(
        terminal["reason"].as_str().unwrap(),
        "controller build/seal/activation stages not yet enabled in this revision; admission left closed; fix-forward"
    );
    cleanup(&tree);
}

#[test]
fn in_use_port_fails_preflight_end_to_end() {
    let tree = build_tree("port-in-use");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    tree.rewrite_target(|text| text.replace("\"local_ports\": []", &format!("\"local_ports\": [{port}]")));
    let outcome = run_command(CommandMode::Preflight, &tree.config_path, FIXED_NOW, &FixtureExecutor::passing())
        .expect("run");
    assert_eq!(outcome.exit_code, 1);
    assert!(outcome.message.contains("port-ownership"), "message: {}", outcome.message);
    let run_dir = tree.single_run_dir();
    let preflight_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("preflight.json")).unwrap()).unwrap();
    assert_eq!(preflight_json["verdict"], "fail");
    let port_check = preflight_json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["id"] == "port-ownership")
        .unwrap();
    assert_eq!(port_check["status"], "fail");
    assert!(port_check["detail"].as_str().unwrap().contains(&port.to_string()));
    drop(listener);
    cleanup(&tree);
}

#[test]
fn contract_hash_drift_fails_preflight_end_to_end() {
    let tree = build_tree("contract-drift");
    tree.rewrite_target(|text| {
        text.replace(
            "\"ssh_host\":",
            "\"frontend_contract_sha256\": \"sha256:deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef\",\n  \"ssh_host\":",
        )
    });
    let outcome = run_command(CommandMode::Preflight, &tree.config_path, FIXED_NOW, &FixtureExecutor::passing())
        .expect("run");
    assert_eq!(outcome.exit_code, 1);
    assert!(outcome.message.contains("frontend-contract-hash"), "message: {}", outcome.message);
    cleanup(&tree);
}

#[test]
fn strict_config_rejects_unknown_fields_without_receipts() {
    let tree = build_tree("strict-config");
    let path = tree.config_path.clone();
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 1,
            "provider": "deepseek",
            "agent_source_root": tree.root.join("agent"),
            "frontend_source_root": tree.root.join("front"),
            "operator_root": tree.operator_root(),
            "runtime_env": tree.operator_root().join("runtime/krw-agent-deploy.env"),
            "target_file": tree.operator_root().join("ops/production-target.json"),
            "frontend_contract": tree.operator_root().join("ops/agent-v1-deployment-contract.json"),
            "timeouts": {"ssh_ms": 5000, "mcp_ms": 5000, "daemon_ready_ms": 90000, "public_ready_ms": 90000},
            "provider_candidates": ["glm", "deepseek"]
        }))
        .unwrap(),
    )
    .unwrap();
    let error = run_command(CommandMode::Deploy, &path, FIXED_NOW, &FixtureExecutor::passing()).unwrap_err();
    assert!(matches!(error, ControllerError::Config(_)), "unexpected error: {error}");
    assert!(!tree.receipt_root().exists(), "no receipts without a parseable config");
    cleanup(&tree);
}

#[test]
fn repeated_run_at_same_timestamp_refuses_to_overwrite_receipts() {
    let tree = build_tree("collision");
    // Same config bytes + same fixed clock => identical run id and receipt
    // directory. Receipts are immutable: the second run must be refused
    // instead of silently overwriting the first run's evidence.
    let first = run_command(CommandMode::Preflight, &tree.config_path, FIXED_NOW, &FixtureExecutor::passing())
        .expect("first run");
    assert_eq!(first.exit_code, 0);
    let second = run_command(CommandMode::Preflight, &tree.config_path, FIXED_NOW, &FixtureExecutor::passing());
    assert!(second.is_err(), "receipt overwrite must be refused: {second:?}");
    let receipt_entries = std::fs::read_dir(tree.receipt_root()).unwrap().count();
    assert_eq!(receipt_entries, 1, "still exactly one run directory");
    cleanup(&tree);
}

#[test]
fn fixtures_are_dummy_only_no_secrets() {
    assert!(!CONTRACT_FIXTURE.contains("sk-"));
    assert!(!TARGET_FIXTURE.contains("password"));
    assert!(TARGET_FIXTURE.contains("dummy-placeholder"));
}
