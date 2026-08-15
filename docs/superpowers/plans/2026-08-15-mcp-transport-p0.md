# MCP Transport P0 Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `tools/call` strictly at-most-once at the HTTP transport layer and enforce an Origin allowlist at the TLS gateway, per the 2026-08-15 MCP transport review (doc B, steps 1–2 of 14).

**Architecture:** `McpHttpClient::request` currently retries every JSON-RPC POST (including non-idempotent `tools/call`) up to 3 times, while `capability-runtime` labels failures `MayHaveDispatched` and routes them to recovery — a contradiction. Split the retry policy: control reads (`tools/list`) keep the bounded retry; capability dispatch (`tools/call`) becomes a single POST. The Node TLS gateway strips `Origin` today without validating it; give it a fail-closed exact-match allowlist.

**Tech Stack:** Rust (tokio, existing hand-rolled SSE client in `crates/tool-mcp`), Node.js (`packaging/local-mcp-gateways/mcp_tls_proxy.mjs`).

## Global Constraints

- `tools/call` must POST exactly once per `call_tool()` invocation; retryable transport errors propagate to the caller (which already maps them to `DeliveryCertainty::MayHaveDispatched` → run-engine recovery).
- `server`-lifecycle control reads (`tools/list`) may keep the existing bounded 3-attempt retry with the deterministic backoff spread.
- Gateway: requests whose `Origin` header is not an exact member of `config.allowedOrigins` get `403 {"ok":false,"error":"origin_forbidden"}`. `allowedOrigins` must be a non-empty array of exact origin strings (`scheme://host[:port]`); boot fails otherwise.
- The gateway must not interpret MCP JSON bodies as part of this change (synthetic readiness/initialize removal is deferred to the rmcp migration plan).
- No new dependencies.

## Verified current state (2026-08-15)

- `crates/tool-mcp/src/lib.rs:750-804` — `request()` retries all methods 3× with fresh JSON-RPC ids.
- `crates/capability-runtime/src/lib.rs:690-709` — caller documents single-dispatch premise; errors tagged `MayHaveDispatched`.
- `packaging/local-mcp-gateways/mcp_tls_proxy.mjs:44-48` — `Origin` dropped, no allowlist; no header logging exists (Mcp-Param-* values are never logged).
- Rust client sends `Origin` from `McpHttpConfig.origin` (capability-runtime `PoolKey`/config path), so a fail-closed allowlist is deployable.

---

### Task 1: `tools/call` AtMostOnce in tool-mcp

**Files:**
- Modify: `crates/tool-mcp/src/lib.rs` (retry loop ~750-804, `call_tool` ~722, `list_tools` ~718, tests)
- Test: same file (`#[cfg(test)]` module)

**Interfaces:**
- Produces: `pub(crate) enum RequestRetry { ControlRead, AtMostOnce }` (private is fine); `call_tool` uses `AtMostOnce`, `list_tools` uses `ControlRead`. Public API (`call_tool`, `list_tools` signatures) unchanged.

- [ ] **Step 1: Write failing tests**
  - Test A (`call_tool_posts_exactly_once_on_stream_loss`): mock server accepts one POST for `tools/call`, responds 200 but truncates the SSE stream (or returns id-mismatched response so `post_message` yields a retryable `MissingStreamResponse`/`IncompleteSse`-class error); assert the client returns `Err` AND the server recorded exactly 1 POST for `tools/call` (adapt the existing mock harness already used by retry tests in this file — locate with `grep -n "retry" crates/tool-mcp/src/lib.rs`).
  - Test B (`tools_list_still_retries`): mock server fails the first `tools/list` POST with a retryable transport error and succeeds on the second; assert `list_tools()` returns Ok and 2 POSTs were observed.
- [ ] **Step 2: Run `cargo test -p tool-mcp` — expect the new tests to fail** (A sees 3 POSTs).
- [ ] **Step 3: Implement** — thread a retry flag through `request()`: `async fn request_with_retry(&self, method, params, retry_control: bool)`; `request` keeps old behavior for control paths; `call_tool` calls the at-most-once variant (single POST, no backoff loop). Preserve the fresh-id and redaction semantics.
- [ ] **Step 4: `cargo test -p tool-mcp` — all pass.**
- [ ] **Step 5: Commit** `fix(tool-mcp): tools/call is at-most-once; only control reads retry`

### Task 2: TLS gateway Origin allowlist

**Files:**
- Modify: `packaging/local-mcp-gateways/mcp_tls_proxy.mjs`
- Modify: `packaging/local-mcp-gateways/README.md` (document `allowedOrigins`)
- Test: `node --test packaging/local-mcp-gateways/mcp_tls_proxy.test.mjs` (new; see step 1 — keep it dependency-free by exporting `originAllowed(origins, headerValue)` and testing that pure function, plus a boot-validation unit for config)

**Interfaces:**
- Config gains required field `allowedOrigins: string[]` (non-empty, each a bare `scheme://host[:port]` string). Missing/empty ⇒ process throws at boot.
- Produces: `export function originAllowed(allowed, headerValue)` → boolean; exact string equality; requests with no `Origin` header are rejected (MCP 2026 requires Origin validation on all HTTP connections).

- [ ] **Step 1: Write failing test file** with cases: exact match passes; wrong port/subdomain/scheme fails; missing header fails; empty config array rejected at boot validation.
- [ ] **Step 2: Run `node --test packaging/local-mcp-gateways/` — expect failures.**
- [ ] **Step 3: Implement** — validate config at boot; in `handleRequest`, before anything else: `const origin = req.headers.origin; if (!originAllowed(config.allowedOrigins, origin)) return sendJson(res, 403, { ok:false, error:"origin_forbidden" });`. Do not log header values.
- [ ] **Step 4: `node --test` passes; `node --check packaging/local-mcp-gateways/mcp_tls_proxy.mjs` clean.**
- [ ] **Step 5: Update stack configs** — find where `KRW_MCP_TLS_PROXY_CONFIG` files are generated (`scripts/start_local_agent_gateway_stack.sh` or `.local/agent-gateway/`); add `allowedOrigins` matching the origin the Rust client sends for each endpoint. If configs are generated by a script, update the generator.
- [ ] **Step 6: Commit** `fix(gateway): fail-closed Origin allowlist on the TLS MCP gateway`

## Deferred (separate plan, not this session)

Doc B steps 3–14: synthetic readiness/initialize removal, `DeliveryPolicy`/`McpToolDispatch`, module split, `rmcp =3.0.0` with `call_tool_once()`, cache disable, Python `mcp==2.0.0`, protocol pinning, fingerprint v5, per-endpoint cutover, `AttestedStatelessV1` removal. Blocked on: rmcp 3.0.0 pin decision, Python v2 SDK migration, and deployment-side config changes that need the local stack cycle.
