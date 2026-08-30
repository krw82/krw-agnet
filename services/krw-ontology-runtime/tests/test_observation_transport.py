"""Wire-level MCP transport tests for the observation capability tools.

B5's ``test_observation_tools.py`` covers the handler and registry-dispatch
layers against a real schema-v1 fixture store.  These tests drive the SAME
fixture store through the actual MCP server transport (streamable-HTTP app +
JSON-RPC ``tools/call``), so the available path is proven on the wire exactly
as an agent sees it: descriptor decode -> store query -> payload doctrine ->
``_meta`` presentation pack.  The unavailable (no-store) path is mirrored at
the wire level too, matching the live smoke's no-store checks.

The full-stack available-path e2e against a release carrying a REAL collected
``observations.sqlite`` is a separate post-merge validation step (it needs
provider collection keys); until then these transport tests are the covering
evidence for tool -> store -> wire, with the adapter/chart legs covered by the
cargo-level tests referenced in the B8 report.
"""

from __future__ import annotations

import json
import os
from pathlib import Path

import pytest
from starlette.testclient import TestClient

from test_observation_tools import _register_fake_release
from krw_capability_runtime.observation.store import (
    MACRO_SERIES_FORMAT,
    MARKET_SERIES_FORMAT,
)
from krw_capability_runtime.transport.mcp.descriptors import (
    PRESENTATION_META_KEY,
    build_registry,
)
from krw_capability_runtime.transport.mcp.http import create_http_app
from krw_capability_runtime.transport.mcp.identity import CapabilityServiceIdentity

_VENDOR_TOKENS = ("fmp", "fred", "polygon")
_ECHO_FIELDS = ("ticker", "canonical_metric")


def _identity() -> CapabilityServiceIdentity:
    return CapabilityServiceIdentity(
        build_id="observation-transport-test",
        tool_schema_sha256="sha256:" + "a" * 64,
        release_manifest_sha256="sha256:" + "b" * 64,
    )


@pytest.fixture()
def wire_client(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> TestClient:
    """One MCP transport over a fake release carrying a fixture store."""

    _register_fake_release(tmp_path)
    monkeypatch.delenv("KRW_ONTOLOGY_ROOT", raising=False)
    monkeypatch.setenv("KRW_ONTOLOGY_RELEASE_ROOT", str(tmp_path))
    client = TestClient(create_http_app(registry=build_registry(), identity=_identity()))
    client.__enter__()
    yield client
    client.__exit__(None, None, None)


def _mcp_headers() -> dict[str, str]:
    return {
        "Accept": "application/json, text/event-stream",
        "Content-Type": "application/json",
        "MCP-Protocol-Version": "2025-06-18",
        "Origin": "https://capability-test.invalid",
    }


def _initialize(client: TestClient) -> None:
    response = client.post(
        "/mcp",
        headers=_mcp_headers(),
        json={
            "jsonrpc": "2.0",
            "id": "observation-transport-init",
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "observation-transport-test", "version": "1"},
            },
        },
    )
    assert response.status_code == 200
    assert response.json()["result"]["protocolVersion"] == "2025-06-18"


def _call_tool(client: TestClient, name: str, arguments: dict) -> dict:
    response = client.post(
        "/mcp",
        headers=_mcp_headers(),
        json={
            "jsonrpc": "2.0",
            "id": f"observation-transport-{name}",
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        },
    )
    assert response.status_code == 200
    message = response.json()
    assert message.get("error") is None
    result = message["result"]
    assert result.get("isError") is not True
    return result


def _assert_no_vendor_tokens(payload: dict) -> None:
    scanned = {key: value for key, value in payload.items() if key not in _ECHO_FIELDS}
    canonical = json.dumps(scanned, ensure_ascii=False, sort_keys=True, separators=(",", ":")).lower()
    leaked = sorted(token for token in _VENDOR_TOKENS if token in canonical)
    assert not leaked, f"observation wire payload leaked collection identifiers: {leaked}"


def test_market_series_available_path_serves_on_the_wire(wire_client: TestClient) -> None:
    _initialize(wire_client)

    result = _call_tool(
        wire_client,
        "krw_market_series",
        {"ticker": "SO", "metric": "last_price", "periods": 5},
    )

    payload = result["structuredContent"]
    assert payload["format"] == MARKET_SERIES_FORMAT
    assert payload["status"] == "available"
    assert payload["advisory_only"] is True
    assert payload["source_usage"] == "research_only"
    assert payload["ticker"] == "SO"
    assert payload["canonical_metric"] == "last_price"
    assert payload["unit"] == "USD_per_share"
    assert payload["currency"] == "USD"
    assert payload["as_of"] == "2026-08-28"
    points = payload["points"]
    assert [point["date"] for point in points] == [
        "2026-08-24",
        "2026-08-25",
        "2026-08-26",
        "2026-08-27",
        "2026-08-28",
    ]
    # The republished 2026-08-27 vintage (93.7) must win over the first
    # release (93.0): latest-vintage resolution survives the wire.
    assert [point["value"] for point in points] == [90.0, 91.0, 92.0, 93.7, 94.0]
    assert all(point["observation_id"] for point in points)
    _assert_no_vendor_tokens(payload)

    meta = result.get("_meta") or {}
    pack = meta.get(PRESENTATION_META_KEY)
    assert pack is not None, "available market series must carry the chart _meta pack"
    assert pack["schema_version"] == 2
    assert pack["mode"] == "observation_series"
    series = pack["series"][0]
    assert series["ticker"] == "SO"
    assert series["scope"] == {"kind": "company_total", "key": "SO"}
    assert series["basis"] == "observation"
    assert series["advisory_only"] is True
    assert [point["period"] for point in series["points"]] == [
        point["date"] for point in points
    ]
    assert [point["object_id"] for point in series["points"]] == [
        point["observation_id"] for point in points
    ]
    _assert_no_vendor_tokens({PRESENTATION_META_KEY: pack})


def test_macro_series_available_path_serves_on_the_wire(wire_client: TestClient) -> None:
    _initialize(wire_client)

    result = _call_tool(wire_client, "krw_macro_series", {"metric": "cpi_yoy", "limit": 4})

    payload = result["structuredContent"]
    assert payload["format"] == MACRO_SERIES_FORMAT
    assert payload["status"] == "available"
    assert payload["advisory_only"] is True
    assert payload["source_usage"] == "research_only"
    assert payload["canonical_metric"] == "cpi_yoy"
    # Macro series are not per-ticker; the available payload omits the key
    # entirely (compact serialization drops None values).
    assert "ticker" not in payload
    assert payload["as_of"] == "2026-08"
    assert [point["date"] for point in payload["points"]] == [
        "2026-05",
        "2026-06",
        "2026-07",
        "2026-08",
    ]
    assert [point["value"] for point in payload["points"]] == [2.9, 2.8, 2.7, 2.6]
    _assert_no_vendor_tokens(payload)

    pack = (result.get("_meta") or {}).get(PRESENTATION_META_KEY)
    assert pack is not None, "available macro series must carry the chart _meta pack"
    series = pack["series"][0]
    assert series["scope"] == {"kind": "macro", "key": "cpi_yoy"}
    assert series["factor"] == "inflation"
    assert series["basis"] == "observation"
    _assert_no_vendor_tokens({PRESENTATION_META_KEY: pack})


def test_unavailable_path_over_the_wire_keeps_meta_absent(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv("KRW_ONTOLOGY_ROOT", raising=False)
    monkeypatch.setenv("KRW_ONTOLOGY_RELEASE_ROOT", str(tmp_path))  # no store file
    assert not (tmp_path / "indexes" / "observations.sqlite").exists()

    client = TestClient(create_http_app(registry=build_registry(), identity=_identity()))
    with client:
        _initialize(client)
        result = _call_tool(
            client,
            "krw_market_series",
            {"ticker": "SO", "metric": "last_price"},
        )

    payload = result["structuredContent"]
    assert payload["format"] == MARKET_SERIES_FORMAT
    assert payload["status"] == "unavailable"
    assert payload["advisory_only"] is True
    assert payload["source_usage"] == "research_only"
    assert payload["points"] == []
    assert (result.get("_meta") or {}).get(PRESENTATION_META_KEY) is None
    _assert_no_vendor_tokens(payload)
    # The release root env is read per call; the probe must not have polluted
    # the process environment for later tests.
    assert os.environ["KRW_ONTOLOGY_RELEASE_ROOT"] == str(tmp_path)
