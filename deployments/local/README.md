# Local deployment examples

`deployment-binding.example.yaml` is the machine-wide physical capability
catalog. It maps ontology and Guru tools to the same canonical
`krw-capabilityd` server, then lists every public-data read-only feed and filing
tool. It is passed once to the release-set resolver. Each AgentImage gets
only its exact sorted referenced subset; unrelated bindings do not change that
image's pinned deployment hash.

Every binding is schema v3 and explicitly sets `mcp_tool_name` and
`tool_session_reuse`. `binding_key` selects the physical endpoint/pool/credential
binding; `mcp_tool_name` is the actual method invoked on that endpoint. The local
examples intentionally use `run-scoped`: an initialized MCP session is never
shared across runs until a deployment opts into `attested-stateless-v1` and its
server supplies both required versioned attestations. See
`docs/MCP_SESSION_ISOLATION.md` for the exact readiness/initialize contract.

`deployment-binding.krw-ontology.example.yaml` remains a narrow fixture for the
single-image compatibility wrapper used by crate tests. It is not the
production daemon path. Production replaces all zero release hashes and
`fixture` builds in the global catalog before startup admission.

`budget-registry.yaml` is likewise a superset covering all eight checked-in
AgentSpecs. Every release pins only the sorted profiles referenced by its own
entrypoints, so adding an unrelated profile cannot perturb queued claims.

Endpoint URLs and bearer values are never stored here. `endpoint_ref`,
`credential_ref`, `url_env`, and `readiness_url_env` remain symbolic until the
runtime resolves them once at startup.

The ontology and Guru bindings all use `KRW_ONTOLOGY_MCP_URL` and
`KRW_ONTOLOGY_READY_URL`, because one `krw-capabilityd` owns the complete
27-tool online read registry. Live deployments replace each zero fixture pin
with the single service's emitted build, schema-bundle, and release-manifest
identity; they do not create a second Guru MCP process or a second pool.

After the immutable ontology release has been admitted, an operator can obtain
those non-secret values without starting the listener:

```bash
cd ~/krw-agnet/services/krw-ontology-runtime
uv run krw-capabilityd --print-identity
```

Copy only the three emitted identity values into the deployment binding and the
three `KRW_CAPABILITYD_EXPECTED_*` environment variables. In production all
three expected values are required; a mismatched sidecar exits before opening
its listener.

`krw-agentd` accepts repeated `--image-dir` arguments (1 through 64) as one
immutable release set. It has no single-image fallback, hot reload, or rollback
selector; changing the set requires a bounded daemon restart.
