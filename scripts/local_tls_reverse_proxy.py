#!/usr/bin/env python3
"""Loopback-only TLS proxy used by the opt-in real-MCP transport smoke test.

This intentionally buffers responses and has no authentication, metrics, or
production hardening.  It exists only to prove that the Rust MCP transport
performs normal certificate verification against a pinned test CA while the
Python capability daemon remains loopback HTTP.  Production must use its
managed same-origin TLS ingress instead.
"""

from __future__ import annotations

import argparse
import http.client
import http.server
import ssl
from typing import ClassVar


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


class ProxyHandler(http.server.BaseHTTPRequestHandler):
    upstream_host: ClassVar[str]
    upstream_port: ClassVar[int]
    upstream_timeout_seconds: ClassVar[float]

    def log_message(self, format: str, *args: object) -> None:
        """Do not place request paths or payload-derived data in test logs."""

    def do_GET(self) -> None:
        self._forward()

    def do_POST(self) -> None:
        self._forward()

    def do_DELETE(self) -> None:
        self._forward()

    def _forward(self) -> None:
        content_length = self.headers.get("Content-Length")
        try:
            body_length = int(content_length) if content_length else 0
        except ValueError:
            self.send_error(400)
            return
        if body_length < 0 or body_length > 1_048_576:
            self.send_error(413)
            return
        body = self.rfile.read(body_length) if body_length else b""
        headers = {
            key: value
            for key, value in self.headers.items()
            if key.lower() not in HOP_BY_HOP and key.lower() != "host"
        }
        headers["Host"] = f"{self.upstream_host}:{self.upstream_port}"
        connection = http.client.HTTPConnection(
            self.upstream_host,
            self.upstream_port,
            timeout=self.upstream_timeout_seconds,
        )
        try:
            connection.request(self.command, self.path, body=body, headers=headers)
            response = connection.getresponse()
            response_body = response.read()
            response_headers = response.getheaders()
            self.send_response(response.status)
            for key, value in response_headers:
                if key.lower() not in HOP_BY_HOP and key.lower() != "content-length":
                    self.send_header(key, value)
            self.send_header("Content-Length", str(len(response_body)))
            self.end_headers()
            self.wfile.write(response_body)
        except (OSError, http.client.HTTPException):
            self.send_error(502)
        finally:
            connection.close()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--listen-port", type=int, required=True)
    parser.add_argument("--upstream-port", type=int, required=True)
    parser.add_argument("--certfile", required=True)
    parser.add_argument("--keyfile", required=True)
    parser.add_argument("--listen-host", default="127.0.0.1")
    parser.add_argument("--upstream-host", default="127.0.0.1")
    parser.add_argument("--timeout-seconds", type=float, default=65.0)
    arguments = parser.parse_args()
    if arguments.listen_host not in {"127.0.0.1", "::1", "localhost"}:
        parser.error("this test proxy may listen only on loopback")
    if not 1 <= arguments.listen_port <= 65535 or not 1 <= arguments.upstream_port <= 65535:
        parser.error("ports must be between 1 and 65535")
    return arguments


def main() -> None:
    arguments = parse_args()
    ProxyHandler.upstream_host = arguments.upstream_host
    ProxyHandler.upstream_port = arguments.upstream_port
    ProxyHandler.upstream_timeout_seconds = arguments.timeout_seconds
    server = http.server.ThreadingHTTPServer(
        (arguments.listen_host, arguments.listen_port), ProxyHandler
    )
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    context.load_cert_chain(certfile=arguments.certfile, keyfile=arguments.keyfile)
    server.socket = context.wrap_socket(server.socket, server_side=True)
    try:
        server.serve_forever()
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
