#!/usr/bin/env node

// Operator-owned HTTPS boundary for existing local MCP workers.  The upstream
// services are not changed: this process terminates TLS, preserves bearer
// headers, and supplies the Agent V1 stateless-session contract only for
// endpoints explicitly configured as attested-stateless-v1.

import { createHash } from "node:crypto";
import fs from "node:fs";
import http from "node:http";
import https from "node:https";
import { pathToFileURL } from "node:url";

const STATELESS_CONTRACT_ID = "krw-agent/mcp-tool-session-stateless/v1";
const MAX_CONTROL_REQUEST_BYTES = 512 * 1024;
const ORIGIN_PATTERN = /^https?:\/\/[A-Za-z0-9.-]+(?::\d{1,5})?$/;

const configPath = process.env.KRW_MCP_TLS_PROXY_CONFIG;

/**
 * Fail-closed Origin check. MCP 2026 requires every HTTP connection to carry
 * and validate an Origin; this gateway is the public protocol boundary, so an
 * exact string match against the configured allowlist is the only pass
 * condition. No header, or any near miss (subdomain, scheme, port, path),
 * never reaches the upstream.
 */
export function originAllowed(allowedOrigins, headerValue) {
  if (!Array.isArray(allowedOrigins)) return false;
  if (typeof headerValue !== "string" || headerValue.length === 0) return false;
  return allowedOrigins.includes(headerValue);
}

export function validateGatewayConfig(config) {
  if (!["run-scoped", "attested-stateless-v1"].includes(config.toolSessionReuse)) {
    throw new Error("toolSessionReuse must be run-scoped or attested-stateless-v1");
  }
  if (
    !Array.isArray(config.allowedOrigins) ||
    config.allowedOrigins.length === 0 ||
    config.allowedOrigins.some((origin) => typeof origin !== "string" || !ORIGIN_PATTERN.test(origin))
  ) {
    throw new Error(
      "allowedOrigins must be a non-empty array of bare origin strings (scheme://host[:port])",
    );
  }
}

let config = null;

const hopByHop = new Set([
  "connection",
  "keep-alive",
  "proxy-authenticate",
  "proxy-authorization",
  "te",
  "trailer",
  "transfer-encoding",
  "upgrade",
]);

function forwardedHeaders(headers) {
  const output = {};
  for (const [key, value] of Object.entries(headers)) {
    // The client origin describes the TLS-facing gateway, not the loopback
    // HTTP upstream. Forwarding it unchanged makes strict MCP servers compare
    // an HTTPS origin against their own HTTP listener and reject valid calls.
    // The gateway remains the only public protocol boundary: the original
    // Origin is accepted on the incoming HTTPS request but is deliberately
    // not propagated across this scheme/authority transition.
    if (
      !hopByHop.has(key.toLowerCase())
      && key.toLowerCase() !== "host"
      && key.toLowerCase() !== "origin"
    ) output[key] = value;
  }
  output.host = `${config.upstream.host}:${config.upstream.port}`;
  return output;
}

function upstreamPath(req) {
  const configured = config.upstreamMcpPath;
  if (!configured) return req.url;
  const [pathname, query] = req.url.split("?", 2);
  if (pathname !== "/mcp" && pathname !== "/mcp/") return req.url;
  return query ? `${configured}?${query}` : configured;
}

function statelessAttestation() {
  if (config.toolSessionReuse !== "attested-stateless-v1") return null;
  // The keys are already RFC 8785/JCS lexical order and all values are
  // validated deployment identity strings.  JSON.stringify therefore emits
  // the exact canonical envelope hashed by crates/tool-mcp.
  const envelope = {
    contract_id: STATELESS_CONTRACT_ID,
    data_release_hash: config.releaseManifestSha256,
    protocol_version: config.protocolVersion,
    server_build: config.buildId,
    server_schema_bundle_hash: config.toolSchemaSha256,
  };
  return {
    contract_id: STATELESS_CONTRACT_ID,
    attestation_sha256: `sha256:${createHash("sha256").update(JSON.stringify(envelope)).digest("hex")}`,
  };
}

function readinessDocument() {
  const document = {
    schema_version: "krw-capabilityd/readiness/v1",
    ok: true,
    fingerprint_match: true,
    service: config.service,
    transport: "streamable-http",
    protocol_version: config.protocolVersion,
    build_id: config.buildId,
    tool_schema_sha256: config.toolSchemaSha256,
    release_manifest_sha256: config.releaseManifestSha256,
    tool_count: config.toolCount,
  };
  const attestation = statelessAttestation();
  if (attestation) document.tool_session_contract = attestation;
  return document;
}

function sendJson(res, status, value) {
  const body = JSON.stringify(value);
  res.writeHead(status, {
    "content-type": "application/json",
    "content-length": Buffer.byteLength(body),
  });
  res.end(body);
}

function sendSseMessage(res, value) {
  const body = `event: message\ndata: ${JSON.stringify(value)}\n\n`;
  res.writeHead(200, {
    "cache-control": "no-cache, no-transform",
    "content-type": "text/event-stream",
    "content-length": Buffer.byteLength(body),
    "x-accel-buffering": "no",
  });
  res.end(body);
}

function upstreamRequest(req, res, body = null) {
  const request = http.request(
    {
      host: config.upstream.host,
      port: config.upstream.port,
      method: req.method,
      path: upstreamPath(req),
      headers: forwardedHeaders(req.headers),
      timeout: config.upstream.timeoutMs ?? 65000,
    },
    (upstream) => {
      const headers = {};
      for (const [key, value] of Object.entries(upstream.headers)) {
        if (!hopByHop.has(key.toLowerCase())) headers[key] = value;
      }
      res.writeHead(upstream.statusCode ?? 502, headers);
      upstream.pipe(res);
    },
  );
  request.on("timeout", () => request.destroy(new Error("upstream_timeout")));
  request.on("error", () => {
    if (!res.headersSent) sendJson(res, 502, { ok: false, error: "upstream_unavailable" });
    else if (!res.writableEnded) res.end();
  });
  if (body !== null) request.end(body);
  else req.pipe(request);
}

function syntheticReady(req, res) {
  if (config.checkUpstreamReady === false) {
    sendJson(res, 200, readinessDocument());
    return;
  }
  const probe = http.request(
    {
      host: config.upstream.host,
      port: config.upstream.port,
      method: "GET",
      path: config.upstreamReadinessPath ?? "/readyz",
      headers: forwardedHeaders(req.headers),
      timeout: config.upstream.timeoutMs ?? 65000,
    },
    (upstream) => {
      upstream.resume();
      if ((upstream.statusCode ?? 500) < 200 || (upstream.statusCode ?? 500) >= 300) {
        sendJson(res, 503, { ok: false, error: "upstream_not_ready" });
        return;
      }
      sendJson(res, 200, readinessDocument());
    },
  );
  probe.on("timeout", () => probe.destroy());
  probe.on("error", () => sendJson(res, 503, { ok: false, error: "upstream_unavailable" }));
  probe.end();
}

function readControlRequest(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    let size = 0;
    req.on("data", (chunk) => {
      size += chunk.length;
      if (size > MAX_CONTROL_REQUEST_BYTES) {
        reject(new Error("control_request_too_large"));
        req.resume();
        return;
      }
      chunks.push(chunk);
    });
    req.once("end", () => resolve(Buffer.concat(chunks)));
    req.once("error", reject);
  });
}

function isStatelessControlRequest(req) {
  return config.toolSessionReuse === "attested-stateless-v1"
    && req.method === "POST"
    && req.url === "/mcp";
}

async function handleRequest(req, res) {
  // The gateway is the only public protocol boundary: validate the client
  // Origin before any upstream work. The value is never forwarded across the
  // scheme transition (see forwardedHeaders) and never logged.
  if (!originAllowed(config.allowedOrigins, req.headers.origin)) {
    sendJson(res, 403, { ok: false, error: "origin_forbidden" });
    return;
  }

  if (req.url === "/readyz" && config.normalizeReadiness) {
    syntheticReady(req, res);
    return;
  }

  if (!isStatelessControlRequest(req)) {
    upstreamRequest(req, res);
    return;
  }

  let body;
  let message;
  try {
    body = await readControlRequest(req);
    message = JSON.parse(body.toString("utf8"));
  } catch {
    sendJson(res, 400, { ok: false, error: "invalid_mcp_control_request" });
    return;
  }

  if (message?.jsonrpc === "2.0" && message.method === "initialize") {
    const attestation = statelessAttestation();
    sendSseMessage(res, {
      jsonrpc: "2.0",
      id: message.id,
      result: {
        protocolVersion: config.protocolVersion,
        capabilities: {
          experimental: { krwAgentToolSession: attestation },
          tools: { listChanged: false },
        },
        serverInfo: { name: config.service, version: config.buildId },
      },
    });
    return;
  }

  if (message?.jsonrpc === "2.0" && message.method === "notifications/initialized") {
    res.writeHead(202);
    res.end();
    return;
  }

  upstreamRequest(req, res, body);
}

function main() {
  if (!configPath) throw new Error("KRW_MCP_TLS_PROXY_CONFIG is required");
  config = JSON.parse(fs.readFileSync(configPath, "utf8"));
  validateGatewayConfig(config);

  const server = https.createServer(
    {
      key: fs.readFileSync(config.tls.keyFile),
      cert: fs.readFileSync(config.tls.certFile),
      minVersion: "TLSv1.2",
    },
    (req, res) => {
      void handleRequest(req, res).catch(() => {
        if (!res.headersSent) sendJson(res, 500, { ok: false, error: "gateway_internal_error" });
        else if (!res.writableEnded) res.end();
      });
    },
  );

  server.listen(config.listen.port, config.listen.host, () => {
    process.stdout.write(
      `${config.service} TLS MCP gateway listening on ${config.listen.host}:${config.listen.port}\n`,
    );
  });

  for (const signal of ["SIGINT", "SIGTERM"]) {
    process.once(signal, () => server.close(() => process.exit(0)));
  }
}

const invokedAsMain =
  process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href;
if (invokedAsMain) main();
