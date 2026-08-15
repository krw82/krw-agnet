# Local MCP TLS gateway

`mcp_tls_proxy.mjs` is the operator-owned HTTPS boundary for pre-existing
loopback MCP services. Its JSON configuration supplies the upstream, pinned
deployment identity, an explicit `toolSessionReuse` declaration, and the
fail-closed `allowedOrigins` allowlist.

## Origin allowlist

MCP 2026 requires Origin validation on every HTTP connection. The gateway
rejects any request whose `Origin` header is not an exact member of
`allowedOrigins` with `403 {"ok":false,"error":"origin_forbidden"}` before any
upstream work; a missing header is also rejected. Entries are bare origin
strings (`scheme://host[:port]`) and the comparison is exact — subdomains,
scheme or port variants, and suffixed hosts never match. The header value is
never forwarded across the TLS→loopback transition and never logged.

```json
{
  "service": "krw-ontology",
  "toolSessionReuse": "run-scoped",
  "allowedOrigins": ["https://127.0.0.1"],
  "upstream": { "host": "127.0.0.1", "port": 8080 },
  "listen": { "host": "127.0.0.1", "port": 9443 },
  "tls": { "keyFile": "/path/key.pem", "certFile": "/path/cert.pem" }
}
```

`attested-stateless-v1` endpoints receive the exact Agent V1 tool-session
attestation in both `/readyz` and the local MCP `initialize` response. The
gateway handles that initialization locally and forwards subsequent tool calls
without a session id; this mode is valid only for upstream MCP services proven
to accept independent calls. `run-scoped` endpoints retain their upstream MCP
initialization and are never granted cross-run session reuse.

Run the gateway tests with
`node --test packaging/local-mcp-gateways/mcp_tls_proxy.test.mjs`.

The example Origin is illustrative only. Production activation overwrites the
allowlist from the sealed `deployments/endpoint-registry.yaml`; operators must
not invent a second Origin in the runtime JSON.
