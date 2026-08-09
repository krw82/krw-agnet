# Future web-host integration guide

## Status and scope

This is a no-application integration kit. It prepares the contracts and
adapters a future web host can import; it does not change web routes, workers,
package manifests, Supabase migrations, or deployment configuration.

The trust boundary is:

```text
browser JSON
  -> parse EnqueueRunIntentV1 (question + bounded selection only)
authenticated server context
  -> AuthenticatedRunOwnershipV1 (tenant/principal/session/run)
trusted product DB
  -> committed-answer claim when existing_answer is selected
pinned daemon release artifact
  -> exact image/model/profile/budget/capability contract
prepareEnqueueRun
  -> frozen, hash-bound agent_v1.enqueue_run request
```

Never deserialize browser JSON as `RunContextV1`, `CommittedAnswerSourceV1`,
`SessionMemoryCarrierV3`, `PreparedEnqueueRunV1`, or
`AuthenticatedRunOwnershipV1`. Those are server/materialized contracts.

## 1. Deployment boot

Generate the public release descriptor from the same resolved release set the
daemon loads. Publish it as canonical JCS with deployment-managed SHA-256 pins,
then load it once during host startup:

```ts
const release = await loadPinnedReleaseArtifact({
  path: process.env.KRW_AGENT_RELEASE_DESCRIPTOR_PATH!,
  expectedArtifactHash: checkedArtifactHash,
  expectedReleaseSetHash: checkedReleaseSetHash,
});
```

The loader rejects symlinks, non-regular or group/world-writable files,
non-canonical JSON, secrets/endpoints, artifact drift, duplicate routes,
protocol/profile mismatches, and anything except the exact physical model
`glm-5.2`. Rotate the descriptor by replacing the deployment artifact
and restarting the host; never reread it per request.

Protocol compatibility is fail-closed:

| Contract | Required value |
| --- | --- |
| Postgres ABI | `agent_v1`, ABI `1` |
| Agent protocol | `7` |
| Immutable claim | `7` |
| Release descriptor | `3` |
| Session-memory carrier | `3` only |
| Provider model | `glm-5.2` only |
| Provider API | `anthropic-messages-v1` |
| Profiles | `glm_high`, `glm_max`, `glm_direct` |

## 2. Ownership and browser input

Create `AuthenticatedRunOwnershipV1` only from authenticated server state and
product DB rows. Generate `run_id` and `mutation_id` server-side. Do not copy
tenant, principal, session, or run IDs from the request body.

Parse the browser body into `EnqueueRunIntentV1`. Its `existing_answer` variant
contains only `source_run_id`; it has no field capable of carrying canonical
source JSON. The intent also has no session-memory field. Session memory is
resolved only by the Rust worker after it owns a live run fence.

The same ownership rule applies to `prepareCancelRun` and
`HostAgentClient.readCommittedOutcome`. The cancel `release` value is a
server-created reservation/credit release receipt, not browser JSON.

## 3. Authoritative materializers

Implement the committed-answer materializer port with a server-only product DB role.

### Committed answer

`materializeCommittedAnswerSource(request)` must perform one authoritative
read that:

1. Finds `source_run_id` in a final product row.
2. Verifies the exact tenant, principal, and session in the request own it.
3. Reads canonical display source and final/hash receipts from that row.
4. Builds and validates `CommittedAnswerSourceV1` from those DB values.
5. Returns `CommittedAnswerSourceClaimV1` with
   `request_hash = canonicalHash(request)`.

The method must never accept canonical source, final receipt, bundle hash, IR
hash, or unit hashes as parameters from the caller. A missing, non-final,
cross-owner, malformed, or hash-inconsistent row is a hard failure.

`committedSourceFromCanonicalJcs` is provided for the repository implementation.
It requires canonical JCS and cross-checks the DB's expected final/bundle/IR
hashes before producing the protocol carrier.

### Session memory (worker-only resolution)

Every host-produced immutable enqueue claim sets `RunRequest.session_memory`
to `null`. After the Rust worker acquires the owned run claim and fencing token,
it calls daemon-only `agent_v1.read_session_memory`, validates the paged delta
chain and CAS frontier, reconstructs the catalog, selects a question-conditioned
view, and injects the carrier only into its effective in-memory request. The
host and browser never materialize or submit it.

The resulting carrier is v3-only and binds:

- `source_revision` (unsigned DB revision),
- `source_frontier_hash` (the catalog CAS frontier),
- `view_hash` (SHA-256 of the exact canonical view), and
- a canonical view whose session hash and semantic view hash validate.

An empty revision resolves to no carrier. There is no carrier-v1 fallback.
`SessionMemoryViewClaimV3` and `validateSessionMemoryViewClaim` exist only for
audit/readback/differential tests of worker-produced resolution; they are not
enqueue inputs. The cross-language frontier transition is:

```text
SHA256("krw.session-memory/frontier-v2" NUL
       parent_frontier_hash NUL decimal_revision NUL delta_hash)
```

The pinned vector for parent `sha256:` + 64 `1` characters, revision `7`, and
delta `sha256:` + 64 `2` characters is
`sha256:f2655508c5e2c43af0292f6e230fa57188befd420b2dc24c204d0b3a36744b42`.

## 4. Prepare once, retry byte-for-byte

Call `prepareEnqueueRun` with the pinned artifact, authenticated ownership,
safe intent, and committed-answer DB materializer. Persist the returned
`PreparedEnqueueRunV1` (or its exact canonical JSON plus audit hashes) before
the first network/database attempt.

Retry `HostAgentClient.enqueue(prepared)` with that exact prepared value.
Never rematerialize an existing answer under the same `run_id` and
`mutation_id`: changed source state changes the immutable snapshot and must be
rejected as a divergent mutation. To use newer source state, create a new run
and mutation.

The prepared request binds:

- all four ownership IDs at the outer ABI and immutable-snapshot layers;
- descriptor artifact and release-set hashes;
- the committed-answer DB materialization claim hash when applicable;
- exact image/runtime/model/profile/budget/capability releases;
- RFC 8785 immutable snapshot and mutation hashes.

## 5. Database roles

Keep host and outbox roles separate. The request role needs only execute access
to `agent_v1.enqueue_run`, `agent_v1.request_cancel`,
`agent_v1.read_committed_outcome`, and `agent_v1.read_final_output`. The outbox worker role needs only
`agent_v1.claim_outbox` and `agent_v1.ack_outbox`. Neither role needs access to
`agent_store`, arbitrary SQL, daemon mutation procedures, provider credentials,
or application tables outside its product repository.

`agent_v1.read_session_memory` remains daemon-only because it requires the live
run fence. Do not grant it to the web request or product outbox roles.

Use `HostPostgresTransport` and `OutboxPostgresTransport` with separate pooled
clients. Both use fixed prepared statements and redact database errors to
SQLSTATE plus a diagnostic hash.

## 6. Product outbox projections

Create the standard authority resolver from a product ownership reader and
the host client. The ownership reader must load server-owned columns by
`run_id`; it must not echo IDs from an HTTP request.

```ts
const authority = createProjectionAuthorityResolver(ownershipReader, hostClient);
const handlers = createProductProjectionHandlers(authority, consumers);

await dispatchOutboxOnce(outboxClient, handlers, {
  workerId: stableWorkerId,
  claimLimit: 50,
  leaseMs: 30_000,
  concurrency: 4,
});
```

The resolver reads the committed agent outcome and supplies the current
`fencing_token`, `run_version`, and `cancel_generation`. The handlers validate
the event against that authority before calling a consumer.

Each consumer transaction must:

1. Lock the product run/projection row.
2. Enforce a unique `idempotency_key` (`dedupe_key`).
3. If the key exists, require the same `event_payload_hash`; return
   `already_applied`. A different hash for the same key is corruption and must
   throw.
4. Reject or safely record `stale_ignored` when the stored
   `(fencing_token, run_version)` is newer. Never overwrite newer state.
5. Apply the projection and store `ProjectionApplyReceiptV1` atomically.
6. For `answer.committed`, write the safe SSE row in that same transaction.

Only a receipt matching outbox ID, dedupe key, payload hash, run ID, fence, and
version allows the handler to return and the dispatcher to ACK. A handler or
ACK failure is safe to retry.

### Exact answer events

- `answer.committed` contains exactly five body-free metadata fields:
  `run_id`, `answer_bundle_hash`, `rendered_message_hash`, nullable
  `memory_revision`, and nullable `memory_frontier_hash`. The memory pair must
  be all-null or all-non-null. No memory delta or canonical source is allowed.
- `answer.presentation` contains schema version plus run/bundle/IR/render hashes.
  It contains no canonical source body. Load display data only from the
  authoritative final product row; release the event if that row is not ready.
- `answer.billing` contains exact bounded `BudgetUsage` only. It is not a price
  or settlement receipt. Never recompute or charge money from this notification;
  obtain monetary settlement from the authoritative billing store.

The generated SSE event is a hash-only `done` notification. Publish it only
after the durable final product row is committed. Clients then read the final
row through the normal authenticated product API.

The daemon's optional `session_memory_delta`, `session_memory_delta_hash`, and
`next_memory_frontier_hash` belong to the internal `commit_final` database
transaction. The delta body is intentionally absent from every outbox payload;
only the resulting revision/frontier metadata crosses the host boundary.

## 7. Operational behavior

- Schedule `dispatchOutboxOnce` from the product worker. The helper creates no
  polling timer and no task per session.
- Treat unsupported event kinds and validation failures as non-deliveries;
  inspect only redacted diagnostic hashes in generic logs.
- Alert on repeated delivery attempts, stale-fence conflicts, divergent dedupe
  payloads, ownership mismatches, release-pin failures, and materializer claim
  failures.
- Never log questions, canonical source, memory views, DB errors, provider
  payloads, credentials, or connection strings.

## 8. Adoption gate

Before application wiring, require all of the following:

- `npm run typecheck` and `npm test` pass in this package.
- Rust/TypeScript protocol, mutation-hash, and frontier-hash vectors match.
- Deployment descriptor hashes are supplied out-of-band and rotate by restart.
- Materializer queries prove tenant/principal/session ownership in tests.
- Consumer tests cover duplicate delivery, divergent dedupe payload, stale
  fence/version, ACK loss, and transaction rollback.
- Host and outbox database roles have the least-privilege grants above.
- No web route accepts a prepared request, canonical source, memory carrier,
  release descriptor, ownership object, fence, or outbox receipt from a client.
