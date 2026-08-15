# krw-agent-deploy — deployment controller (Wave 8 core)

Implementation of `doc/refetoring/05-deployment-and-front-contract.md`.
This revision delivers the honest fail-closed core: config parsing, the
ordered read-only preflight, immutable receipts, and the forward-only stage
machine validated up to the "read-only dry-run receipt" bar. No real
activation exists in this revision by design — a `deploy` can never
half-run.

## CLI surface

```text
krw-agent-deploy --config <absolute production config> preflight   # read-only checks + preflight receipt, exit 0/1
krw-agent-deploy --config <absolute production config> dry-run     # preflight + build/seal input resolution + dry-run receipt, exit 0/1
krw-agent-deploy --config <absolute production config> deploy      # preflight, then TERMINAL FAILURE at stage `build`, exit 1
```

Exit codes: `0` pass / dry-run-ok, `1` fail-closed outcome, `2` controller
error (config unreadable/unparseable, receipt write failure). When the
config itself cannot be parsed no receipt is written (the operator root is
not yet known); the error goes to stderr.

## Stage list (doc/refetoring/05) and revision status

| # | stage id | status in this revision |
|---|---|---|
| 1 | `preflight` | implemented (read-only) |
| 2 | `preflight_receipt` | implemented (immutable, write-once) |
| 3 | `build` | not implemented — deploy stops here, fail-closed |
| 4 | `seal` | not implemented |
| 5 | `frontend_image_prepare` | not implemented |
| 6 | `migrations` (forward-only) | not implemented |
| 7 | `admission_close` | not implemented |
| 8 | `local_activation` | not implemented |
| 9 | `remote_activation` | not implemented |
| 10 | `deep_readiness` | not implemented |
| 11 | `admission_open` | not implemented |
| 12 | `terminal_success_receipt` | not implemented |

Stages 3-12 are declared in `src/stages.rs::STAGE_TABLE` as
`NotImplementedThisRevision`. When a live `deploy` walk reaches one, the
controller writes a terminal failure receipt with
`admission: closed` and exits non-zero. Stages 6-9 (migrations, admission
close/open, local/remote activation, deep readiness) can therefore never
execute partially: either the whole controller stops before them (this
revision, always) or a future revision implements them fully. Failures are
recorded with admission closed; recovery is fix-forward only — the
controller never rolls back binaries or DB schema.

## Config validation rules (fail-closed)

Parsed from the 05-doc production config shape, `schema_version` must be
exactly 1:

- `provider` — REQUIRED, non-empty, no default or newest-candidate
  selection; must match the target file's `provider` exactly.
- `agent_source_root`, `frontend_source_root`, `operator_root`,
  `runtime_env`, `target_file`, `frontend_contract` — REQUIRED, all
  absolute (relative paths rejected with the field named).
- `timeouts.{ssh_ms,mcp_ms,daemon_ready_ms,public_ready_ms}` — REQUIRED,
  positive integers.
- Unknown top-level or `timeouts` fields are rejected (a stale config can
  never select unintended behavior).
- Secret values are never stored in the config or receipts; the runtime env
  file is read for KEY NAMES only.

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
- `ssh-remote-reachable` — BatchMode ssh probe (Skipped without `ssh_host`).
- `remote-env-required-keys` — required KEY NAMES present in the runtime env
  file; values are never recorded (Skipped when none declared).
- `supabase-migration-plan` — ordered migration plan recorded in the target
  file, read-only ordering proof only (Skipped when absent).
- `db-agent-v1-abi` — `psql -c 'select 1'` reachability through the
  `db_url_env` env-var handle plus the declared `db_abi` (Skipped without a
  handle).
- `mcp-endpoint-reachable` — TCP connect to each `mcp_endpoints`
  `host:port`; connectivity only, never an LLM call (Skipped when none).

Structured-stub note: the remote probes above are real read-only executors,
but their deeper 05 semantics (exact Origin/TLS/pin checks, ABI SQL
verification, remote disk inspection) land with stages 6-11 in the
controller-completion pass; the executor trait is the extension point.

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
- `terminal.json` — `outcome` (`dry-run-ok` | `failure`), `admission`
  (`closed` for deploy failures per the 05 failure policy; `not-touched`
  for dry-run, which never mutates admission), `reached_stage`,
  `failed_stage`, `reason`, `receipt_path` (the preflight receipt), and
  dry-run `validations`.

`dry-run` additionally validates that build/seal inputs resolve (agent
workspace + `Cargo.toml` present, frontend source root present,
`cargo metadata --no-deps` readable, output dir absent) without executing
any build.

## Path to completion (finish line for this controller)

1. Implement stage 3 `build` (exact source commit build into the run output
   directory, sealed against the receipt hashes) and stage 4 `seal` (one
   selected runtime release set, signed against the operator trust
   registry).
2. Implement stage 5 `frontend_image_prepare` from the release manifest.
3. Implement stages 6-9 (forward-only migrations with the sealed DB ABI
   checker, admission close, local capability/gateway/daemon activation,
   remote web + outbox activation), then 10-12 (bounded deep readiness,
   admission open, terminal success receipt). Keep the invariant: any
   failure writes a terminal failure receipt with admission closed and
   exits non-zero; never roll back.
4. Replace the frontend `npm run prod:deploy:full` body with the thin
   adapter that runs `krw-agent-deploy --config <absolute config> deploy`.
5. Wave 9 deletion: remove old deploy/pause/recover scripts, rollback
   branches, dual/newest candidate selection, old provider aliases, the
   `.deploy` phase graph, and the legacy scripts this controller supersedes
   (`build_sealed_dual_provider_release.sh` flow becomes controller stages).

## Testing

`cargo test -p krw-agent-deploy` — unit tests per module (config, target,
contract, executor, checks, receipts, stages) plus
`tests/integration.rs` driving `run_command` end to end against tempdir
trees assembled from `tests/fixtures/` (dummy local paths and placeholder
provider ids only; no credentials). No test touches a real operator root.
