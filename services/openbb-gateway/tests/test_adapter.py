"""Adapter unit tests: readiness synthesis, host validation, config.

All transport is injected or offline — no test touches a network socket
except the loopback-host validation path, which is deliberately env-gated.
"""

from __future__ import annotations

import json
import unittest

from openbb_gateway.adapter import (
    AdapterConfig,
    UpstreamHostError,
    build_readiness_document,
    validate_upstream_host,
)


def _upstream_fixture(tools: list[dict]) -> tuple[callable, dict[str, int]]:
    """Fake loopback transport recording calls; serves tools/list via SSE."""

    calls: dict[str, int] = {"initialize": 0, "notifications": 0, "tools/list": 0}
    session_headers = {"mcp-session-id": "fixture-session"}

    def transport(method: str, body: bytes) -> tuple[int, dict[str, str], bytes]:
        request = json.loads(body)
        if request.get("method") == "initialize":
            calls["initialize"] += 1
            return 200, session_headers, b"{}"
        if request.get("method") == "notifications/initialized":
            calls["notifications"] += 1
            return 202, {}, b""
        if request.get("method") == "tools/list":
            calls["tools/list"] += 1
            payload = {"result": {"tools": tools}}
            frame = f"data: {json.dumps(payload)}\n\n".encode()
            return 200, {}, frame
        raise AssertionError(f"unexpected upstream call: {request}")

    return transport, calls


class ReadinessDocumentTests(unittest.TestCase):
    def test_document_shape_and_fingerprints_are_deterministic(self) -> None:
        tools = [
            {"name": "equity_price_historical", "inputSchema": {"type": "object"}},
            {"name": "economy_cpi", "inputSchema": {"type": "object"}},
        ]
        transport, calls = _upstream_fixture(tools)
        config = AdapterConfig.from_env({})

        first = build_readiness_document(config, transport)
        second = build_readiness_document(config, _upstream_fixture(tools)[0])

        self.assertTrue(first["ok"])
        self.assertTrue(first["fingerprint_match"])
        self.assertEqual(first["tool_count"], 2)
        self.assertTrue(first["tool_schema_sha256"].startswith("sha256:"))
        self.assertTrue(first["release_manifest_sha256"].startswith("sha256:"))
        self.assertNotEqual(
            first["tool_schema_sha256"], first["release_manifest_sha256"]
        )
        # Canonical bundles: identical tool sets hash identically.
        self.assertEqual(first["tool_schema_sha256"], second["tool_schema_sha256"])

    def test_tool_order_changes_the_schema_fingerprint(self) -> None:
        transport_a, _ = _upstream_fixture(
            [{"name": "a"}, {"name": "b"}]
        )
        transport_b, _ = _upstream_fixture(
            [{"name": "b"}, {"name": "a"}]
        )
        config = AdapterConfig.from_env({})
        left = build_readiness_document(config, transport_a)
        right = build_readiness_document(config, transport_b)
        self.assertNotEqual(
            left["tool_schema_sha256"], right["tool_schema_sha256"]
        )

    def test_upstream_failure_raises_and_readyz_would_503(self) -> None:
        def broken(method: str, body: bytes):
            return 500, {}, b"{}"

        with self.assertRaises(RuntimeError):
            build_readiness_document(AdapterConfig.from_env({}), broken)


class UpstreamHostValidationTests(unittest.TestCase):
    def test_loopback_is_rejected_without_the_local_opt_in(self) -> None:
        with self.assertRaises(UpstreamHostError):
            validate_upstream_host("127.0.0.1")

    def test_loopback_is_allowed_with_the_explicit_opt_in(self) -> None:
        validate_upstream_host("127.0.0.1", allow_loopback=True)

    def test_unresolvable_host_is_rejected(self) -> None:
        with self.assertRaises(UpstreamHostError):
            validate_upstream_host("definitely-not-a-host.invalid")


class AdapterConfigTests(unittest.TestCase):
    def test_defaults_match_the_local_dev_profile(self) -> None:
        config = AdapterConfig.from_env({})
        self.assertEqual(config.upstream_host, "127.0.0.1")
        self.assertEqual(config.upstream_port, 8001)
        self.assertEqual(config.listen_port, 9444)
        self.assertFalse(config.allow_local_upstream)
        # Zero-pin default: the engine rejects it client-side by design.
        self.assertTrue(config.release_sha256.endswith("0" * 64))

    def test_env_overrides_apply(self) -> None:
        config = AdapterConfig.from_env(
            {
                "OPENBB_ADAPTER_UPSTREAM_PORT": "9001",
                "OPENBB_GATEWAY_ALLOW_LOCAL_UPSTREAM": "1",
            }
        )
        self.assertEqual(config.upstream_port, 9001)
        self.assertTrue(config.allow_local_upstream)


if __name__ == "__main__":
    unittest.main()
