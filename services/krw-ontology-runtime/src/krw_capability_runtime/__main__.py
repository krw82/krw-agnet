"""Standalone entrypoint for the canonical read-only capability daemon."""

from __future__ import annotations

import argparse
import json
import os

from krw_capability_runtime.config.paths import ONTOLOGY_ENV_ENV
from krw_capability_runtime.mcp_server.runtime import configured_mcp_root, prepare_mcp_runtime
from krw_capability_runtime.transport.mcp.descriptors import build_registry
from krw_capability_runtime.transport.mcp.http import HttpTransportConfig, create_http_app
from krw_capability_runtime.transport.mcp.identity import (
    assert_expected_identity,
    build_service_identity,
)

_DEFAULT_HOST = "127.0.0.1"
_DEFAULT_PORT = 8088


def _listener_setting() -> tuple[str, int, HttpTransportConfig]:
    """Parse the intentionally small local-listener configuration surface."""

    host = os.getenv("KRW_CAPABILITYD_HOST", _DEFAULT_HOST).strip()
    if host not in {"127.0.0.1", "::1", "localhost"}:
        raise RuntimeError(
            "KRW_CAPABILITYD_HOST must be a loopback address; expose this local sidecar through a trusted deployment binding instead."
        )
    raw_port = os.getenv("KRW_CAPABILITYD_PORT", str(_DEFAULT_PORT)).strip()
    try:
        port = int(raw_port)
    except ValueError as error:
        raise RuntimeError("KRW_CAPABILITYD_PORT must be an integer") from error
    if not 1 <= port <= 65_535:
        raise RuntimeError("KRW_CAPABILITYD_PORT must be within 1..65535")
    config = HttpTransportConfig()
    config.validate()
    return host, port, config


def _arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(prog="krw-capabilityd")
    parser.add_argument(
        "--print-identity",
        action="store_true",
        help="verify the configured release and print only its non-secret deployment pins",
    )
    return parser.parse_args()


def _admit_identity():
    """Verify the release once and derive the exact sidecar identity from it."""

    verification = prepare_mcp_runtime(
        root=configured_mcp_root(),
        env=os.getenv(ONTOLOGY_ENV_ENV),
        expected_release_id=os.getenv("KRW_CAPABILITYD_EXPECTED_RELEASE_ID"),
        store_mode="persistent",
    )
    registry = build_registry()
    identity = build_service_identity(registry, verification)
    return verification, registry, identity


def main() -> None:
    """Admit one immutable release, then inspect or run one HTTP sidecar."""

    args = _arguments()
    verification, registry, identity = _admit_identity()
    if args.print_identity:
        print(
            json.dumps(
                identity.readiness_document(tool_count=len(registry.list_tools())),
                ensure_ascii=False,
                sort_keys=True,
            )
        )
        return
    assert_expected_identity(
        identity,
        require_all=verification.get("env") == "prod",
    )
    host, port, transport_config = _listener_setting()
    # `workers=1` is intentional: a second Uvicorn worker would load a second
    # immutable ontology store and defeat the machine-wide sharing boundary.
    import uvicorn

    uvicorn.run(
        create_http_app(
            registry=registry,
            identity=identity,
            config=transport_config,
        ),
        host=host,
        port=port,
        log_level="warning",
        access_log=False,
    )


if __name__ == "__main__":
    main()
