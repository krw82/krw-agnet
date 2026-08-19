//! End-to-end integration tests through the controller library API.
//!
//! These tests drive `krw_agent_deploy::stages::run_command` (the exact code
//! the thin `main.rs` CLI calls) against a tempdir fixture tree assembled
//! from the checked-in fixtures under `tests/fixtures/`. Nothing touches a
//! real operator root; the fixture executors stand in for world-observing
//! probes (preflight) and world-mutating stage commands (deploy).

use std::collections::BTreeSet;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use krw_agent_deploy::executor::FixtureExecutor;
use krw_agent_deploy::pipeline::FixtureStageExecutor;
use krw_agent_deploy::stages::{CommandMode, ControllerError, run_command};

const CONTRACT_FIXTURE: &str = include_str!("fixtures/agent-v1-deployment-contract.json");
const TARGET_FIXTURE: &str = include_str!("fixtures/production-target.json");

static DIR_SEQUENCE: AtomicU32 = AtomicU32::new(0);
/// 2026-08-16T01:20:00Z (fixed clock keeps run ids deterministic).
const FIXED_NOW: u64 = 1_786_843_200;

/// Rollback-shaped tokens that must never appear in any controller command
/// id or argv (fix-forward only; the controller has no rollback paths).
const ROLLBACK_TOKENS: [&str; 5] = ["rollback", "restore", "revert", "migrate down", "db:reset"];

/// The one sanctioned appearance of a rollback-shaped token: the legacy
/// stale-image-tag PRUNE pattern (`candidate-*|release-*|rollback-*`) in the
/// stage-12 best-effort cleanup payload. Removing superseded tag labels is
/// artifact cleanup, not a rollback path.
const PRUNE_TAG_PATTERN_EXCEPTION: &str = "cleanup.remote-prune-stale";

fn assert_no_rollback_commands(executor: &FixtureStageExecutor) {
    for command in executor.commands.borrow().iter() {
        let haystack = format!("{} {}", command.id, command.argv.join(" "));
        for token in ROLLBACK_TOKENS {
            if command.id == PRUNE_TAG_PATTERN_EXCEPTION && token == "rollback" {
                continue;
            }
            assert!(
                !haystack.to_ascii_lowercase().contains(token),
                "rollback-shaped token `{token}` found in command `{}`: {haystack}",
                command.id
            );
        }
    }
}

/// gcloud transport argv prefix for the checked-in fixture target (gcp
/// block declared): instance, project, and zone exactly as recorded.
const GCLOUD_ARGV_PREFIX: [&str; 9] = [
    "gcloud",
    "compute",
    "ssh",
    "krw-agent-prod-dummy-placeholder",
    "--project",
    "krw-prod-dummy-placeholder",
    "--zone",
    "asia-northeast3-a",
    "--command",
];

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

    fn terminal_json(&self) -> serde_json::Value {
        let run_dir = self.single_run_dir();
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("terminal.json")).unwrap())
            .unwrap()
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
    std::fs::write(
        agent_root.join("Cargo.toml"),
        "[package]\nname = \"fixture-agent\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(&frontend_root).unwrap();
    std::fs::create_dir_all(operator_root.join("signing")).unwrap();
    std::fs::create_dir_all(operator_root.join("ops")).unwrap();
    std::fs::write(
        operator_root.join("signing/release-private.pk8"),
        b"fixture-pkcs8-material",
    )
    .unwrap();
    std::fs::create_dir_all(operator_root.join("runtime")).unwrap();
    // Operator provider configuration consumed by the seal stage (both
    // providers: the seal loop mirrors the legacy dual-provider flow).
    let registry = krw_agent_release_authorization::ReleaseTrustRegistryV1 {
        schema_version: 1,
        registry_id: "krw.deploy.integration-test".to_owned(),
        minimum_sequence: 1,
        keys: vec![krw_agent_release_authorization::ReleaseTrustKeyV1 {
            key_id: "integration-test-key".to_owned(),
            ed25519_public_key_hex: "55".repeat(32),
            not_before_unix_seconds: 0,
            not_after_unix_seconds: 4_102_444_800,
            revoked: false,
        }],
    };
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
        CONTRACT_FIXTURE,
    )
    .unwrap();
    std::fs::write(
        operator_root.join("ops/production-target.json"),
        TARGET_FIXTURE,
    )
    .unwrap();
    std::fs::write(
        operator_root.join("runtime/krw-agent-deploy.env"),
        "# fixture env: names only, dummy values\nKRW_AGENT_DB_URL=postgresql://fixture-dummy\nKRW_AGENT_DATABASE_URL=postgresql://fixture-dummy-agent\n",
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

/// Ordered command ids the remote-target happy path must issue.
const REMOTE_HAPPY_PATH_COMMANDS: [&str; 45] = [
    // stage 3 build
    "build.dual-provider-bundles",
    // stage 4 seal (legacy dual loop: the non-selected provider seals first
    // with suffixed ids, then the selected provider, then the finalize gate)
    "seal.prepare-production-candidate-glm",
    "seal.sign-release-authorization-glm",
    "seal.seal-production-candidate-glm",
    "seal.prepare-production-candidate",
    "seal.sign-release-authorization",
    "seal.seal-production-candidate",
    "seal.finalize-dual-release",
    // stage 5 frontend source preparation (LOCAL: archive + descriptor copy
    // only — the image is built ON THE VM from the shipped source)
    "frontend-image.resolve-commit",
    "frontend-image.archive-source",
    "frontend-image.descriptor-copy",
    // stage 6 migrations (rpc contract gate + read-only dry-run)
    "migrations.rpc-contract-verify",
    "migrations.db-push-dry-run",
    // stage 7 admission close + legacy migration/ABI order
    "admission.read-previous",
    "admission.close",
    "admission.verify-closed",
    "migrations.agent-release-apply",
    "migrations.db-push",
    "migrations.schema-smoke",
    "migrations.sealed-database-abi",
    "migrations.abi-verify-procedure-0",
    "migrations.abi-verify-column-0",
    // stage 8 local activation (legacy order incl. gateways + daemon start)
    "activation.agentd-stage",
    "activation.agentd-activate",
    "activation.capabilityd-activate",
    "activation.mcp-gateways-prepare",
    "activation.mcp-gateways-activate",
    "activation.sealed-mcp-abi",
    "activation.agentd-start",
    // stage 9 shipping (source archive + descriptor only) + remote activation
    "ship.scp-front-archive",
    "ship.scp-agent-descriptor",
    "ship.remote-prepare",
    "remote.candidate-abi",
    "remote.web-up",
    "readiness.remote-web-healthz",
    // stage 10 deep readiness
    "readiness.daemon-metrics",
    "readiness.mcp-ready-0",
    "readiness.web-deep",
    "readiness.public-healthz-0",
    "readiness.public-healthz-1",
    "readiness.db-heartbeat",
    // stage 11 admission open
    "admission.open",
    "readiness.admission-open-verify",
    // stage 12 best-effort cleanup
    "cleanup.remote-prune-stale",
    "cleanup.remote-prune-post-activation",
];

#[test]
fn preflight_then_dry_run_produce_read_only_receipts() {
    let tree = build_tree("happy");

    // Preflight: read-only, receipt verdict pass, no terminal receipt.
    let preflight = run_command(
        CommandMode::Preflight,
        &tree.config_path,
        FIXED_NOW,
        &FixtureExecutor::passing(),
        &FixtureStageExecutor::passing(),
    )
    .expect("preflight run");
    assert_eq!(preflight.exit_code, 0, "message: {}", preflight.message);
    let run_dir = tree.single_run_dir();
    assert!(run_dir.join("preflight.json").is_file());
    assert!(!run_dir.join("terminal.json").exists());
    let preflight_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("preflight.json")).unwrap())
            .unwrap();
    assert_eq!(preflight_json["verdict"], "pass");
    assert_eq!(preflight_json["executor_kind"], "fixture");
    let checks = preflight_json["checks"].as_array().unwrap();
    assert_eq!(checks.len(), 15);
    // Remote fixtures from production-target.json are exercised, not skipped.
    let ids = checks
        .iter()
        .map(|check| check["id"].as_str().unwrap().to_owned())
        .collect::<BTreeSet<_>>();
    for expected in [
        "gcp-target-describable",
        "ssh-remote-reachable",
        "db-agent-v1-abi",
        "mcp-endpoint-reachable",
        "supabase-migration-plan",
        "remote-env-required-keys",
    ] {
        assert!(
            ids.contains(expected),
            "missing check {expected} in {ids:?}"
        );
    }
    assert_eq!(
        preflight_json["target_identity"]["provider"], "deepseek",
        "target identity recorded: {}",
        preflight_json["target_identity"]
    );

    // Dry-run at a different timestamp: full receipt chain, dry-run-ok.
    let dry_run = run_command(
        CommandMode::DryRun,
        &tree.config_path,
        FIXED_NOW + 60,
        &FixtureExecutor::passing(),
        &FixtureStageExecutor::passing(),
    )
    .expect("dry-run run");
    assert_eq!(dry_run.exit_code, 0, "message: {}", dry_run.message);
    assert_eq!(dry_run.outcome, "dry-run-ok");
    let dry_run_dir = dry_run
        .terminal_receipt_path
        .expect("terminal receipt")
        .parent()
        .unwrap()
        .to_path_buf();
    let terminal: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dry_run_dir.join("terminal.json")).unwrap())
            .unwrap();
    assert_eq!(terminal["outcome"], "dry-run-ok");
    assert_eq!(terminal["admission"], "not-touched");
    assert_eq!(terminal["reached_stage"], "dry_run");
    assert!(
        terminal["receipt_path"]
            .as_str()
            .unwrap()
            .ends_with("preflight.json")
    );

    // Read-only invariant: no release output tree, only receipts under
    // deploy-receipts, and the source fixtures are untouched.
    assert!(!tree.operator_root().join("releases").exists());
    let receipt_entries = std::fs::read_dir(tree.receipt_root()).unwrap().count();
    assert_eq!(receipt_entries, 2, "two runs, two receipt directories");
    cleanup(&tree);
}

#[test]
fn deploy_full_walk_on_remote_target_reaches_open_success() {
    let tree = build_tree("deploy-success");
    let stages = FixtureStageExecutor::passing();
    let outcome = run_command(
        CommandMode::Deploy,
        &tree.config_path,
        FIXED_NOW,
        &FixtureExecutor::passing(),
        &stages,
    )
    .expect("deploy run");
    assert_eq!(outcome.exit_code, 0, "message: {}", outcome.message);
    assert_eq!(outcome.outcome, "success");

    // Every stage receipt 3-12 exists, write-once, in the run dir.
    let run_dir = tree.single_run_dir();
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

    // Ordered command records: the exact executor walk. (TCP probes are
    // recorded separately and asserted below.)
    let expected = REMOTE_HAPPY_PATH_COMMANDS.to_vec();
    assert_eq!(
        stages.command_ids(),
        expected,
        "ordered command ids diverged"
    );
    assert_eq!(
        stages.probes.borrow().len(),
        1,
        "one TCP probe for the one mcp endpoint"
    );
    assert_eq!(stages.probes.borrow()[0].id, "readiness.mcp-tcp-0");
    assert_no_rollback_commands(&stages);

    // The fixture target declares a gcp block => every remote command rides
    // the production gcloud transport with the shell payload as the single
    // --command element.
    for id in [
        "admission.read-previous",
        "admission.close",
        "admission.verify-closed",
        "ship.remote-prepare",
        "remote.candidate-abi",
        "remote.web-up",
        "readiness.remote-web-healthz",
        "readiness.web-deep",
        "admission.open",
        "readiness.admission-open-verify",
        "cleanup.remote-prune-stale",
        "cleanup.remote-prune-post-activation",
    ] {
        let command = stages
            .commands
            .borrow()
            .iter()
            .find(|command| command.id == id)
            .cloned()
            .unwrap_or_else(|| panic!("remote command `{id}` missing"));
        assert_eq!(
            &command.argv[..9],
            &GCLOUD_ARGV_PREFIX[..],
            "command {id}: {:?}",
            command.argv
        );
        assert_eq!(
            command.argv.len(),
            10,
            "one payload element: {:?}",
            command.argv
        );
        assert!(
            command.env_keys_used.is_empty(),
            "no env keys on transport argv: {id}"
        );
    }

    // Uploads ride the legacy gcloud scp transport with keep-alive flags —
    // and the ship manifest is EXACTLY the two legacy uploads: the immutable
    // source archive and the public descriptor. No image is ever shipped.
    let expected_scp_prefix: Vec<String> = [
        "gcloud",
        "compute",
        "scp",
        "--project",
        "krw-prod-dummy-placeholder",
        "--zone",
        "asia-northeast3-a",
        "--scp-flag=-oServerAliveInterval=15",
        "--scp-flag=-oServerAliveCountMax=8",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let scp_ids = stages
        .commands
        .borrow()
        .iter()
        .filter(|command| command.id.starts_with("ship.scp-"))
        .map(|command| command.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        scp_ids,
        [
            "ship.scp-front-archive".to_owned(),
            "ship.scp-agent-descriptor".to_owned()
        ],
        "exactly the two legacy uploads: {scp_ids:?}"
    );
    for (id, expected_remote, expected_local_suffix) in [
        (
            "ship.scp-front-archive",
            "/tmp/krw-front-20260816T012000Z-",
            "ship/front.tar.gz",
        ),
        (
            "ship.scp-agent-descriptor",
            "/tmp/krw-agent-public-release-20260816T012000Z-",
            "ship/public-release.json",
        ),
    ] {
        let command = stages
            .commands
            .borrow()
            .iter()
            .find(|command| command.id == id)
            .cloned()
            .unwrap_or_else(|| panic!("scp command `{id}` missing"));
        assert_eq!(
            &command.argv[..9],
            &expected_scp_prefix[..],
            "command {id}: {:?}",
            command.argv
        );
        assert_eq!(
            command.argv.len(),
            11,
            "local + remote path pair: {:?}",
            command.argv
        );
        assert!(
            command.argv[9].ends_with(expected_local_suffix),
            "command {id}: {:?}",
            command.argv
        );
        assert!(
            command.argv[10].starts_with("krw-agent-prod-dummy-placeholder:"),
            "command {id}: {:?}",
            command.argv
        );
        assert!(
            command.argv[10].contains(expected_remote),
            "command {id}: {:?}",
            command.argv
        );
    }

    // No-local-docker invariant: the controller NEVER builds, tags, saves, or
    // inspects an image locally — every docker interaction lives inside the
    // remote prepare/activate payloads, exactly like the legacy script.
    let local_docker = stages
        .commands
        .borrow()
        .iter()
        .filter(|command| {
            command
                .argv
                .first()
                .is_some_and(|program| program == "docker")
        })
        .map(|command| command.id.clone())
        .collect::<Vec<_>>();
    assert!(
        local_docker.is_empty(),
        "no local docker commands may exist: {local_docker:?}"
    );

    // The remote prepare payload builds, tags, and candidate-validates the
    // image ON THE VM from the shipped source (legacy verbatim).
    let prepare = stages
        .commands
        .borrow()
        .iter()
        .find(|command| command.id == "ship.remote-prepare")
        .cloned()
        .expect("remote prepare command");
    let payload = &prepare.argv[9];
    assert!(
        payload.contains("compose_stage build web"),
        "payload must build on the VM"
    );
    assert!(
        payload.contains("docker tag \"$CANDIDATE_IMAGE\" \"$CANDIDATE_TAG\""),
        "payload must tag the candidate on the VM"
    );
    assert!(
        payload.contains("compose_stage run --rm --no-deps -T --entrypoint node web - <<'NODE'"),
        "payload runs the in-container candidate preflight"
    );
    assert!(
        payload.contains("gcp_agent_v1_candidate_preflight"),
        "payload embeds the legacy preflight check id"
    );
    assert!(
        !payload.contains("docker load"),
        "payload must never load a shipped image: {payload}"
    );
    // The remote build reports the candidate image id as the run's digest.
    let terminal_for_digest = tree.terminal_json();
    assert!(
        terminal_for_digest["artifacts"]["frontend_image_digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:"),
        "remote-built digest recorded: {terminal_for_digest}"
    );

    // The billing-v2 cutover helpers stay gated OFF in the normal
    // --with-agent-release path (legacy: BILLING_V2_CUTOVER=0): no billing
    // command ever runs.
    assert!(
        !stages.command_ids().iter().any(|id| id.contains("billing")),
        "billing cutover commands must not run on the normal path: {:?}",
        stages.command_ids()
    );

    // Stage 12 receipt records the best-effort cleanup commands as passed.
    let stage12: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("stage-12-terminal_success_receipt.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(stage12["status"], "pass");
    assert_eq!(
        stage12["commands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|record| record["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "cleanup.remote-prune-stale",
            "cleanup.remote-prune-post-activation"
        ]
    );

    // Terminal success receipt with the full artifact chain.
    let terminal = tree.terminal_json();
    assert_eq!(terminal["outcome"], "success");
    assert_eq!(terminal["admission"], "open");
    assert_eq!(terminal["reached_stage"], "terminal_success_receipt");
    assert!(terminal.get("failed_stage").is_none());
    assert_eq!(terminal["artifacts"]["previous_admission"], "open");
    assert_eq!(
        terminal["artifacts"]["applied_migrations"],
        serde_json::json!(["0001_agent_v1", "0022_daemon_mcp_readiness"])
    );
    assert!(
        terminal["artifacts"]["descriptor_sha256"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(
        terminal["artifacts"]["frontend_image_digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(
        terminal.get("skipped").is_none(),
        "no skips on a remote-target walk: {terminal}"
    );

    // Stage receipts keep env placeholders unresolved (no secret values).
    let stage7: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("stage-7-admission_close.json")).unwrap(),
    )
    .unwrap();
    let abi_argv = stage7["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| {
            record["id"]
                .as_str()
                .unwrap()
                .starts_with("migrations.abi-verify-procedure")
        })
        .unwrap()["argv"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(abi_argv[1], "<env:KRW_AGENT_DB_URL>");
    assert!(
        !std::fs::read_to_string(run_dir.join("stage-7-admission_close.json"))
            .unwrap()
            .contains("postgresql://fixture-dummy")
    );
    cleanup(&tree);
}

/// One representative injected failure per stage 3..11: terminal failure
/// receipt, correct failed stage, correct admission posture, and no
/// rollback-shaped commands anywhere in the walk.
#[test]
fn deploy_failure_matrix_covers_every_stage_three_to_eleven() {
    let matrix: [(&str, &str, &str); 9] = [
        // (injected failing command id, expected failed_stage, expected admission)
        ("build.dual-provider-bundles", "build", "not-touched"),
        ("seal.prepare-production-candidate", "seal", "not-touched"),
        (
            "frontend-image.archive-source",
            "frontend_image_prepare",
            "not-touched",
        ),
        ("migrations.db-push-dry-run", "migrations", "not-touched"),
        ("admission.close", "admission_close", "closed"),
        ("activation.agentd-stage", "local_activation", "closed"),
        ("remote.web-up", "remote_activation", "closed"),
        ("readiness.daemon-metrics", "deep_readiness", "closed"),
        ("admission.open", "admission_open", "closed"),
    ];
    for (failing_id, failed_stage, admission) in matrix {
        let tag: String = failing_id
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character
                } else {
                    '-'
                }
            })
            .collect();
        let tree = build_tree(&format!("matrix-{tag}"));
        let mut stages = FixtureStageExecutor::passing();
        stages
            .failures
            .insert(failing_id.to_owned(), "injected failure".to_owned());
        let outcome = run_command(
            CommandMode::Deploy,
            &tree.config_path,
            FIXED_NOW,
            &FixtureExecutor::passing(),
            &stages,
        )
        .expect("deploy run completes (fail-closed)");
        assert_eq!(
            outcome.exit_code, 1,
            "[{failing_id}] message: {}",
            outcome.message
        );
        assert_eq!(outcome.outcome, "failure", "[{failing_id}]");
        let terminal = tree.terminal_json();
        assert_eq!(
            terminal["outcome"], "failure",
            "[{failing_id}] terminal: {terminal}"
        );
        assert_eq!(
            terminal["failed_stage"], failed_stage,
            "[{failing_id}] terminal: {terminal}"
        );
        assert_eq!(
            terminal["reached_stage"], failed_stage,
            "[{failing_id}] terminal: {terminal}"
        );
        assert_eq!(
            terminal["admission"], admission,
            "[{failing_id}] terminal: {terminal}"
        );
        assert!(
            terminal["reason"].as_str().unwrap().contains(failing_id),
            "[{failing_id}] reason: {terminal}"
        );
        assert!(
            terminal["reason"].as_str().unwrap().contains("fix-forward"),
            "[{failing_id}] reason: {terminal}"
        );

        // The failing stage's own receipt records the failed command.
        let run_dir = tree.single_run_dir();
        let index = REMOTE_HAPPY_PATH_COMMANDS
            .iter()
            .position(|id| *id == failing_id)
            .unwrap_or_else(|| panic!("[{failing_id}] not part of the happy-path command set"));
        let stage_file = stage_receipt_file_for(failed_stage);
        let stage_json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(run_dir.join(&stage_file)).unwrap())
                .unwrap();
        assert_eq!(
            stage_json["status"], "fail",
            "[{failing_id}] stage receipt: {stage_json}"
        );
        assert!(
            stage_json["commands"]
                .as_array()
                .unwrap()
                .iter()
                .any(|record| record["id"] == failing_id && record["outcome"] == "fail"),
            "[{failing_id}] failed command not recorded: {stage_json}"
        );
        let _ = index;

        // No stage AFTER the failed one left a receipt.
        let stage_order = [
            "build",
            "seal",
            "frontend_image_prepare",
            "migrations",
            "admission_close",
            "local_activation",
            "remote_activation",
            "deep_readiness",
            "admission_open",
            "terminal_success_receipt",
        ];
        let failed_position = stage_order
            .iter()
            .position(|stage| *stage == failed_stage)
            .unwrap();
        for later in stage_order.iter().skip(failed_position + 1) {
            assert!(
                !run_dir.join(stage_receipt_file_for(later)).exists(),
                "[{failing_id}] later stage receipt `{later}` must not exist"
            );
        }

        // Fix-forward: the walk issued no rollback-shaped command, ever.
        assert_no_rollback_commands(&stages);
        cleanup(&tree);
    }
}

fn stage_receipt_file_for(stage: &str) -> String {
    let index = match stage {
        "build" => 3,
        "seal" => 4,
        "frontend_image_prepare" => 5,
        "migrations" => 6,
        "admission_close" => 7,
        "local_activation" => 8,
        "remote_activation" => 9,
        "deep_readiness" => 10,
        "admission_open" => 11,
        "terminal_success_receipt" => 12,
        other => panic!("unknown stage {other}"),
    };
    format!("stage-{index}-{stage}.json")
}

#[test]
fn migration_dry_run_failure_aborts_before_any_mutation() {
    let tree = build_tree("migration-dry-run-fail");
    let mut stages = FixtureStageExecutor::passing();
    stages.failures.insert(
        "migrations.db-push-dry-run".to_owned(),
        "injected dry-run failure".to_owned(),
    );
    let outcome = run_command(
        CommandMode::Deploy,
        &tree.config_path,
        FIXED_NOW,
        &FixtureExecutor::passing(),
        &stages,
    )
    .expect("run");
    assert_eq!(outcome.exit_code, 1);
    let terminal = tree.terminal_json();
    assert_eq!(terminal["failed_stage"], "migrations");
    assert_eq!(terminal["admission"], "not-touched");
    let ids = stages.command_ids();
    assert!(ids.contains(&"migrations.db-push-dry-run".to_owned()));
    assert!(
        !ids.contains(&"migrations.db-push".to_owned()),
        "real push must never run: {ids:?}"
    );
    assert!(
        !ids.iter().any(|id| id.starts_with("admission.")),
        "admission untouched: {ids:?}"
    );
    assert!(
        !ids.iter().any(|id| id.starts_with("activation.")),
        "no activation: {ids:?}"
    );
    // The runtime env file was never rewritten by admission upserts.
    let env_text =
        std::fs::read_to_string(tree.operator_root().join("runtime/krw-agent-deploy.env")).unwrap();
    assert!(
        !env_text.contains("KRW_AGENT_ADMISSION_MODE"),
        "env: {env_text}"
    );
    cleanup(&tree);
}

#[test]
fn abi_verification_failure_after_push_keeps_admission_closed() {
    // The 05 migration ordering rule: the push has already happened, so the
    // failure must leave admission closed with the applied list recorded.
    let tree = build_tree("abi-fail");
    let mut stages = FixtureStageExecutor::passing();
    stages.failures.insert(
        "migrations.abi-verify-procedure-0".to_owned(),
        "injected ABI failure".to_owned(),
    );
    let outcome = run_command(
        CommandMode::Deploy,
        &tree.config_path,
        FIXED_NOW,
        &FixtureExecutor::passing(),
        &stages,
    )
    .expect("run");
    assert_eq!(outcome.exit_code, 1);
    let terminal = tree.terminal_json();
    assert_eq!(terminal["failed_stage"], "admission_close");
    assert_eq!(terminal["admission"], "closed");
    assert_eq!(terminal["artifacts"]["previous_admission"], "open");
    assert_eq!(
        terminal["artifacts"]["applied_migrations"],
        serde_json::json!(["0001_agent_v1", "0022_daemon_mcp_readiness"]),
        "pushed migrations recorded for fix-forward"
    );
    let ids = stages.command_ids();
    assert!(
        ids.contains(&"migrations.db-push".to_owned()),
        "push ran before the ABI check: {ids:?}"
    );
    assert!(
        !ids.iter().any(|id| id.starts_with("activation.")),
        "no activation after ABI failure"
    );
    // No rollback of the pushed schema.
    assert_no_rollback_commands(&stages);
    cleanup(&tree);
}

#[test]
fn drift_between_preflight_and_mutating_stage_fails_closed() {
    let tree = build_tree("drift");
    let mut stages = FixtureStageExecutor::passing();
    // The preflight fixture executor still reports the original HEAD; the
    // stage executor now observes a different one. Stage 5 pins the frontend
    // commit against the preflight receipt, so the drift fires there —
    // before anything mutates.
    stages.head = "ffffffffffffffffffffffffffffffffffffffff".to_owned();
    let outcome = run_command(
        CommandMode::Deploy,
        &tree.config_path,
        FIXED_NOW,
        &FixtureExecutor::passing(),
        &stages,
    )
    .expect("run");
    assert_eq!(outcome.exit_code, 1, "message: {}", outcome.message);
    let terminal = tree.terminal_json();
    assert_eq!(
        terminal["failed_stage"], "frontend_image_prepare",
        "the frontend commit pin catches drift at the image stage: {terminal}"
    );
    assert!(
        terminal["reason"].as_str().unwrap().contains("drifted"),
        "reason: {terminal}"
    );
    assert_eq!(terminal["admission"], "not-touched");
    let ids = stages.command_ids();
    assert!(
        !ids.iter().any(|id| id.contains("migrations.")),
        "no migration command after drift: {ids:?}"
    );
    assert!(
        !ids.iter().any(|id| id.starts_with("admission.")),
        "no admission command after drift: {ids:?}"
    );
    let run_dir = tree.single_run_dir();
    let stage5: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("stage-5-frontend_image_prepare.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(stage5["status"], "fail");
    assert!(
        stage5["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|record| record["id"] == "frontend-image.resolve-commit"),
        "the drift was observed through the recorded resolve command: {stage5}"
    );
    cleanup(&tree);
}

#[test]
fn local_only_target_skips_remote_and_runs_local_admission() {
    let tree = build_tree("local-only");
    tree.rewrite_target(|text| {
        text.replace("\"ssh_host\": \"deploy@127.0.0.1\",", "")
            .replace("\"remote_front_dir\": \"/home/deploy/krw-ontology-front-dummy-placeholder\",", "")
            .replace("\"compose_project\": \"krw-ontology-front-dummy\",", "")
            .replace(
                "\"gcp\": {\n    \"project\": \"krw-prod-dummy-placeholder\",\n    \"zone\": \"asia-northeast3-a\",\n    \"instance\": \"krw-agent-prod-dummy-placeholder\",\n    \"instance_id\": \"0000000000000000000\"\n  },\n  ",
                "",
            )
    });
    let stages = FixtureStageExecutor::passing();
    let outcome = run_command(
        CommandMode::Deploy,
        &tree.config_path,
        FIXED_NOW,
        &FixtureExecutor::passing(),
        &stages,
    )
    .expect("run");
    assert_eq!(outcome.exit_code, 0, "message: {}", outcome.message);
    let ids = stages.command_ids();
    assert!(
        !ids.iter().any(|id| id.starts_with("remote.")),
        "no remote activation: {ids:?}"
    );
    assert!(
        !ids.iter()
            .any(|id| id.contains("ssh") || id == "readiness.remote-web-healthz"),
        "no ssh: {ids:?}"
    );
    assert!(
        ids.contains(&"admission.close-local".to_owned()),
        "local close command: {ids:?}"
    );
    assert!(
        ids.contains(&"admission.verify-closed-local".to_owned()),
        "local close verify: {ids:?}"
    );
    assert!(
        ids.contains(&"admission.open-local".to_owned()),
        "local open command: {ids:?}"
    );
    assert!(
        ids.contains(&"readiness.admission-open-verify-local".to_owned()),
        "local open verify: {ids:?}"
    );

    let terminal = tree.terminal_json();
    assert_eq!(terminal["outcome"], "success");
    assert_eq!(terminal["admission"], "open");
    let skipped = terminal["skipped"].as_array().unwrap();
    assert!(
        skipped
            .iter()
            .any(|record| record["stage"] == "remote_activation"),
        "skipped: {skipped:?}"
    );
    assert!(
        skipped
            .iter()
            .any(|record| record["stage"] == "admission_close"
                && record["operation"] == "admission.read-previous"),
        "remote admission half skipped: {skipped:?}"
    );
    // previous admission was read from the LOCAL runtime env (absent -> closed).
    assert_eq!(terminal["artifacts"]["previous_admission"], "closed");
    // The local admission upserts actually rewrote the runtime env file.
    let env_text =
        std::fs::read_to_string(tree.operator_root().join("runtime/krw-agent-deploy.env")).unwrap();
    assert!(
        env_text.contains("KRW_AGENT_ADMISSION_MODE=open"),
        "env: {env_text}"
    );
    let run_dir = tree.single_run_dir();
    let stage9: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("stage-9-remote_activation.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(stage9["status"], "skipped");
    assert_no_rollback_commands(&stages);
    cleanup(&tree);
}

#[test]
fn admission_capture_records_the_previous_gate_value_verbatim() {
    // A previously `drain` gate is captured as the previous value and still
    // ends `open` on success (a fresh release must be usable; the captured
    // value is recorded for the operator, not restored). Local-only target
    // (no gcp block, no ssh_host) so the captured value comes from the
    // local runtime env file.
    let tree = build_tree("admission-capture");
    std::fs::write(
        tree.operator_root().join("runtime/krw-agent-deploy.env"),
        "# fixture env: names only, dummy values\nKRW_AGENT_DB_URL=postgresql://fixture-dummy\nKRW_AGENT_DATABASE_URL=postgresql://fixture-dummy-agent\nKRW_AGENT_ADMISSION_MODE=drain\n",
    )
    .unwrap();
    tree.rewrite_target(|text| {
        text.replace("\"ssh_host\": \"deploy@127.0.0.1\",", "")
            .replace(
                "\"gcp\": {\n    \"project\": \"krw-prod-dummy-placeholder\",\n    \"zone\": \"asia-northeast3-a\",\n    \"instance\": \"krw-agent-prod-dummy-placeholder\",\n    \"instance_id\": \"0000000000000000000\"\n  },\n  ",
                "",
            )
    });
    let stages = FixtureStageExecutor::passing();
    let outcome = run_command(
        CommandMode::Deploy,
        &tree.config_path,
        FIXED_NOW,
        &FixtureExecutor::passing(),
        &stages,
    )
    .expect("run");
    assert_eq!(outcome.exit_code, 0, "message: {}", outcome.message);
    let ids = stages.command_ids();
    assert!(
        ids.contains(&"admission.close-local".to_owned()),
        "local admission path: {ids:?}"
    );
    assert!(
        !ids.iter().any(|id| id == "admission.read-previous"),
        "remote read skipped: {ids:?}"
    );
    let terminal = tree.terminal_json();
    assert_eq!(terminal["artifacts"]["previous_admission"], "drain");
    assert_eq!(terminal["admission"], "open");
    let env_text =
        std::fs::read_to_string(tree.operator_root().join("runtime/krw-agent-deploy.env")).unwrap();
    assert!(!env_text.contains("drain"), "env: {env_text}");
    cleanup(&tree);
}

#[test]
fn ssh_only_target_keeps_plain_ssh_remote_transport() {
    // No gcp block: the remote commands stay on the plain ssh transport
    // (BatchMode + ConnectTimeout from config), same payloads and stages.
    let tree = build_tree("ssh-transport");
    tree.rewrite_target(|text| {
        text.replace(
            "\"gcp\": {\n    \"project\": \"krw-prod-dummy-placeholder\",\n    \"zone\": \"asia-northeast3-a\",\n    \"instance\": \"krw-agent-prod-dummy-placeholder\",\n    \"instance_id\": \"0000000000000000000\"\n  },\n  ",
            "",
        )
    });
    let stages = FixtureStageExecutor::passing();
    let outcome = run_command(
        CommandMode::Deploy,
        &tree.config_path,
        FIXED_NOW,
        &FixtureExecutor::passing(),
        &stages,
    )
    .expect("run");
    assert_eq!(outcome.exit_code, 0, "message: {}", outcome.message);
    assert_eq!(outcome.outcome, "success");
    let read_previous = stages
        .commands
        .borrow()
        .iter()
        .find(|command| command.id == "admission.read-previous")
        .cloned()
        .expect("read-previous command");
    assert_eq!(read_previous.argv[0], "ssh");
    assert_eq!(&read_previous.argv[1..3], &["-o", "BatchMode=yes"]);
    assert_eq!(read_previous.argv[4], "ConnectTimeout=5");
    assert_eq!(read_previous.argv[5], "deploy@127.0.0.1");
    assert!(
        read_previous.argv[6].contains("KRW_AGENT_ADMISSION_MODE"),
        "payload: {}",
        read_previous.argv[6]
    );
    let web_up = stages
        .commands
        .borrow()
        .iter()
        .find(|command| command.id == "remote.web-up")
        .cloned()
        .expect("web-up command");
    assert_eq!(web_up.argv[0], "ssh");
    // The preflight receipt also records the plain-ssh reachability shape.
    let run_dir = tree.single_run_dir();
    let preflight_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("preflight.json")).unwrap())
            .unwrap();
    let reachability = preflight_json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["id"] == "ssh-remote-reachable")
        .unwrap();
    assert_eq!(reachability["status"], "pass");
    assert!(
        reachability["detail"]
            .as_str()
            .unwrap()
            .contains("ssh `deploy@127.0.0.1` reachable"),
        "detail: {}",
        reachability["detail"]
    );
    assert_no_rollback_commands(&stages);
    cleanup(&tree);
}

#[test]
fn deploy_with_failing_preflight_writes_no_stage_receipts() {
    let tree = build_tree("deploy-preflight-fail");
    std::fs::remove_file(tree.operator_root().join("signing/release-private.pk8")).unwrap();
    let stages = FixtureStageExecutor::passing();
    let outcome = run_command(
        CommandMode::Deploy,
        &tree.config_path,
        FIXED_NOW,
        &FixtureExecutor::passing(),
        &stages,
    )
    .expect("run");
    assert_eq!(outcome.exit_code, 1);
    let terminal = tree.terminal_json();
    assert_eq!(terminal["failed_stage"], "preflight");
    assert_eq!(terminal["admission"], "closed");
    let run_dir = tree.single_run_dir();
    assert!(!run_dir.join("stage-3-build.json").exists());
    assert!(
        stages.commands.borrow().is_empty(),
        "no stage command may run after a failed preflight"
    );
    cleanup(&tree);
}

#[test]
fn in_use_port_fails_preflight_end_to_end() {
    let tree = build_tree("port-in-use");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    tree.rewrite_target(|text| {
        text.replace("\"local_ports\": []", &format!("\"local_ports\": [{port}]"))
    });
    let outcome = run_command(
        CommandMode::Preflight,
        &tree.config_path,
        FIXED_NOW,
        &FixtureExecutor::passing(),
        &FixtureStageExecutor::passing(),
    )
    .expect("run");
    assert_eq!(outcome.exit_code, 1);
    assert!(
        outcome.message.contains("port-ownership"),
        "message: {}",
        outcome.message
    );
    let run_dir = tree.single_run_dir();
    let preflight_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("preflight.json")).unwrap())
            .unwrap();
    assert_eq!(preflight_json["verdict"], "fail");
    let port_check = preflight_json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["id"] == "port-ownership")
        .unwrap();
    assert_eq!(port_check["status"], "fail");
    assert!(
        port_check["detail"]
            .as_str()
            .unwrap()
            .contains(&port.to_string())
    );
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
    let outcome = run_command(
        CommandMode::Preflight,
        &tree.config_path,
        FIXED_NOW,
        &FixtureExecutor::passing(),
        &FixtureStageExecutor::passing(),
    )
    .expect("run");
    assert_eq!(outcome.exit_code, 1);
    assert!(
        outcome.message.contains("frontend-contract-hash"),
        "message: {}",
        outcome.message
    );
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
    let error = run_command(
        CommandMode::Deploy,
        &path,
        FIXED_NOW,
        &FixtureExecutor::passing(),
        &FixtureStageExecutor::passing(),
    )
    .unwrap_err();
    assert!(
        matches!(error, ControllerError::Config(_)),
        "unexpected error: {error}"
    );
    assert!(
        !tree.receipt_root().exists(),
        "no receipts without a parseable config"
    );
    cleanup(&tree);
}

#[test]
fn repeated_run_at_same_timestamp_refuses_to_overwrite_receipts() {
    let tree = build_tree("collision");
    // Same config bytes + same fixed clock => identical run id and receipt
    // directory. Receipts are immutable: the second run must be refused
    // instead of silently overwriting the first run's evidence.
    let first = run_command(
        CommandMode::Preflight,
        &tree.config_path,
        FIXED_NOW,
        &FixtureExecutor::passing(),
        &FixtureStageExecutor::passing(),
    )
    .expect("first run");
    assert_eq!(first.exit_code, 0);
    let second = run_command(
        CommandMode::Preflight,
        &tree.config_path,
        FIXED_NOW,
        &FixtureExecutor::passing(),
        &FixtureStageExecutor::passing(),
    );
    assert!(
        second.is_err(),
        "receipt overwrite must be refused: {second:?}"
    );
    let receipt_entries = std::fs::read_dir(tree.receipt_root()).unwrap().count();
    assert_eq!(receipt_entries, 1, "still exactly one run directory");
    cleanup(&tree);
}

#[test]
fn source_archive_failure_aborts_before_any_mutation() {
    // The candidate image is built LATE (inside the remote prepare in stage
    // 9); a failure while preparing the immutable source archive stops the
    // walk before anything ships or the gate moves. The remote preflight's
    // failure surface is covered by the remote-prepare failure test.
    let tree = build_tree("archive-source-fail");
    let mut stages = FixtureStageExecutor::passing();
    stages.failures.insert(
        "frontend-image.archive-source".to_owned(),
        "injected archive failure".to_owned(),
    );
    let outcome = run_command(
        CommandMode::Deploy,
        &tree.config_path,
        FIXED_NOW,
        &FixtureExecutor::passing(),
        &stages,
    )
    .expect("run");
    assert_eq!(outcome.exit_code, 1, "message: {}", outcome.message);
    let terminal = tree.terminal_json();
    assert_eq!(terminal["failed_stage"], "frontend_image_prepare");
    assert_eq!(terminal["admission"], "not-touched");
    let ids = stages.command_ids();
    assert!(
        ids.contains(&"frontend-image.archive-source".to_owned()),
        "ids: {ids:?}"
    );
    assert!(
        !ids.iter().any(|id| id.starts_with("ship.")),
        "nothing ships after a failed archive: {ids:?}"
    );
    assert!(
        !ids.iter().any(|id| id.starts_with("admission.")),
        "admission untouched: {ids:?}"
    );
    assert!(
        !ids.contains(&"migrations.db-push".to_owned()),
        "no push: {ids:?}"
    );
    // No docker interaction anywhere (local build is not part of the flow).
    assert!(
        !stages.commands.borrow().iter().any(|command| command
            .argv
            .first()
            .is_some_and(|program| program == "docker")),
        "no local docker commands: {:?}",
        stages.command_ids()
    );
    assert_no_rollback_commands(&stages);
    cleanup(&tree);
}

#[test]
fn remote_prepare_failure_keeps_admission_closed_after_uploads() {
    // A failure at the new shipping sub-stage: archives were uploaded, the
    // remote prepare failed — the deploy stops fail-closed with admission
    // already closed (schema may have moved under it).
    let tree = build_tree("remote-prepare-fail");
    let mut stages = FixtureStageExecutor::passing();
    stages.failures.insert(
        "ship.remote-prepare".to_owned(),
        "injected prepare failure".to_owned(),
    );
    let outcome = run_command(
        CommandMode::Deploy,
        &tree.config_path,
        FIXED_NOW,
        &FixtureExecutor::passing(),
        &stages,
    )
    .expect("run");
    assert_eq!(outcome.exit_code, 1);
    let terminal = tree.terminal_json();
    assert_eq!(terminal["failed_stage"], "remote_activation");
    assert_eq!(terminal["admission"], "closed");
    let ids = stages.command_ids();
    assert!(
        ids.contains(&"ship.scp-agent-descriptor".to_owned()),
        "uploads happened before prepare: {ids:?}"
    );
    assert!(
        !ids.contains(&"remote.candidate-abi".to_owned()),
        "no candidate ABI after failed prepare: {ids:?}"
    );
    assert!(
        !ids.contains(&"remote.web-up".to_owned()),
        "no activation after failed prepare: {ids:?}"
    );
    assert_no_rollback_commands(&stages);
    cleanup(&tree);
}

#[test]
fn cleanup_failures_never_fail_the_deploy() {
    // 05 artifact cleanup is best-effort post-success: a failing prune is
    // recorded in the stage-12 receipt, and the run still ends open.
    let tree = build_tree("cleanup-fail");
    let mut stages = FixtureStageExecutor::passing();
    stages.failures.insert(
        "cleanup.remote-prune-stale".to_owned(),
        "injected cleanup failure".to_owned(),
    );
    let outcome = run_command(
        CommandMode::Deploy,
        &tree.config_path,
        FIXED_NOW,
        &FixtureExecutor::passing(),
        &stages,
    )
    .expect("run");
    assert_eq!(outcome.exit_code, 0, "message: {}", outcome.message);
    assert_eq!(outcome.outcome, "success");
    let terminal = tree.terminal_json();
    assert_eq!(terminal["admission"], "open");
    let run_dir = tree.single_run_dir();
    let stage12: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("stage-12-terminal_success_receipt.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        stage12["status"], "pass",
        "cleanup failure must not fail stage 12: {stage12}"
    );
    let prune = stage12["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["id"] == "cleanup.remote-prune-stale")
        .unwrap();
    assert_eq!(prune["outcome"], "fail");
    assert!(
        stage12["notes"].as_array().unwrap().iter().any(|note| note
            .as_str()
            .unwrap()
            .contains("best-effort cleanup `cleanup.remote-prune-stale` failed")),
        "failure noted: {stage12}"
    );
    cleanup(&tree);
}

#[test]
fn dry_run_migration_include_all_retry_is_detected_from_dry_run_output() {
    // Supabase reports out-of-order migrations as a successful dry run that
    // mentions --include-all: the controller must re-plan with the flag and
    // apply with it (legacy MIGRATION_INCLUDE_ALL).
    let tree = build_tree("include-all");
    let mut stages = FixtureStageExecutor::passing();
    stages.outputs.insert(
        "migrations.db-push-dry-run".to_owned(),
        "Need to rerun with --include-all\n".to_owned(),
    );
    let outcome = run_command(
        CommandMode::Deploy,
        &tree.config_path,
        FIXED_NOW,
        &FixtureExecutor::passing(),
        &stages,
    )
    .expect("run");
    assert_eq!(outcome.exit_code, 0, "message: {}", outcome.message);
    let ids = stages.command_ids();
    let dry_run_position = ids
        .iter()
        .position(|id| id == "migrations.db-push-dry-run")
        .expect("dry-run ran");
    let include_position = ids
        .iter()
        .position(|id| id == "migrations.db-push-dry-run-include-all")
        .expect("include-all dry-run re-ran");
    let push_position = ids
        .iter()
        .position(|id| id == "migrations.db-push-include-all")
        .expect("include-all push");
    assert!(dry_run_position < include_position, "ids: {ids:?}");
    assert!(include_position < push_position, "ids: {ids:?}");
    cleanup(&tree);
}

#[test]
fn remote_target_without_two_site_origins_fails_closed_at_activation() {
    // The legacy activation payload hard-requires two public origins; the
    // controller refuses to ship to a target that does not declare them.
    let tree = build_tree("origins-missing");
    tree.rewrite_target(|text| {
        text.replace(
            ",\n  \"site_origins\": [\"https://one.example.com\", \"https://two.example.com\"]",
            "",
        )
    });
    let stages = FixtureStageExecutor::passing();
    let outcome = run_command(
        CommandMode::Deploy,
        &tree.config_path,
        FIXED_NOW,
        &FixtureExecutor::passing(),
        &stages,
    )
    .expect("run");
    assert_eq!(outcome.exit_code, 1, "message: {}", outcome.message);
    let terminal = tree.terminal_json();
    assert_eq!(terminal["failed_stage"], "remote_activation");
    assert_eq!(terminal["admission"], "closed");
    assert!(
        terminal["reason"]
            .as_str()
            .unwrap()
            .contains("site_origins"),
        "reason: {terminal}"
    );
    let ids = stages.command_ids();
    assert!(
        !ids.iter().any(|id| id.starts_with("ship.")),
        "nothing ships without origins: {ids:?}"
    );
    cleanup(&tree);
}

#[test]
fn fixtures_are_dummy_only_no_secrets() {
    assert!(!CONTRACT_FIXTURE.contains("sk-"));
    assert!(!TARGET_FIXTURE.contains("password"));
    assert!(TARGET_FIXTURE.contains("dummy-placeholder"));
}

#[test]
fn failure_receipts_never_embed_runtime_env_values() {
    // Run one deep failure and scan every receipt for the fixture env value
    // (the only "secret-shaped" string in the fixture tree).
    let tree = build_tree("no-secret-leak");
    let mut stages = FixtureStageExecutor::passing();
    stages
        .failures
        .insert("readiness.db-heartbeat".to_owned(), "injected".to_owned());
    let outcome = run_command(
        CommandMode::Deploy,
        &tree.config_path,
        FIXED_NOW,
        &FixtureExecutor::passing(),
        &stages,
    )
    .expect("run");
    assert_eq!(outcome.exit_code, 1);
    for entry in std::fs::read_dir(tree.single_run_dir()).unwrap() {
        let path = entry.unwrap().path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(
                !text.contains("postgresql://fixture-dummy"),
                "receipt {} leaked an env value: {text}",
                path.display()
            );
        }
    }
    cleanup(&tree);
}
