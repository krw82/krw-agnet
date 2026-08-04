"""Local Streamable HTTP transport for the shared capability daemon.

The ontology runtime owns one process-wide registry, lane limiter, and opened
release.  HTTP MCP sessions are bounded transport state only; they never own
their own ontology store or Python worker pool.  This module intentionally
uses the low-level MCP server object from :mod:`server` directly.
"""

from __future__ import annotations

from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from dataclasses import dataclass

from mcp.server.streamable_http_manager import StreamableHTTPSessionManager
from starlette.applications import Starlette
from starlette.requests import Request
from starlette.responses import JSONResponse
from starlette.routing import Mount, Route

from krw_capability_runtime.transport.mcp.descriptors import CapabilityRegistry
from krw_capability_runtime.transport.mcp.identity import CapabilityServiceIdentity
from krw_capability_runtime.transport.mcp.server import create_server

_MCP_PATH = "/mcp"
_HEALTH_PATH = "/healthz"
_DEFAULT_SESSION_IDLE_SECONDS = 300.0
_MIN_SESSION_IDLE_SECONDS = 30.0
_MAX_SESSION_IDLE_SECONDS = 3_600.0


@dataclass(frozen=True)
class HttpTransportConfig:
    """Closed non-secret configuration for the local HTTP MCP listener."""

    session_idle_seconds: float = _DEFAULT_SESSION_IDLE_SECONDS

    def validate(self) -> None:
        if not (
            _MIN_SESSION_IDLE_SECONDS
            <= self.session_idle_seconds
            <= _MAX_SESSION_IDLE_SECONDS
        ):
            raise ValueError(
                "session_idle_seconds must stay within the bounded local MCP range"
            )


def create_http_app(
    *,
    registry: CapabilityRegistry,
    identity: CapabilityServiceIdentity,
    config: HttpTransportConfig | None = None,
) -> Starlette:
    """Create a stateful, bounded Streamable HTTP MCP application.

    ``StreamableHTTPSessionManager`` is instantiated exactly once per ASGI app
    and entered through the ASGI lifespan.  The manager shares the registry
    and never creates a separate ontology runtime per MCP session.
    """

    resolved_registry = registry
    resolved_config = config or HttpTransportConfig()
    resolved_config.validate()
    server = create_server(resolved_registry, server_version=identity.build_id)
    manager = StreamableHTTPSessionManager(
        server,
        json_response=True,
        stateless=False,
        session_idle_timeout=resolved_config.session_idle_seconds,
    )

    @asynccontextmanager
    async def lifespan(_: Starlette) -> AsyncIterator[None]:
        async with manager.run():
            yield

    async def health(_: Request) -> JSONResponse:
        return JSONResponse(identity.readiness_document(tool_count=len(resolved_registry.list_tools())))

    return Starlette(
        debug=False,
        routes=[
            Route(_HEALTH_PATH, health, methods=["GET"]),
            Mount(_MCP_PATH, app=manager.handle_request),
        ],
        lifespan=lifespan,
    )
