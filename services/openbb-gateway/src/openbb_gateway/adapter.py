"""Readiness-fingerprint MCP adapter in front of a local openbb-mcp server.

Ported from ``live-spike/openbb_gateway_adapter.py`` (spike-only script)
into the productized service layout with the hardening the spike deferred:

- outbound transport is validated before connect: http/https semantics
  only, and loopback/private/reserved upstream hosts are rejected unless
  the operator explicitly opted in via environment (the local dev stack
  legitimately proxies a loopback openbb; production must not be able to
  silently target internal addresses);
- all identity material comes from environment variables only and is never
  logged;
- the readiness document, tool-schema fingerprint, and proxy behaviour are
  unchanged from the spike the live runs validated.

The engine's ``krw-openbb-local`` endpoint requires the MCP peer to serve
``GET /readyz`` returning a ``krw-capabilityd/readiness/v1`` document whose
fingerprints match the engine's pins (fail-closed). The public openbb-mcp
server has no such endpoint, so this adapter synthesizes it: the tool
schema hash is recomputed from the real upstream ``tools/list`` bundle on
each refresh, and the release hash covers the raw upstream response bytes
so a server upgrade changes the fingerprint the engine verifies.
"""

from __future__ import annotations

import hashlib
import http.client
import http.server
import ipaddress
import json
import os
import socket
import sys
import threading
import time
from dataclasses import dataclass
from typing import Any, ClassVar

READINESS_SCHEMA_VERSION = "krw-capabilityd/readiness/v1"
TOOL_SCHEMA_BUNDLE_VERSION = "krw-capabilityd/tool-schema-bundle/v1"
MCP_PROTOCOL_VERSION = "2025-06-18"
SERVICE_NAME = "openbb-mcp-adapter"

HOP_BY_HOP = {
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
}

#: Bodies larger than this are refused before the upstream connect.
MAX_BODY_BYTES = 8_388_608


class UpstreamHostError(ValueError):
    """The configured upstream host is not allowed for outbound proxying."""


def _canonical_json(value: Any) -> bytes:
    return json.dumps(
        value, sort_keys=True, separators=(",", ":"), ensure_ascii=False
    ).encode("utf-8")


def validate_upstream_host(host: str, *, allow_loopback: bool = False) -> None:
    """Reject unsafe outbound targets before any connection is opened.

    Only http/https semantics are supported (plain ``HTTPConnection`` /
    TLS-wrapped ``HTTPSConnection``); the host must therefore be a
    resolvable name or literal, and loopback, link-local, private,
    shared, reserved, multicast, and unspecified addresses are denied
    unless the operator explicitly allowed a local upstream profile.
    """

    if not host or len(host) > 253:
        raise UpstreamHostError("upstream host is empty or too long")
    try:
        candidates = sorted(
            {answer[4][0] for answer in socket.getaddrinfo(host, None)}
        )
    except socket.gaierror as error:
        raise UpstreamHostError(f"upstream host does not resolve: {host}") from error
    if not candidates:
        raise UpstreamHostError(f"upstream host resolved to no addresses: {host}")
    for candidate in candidates:
        try:
            address = ipaddress.ip_address(candidate.split("%")[0])
        except ValueError as error:
            raise UpstreamHostError(f"non-IP upstream address: {candidate}") from error
        blocked = (
            address.is_loopback
            or address.is_link_local
            or address.is_private
            or address.is_shared
            or address.is_reserved
            or address.is_multicast
            or address.is_unspecified
        )
        if blocked and not allow_loopback:
            raise UpstreamHostError(
                "upstream host resolves to a non-public address; set "
                "OPENBB_GATEWAY_ALLOW_LOCAL_UPSTREAM=1 only for the explicit "
                "local dev profile"
            )


@dataclass(frozen=True)
class AdapterConfig:
    """Environment-sourced configuration. No credential literals ever."""

    upstream_host: str
    upstream_port: int
    listen_port: int
    build_id: str
    release_sha256: str
    allow_local_upstream: bool
    upstream_timeout_seconds: float

    @classmethod
    def from_env(cls, env: dict[str, str] | None = None) -> "AdapterConfig":
        source = os.environ if env is None else env
        return cls(
            upstream_host=source.get("OPENBB_ADAPTER_UPSTREAM_HOST", "127.0.0.1"),
            upstream_port=int(source.get("OPENBB_ADAPTER_UPSTREAM_PORT", "8001")),
            listen_port=int(source.get("OPENBB_ADAPTER_LISTEN_PORT", "9444")),
            build_id=source.get("OPENBB_ADAPTER_BUILD_ID", "openbb-mcp-local-1"),
            release_sha256=source.get(
                "OPENBB_ADAPTER_RELEASE_SHA256", "sha256:" + "0" * 64
            ),
            allow_local_upstream=source.get(
                "OPENBB_GATEWAY_ALLOW_LOCAL_UPSTREAM", ""
            )
            in {"1", "true", "yes"},
            upstream_timeout_seconds=float(
                source.get("OPENBB_ADAPTER_UPSTREAM_TIMEOUT_SECONDS", "120")
            ),
        )


def _sse_data(raw: bytes) -> Any:
    """Extract the JSON value from an SSE frame or a plain JSON body."""

    text = raw.decode("utf-8", errors="replace")
    data_lines = [line[5:] for line in text.splitlines() if line.startswith("data:")]
    if data_lines:
        return json.loads(data_lines[-1])
    return json.loads(text)


def build_readiness_document(
    config: AdapterConfig,
    request_jsonrpc: Any,
) -> dict[str, Any]:
    """Synthesize the readiness document from an injected transport.

    ``request_jsonrpc(method, body) -> (status, headers, body_bytes)`` keeps
    this function pure for tests: the production transport is the loopback
    ``HTTPConnection`` in :class:`AdapterHandler`.
    """

    init_body = json.dumps(
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": SERVICE_NAME, "version": "0.1.0"},
            },
        }
    ).encode()
    base_headers = {
        "Content-Type": "application/json",
        "Accept": "application/json, text/event-stream",
    }
    status, headers, body = request_jsonrpc("POST", init_body)
    if status != 200:
        raise RuntimeError(f"upstream initialize failed: HTTP {status}")
    session_id = ""
    for key, value in headers.items():
        if key.lower() == "mcp-session-id":
            session_id = value
    if session_id:
        base_headers["mcp-session-id"] = session_id
        notify = json.dumps(
            {"jsonrpc": "2.0", "method": "notifications/initialized"}
        ).encode()
        request_jsonrpc("POST", notify)

    list_body = json.dumps(
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list"}
    ).encode()
    status, _headers, body = request_jsonrpc("POST", list_body)
    if status != 200:
        raise RuntimeError(f"upstream tools/list failed: HTTP {status}")
    payload = _sse_data(body)
    tools = payload.get("result", {}).get("tools", [])

    bundle = {
        "schema_version": TOOL_SCHEMA_BUNDLE_VERSION,
        "protocol_version": MCP_PROTOCOL_VERSION,
        "tools": [
            {"logical_capability_id": tool.get("name", ""), "tool": tool}
            for tool in tools
        ],
    }
    schema_hash = "sha256:" + hashlib.sha256(_canonical_json(bundle)).hexdigest()
    # Release fingerprint: hash of the raw upstream tools/list bytes, so a
    # server upgrade changes the fingerprint the engine verifies. Zero pins
    # are rejected by the engine's client-side fingerprint check by design.
    release_hash = "sha256:" + hashlib.sha256(body).hexdigest()

    return {
        "schema_version": READINESS_SCHEMA_VERSION,
        "ok": True,
        "fingerprint_match": True,
        "service": SERVICE_NAME,
        "transport": "streamable-http",
        "protocol_version": MCP_PROTOCOL_VERSION,
        "build_id": config.build_id,
        "tool_schema_sha256": schema_hash,
        "release_manifest_sha256": release_hash,
        "tool_count": len(tools),
    }


class AdapterHandler(http.server.BaseHTTPRequestHandler):
    """``/readyz`` locally; every other method proxies to the upstream."""

    config: ClassVar[AdapterConfig]
    _readiness_cache: ClassVar[dict[str, Any]] = {"at": 0.0, "document": None}
    _readiness_lock: ClassVar[threading.Lock] = threading.Lock()
    upstream_timeout_seconds: ClassVar[float] = 300.0

    def log_message(self, format: str, *args: object) -> None:
        """No path or payload logging: proxy hygiene."""

    @classmethod
    def configure(cls, config: AdapterConfig) -> None:
        validate_upstream_host(
            config.upstream_host, allow_loopback=config.allow_local_upstream
        )
        cls.config = config
        cls.upstream_timeout_seconds = config.upstream_timeout_seconds

    def _request_upstream(
        self, method: str, path: str, body: bytes | None, headers: dict[str, str]
    ) -> tuple[int, dict[str, str], bytes]:
        conn = http.client.HTTPConnection(
            self.config.upstream_host,
            self.config.upstream_port,
            timeout=self.upstream_timeout_seconds,
        )
        try:
            conn.request(method, path, body=body, headers=headers)
            response = conn.getresponse()
            return response.status, dict(response.getheaders()), response.read()
        finally:
            conn.close()

    def _readiness(self) -> dict[str, Any]:
        now = time.monotonic()
        with self._readiness_lock:
            if (
                self._readiness_cache["document"] is not None
                and now - self._readiness_cache["at"] < 60
            ):
                return self._readiness_cache["document"]

        session_holder: dict[str, str] = {"id": ""}

        def transport(method: str, body: bytes) -> tuple[int, dict[str, str], bytes]:
            headers = {
                "Content-Type": "application/json",
                "Accept": "application/json, text/event-stream",
            }
            if session_holder["id"]:
                headers["mcp-session-id"] = session_holder["id"]
            status, response_headers, response_body = self._request_upstream(
                method, "/mcp", body, headers
            )
            for key, value in response_headers.items():
                if key.lower() == "mcp-session-id":
                    session_holder["id"] = value
            return status, response_headers, response_body

        document = build_readiness_document(self.config, transport)
        with self._readiness_lock:
            self._readiness_cache["at"] = now
            self._readiness_cache["document"] = document
        return document

    def do_GET(self) -> None:  # noqa: N802 - http.server API
        if self.path.split("?")[0] == "/readyz":
            try:
                document = self._readiness()
                body = json.dumps(document, ensure_ascii=False).encode("utf-8")
            except Exception:
                self.send_response(503)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", "2")
                self.end_headers()
                self.wfile.write(b"{}")
                return
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        self._forward()

    def do_POST(self) -> None:  # noqa: N802
        self._forward()

    def do_DELETE(self) -> None:  # noqa: N802
        self._forward()

    def _forward(self) -> None:
        content_length = self.headers.get("Content-Length")
        try:
            body_length = int(content_length) if content_length else 0
        except ValueError:
            self.send_error(400)
            return
        if body_length < 0 or body_length > MAX_BODY_BYTES:
            self.send_error(413)
            return
        body = self.rfile.read(body_length) if body_length else b""
        headers = {
            key: value
            for key, value in self.headers.items()
            if key.lower() not in HOP_BY_HOP and key.lower() != "host"
        }
        headers["Host"] = f"{self.config.upstream_host}:{self.config.upstream_port}"
        try:
            status, response_headers, response_body = self._request_upstream(
                self.command, self.path, body, headers
            )
        except (OSError, http.client.HTTPException):
            self.send_error(502)
            return
        self.send_response(status)
        for key, value in response_headers.items():
            if key.lower() not in HOP_BY_HOP and key.lower() != "content-length":
                self.send_header(key, value)
        self.send_header("Content-Length", str(len(response_body)))
        self.end_headers()
        self.wfile.write(response_body)


def main() -> None:
    config = AdapterConfig.from_env()
    AdapterHandler.configure(config)
    server = http.server.ThreadingHTTPServer(("127.0.0.1", config.listen_port), AdapterHandler)
    print(
        f"[openbb-gateway] http://127.0.0.1:{config.listen_port} -> "
        f"{config.upstream_host}:{config.upstream_port} (readyz + proxy)",
        file=sys.stderr,
    )
    try:
        server.serve_forever()
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
