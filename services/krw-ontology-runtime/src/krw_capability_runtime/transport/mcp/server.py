"""Low-level MCP server for the canonical capability registry."""

from __future__ import annotations

from collections.abc import Mapping

from mcp.server import Server

from krw_capability_runtime.transport.mcp.descriptors import CapabilityRegistry


def create_server(
    registry: CapabilityRegistry,
    *,
    server_version: str,
) -> Server:
    """Create a schema-first MCP server from the immutable descriptor registry."""

    server = Server(
        "krw-capabilityd",
        version=server_version,
        instructions=(
            "Read-only KRW ontology and Guru capabilities. "
            "Use each descriptor's exact input schema."
        ),
    )

    @server.list_tools()
    async def list_tools():
        return registry.list_tools()

    @server.call_tool(validate_input=False)
    async def call_tool(name: str, arguments: Mapping[str, object]):
        # Descriptor-owned Pydantic validation runs after transport parsing so
        # root SearchPlan corrections remain structured rather than becoming a
        # generic MCP schema failure.
        return await registry.dispatch(name, arguments)

    return server
