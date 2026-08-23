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
| 5 | `frontend_image_prepare` | `frontend-image.resolve-commit` (`git rev-parse HEAD`, pinned against the preflight receipt — drift fails closed here), `frontend-image.archive-source` (legacy `create_archives`: `git archive <commit> \| gzip -n` → `<output>/ship/front.tar.gz`), `frontend-image.descriptor-copy` (legacy `create_agent_descriptor_upload`: the public Rust descriptor copied to `<output>/ship/public-release.json`, mode 0644, gated by a controller-side sha256 check against the sealed descriptor hash — the ONLY Rust artifact that ever leaves the Mac). LOCAL SOURCE PREPARATION ONLY: no image is built, tagged, saved, or inspected locally — the VM builds the image from this immutable context in stage 9, exactly like the legacy script (no cross-architecture image shipping ever happens). `frontend_image_digest` in the terminal artifacts is therefore populated from the remote build's reported `CANDIDATE_IMAGE=<id>` (or `<unavailable>` if the remote did not report one) |
| 6 | `migrations` | `migrations.rpc-contract-verify` (`verify-supabase-schema-smoke-rpc-contract.mjs`), `migrations.db-push-dry-run` (read-only plan gate with the legacy IPv6→TLS-pooler retry baked into the wrapper; `migrations.db-push-dry-run-include-all` re-runs when Supabase's out-of-order advisory appears) — nothing is applied before admission close (05 migration ordering rule 3) |
| 7 | `admission_close` | `admission.read-previous`, `admission.close` (legacy backend-mode gate + `upsert_env_value` + `docker compose up -d --no-deps --force-recreate --wait web agent-v1-outbox` on the ACTIVE release), `admission.verify-closed`, then the legacy apply order: `migrations.agent-release-apply` (`apply-agent-v1-production-migrations.sh --agent-release <sealed bundle> --skip-front-migrations`), `migrations.db-push` (+`-include-all` variant; forward-only apply), `migrations.schema-smoke` (`supabase-schema-smoke.mjs` + `verify-agent-v1-compatibility.mjs` in one sourced-env shell), `migrations.sealed-database-abi` (the sealed bundle's own `krw-agentd-start-local --database-check` through a temp `current` symlink + 0600 overlay), and `migrations.abi-verify-procedure-N` / `migrations.abi-verify-column-N` (psql `information_schema` queries proving the contract's `required_procedures` / `required_columns`) |
| 8 | `local_activation` | `activation.agentd-stage`, [`activation.runtime-env-candidate` — only when the runtime env declares `KRW_AGENT_RUNTIME_ENV_CANDIDATE`; the legacy `activate-forward-runtime-env.sh` forward activation], `activation.agentd-activate` (`--defer-start`; `--env-file` follows `KRW_AGENT_RUNTIME_ENV_FILE` when declared), `activation.capabilityd-activate`, `activation.mcp-gateways-prepare` / `activation.mcp-gateways-activate` (`install-krw-agent-local-mcp-gateways.mjs --mode prepare\|activate`), `activation.sealed-mcp-abi` (`krw-agentd-start-local --mcp-check` through the ACTIVE gateway), `activation.agentd-start` — the legacy `WITH_AGENT_RELEASE` order verbatim, with `KRW_AGENT_LOCAL_INSTALL_ROOT` from the runtime env |
| 9 | `remote_activation` | `ship.scp-front-archive` + `ship.scp-agent-descriptor` (`gcloud compute scp ... --scp-flag=-oServerAliveInterval=15 --scp-flag=-oServerAliveCountMax=8`, three attempts; plain `scp -o BatchMode=yes` for ssh-only targets) — the ship manifest is EXACTLY the two legacy uploads: the immutable source archive and the public descriptor; NO image tarball is ever created or shipped. `ship.remote-prepare` ports `run_remote_prepare` verbatim: extract to `<remote_front_dir>/.simple-deploy/releases/<release>`, runtime.env assembly + market-issue token gates, descriptor install + sha256 gate, all Rust Agent V1 env pins, admission `closed`, then the legacy docker sequence ON THE VM — OLD_WEB/OLD_IMAGE capture (+ bootstrap rebuild branch), `compose_stage build web` with `NEXT_PUBLIC_APP_VERSION=<release>` (the Docker builder recreates generated workers inside the shipped immutable context), candidate tag from `:local`, the in-container candidate preflight node script (descriptor provider pin via the compose read-only mount, tenant partition, CA/TLS shape, `select 1` over the production TLS pooler), old image restored under the stable `local` tag, and `forward-activation.env`; the payload reports `CANDIDATE_IMAGE=<id>` which becomes the run's `frontend_image_digest`. Then `remote.candidate-abi` (legacy `run_remote_validate_agent_candidate_abi`: in-container queue/outbox ABI proof) and `remote.web-up` (the FULL legacy `run_remote_activate`: sidecar retirement, candidate→`local` promotion, the complete service set recreate, in-container + two public origins healthz at the new deployment id, conditional market workers, `current` symlink flip), `readiness.remote-web-healthz` — remote targets must declare at least two `site_origins` (legacy `siteOrigins`), fail-closed otherwise; Skipped for local-only targets |
| 10 | `deep_readiness` | `readiness.daemon-metrics` (local daemon `http://127.0.0.1:<metrics-port>/metrics`, `daemon_ready_ms`), `readiness.web-deep` (web `/api/healthz/deep` with internal-key headers, `public_ready_ms`), `readiness.public-healthz-N` (each `site_origins` entry must serve the new deployment id from the operator host), `readiness.db-heartbeat` (psql on `public.agent_v1_daemon_heartbeats` latest row verifying provider + descriptor hash + `mcp_ready`) — process + product layers, never an LLM call. MCP readiness is proven by the sealed-mcp-abi stage, the daemon boot preflight, and the 30s heartbeat receipt; per-endpoint TCP/readyz probes duplicated those contracts and added deploy-failure surface |
| 11 | `admission_open` | `admission.open` (the legacy `run_remote_open_research_admission` against the STAGE runtime env: prove deep health reports `closed` first, upsert `open`, recreate web/outbox, verify `open`, re-close in-environment on any failure), `readiness.admission-open-verify` (deep health must report `checks.agent_v1.admission == "open"`) |
| 12 | `terminal_success_receipt` | `cleanup.remote-prune-stale` + `cleanup.remote-prune-post-activation` (the legacy bounded prune payloads: inactive stage dirs with the active-release guard, exact `krw-ontology-front-web`/`caddy` repo names and `candidate-*\|release-*\|rollback-*` tag patterns, BuildKit cache capped at 12GB) as BEST-EFFORT commands before the receipt — a cleanup failure is recorded and never fails the deploy — then the immutable `terminal.json` (`outcome: success`, `admission: open`) |


### Deliberately not ported (gated OFF in the legacy normal `--with-agent-release` path)

The legacy deploy script gates several mechanisms behind flags the approved
production path never passes; the controller respects that gating and does
NOT issue any of these commands (an integration test asserts no `billing`
command id ever runs):

- `run_remote_pause_billing_for_cutover` / `run_remote_verify_billing_callback_redaction` /
  `run_remote_enable_billing_v2_writes` / `run_remote_disable_billing_v2_writes` and
  `verify-billing-production-readiness.sh` — only under `--billing-v2-cutover`
  (which additionally requires `--apply-migrations --require-live-toss` and
  forbids `--skip-web`); the normal path has `BILLING_V2_CUTOVER=0`.
- `run_data_publish` / `prepare_data_environment` — only under `--with-data`.
- `prepare_mac_runtime` / `materialize_runtime_ontology_plugin` (runtime
  ontology plugin materialization) — the legacy materializes the plugin only
  on the `WITH_AGENT_RELEASE=0 && SKIP_MAC=0` Node-worker path; the Rust
  agent release path never calls it.
- `run_remote_capture_research_admission` / `run_remote_restore_research_admission` —
  the frontend-only (`WITH_AGENT_RELEASE=0`) admission capture/restore pair;
  the Rust path closes once and opens after deep readiness instead.

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
