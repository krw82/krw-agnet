"""OpenBB gateway for the KRW engine.

Two responsibilities, both service-layer only (the engine core is untouched):

- ``adapter``: the readiness-fingerprint MCP reverse proxy in front of a
  local openbb-mcp server. The engine's endpoint contract requires the MCP
  peer to serve a ``krw-capabilityd/readiness/v1`` document from the same
  origin; the public openbb server does not, so this adapter synthesizes
  the document from the upstream ``tools/list`` bundle and proxies every
  other request verbatim.
- ``widget_publisher``: translates the engine's deterministic
  ``krw-visualization`` artifacts (chart views) into workspace
  ``create_widget`` function-call payloads so research results surface as
  dashboard widgets instead of sinking into chat text.

Security posture: outbound requests are http/https only, upstream hosts
are validated before connect, and loopback/private/reserved targets are
rejected unless the operator explicitly opts in (the local dev stack
points at a loopback openbb by design). Credentials are read from
environment variable names only; none are ever stored or logged.
"""

from openbb_gateway.adapter import AdapterConfig, AdapterHandler, main
from openbb_gateway.widget_publisher import (
    widget_uuid,
    publishable_widgets,
    to_openbb_function_call,
)

__all__ = [
    "AdapterConfig",
    "AdapterHandler",
    "main",
    "widget_uuid",
    "publishable_widgets",
    "to_openbb_function_call",
]
