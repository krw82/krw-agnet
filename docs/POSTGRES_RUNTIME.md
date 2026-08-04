# PostgreSQL daemon runtime

`krw-agent-persistence` exposes the production database boundary in two parts:

- `postgres::PostgresJsonExecutor` is a TLS-only, bounded connection pool. It prepares the
  complete fixed `agent_v1.*($1::jsonb)` inventory on every admitted connection and has no
  arbitrary SQL method.
- `daemon::RunSupervisor` owns claim admission, lease renewal, fencing, defer/fail release, and
  graceful drain. `krw-agentd` injects the direct DeepSeek executor, pooled MCP runtime, and
  encrypted artifact repository.
- Atomic final inserts presentation/billing outbox rows, but `krw-agentd` deliberately installs no
  outbox handler. The product host owns real sink delivery and ACK; a no-op adapter can never
  acknowledge an undelivered event.

The connection URL is accepted only through an environment-variable reference. For example,
`--database-url-env KRW_AGENT_DATABASE_URL` names the variable; the URL is never a CLI value or a
debug field. The pool forces certificate-verified TLS and can optionally add CA PEM material from
another environment-variable reference.

```bash
KRW_AGENT_DATABASE_URL='postgresql://…?sslmode=require' \
cargo run -p krw-agentd -- \
  --image-dir /path/to/company-image \
  --image-dir /path/to/feed-image \
  --deployment-binding deployments/production/deployment-binding.yaml \
  --model-registry deployments/production/model-registry.yaml \
  --budget-registry deployments/production/budget-registry.yaml \
  --endpoint-registry deployments/production/endpoint-registry.yaml \
  --database-check
```

`--database-check` connects and prepares all 20 procedures but does not claim a run. The v1 SQL
ABI does not expose a separate migration-version/readiness function, so successful preparation of
the entire inventory is the strongest side-effect-free version check currently possible.

## Required integration contracts

- Every run-engine ABI mutation must hold the claim's `LeaseCoordinator::mutation_guard` and
  update that guard from the returned run version. This serializes it with heartbeats.
- `agent_v1.checkpoint_run_state(jsonb)` records only a bounded encrypted-CAS reference, plaintext
  size, content hash, recovery-schema hash, provider checkpoint sequence, and action frontier. The
  artifact must be durably stored before the procedure is called; runtime-state bytes and secrets
  never enter PostgreSQL.
- Each action state transition advances a monotonic action-frontier sequence and extends a
  domain-separated SHA-256 chain with the accepted mutation hash. A runtime-state checkpoint must
  match the database's current provider and action frontiers under the current fence and run
  version. Exact mutation replay is idempotent;
  divergent mutation replay, stale fences, and mismatched frontiers fail closed.
- A claimed run is not sent to the run engine until the injected `RecoveryArtifactStore` loads the
  latest runtime-state checkpoint plus every episode/arguments/result artifact under the exact
  tenant/principal/run scope, respects per-artifact and aggregate byte caps, and the daemon
  independently verifies content hashes. Runtime-state artifacts additionally require an exact
  declared-size match. A checkpoint may trail a later durable DB frontier after a crash, but it can
  never describe a frontier ahead of the database receipt; ordered episode/action receipts provide
  the reconciliation delta.
- `ClaimedRunExecutor::Committed` means the atomic final transaction already committed;
  the supervisor never fabricates a successful terminal record.
- An `OutboxHandler` must apply `dedupe_key` idempotently at its sink. The dispatcher acknowledges
  only after delivery and otherwise releases the leased event with a redacted error hash.

## Live daemon

Database, DeepSeek, MCP, and artifact-key values are injected by a secret manager. The CLI accepts
only environment-variable names, never secret material. An artifact key is exactly 32 bytes encoded
as 64 hex characters. The active version writes new artifacts; repeated `--artifact-read-key
VERSION:ENV_NAME` arguments permit short-TTL reads during rotation.

```bash
cargo run -p krw-agentd -- \
  --image-dir /srv/krw-agent/images/company-research \
  --image-dir /srv/krw-agent/images/feed-research \
  --image-dir /srv/krw-agent/images/source-filing \
  --deployment-binding /etc/krw-agent/deployment-binding.yaml \
  --model-registry /etc/krw-agent/model-registry.yaml \
  --budget-registry /etc/krw-agent/budget-registry.yaml \
  --endpoint-registry /etc/krw-agent/endpoint-registry.yaml \
  --release-authorization /etc/krw-agent/release-authorization.json \
  --release-trust-registry /etc/krw-agent/release-trust-registry.json \
  --runtime-version 0.1.0 \
  --worker-id krw-agentd-node-a \
  --database-url-env KRW_AGENT_DATABASE_URL \
  --artifact-root /var/lib/krw-agent/artifacts \
  --artifact-active-key-version 3 \
  --artifact-active-key-env KRW_AGENT_ARTIFACT_KEY_V3 \
  --artifact-read-key 2:KRW_AGENT_ARTIFACT_KEY_V2
```

`--database-check` never admits a claim. It validates the configured provider credential's
presence but makes no DeepSeek or MCP request. It may omit release authorization artifacts for
authoring/DB diagnostics; when either artifact is supplied, both must verify the exact resolved
descriptor. Without `--check` or `--database-check`, claim admission starts only after a signed
release authorization, worker identity, encrypted artifact store, exact registries, capability
pool, durable run store, and memory bounds have been constructed successfully.

Repeated `--image-dir` arguments define one immutable startup release set of 1 through 64 verified
images. Exact duplicate content hashes are represented once; duplicate agent identities and
duplicate `(run_kind, locale)` ownership fail startup. The daemon logs a deterministic release-set
hash and accepts claims for every member hash. It has no default-image fallback, hot reload, or
rollback selector. A receipt is routed by `agent_image_hash` before payload validation, then every
claim pin is rederived against that exact release.

The default is at most 16 active runs. Additional sessions remain queued in PostgreSQL instead of
allocating a task, prompt, transcript, MCP client, or provider client. DeepSeek HTTP/2 clients, the
MCP single-flight pool, content-interned prompt blobs, immutable images, and precompiled capability
catalogs are shared machine-wide. Resident runtime memory is `O(images + active runs)`, not
`O(sessions)`.
`SIGTERM`/Ctrl-C stops new claims and performs a bounded drain.

## Principal-fair database queue

Queued and deferred runs are rows in `agent_store.runs`; the daemon never mirrors the database
queue into tasks, timers, or an in-memory scheduler. Fairness is unit-cost start-time fair queueing
over the immutable `(tenant_id, principal_id)` identity:

```text
S(run) = max(V, F(tenant, principal))
F(run) = S(run) + 1
```

`V` is the singleton database virtual-start clock and `F(tenant, principal)` is one monotonic tail
row per principal. Thus scheduler metadata is `O(principals)`, independent of queued run count.
Tag allocation locks the clock and principal row in one transaction. A bigint tag at its maximum
cannot wrap: enqueue/defer fails with typed `K1022` and the transaction leaves no run or tail
fragment behind.

For a normal claim, PostgreSQL selects the eligible, worker-compatible, unlocked row with minimum
`(queue_start_tag, queue_finish_tag, available_at, created_at, run_id)` and advances
`V = max(V, queue_start_tag)`. The last fields are deterministic tie-breakers; host-supplied
`priority` remains in the v1 ABI and claim receipt for compatibility but has no scheduling effect.
Partial indexes separately support expired-active recovery, fair claim order, and `available_at`
eligibility. The partial unique `(tenant_id, session_id)` active index plus the claim anti-join keeps
at most one active run in a tenant/session while allowing another session for the same principal to
proceed.

Expired active leases are queried before any queued/deferred row, ordered by
`(lease_deadline, created_at, run_id)`. Reclaiming one does not consume virtual service a second
time. `SKIP LOCKED` means both passes choose the deterministic minimum among rows this claim can
lock; a row already being resolved by another transaction is not double-claimed.

Enqueue first wins the immutable `run_id` insert and only that winner allocates a fair tag.
Concurrent duplicates and exact mutation replay therefore neither replace identity nor advance a
principal tail. `fail_or_defer` verifies mutation replay, fence, lease, and run version before
allocating a new tail tag; every transient deferral consequently rejoins behind that principal's
already-tagged work instead of reusing its old head position.

## Deterministic fault coverage

- `run-engine::crash_recovery_replays_committed_history_without_provider_or_tool_redispatch`
  verifies recovery without duplicate provider/tool dispatch.
- `persistence::reclaimed_run_does_not_execute_with_unverified_artifacts` plus the runtime-state
  preflight tests bind recovery scope, hash, schema, order, and exact size.
- `persistence::stale_lease_cancels_executor_without_writing_with_old_fence` verifies a lost fence
  cannot emit a stale failure mutation.
- `run-engine::cancellation_after_dispatch_never_observes_or_commits_result` verifies a cancel
  race cannot commit an already-dispatched result.
- `runtime-persistence::final_commit_ack_loss_is_resolved_only_by_the_exact_durable_hash` verifies
  ambiguous final success only from the identical durable hash; the paired test verifies terminal
  cancellation wins.
- `scheduler::database_oracle_interleaves_100_continuously_eligible_principals` and its noisy,
  late-arrival, blocked-session, deferral, recovery, replay, and overflow companions model the exact
  SQL tag invariant without constructing a production queue in Rust.
- `persistence::embedded_migration_contains_principal_fair_queue_contracts` pins the scheduler
  tables, overflow code, claim order, deferral retag, supporting indexes, and absence of priority
  ordering in the embedded migration.

These deterministic tests establish orchestration contracts, not live-provider quality or load
capacity. Credentialed acceptance and soak gates remain mandatory.
