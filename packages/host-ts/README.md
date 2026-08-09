# `@krw-agent/host`

Standalone TypeScript integration boundary for the KRW product host and the
machine-wide Rust daemon's `agent_v1` Postgres ABI. Nothing in this package is
wired into the web application.

The public path is intentionally narrow:

- `loadPinnedReleaseArtifact` loads one canonical, daemon-generated release
  descriptor and pins its artifact/release hashes for the process lifetime.
- `prepareEnqueueRun` combines authenticated ownership with a browser-safe
  intent and any required committed-answer DB claim, then produces the exact frozen
  `agent_v1.enqueue_run` envelope.
- `HostAgentClient` exposes prepared enqueue/cancel plus ownership-bound
  committed-outcome reads. `HostOutboxClient` is a separate privilege surface.
- `createProductProjectionHandlers` validates current outbox payloads, obtains
  an authoritative database fence, and requires hash-bound idempotency receipts
  from product projection consumers before ACK.

Pinned compatibility is strict: protocol v7, claim schema v7, public release
descriptor v3, session-memory carrier v3, and the sole physical model
`glm-5.2`. There is no carrier-v1 or model-alias fallback.

The package contains no provider loop, prompt builder, MCP client, workflow
policy, scheduler, pricing engine, credential, endpoint, or application route.
Those remain owned by the Rust release/runtime or the future product host.

See [INTEGRATION.md](./INTEGRATION.md) for the future application wiring and
required database transaction semantics.

## Verify

```sh
npm run typecheck
npm test
```
