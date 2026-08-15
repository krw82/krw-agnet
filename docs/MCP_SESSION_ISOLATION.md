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

There is no default and deployment binding schema v1 is rejected. The
checked-in schema-v3 local and production example bindings now activate
`attested-stateless-v1` for read-only ontology and guru capabilities (see the
matrix below). Every other capability, including all `krw_feed_*`, filings, and
insider-transaction tools, stays `run-scoped` because its MCP session may carry
state that must not cross a run boundary. `auth_scope` remains `tenant` for the
activated capabilities; only `tool_session_reuse` changes.

The matrix is also checked offline by
`scripts/test_dual_provider_release.py`; a provider release is not accepted if
one provider accidentally changes a Feed/Filings binding to stateless reuse.
The test does not contact an MCP server and therefore cannot replace the live
readiness + `initialize` attestation check in the local gateway installer.

## Model-free retrieval audit

For a running local stack, the bounded read path can be checked without a
provider call:

```bash
python3 scripts/run_direct_mcp_audit.py \
  --operator-root "$HOME/krw-agnet-prod" \
  --ticker AAPL
```

The audit calls readiness, `initialize`, `tools/list`, and one representative
read for Ontology, Feed, Filings, and Guru. It also follows one Ontology object
through `trace` and `chain`. It records latency, result counts, pagination,
tool-error state, ticker consistency, and chain payload presence. Empty Feed or
Filings data is reported as an observed empty result, not a false test failure.
The report explicitly says that no model was called; it cannot judge whether a
later synthesized investment insight is useful. `--strict-auth` can be used in
CI or release operations when missing Feed/Filings credentials must fail the
audit rather than be reported as skipped.

## Capability matrix (checked-in example bindings)

| Capability | `tool_session_reuse` | `auth_scope` | Why |
| --- | --- | --- | --- |
| `krw_ontology_query_context` | `attested-stateless-v1` | tenant | pure read, no session state |
| `krw_ontology_query` | `attested-stateless-v1` | tenant | pure read, no session state |
| `krw_ontology_trace` | `attested-stateless-v1` | tenant | pure read, no session state |
| `krw_ontology_chain` | `attested-stateless-v1` | tenant | pure read, no session state |
| `krw_guru_query_context` | `attested-stateless-v1` | tenant | pure read, no session state |
| `krw_guru_company_brief` | `attested-stateless-v1` | tenant | pure read, no session state |
| `krw_guru_review_company_evidence` | `attested-stateless-v1` | tenant | pure read, no session state |
| `list_feed_items`, `get_feed_items`, `get_feed_context` | `run-scoped` | tenant | feed service may carry session state |
| `search_catalog_filings`, `get_filing`, `get_filing_brief`, `list_filing_sections`, `read_filing_section`, `list_filing_documents`, `read_filing_document`, `get_form4_insider_transactions` | `run-scoped` | tenant | filings service may carry cursor/personalization state |

`skill.load` is not part of this matrix. It is a closed local builtin resolved
from the immutable AgentImage and has no endpoint, credential, MCP session, or
deployment binding.

## Attestation contract identifier

The contract id is defined as the Rust constant
`STATELESS_TOOL_SESSION_CONTRACT_ID` in `crates/tool-mcp/src/lib.rs`:

```rust
pub const STATELESS_TOOL_SESSION_CONTRACT_ID: &str =
    "krw-agent/mcp-tool-session-stateless/v1";
```

## Deployment dependency: krw-capabilityd

Activating `attested-stateless-v1` in a binding is a deployment contract, not a
purely agent-side switch. The agent runtime fail-closes connection initialization
before any tool call when the required evidence is absent, malformed, stale, or
mismatched, so production rollout requires that `krw-capabilityd` (the MCP
capability sidecar) emit the attestation in **both** of these places:

1. The HTTPS readiness document, under `tool_session_contract`.
2. The MCP `initialize` result, under
   `capabilities.experimental.krwAgentToolSession`.

Both objects must independently carry `contract_id` and `attestation_sha256`,
the `contract_id` must equal the constant above, and `attestation_sha256` must
equal the JCS digest derived from the deployment-pinned
`protocol_version`/`server_build`/`server_schema_bundle_hash`/`data_release_hash`.
Until a capabilityd build emits both attestations, leaving that capability's
binding at `run-scoped` is the only correct option. `krw-agentd --check` does
not verify attestation (no live server is contacted); attestation is enforced at
session initialization time only.

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
