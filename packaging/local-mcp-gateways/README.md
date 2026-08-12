# Local MCP TLS gateway

`mcp_tls_proxy.mjs` is the operator-owned HTTPS boundary for pre-existing
loopback MCP services. Its JSON configuration supplies the upstream, pinned
deployment identity, and an explicit `toolSessionReuse` declaration.

`attested-stateless-v1` endpoints receive the exact Agent V1 tool-session
attestation in both `/readyz` and the local MCP `initialize` response. The
gateway handles that initialization locally and forwards subsequent tool calls
without a session id; this mode is valid only for upstream MCP services proven
to accept independent calls. `run-scoped` endpoints retain their upstream MCP
initialization and are never granted cross-run session reuse.
