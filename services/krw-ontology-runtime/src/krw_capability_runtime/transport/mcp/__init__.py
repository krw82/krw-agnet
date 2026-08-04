"""Low-level MCP transport built from typed capability descriptors."""

from krw_capability_runtime.transport.mcp.descriptors import CapabilityRegistry, build_registry

__all__ = ["CapabilityRegistry", "build_registry"]
