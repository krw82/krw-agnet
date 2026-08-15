import assert from "node:assert/strict";
import { test } from "node:test";

import {
  originAllowed,
  upstreamReadinessMatches,
  validateGatewayConfig,
} from "./mcp_tls_proxy.mjs";

function validConfig(overrides = {}) {
  return {
    service: "krw-ontology",
    toolSessionReuse: "run-scoped",
    allowedOrigins: ["https://krw-agent.local"],
    checkUpstreamReady: true,
    normalizeReadiness: true,
    upstream: { host: "127.0.0.1", port: 8080 },
    tls: { keyFile: "/tmp/key.pem", certFile: "/tmp/cert.pem" },
    listen: { host: "127.0.0.1", port: 9443 },
    ...overrides,
  };
}

test("origin allowlist is an exact-string match", () => {
  const allowed = ["https://krw-agent.local", "https://worker.krw.local:8443"];
  assert.equal(originAllowed(allowed, "https://krw-agent.local"), true);
  assert.equal(originAllowed(allowed, "https://worker.krw.local:8443"), true);
});

test("origin allowlist rejects every near miss", () => {
  const allowed = ["https://krw-agent.local"];
  // Suffix/subdomain tricks.
  assert.equal(originAllowed(allowed, "https://krw-agent.local.attacker.test"), false);
  assert.equal(originAllowed(allowed, "https://evil-krw-agent.local"), false);
  // Scheme and port differences are different origins.
  assert.equal(originAllowed(allowed, "http://krw-agent.local"), false);
  assert.equal(originAllowed(allowed, "https://krw-agent.local:443"), false);
  assert.equal(originAllowed(allowed, "https://krw-agent.local/"), false);
  // Absent or malformed headers never pass a fail-closed gateway.
  assert.equal(originAllowed(allowed, undefined), false);
  assert.equal(originAllowed(allowed, ""), false);
  assert.equal(originAllowed(allowed, 42), false);
});

test("gateway config requires a non-empty exact-origin allowlist", () => {
  assert.doesNotThrow(() => validateGatewayConfig(validConfig()));
  assert.throws(() => validateGatewayConfig(validConfig({ allowedOrigins: undefined })));
  assert.throws(() => validateGatewayConfig(validConfig({ allowedOrigins: [] })));
  assert.throws(() =>
    validateGatewayConfig(validConfig({ allowedOrigins: ["https://ok.local", 7] })),
  );
  assert.throws(() =>
    validateGatewayConfig(validConfig({ allowedOrigins: "https://krw-agent.local" })),
  );
  // Origins must be bare origin strings: no path, query, or fragment.
  assert.throws(() =>
    validateGatewayConfig(validConfig({ allowedOrigins: ["https://krw-agent.local/mcp"] })),
  );
  assert.throws(() =>
    validateGatewayConfig(validConfig({ allowedOrigins: ["https://krw-agent.local?x=1"] })),
  );
  assert.throws(() =>
    validateGatewayConfig(validConfig({ allowedOrigins: ["http://krw-agent.local"] })),
  );
});

test("gateway config still validates the session-reuse contract", () => {
  assert.throws(() =>
    validateGatewayConfig(validConfig({ toolSessionReuse: "always" })),
  );
});

test("gateway config cannot silently synthesize readiness without probing upstream", () => {
  assert.throws(() => validateGatewayConfig(validConfig({ checkUpstreamReady: false })));
  assert.throws(() => validateGatewayConfig(validConfig({ normalizeReadiness: false })));
});

test("upstream readiness must be healthy and cannot contradict the pinned identity", () => {
  const config = {
    ...validConfig(),
    protocolVersion: "2025-06-18",
    buildId: "build-1",
    toolSchemaSha256: "sha256:schema",
    releaseManifestSha256: "sha256:release",
    toolCount: 3,
  };
  assert.equal(upstreamReadinessMatches(config, { ok: true }), true);
  assert.equal(upstreamReadinessMatches(config, { ok: false }), false);
  assert.equal(upstreamReadinessMatches(config, { ok: true, build_id: "old" }), false);
  assert.equal(upstreamReadinessMatches(config, {
    ok: true,
    protocol_version: "2025-06-18",
    build_id: "build-1",
    tool_schema_sha256: "sha256:schema",
    release_manifest_sha256: "sha256:release",
    tool_count: 3,
  }), true);
});
