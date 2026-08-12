"""Local stateless Streamable HTTP transport for the shared capability daemon.

The ontology runtime owns one process-wide registry, lane limiter, and opened
release.  The capability gateway advertises an attested stateless MCP contract,
so the upstream transport must have the same semantics: no per-client session
table and no session timeout that can invalidate a later tool call.
"""

from __future__ import annotations

from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
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
class HttpTransportConfig:
    """Closed non-secret configuration kept for the stable constructor API."""

    def validate(self) -> None:
        return None


def create_http_app(
    *,
    registry: CapabilityRegistry,
    identity: CapabilityServiceIdentity,
    config: HttpTransportConfig | None = None,
) -> Starlette:
    """Create the shared stateless Streamable HTTP MCP application.

    ``StreamableHTTPSessionManager`` is instantiated exactly once per ASGI app
    and entered through the ASGI lifespan.  Stateless mode shares the registry
    without creating a per-request MCP session table.
    """

    resolved_registry = registry
    resolved_config = config or HttpTransportConfig()
    resolved_config.validate()
    server = create_server(resolved_registry, server_version=identity.build_id)
    manager = StreamableHTTPSessionManager(
        server,
        json_response=True,
        stateless=True,
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
