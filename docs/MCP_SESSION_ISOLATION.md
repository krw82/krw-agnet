# MCP tool-session isolation

An initialized MCP HTTP session is stateful unless both sides prove otherwise.
KRW therefore treats authorization scope and tool-session reuse as two separate
deployment contracts.

Every `CapabilityBinding` must declare exactly one `tool_session_reuse` value:

- `run-scoped`: the pool key always includes tenant, principal, and run IDs,
  regardless of the broader data `auth_scope`. No initialized MCP session can
  cross a run boundary.
- `attested-stateless-v1`: the normal `auth_scope` partition may be reused, but
  only after both readiness and MCP `initialize` attest the exact stateless
  contract described below. Missing, malformed, stale, or mismatched evidence
  fails connection initialization before any tool call.

There is no default and deployment binding schema v1 is rejected. The checked-in
schema-v2 local examples explicitly use `run-scoped`, so they are safe before
any existing MCP server implements the opt-in attestation.

## Versioned cryptographic attestation

The client computes SHA-256 over RFC 8785/JCS bytes for this exact envelope:

```json
{
  "contract_id": "krw-agent/mcp-tool-session-stateless/v1",
  "data_release_hash": "sha256:<pinned release hash>",
  "protocol_version": "<pinned MCP protocol version>",
  "server_build": "<pinned build>",
  "server_schema_bundle_hash": "sha256:<pinned schema bundle hash>"
}
```

The HTTPS readiness document must include:

```json
{
  "tool_session_contract": {
    "contract_id": "krw-agent/mcp-tool-session-stateless/v1",
    "attestation_sha256": "sha256:<JCS envelope digest>"
  }
}
```

The MCP initialize result must independently repeat the same object at:

```text
capabilities.experimental.krwAgentToolSession
```

The nested object rejects unknown fields. The digest is bound to the exact MCP
protocol, server build, schema bundle, and data release already pinned by the
deployment. HTTPS authenticates the configured same-origin server; the digest
prevents a different contract version or stale pinned release from silently
enabling sharing.

## Pool partition rules

| Declared reuse | Data auth scope | Effective initialized-session partition |
| --- | --- | --- |
| `run-scoped` | any | tenant + principal + run |
| `attested-stateless-v1` | public | machine-wide after attestation |
| `attested-stateless-v1` | tenant | tenant after attestation |
| `attested-stateless-v1` | principal | tenant + principal after attestation |
| `attested-stateless-v1` | run | tenant + principal + run after attestation |

Endpoint URL, readiness URL, Origin, transport, protocol, server build, schema
bundle, data release, auth scope, reuse policy, credential version, TLS profile,
connection bound, and timeout remain part of pool/release identity. Changing any
one creates a different entry and a different resolved capability fingerprint.

`attested-stateless-v1` means tool results and behavior cannot depend on prior
calls in that MCP session. If a server keeps cursor, personalization, temporary
research, transaction, or workflow state in the session, its binding must stay
`run-scoped`.
