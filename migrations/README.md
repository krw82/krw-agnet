# `agent_v1` database ABI

Apply migrations in lexical order. `0001_agent_v1.sql` creates private storage
in `agent_store` and an EXECUTE-only JSON procedure surface in `agent_v1`.
`0002_action_finalization.sql` adds the durable
observe → validate/policy → accepted/rejected action protocol. Apply `0002`
before deploying a daemon that prepares or calls `agent_v1.finalize_action`.
The migration drops the pre-finalization `commit_action` procedure and rejects
databases containing its receipt-less `committed` action state. There is no
rolling compatibility path: every accepted or rejected result must carry the
exact validation and deny-monotone policy receipt hashes.

`0003_bounded_child.sql` adds the single bounded-child receipt state machine.
`0004_session_memory.sql` adds the tenant + principal + session-owned,
append-only SessionMemoryDeltaV3 log.
`commit_final` advances the memory frontier with a parent/revision compare-and-
swap in the same transaction as the answer bundle, settlement, terminal run,
and outbox. `read_session_memory` is the only runtime read surface; it validates
the exact active run owner and fence and returns at most eight ordered deltas per
page. Delta bodies never enter any outbox payload.

`0005_session_memory_snapshot.sql` adds append-only canonical
SessionMemorySnapshotV3 projection checkpoints. Normal reads return the latest
snapshot plus at most 32 newer audit deltas; the owner/fence-scoped
`audit_rebuild` mode is reserved for healing a missing or stale checkpoint.
Checkpoint insertion is a run-version mutation and requires the exact current
revision/frontier. Apply `0001` through `0005` in lexical order.

`0006_read_final_output.sql` adds the host-facing `read_final_output`
projection of a terminal run's rendered answer for callers outside the daemon.

`0007_deferred_attempts_cap.sql` adds the `runs.deferred_attempts` counter and
replaces `fail_or_defer` so the defer branch increments it and transitions to
`failed` with `reason_code='deferred_attempts_exhausted'` once 16 defers are
recorded. The explicit `fail` branch is unchanged. The cap binds *consecutive
defers within a single claim lifetime*, not the cumulative lifetime total of a
long-running run: `0010` resets the counter to 0 whenever `claim_run` freshly
claims a non-active run (see `0010`).

`0008_row_retention.sql` adds `reap_retained_runs`, a bounded reaper that
deletes terminal runs older than a retention cutoff along with their run-owned
child rows (mutations, provider_episodes, actions, run_state_checkpoints,
answer_bundles, settlements, outbox, child_executions, and
session_memory_snapshot_mutations), in FK order, capped at `max_runs` per
invocation using `FOR UPDATE SKIP LOCKED`. Session-scoped memory projections
(`session_memory_frontiers`, `session_memory_snapshots`, and the append-only
`session_memory_deltas`) are intentionally preserved: they are keyed by session
rather than run, and deltas reject deletion by trigger.

`0016_session_memory_source_retention.sql` completes that separation for
databases that have the original run-owned foreign keys. It creates a compact
`session_memory_sources` hash tombstone, backfills it from existing
deltas/snapshots, and re-points the frontier, delta, and snapshot source links
to the tombstone. The heavy provider/action/checkpoint/run graph can then be
reaped without losing same-room follow-up context. Normal soft deletion never
removes this projection. The server-only `agent_v1.retire_session_memory`
procedure is an idempotent, owner-scoped hard-purge path for a trusted product
`product_hard_purge` lifecycle receipt and refuses to run while any queued,
deferred, or active room run exists.

`0009_migration_tracking.sql` adds the `agent_store.schema_migrations` table
that records which migration versions have been applied, replacing the legacy
sentinel-file mechanism. `scripts/apply_migrations.sh` consults this table to
decide which files to run; it takes a session-level advisory lock so two
concurrent runners cannot double-apply the same migration. Each newly applied
file is recorded with its `shasum -a 256` checksum.

### Checksum verification policy

Before applying any pending migration, `apply_migrations.sh` verifies the
checksum of every already-applied migration file (version <= `APPLIED_MAX`):

- For each `schema_migrations` row with `version <= APPLIED_MAX`, the runner
  computes `shasum -a 256` of the matching `migrations/NNNN_*.sql` file and
  compares it to the stored `checksum`.
- On mismatch, the script prints `checksum_mismatch: <file>` to stderr and
  exits non-zero, *before* any pending migration is applied. A drifted or
  locally edited migration file that has already been recorded is therefore
  a hard pre-flight failure.
- Rows written by the 0009 backfill carry the literal checksum `'backfill'`
  (versions 1-9 on databases that ran 0001-0008 before tracking existed).
  These are skipped, because `'backfill'` is not a real hash. Only migrations
  recorded by the runner itself (version >= the first migration applied after
  0009) carry a sha256 and are verified.

This makes the migration set tamper-evident: editing an applied migration in
place, or swapping in a different file with the same version number, is
detected on the next run.

`0010_claim_resets_deferred_attempts.sql` redefines `agent_v1.claim_run` so
that freshly claiming a queued/deferred run resets
`runs.deferred_attempts` to 0 (reclaims of an already-active run preserve the
counter). This makes the 16-cap from `0007` a per-claim-lifetime bound on
consecutive defers without progress, so a long-running run that legitimately
defers across many claims does not surface a false
`deferred_attempts_exhausted` failure. Apply `0007` through `0010` in lexical
order after `0006`.

`0011_read_final_projection.sql` adds the full-owner final Markdown projection
used by product surfaces. It returns only rendered Markdown, usage counters,
and public ledger hashes.

`0012_read_terminal_trace.sql` adds a separate full-owner terminal action
trace for operator quality review. It returns at most the run's bounded
capability IDs, their final action stages, and opaque result hashes. It never
returns model prompts, provider episodes, action arguments, raw capability
results, artifact references, or evidence bodies.

The migration deliberately does not create deployment roles. After applying
it as the schema owner, deployment automation should grant the daemon role
only:

```sql
GRANT USAGE ON SCHEMA agent_v1 TO krw_agent_daemon;
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA agent_v1 TO krw_agent_daemon;
```

Do not grant the daemon role `USAGE` on `agent_store`, table CRUD, sequence
access, schema creation, or ownership. A separate host role may receive only
the `enqueue_run`, `request_cancel`, `read_committed_outcome`,
`read_final_output`, `read_final_projection`, and `read_terminal_trace`
functions if those calls are made outside the daemon.

`request_cancel` is intentionally host-linearized: it authenticates tenant and
locks the run row, but does not require the current worker's fence or version.
This lets a queued run or a run whose worker has disappeared be cancelled. All
daemon-owned mutations still require a live lease, exact fence, and expected
run version.

Every mutating request contains a unique `mutation_id`; the Rust adapter adds
an RFC 8785-derived `mutation_hash`. PostgreSQL persists both the hash and the
complete JSON request. A retry with byte-semantically equal JSON returns the
stored response, while any divergent reuse raises SQLSTATE `K1004`.

Provider episodes store only a content hash and encrypted/CAS artifact
reference. `begin_action` requires that episode receipt to exist before a tool
can be dispatched. Raw result bytes are durably referenced by `observe_action`;
`finalize_action` then binds their exact hash to accepted/rejected disposition,
validation receipt, and deny-monotone policy receipt. `commit_final` stores the answer bundle, settlement,
terminal state, and all outbox rows in the same transaction. `request_cancel`
locks the same run row, so final-first and cancel-first are the only terminal
linearizations.

Claiming is serialized to one active run per `(tenant_id, session_id)`. A
reclaimed run receipt includes the latest provider episode plus every
non-terminal action receipt, so a new fence can recover without redispatching
an ambiguous call. Observed results are never downgraded to ambiguous.

`fail_or_defer` releases a lease either into a delayed queue or a terminal
failure transaction. `claim_outbox`/`ack_outbox` provide a bounded,
lease-protected dispatcher ABI; their payloads are visible only after the
answer/cancel/failure transaction commits.
