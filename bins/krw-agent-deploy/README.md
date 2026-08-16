# krw-agent-deploy — deployment controller

Implementation of `doc/refetoring/05-deployment-and-front-contract.md`.
All twelve controller stages are implemented: config parsing, the ordered
read-only preflight, immutable receipts, and the real forward-only deploy
walk (build, seal, frontend image prepare, forward-only migrations,
admission close, local/remote activation, bounded deep readiness, admission
open, terminal success receipt). Any stage failure writes a terminal
failure receipt with the correct admission posture and exits 1; the
controller has no rollback paths anywhere — recovery is always fix-forward
through a fresh run.

## CLI surface

```text
krw-agent-deploy --config <absolute production config> preflight   # read-only checks + preflight receipt, exit 0/1
krw-agent-deploy --config <absolute production config> dry-run     # preflight + build/seal input resolution + dry-run receipt, exit 0/1
krw-agent-deploy --config <absolute production config> deploy      # the full 12-stage forward-only walk, exit 0/1
```

Exit codes: `0` pass / dry-run-ok / deploy success, `1` fail-closed
outcome, `2` controller error (config unreadable/unparseable, receipt
write failure). When the config itself cannot be parsed no receipt is
written (the operator root is not yet known); the error goes to stderr.

## Stage list (doc/refetoring/05) — all implemented

| # | stage id | executor command ids (structured records) |
|---|---|---|
| 1 | `preflight` | read-only checks (see below) |
| 2 | `preflight_receipt` | immutable `preflight.json` |
| 3 | `build` | `build.dual-provider-bundles` — `scripts/build_dual_provider_release.sh --output-root <run-output>` in the agent source root |
| 4 | `seal` | `seal.prepare-production-candidate`, `seal.sign-release-authorization`, `seal.seal-production-candidate`, `seal.finalize-dual-release` — ports the legacy `build_sealed_dual_provider_release.sh` sequence for the ONE config-selected provider; the active signing key id is resolved (pure controller logic) from `<operator>/config/<provider>/release-trust-registry.json` (exactly one non-revoked key valid now); the authorization is written under `<operator>/signing/releases/<run-name>/` |
| 5 | `frontend_image_prepare` | `frontend-image.build-web` (`docker compose build web` in the frontend source root), `frontend-image.inspect-digest` (`docker image inspect <project>-web:latest --format {{.Id}}`) |
| 6 | `migrations` | `migrations.db-push-dry-run` (`<frontend>/scripts/supabase-cli.sh db push --dry-run`) — the read-only plan gate; nothing is applied before admission close (05 migration ordering rule 3) |
| 7 | `admission_close` | `admission.read-previous`, `admission.close`, `admission.verify-closed` (remote-shell payloads over the selected transport — gcloud `compute ssh ... --command` for gcp targets, plain BatchMode ssh otherwise — porting the legacy `value_of` awk + `upsert_env_value` + `docker compose up -d --no-deps --force-recreate --wait web agent-v1-outbox` + in-container `/api/healthz`), then `migrations.db-push` (forward-only apply) and `migrations.abi-verify-procedure-N` / `migrations.abi-verify-column-N` (psql `information_schema` queries proving the contract's `required_procedures` / `required_columns` entries) |
| 8 | `local_activation` | `activation.agentd-stage`, `activation.capabilityd-activate`, `activation.agentd-activate` — the sealed bundle's `packaging/launchd/install-local-mac-agentd-release.sh` (`--mode stage`, then `--mode activate ... --env-file <runtime-env>`) and `install-local-mac-capabilityd-release.sh` (`--mode activate ... --operator-root`) with `KRW_AGENT_LOCAL_INSTALL_ROOT` from the runtime env |
| 9 | `remote_activation` | `remote.web-up` (remote shell `docker compose up -d --force-recreate --wait web agent-v1-outbox market-web-source-worker market-issue-enrichment-worker`), `readiness.remote-web-healthz` (in-container healthz asserting `deployment_id == release id`) — both over the selected transport (Skipped for local-only targets) |
| 10 | `deep_readiness` | `readiness.daemon-metrics` (local daemon `http://127.0.0.1:<metrics-port>/metrics`, `daemon_ready_ms`), `readiness.mcp-tcp-N` TCP probes + `readiness.mcp-ready-N` (`<endpoint>/readyz`, `mcp_ms`), `readiness.web-deep` (web `/api/healthz/deep` with internal-key headers, `public_ready_ms`), `readiness.db-heartbeat` (psql on `public.agent_v1_daemon_heartbeats` latest row verifying provider + descriptor hash + `mcp_ready`) — three layers, never an LLM call |
| 11 | `admission_open` | `admission.open` (upsert `open` + recreate web/outbox), `readiness.admission-open-verify` (deep health must report `checks.agent_v1.admission == "open"`) |
| 12 | `terminal_success_receipt` | immutable `terminal.json` (`outcome: success`, `admission: open`) |

### Executor model

Every world interaction in stages 3-12 goes through the `StageExecutor`
trait (`RealStageExecutor` / `FixtureStageExecutor`, mirroring the
preflight `PreflightExecutor` pair). Commands are structured records
`{stage, id, argv, cwd, env_keys_used, timeout_ms}`:

- **Remote transport selection**: when the target file declares a `gcp`
  block, ALL remote commands (admission read/upsert, compose up, healthz,
  deep readiness remote checks) go through
  `gcloud compute ssh <gcp.instance> --project <gcp.project> --zone <gcp.zone> --command <payload>`
  — the legacy production transport (`run_remote_buffered_script` in the
  old deploy script). Plain `ssh -o BatchMode=yes -o ConnectTimeout=<ssh_ms> <host> <payload>`
  remains only for targets that declare `ssh_host` WITHOUT a gcp block.
  Payloads are identical across transports; buffering/timeout semantics
  (record `timeout_ms`, captured stdout) are unchanged.
- `argv` keeps `<env:NAME>` placeholders unresolved — the real executor
  substitutes values from the operator runtime env file (`<operator>`
  `runtime/krw-agent-deploy.env`) into the child process only; receipts
  and tests assert the placeholder form, so secret values are never
  recorded, printed, or logged.
- `env_keys_used` lists env var NAMES (the special `<runtime-env:all>`
  entry marks commands whose whole runtime env file is exported into the
  child via `set -a; . file`, the legacy packaging behavior).
- `readiness.*` commands are retried by the real executor until their
  config-derived timeout; the fixture executor is single-shot so tests
  stay deterministic. No `--max-readiness-poll-ms` flag exists — only
  config timeouts bound the wait.
- The fixture executor records every spec (tests assert exact argv,
  including the chosen transport), simulates the sealed-bundle files a
  real build leaves behind, and learns the run's release id / provider /
  descriptor hash from the observed argv so canned healthz and heartbeat
  outputs validate.

### Drift guard

Before each mutating stage (6-11) the controller re-reads the agent HEAD
and re-hashes the config; any divergence from the preflight receipt is a
terminal failure at that stage (nothing after the receipt may mutate a
drifted tree).

### Failure policy (no rollback, anywhere)

Any stage failure writes the stage receipt (`status: fail`, including the
failing command record) and the terminal failure receipt, then exits 1:

- admission is `not-touched` for failures before `admission_close`
  (build, seal, frontend image, migration dry-run — nothing mutated),
- admission is `closed` from `admission_close` onward (the 05 rule: a
  pushed schema never serves an older runtime; the queue stays stopped),
- success is the only path that ends `open`.

There are no rollback, restore, revert, or down-migration commands in the
controller; the test suite asserts the recorded command log of every
failure path never contains rollback-shaped ids or argv. A re-run is
always a fresh run (new run dir; `output-dir-absent` is enforced).

### Local-only targets

The target file may omit BOTH the `gcp` block and `ssh_host` for local-dev
deployments: stage 9 records `Skipped-with-reason`, and the remote halves
of admission close/open record skips while operating on the LOCAL gateway
stack — the controller upserts `KRW_AGENT_ADMISSION_MODE` on the local
runtime env file and recreates the local compose `web agent-v1-outbox`
services through the executor (`--env-file <runtime-env>`). Deep
readiness' product layer then curls the local `/api/healthz/deep` with
the internal key from the child env. Any declared remote transport (gcp
block or `ssh_host`) without `remote_front_dir` fails closed at admission
close.

## Config validation rules (fail-closed)

Parsed from the 05-doc production config shape, `schema_version` must be
exactly 1:

- `provider` — REQUIRED, non-empty, no default or newest-candidate
  selection; must match the target file's `provider` exactly.
- `agent_source_root`, `frontend_source_root`, `operator_root`,
  `runtime_env`, `target_file`, `frontend_contract` — REQUIRED, all
  absolute (relative paths rejected with the field named).
- `timeouts.{ssh_ms,mcp_ms,daemon_ready_ms,public_ready_ms}` — REQUIRED,
  positive integers. These are the ONLY bounds; no readiness flags exist.
- Unknown top-level or `timeouts` fields are rejected (a stale config can
  never select unintended behavior).
- Secret values are never stored in the config or receipts; the runtime env
  file is read for KEY NAMES only (values flow into child processes
  exclusively).

## Preflight checks (stable ids, 05-doc order)

Filesystem checks run directly; world-observing checks go through the
`PreflightExecutor` trait (`RealCommandExecutor` shells out strictly
read-only: `git status/rev-parse`, `command -v`,
`cargo metadata --no-deps`, `gcloud ... describe --format=json`,
`ssh -o BatchMode=yes ... true`, `psql -c 'select 1'`, TCP connect). No
check mutates files, env, gateway, launchd, DB, or a remote host. The
`FixtureExecutor` drives deterministic tests.

Implemented checks:

- `config-schema` — strict config validation summary.
- `target-explicit-provider` — target file parses, has exactly one explicit
  `provider` matching the config, and no legacy ambiguity keys
  (`providers`, `provider_candidates`, `provider_alias`).
- `agent-source-clean-commit` / `frontend-source-clean-commit` — clean
  porcelain + recorded HEAD.
- `output-dir-absent` — the run's deterministic output directory
  (`<operator_root>/releases/<run-name>`) must not exist.
- `required-commands` — git, cargo, python3, node, npm, gcloud, ssh, docker.
- `frontend-contract-hash` — see below.
- `signing-trust-validity` — `<operator_root>/signing/release-private.pk8`
  readable and non-empty; when
  `<operator_root>/signing/release-trust-registry.json` is published it must
  parse as a canonical `ReleaseTrustRegistryV1` (via
  `krw-agent-release-authorization`); absent registry records a Skipped.
- `port-ownership` — `TcpListener::bind("127.0.0.1", port)` read-only probe
  for every `local_ports` entry in the target file (empty → Skipped).
- `gcp-target-describable` — read-only `gcloud compute instances describe`
  (Skipped when the target records no gcp section).
- `ssh-remote-reachable` — remote-shell reachability with the SAME
  transport-selection rule as the deploy stages: a gcp block means a
  read-only `gcloud compute ssh ... --command 'echo ok'` probe; an
  `ssh_host` without a gcp block means the existing
  `ssh -o BatchMode=yes` probe (Skipped when neither is declared).
- `remote-env-required-keys` — required KEY NAMES present in the runtime env
  file; values are never recorded (Skipped when none declared).
- `supabase-migration-plan` — ordered migration plan recorded in the target
  file, read-only ordering proof only (Skipped when absent).
- `db-agent-v1-abi` — `psql -c 'select 1'` reachability through the
  `db_url_env` env-var handle plus the declared `db_abi` (Skipped without a
  handle).
- `mcp-endpoint-reachable` — TCP connect to each `mcp_endpoints`
  `host:port`; connectivity only, never an LLM call (Skipped when none).

## Frontend deployment contract hash check

The frontend owns one canonical contract artifact
(`agent-v1-deployment-contract.json`); the agent side only verifies that
file. `src/contract.rs` ports `scripts/verify_frontend_deployment_contract.py`:
required fields and exact values (`contract_id`, `agent_abi` = `agent_v1_v7`,
projection contracts, admission policy, ...) are validated, and the hash is
the SHA-256 of the canonical JSON (sorted keys, compact separators) — not of
the raw bytes, so frontend formatting churn cannot change the identity. The
operator target file may pin `frontend_contract_sha256`
(`sha256:<hex>` or bare hex); a pin mismatch fails preflight. Without a pin
the hash is still computed and recorded in the receipt.

The contract may additionally pin DB ABI lists — `required_procedures`
(e.g. `agent_v1.enqueue_run(jsonb)`) and `required_columns` (e.g.
`agent_v1_daemon_heartbeats.mcp_ready`) — which stage 7 verifies with psql
immediately after the forward-only `db push` (05 migration ordering
rule 4).

## Receipts

Written under `<operator_root>/deploy-receipts/<utc-ts>-<run-id>/` where
`run-id` is the 8-hex prefix of the config file sha256 (deterministic per
config + clock). Receipts are write-once; overwriting is an error.

- `preflight.json` — `schema_version`, `receipt_kind`, `run_id`,
  `created_at_utc`, `command`, `executor_kind`, `input_hashes`
  (`config_sha256`, `agent_head`, `frontend_head`,
  `frontend_contract_sha256`, `target_file_sha256`), `target_identity`
  (provider + gcp project/zone/instance/instance-id when present), the
  ordered `checks` array (`id`/`status`/`detail`), and `verdict`.
- `stage-<n>-<id>.json` (n = 3..12) — per-stage receipt: `stage_index`,
  `stage`, `status` (`pass` | `skipped` | `fail`), optional `reason`, the
  executed `commands` (exact `{id, argv, cwd, env_keys_used, timeout_ms,
  outcome}` records with env placeholders unresolved), `probes` (TCP
  records), and `notes`.
- `terminal.json` — `outcome` (`success` | `dry-run-ok` | `failure`),
  `admission` (`open` on success; `closed` for deploy failures from
  `admission_close` onward; `not-touched` before it and for dry-run),
  `reached_stage`, `failed_stage`, `reason`, `receipt_path` (the preflight
  receipt), dry-run `validations`, `artifacts` (`release_dir`,
  `release_id`, `descriptor_sha256`, `frontend_image_digest`,
  `applied_migrations` parsed from the `db push` output,
  `previous_admission` captured before close), and `skipped` records
  (local-only remote halves). No secrets ever appear in any receipt.

`dry-run` additionally validates that build/seal inputs resolve (agent
workspace + `Cargo.toml` present, frontend source root present,
`cargo metadata --no-deps` readable, output dir absent) without executing
any build.

## Path to completion (remaining integration steps)

1. Replace the frontend `npm run prod:deploy:full` body with the thin
   adapter that runs `krw-agent-deploy --config <absolute config> deploy`.
2. Wave 9 deletion: remove old deploy/pause/recover scripts, rollback
   branches, dual/newest candidate selection, old provider aliases, the
   `.deploy` phase graph, and the legacy scripts this controller supersedes
   (`build_sealed_dual_provider_release.sh` flow is now controller stages
   3-4).

## Testing

`cargo test -p krw-agent-deploy` — unit tests per module (config, target,
contract, executor, checks, receipts, stages, pipeline: trust-key
resolution, env upsert semantics, migration/ABI/healthz parsing, payload
construction, exact argv for every command) plus `tests/integration.rs`
driving `run_command` end to end against tempdir trees assembled from
`tests/fixtures/`: the 12-stage happy path (ordered command records,
success receipt with artifacts), an injected failure at each stage 3-11
(correct failed stage, admission posture, no rollback commands), drift
between preflight and stage 6, migration dry-run abort, ABI-failure-after-
push, admission capture values, local-only skipping, and a no-secret-leak
scan of every receipt. No test touches a real operator root.
