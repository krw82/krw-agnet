# KRW capability runtime

This crate is the production boundary between the generic run engine and the
existing KRW ontology MCP server.

Construction is deliberately split by lifetime:

1. At process startup, build one bounded `McpClientPool` and resolve deployment
   configuration once.
2. Compile one `CapabilityCatalog` from the immutable `AgentImage` and the
   `Arc<ResolvedRuntime>`. Compilation rejects unknown evidence mappings,
   mismatched tool names, non-read-only capabilities, schema pin drift, and
   missing deployment bindings.
3. For each active run, call `PooledMcpCapabilityRuntime::for_run` with the
   shared catalog, shared `PooledMcpTransport`, and the fixed tenant, principal,
   and run IDs. This object contains no prompt blobs, endpoint copies,
   credentials, or MCP clients.

The closed adapters currently supported are:

| AgentImage mapping | MCP tool | Projection |
| --- | --- | --- |
| `research_state_v2` | `krw_ontology_query_context` | canonical ResearchState v2, including its typed answerability verdict |
| `targeted_evidence_v1` | `krw_ontology_query` | supplemental evidence that can never authorize a strong claim |
| `trace_lineage_v1` | `krw_ontology_trace` | supplemental trace evidence that can never authorize a strong claim |

Every invocation rechecks the run ID, canonical argument hash, deterministic
action key, input schema hash, ordered output-contract hash, normalized-result
schema hash, and the exact resolved `CapabilityBinding` before dispatch.
`tools/call` output must contain exactly one JSON text item. If
`structuredContent` is also present, its RFC 8785 bytes must agree with the
text JSON. A tool-level `isError` result is never mapped as evidence; only the
pinned query-context correction contract is accepted as a typed control
result.

Pool sharing follows `CapabilityBinding.auth_scope`. Public, tenant, and
principal sharing is permitted for these read-only KRW adapters because their
MCP session is initialization-only and the server is semantically stateless.
Any future adapter that retains request or personal state must declare
`principal` or `run` scope; it must also be added as an explicit typed adapter.
There is no script, arbitrary validator, or unknown-mapping escape hatch.

The readiness probe pins the server build, complete MCP schema bundle, and data
release before initialization. Pool identity additionally pins endpoint,
readiness endpoint, origin, protocol, credential rotation, TLS profile,
authorization partition, connection limit, and timeout. Deployment URLs and
bearer values are redacted from `Debug`; bearer cloning is deferred until a
new pool entry actually wins single-flight initialization.

