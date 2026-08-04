# Contract identity and schema domains

Every contract in `AgentSpec` pins an exact RFC 8785 schema artifact hash. The compiler rejects empty, zero, duplicate, or unresolved contract identities. Production startup additionally compares every `(contract id, content hash)` pair with the embedded closed registry in `krw-agent-contracts`.

Three different hashes must never be substituted for one another:

- `input_schema_hash`: the exact canonical schema for capability arguments.
- `output_schema_hash`: the ordered composite hash of the capability's declared remote and normalized output contracts.
- `server_schema_bundle_hash`: the deployment readiness fingerprint for the remote MCP server's complete schema bundle.

`data_release_hash` independently identifies the data snapshot. The action fingerprint binds canonical arguments, the three schema domains above, server build, data release, capability id, run id, and `AgentImage` hash. A change in any of them produces a different logical action key.

`tool_session_reuse` independently pins the physical MCP session boundary.
`run-scoped` forces the full run identity into the pool partition;
`attested-stateless-v1` is admitted only when readiness and MCP initialize
repeat the exact JCS SHA-256 contract bound to protocol/build/schema/data pins.
See [`MCP_SESSION_ISOLATION.md`](MCP_SESSION_ISOLATION.md).

DeepSeek tool definitions are built only by `build_tool_definitions`. The builder loads the input schema from the embedded registry, verifies its bytes against the image pin, and computes the provider `tool_schema_hash` over the resulting deterministic definitions. `RunInput` has no provider-message, tool-definition, or tool-hash fields: the engine derives all three from a verified `LoadedImage`, so the host has nothing it can replace and rehash.

AgentImage format v2 also pins each model-driven state to one explicit role and gives every terminal an explicit `succeeded`, `stopped`, or `failed` disposition. Runtime state uses one image-bound, serializable typed interpreter with exact event/guard resolution and hard visit/fuel bounds; there is no second parity cursor. An answer can be committed only after the compiled `compose -> verify -> commit -> succeeded` path. For `company_research`, compose emits bounded direct Markdown and verification binds the kernel-owned EvidenceLedger receipt; it does not claim that free prose has been reversibly converted into a typed claim graph. Failed output-boundary verification may enter only the compiled, budget-bounded repair transition.

The canonical registry contains three authority domains:

- five Python/Pydantic-authority ontology contracts exported from `krw-ontology`;
- 22 source-hash-pinned input/result contracts audited from the three public
  feed tools and eight public filing tools in `krw-ontology-front`;
- two kernel-authority contracts:

  - `normalized-capability-result/v1`, the bounded result envelope used after capability adaptation;
  - `final-markdown/v1`, the bounded direct Markdown primitive used by Korean company research; its
    EvidenceLedger receipt is committed atomically with the output;
  - `answer-ir/v1`, retained only for typed product contracts that explicitly require a structured answer
    representation.

The front contracts also expose typed request/result exchange guards. They bind
returned issue/filing identities to the exact request, bind SEC URLs to CIK and
accession, and bind document/section read inputs to the prior list artifact.
Schema lookup alone does not invoke these guards; capability adapters must call
them before evidence ingestion.

The remote server URL, credentials, build, schema bundle fingerprint, and data release remain deployment data and are never included in `AgentImage`.
