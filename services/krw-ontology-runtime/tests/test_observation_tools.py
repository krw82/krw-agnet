"""Observation capability tools: market-series and macro-series serving tests.

Pins the B5 contract for ``krw_market_series`` / ``krw_macro_series``:

- delegation to the imported observations store under the configured release
  root (``indexes/observations.sqlite``);
- ``advisory_only: true`` and ``source_usage: "research_only"`` preserved in
  every served shape;
- vendor scrub: no provider name ever crosses the MCP boundary, even though
  the store's own catalog carries provider columns;
- a missing store is an ``unavailable`` payload, never an exception;
- bounded inputs (periods <= 260, macro limit <= 24, canonical ticker);
- the two new tools are registered with the declared annotations and lane.
"""

from __future__ import annotations

import asyncio
import json
import sqlite3
from pathlib import Path

import pytest
from pydantic import ValidationError

from krw_capability_runtime.observation import tools as observation_tools
from krw_capability_runtime.observation.store import (
    MACRO_SERIES_FORMAT,
    MARKET_SERIES_FORMAT,
    OBSERVATIONS_BUILDER_VERSION,
    OBSERVATIONS_SCHEMA_VERSION,
    create_observations_schema,
    observation_id_for,
    verify_observations_schema,
    write_observation_metadata,
)
from krw_capability_runtime.observation.tools import (
    MacroSeriesRequest,
    MarketSeriesRequest,
    macro_series_tool,
    market_series_tool,
)
from krw_capability_runtime.transport.mcp.descriptors import (
    PRESENTATION_META_KEY,
    CapabilityLane,
    build_registry,
)


def canonical_json(payload: object) -> str:
    return json.dumps(
        payload, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False
    )


def _register_fake_release(root: Path) -> Path:
    """Materialize one tiny schema-v1 observations store under a fake release."""

    indexes = root / "indexes"
    indexes.mkdir(parents=True)
    store_path = indexes / "observations.sqlite"
    conn = sqlite3.connect(store_path)
    create_observations_schema(conn)

    # Market series: SO daily last_price with one superseded vintage on
    # 2026-08-27 (the revision must win under the shared latest-vintage
    # ordering) — the catalog row deliberately carries the vendor column so
    # the scrub assertions below are meaningful.
    conn.execute(
        """
        INSERT INTO series_catalog (
            series_key, domain, canonical_metric, unit, frequency, adjustment,
            factor, ticker, provider, provider_series_id, is_per_ticker, status,
            observation_count, first_phenomenon_time, last_phenomenon_time,
            last_result_time
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        (
            "price_close:SO",
            "price",
            "last_price",
            "USD_per_share",
            "daily",
            "none",
            None,
            "SO",
            "fmp",
            "SO",
            0,
            "available",
            5,
            "2026-08-24",
            "2026-08-28",
            None,
        ),
    )
    market_rows = [
        ("2026-08-24", "90.0", "", None),
        ("2026-08-25", "91.0", "", None),
        ("2026-08-26", "92.0", "", None),
        # First release, then a republished vintage for the same phenomenon day.
        ("2026-08-27", "93.0", "", None),
        ("2026-08-27", "93.7", "2026-08-28T12:00:00Z", "2026-08-28T12:00:00Z"),
        ("2026-08-28", "94.0", "", None),
    ]
    for phenomenon_time, value, vintage, result_time in market_rows:
        conn.execute(
            """
            INSERT INTO observations (
                observation_id, series_key, phenomenon_time, value, result_time,
                vintage, provenance_json
            ) VALUES (?, ?, ?, ?, ?, ?, ?)
            """,
            (
                observation_id_for("price_close:SO", phenomenon_time, vintage or None),
                "price_close:SO",
                phenomenon_time,
                float(value),
                result_time,
                vintage,
                "{}",
            ),
        )

    # Macro series: cpi_yoy monthly points from the FRED-backed family.
    conn.execute(
        """
        INSERT INTO series_catalog (
            series_key, domain, canonical_metric, unit, frequency, adjustment,
            factor, ticker, provider, provider_series_id, is_per_ticker, status,
            observation_count, first_phenomenon_time, last_phenomenon_time,
            last_result_time
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        (
            "macro_cpi_yoy",
            "macro",
            "cpi_yoy",
            "percent",
            "monthly",
            "seasonal",
            "inflation",
            None,
            "fred",
            "CPIAUCSL",
            0,
            "available",
            4,
            "2026-05",
            "2026-08",
            None,
        ),
    )
    for month, value in (("2026-05", 2.9), ("2026-06", 2.8), ("2026-07", 2.7), ("2026-08", 2.6)):
        conn.execute(
            """
            INSERT INTO observations (
                observation_id, series_key, phenomenon_time, value, result_time,
                vintage, provenance_json
            ) VALUES (?, ?, ?, ?, ?, ?, ?)
            """,
            (
                observation_id_for("macro_cpi_yoy", month, "2026-release"),
                "macro_cpi_yoy",
                month,
                value,
                "2026-release",
                "2026-release",
                "{}",
            ),
        )

    write_observation_metadata(
        conn,
        {
            "schema_version": OBSERVATIONS_SCHEMA_VERSION,
            "builder_version": OBSERVATIONS_BUILDER_VERSION,
            "built_at": "2026-08-30T00:00:00Z",
        },
    )
    conn.commit()
    conn.close()

    verification = verify_observations_schema(store_path)
    assert verification["ok"], verification["errors"]
    return store_path


@pytest.fixture()
def fake_release(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    store_path = _register_fake_release(tmp_path)
    monkeypatch.delenv("KRW_ONTOLOGY_ROOT", raising=False)
    monkeypatch.setenv("KRW_ONTOLOGY_RELEASE_ROOT", str(tmp_path))
    assert observation_tools.observations_store_path() == store_path
    return store_path


def test_market_series_tool_serves_latest_vintage_points(fake_release: Path) -> None:
    result = market_series_tool(ticker="SO", metric="last_price", periods=5)

    assert result["format"] == MARKET_SERIES_FORMAT
    assert result["status"] == "available"
    assert result["ticker"] == "SO"
    assert result["canonical_metric"] == "last_price"
    assert result["unit"] == "USD_per_share"
    assert result["currency"] == "USD"
    assert result["fetched_at"] == "2026-08-30T00:00:00Z"
    assert result["as_of"] == "2026-08-28"
    # Exactly one row per phenomenon day, latest vintage wins, ascending order.
    dates = [point["date"] for point in result["points"]]
    assert dates == ["2026-08-24", "2026-08-25", "2026-08-26", "2026-08-27", "2026-08-28"]
    revised = next(point for point in result["points"] if point["date"] == "2026-08-27")
    assert revised["value"] == 93.7
    # periods bounds the served window to the most recent days.
    bounded = market_series_tool(ticker="SO", metric="last_price", periods=2)
    assert [point["date"] for point in bounded["points"]] == ["2026-08-27", "2026-08-28"]


def test_market_series_tool_scrubs_vendor_and_stays_advisory(fake_release: Path) -> None:
    result = market_series_tool(ticker="SO", metric="last_price", periods=5)

    assert result["format"] == "market-series/v1"
    assert result["advisory_only"] is True
    assert result["source_usage"] == "research_only"
    assert "fmp" not in canonical_json(result).lower()
    assert "fred" not in canonical_json(result).lower()
    assert "polygon" not in canonical_json(result).lower()
    assert "provider" not in canonical_json(result)


def test_macro_series_tool_serves_bounded_macro_series(fake_release: Path) -> None:
    result = macro_series_tool(metric="cpi_yoy", limit=3)

    assert result["format"] == MACRO_SERIES_FORMAT
    assert result["status"] == "available"
    assert result["canonical_metric"] == "cpi_yoy"
    assert result["frequency"] == "monthly"
    assert result["factor"] == "inflation"
    assert result["advisory_only"] is True
    assert result["source_usage"] == "research_only"
    assert [point["date"] for point in result["points"]] == ["2026-06", "2026-07", "2026-08"]
    assert result["as_of"] == "2026-08"
    lowered = canonical_json(result).lower()
    assert "fred" not in lowered and "fmp" not in lowered and "polygon" not in lowered


def test_unknown_series_is_no_data_and_never_an_error(fake_release: Path) -> None:
    market = market_series_tool(ticker="SO", metric="trailing_pe_ttm", periods=10)
    macro = macro_series_tool(metric="fed_funds_rate", limit=10)

    assert market["format"] == MARKET_SERIES_FORMAT
    assert market["status"] == "no_data"
    assert market["points"] == []
    assert market["advisory_only"] is True
    assert market["source_usage"] == "research_only"
    assert macro["status"] == "no_data"
    assert macro["points"] == []
    assert macro["advisory_only"] is True


def test_metric_request_echo_never_trips_the_vendor_guard(fake_release: Path) -> None:
    """A caller-validated metric id that spells a transport name is echoed
    input, not a vendor leak: both tools must answer no_data, not raise."""

    market = market_series_tool(ticker="SO", metric="fmp_last_price", periods=5)
    macro = macro_series_tool(metric="fred_gdp", limit=5)

    assert market["format"] == MARKET_SERIES_FORMAT
    assert market["status"] == "no_data"
    assert market["canonical_metric"] == "fmp_last_price"
    assert market["points"] == []
    assert market["advisory_only"] is True
    assert macro["format"] == MACRO_SERIES_FORMAT
    assert macro["status"] == "no_data"
    assert macro["canonical_metric"] == "fred_gdp"
    assert macro["points"] == []
    assert macro["advisory_only"] is True


def test_absent_store_serves_unavailable_without_raising(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv("KRW_ONTOLOGY_ROOT", raising=False)
    monkeypatch.setenv("KRW_ONTOLOGY_RELEASE_ROOT", str(tmp_path))

    market = market_series_tool(ticker="SO", metric="last_price", periods=5)
    macro = macro_series_tool(metric="cpi_yoy", limit=6)

    assert market["format"] == MARKET_SERIES_FORMAT
    assert market["status"] == "unavailable"
    assert market["points"] == []
    assert market["advisory_only"] is True
    assert market["source_usage"] == "research_only"
    assert macro["format"] == MACRO_SERIES_FORMAT
    assert macro["status"] == "unavailable"
    assert macro["advisory_only"] is True


def test_doctrine_guard_fails_closed_on_vendor_leak(
    fake_release: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    leaked = market_series_tool(ticker="SO", metric="last_price", periods=5)
    leaked["provider"] = "fmp"

    monkeypatch.setattr(
        observation_tools,
        "query_market_series",
        lambda *_args, **_kwargs: leaked,
    )
    with pytest.raises(RuntimeError, match="collection identifiers"):
        market_series_tool(ticker="SO", metric="last_price", periods=5)


@pytest.mark.parametrize("periods", [0, -1, 261, 1000])
def test_market_series_rejects_periods_outside_the_bound(periods: int) -> None:
    with pytest.raises(ValidationError):
        MarketSeriesRequest(ticker="SO", metric="last_price", periods=periods)


@pytest.mark.parametrize("limit", [0, -1, 25])
def test_macro_series_rejects_limits_outside_the_bound(limit: int) -> None:
    with pytest.raises(ValidationError):
        MacroSeriesRequest(metric="cpi_yoy", limit=limit)


@pytest.mark.parametrize("ticker", ["so", "AAPL/../../", "", "AAPL_", "a"])
def test_market_series_rejects_noncanonical_tickers(ticker: str) -> None:
    with pytest.raises(ValidationError):
        MarketSeriesRequest(ticker=ticker, metric="last_price")


@pytest.mark.parametrize("metric", ["Last Price", "last-price", "", "UPPER"])
def test_series_requests_reject_noncanonical_metrics(metric: str) -> None:
    with pytest.raises(ValidationError):
        MarketSeriesRequest(ticker="SO", metric=metric)
    with pytest.raises(ValidationError):
        MacroSeriesRequest(metric=metric)


def test_series_requests_reject_unknown_arguments() -> None:
    with pytest.raises(ValidationError):
        MarketSeriesRequest(ticker="SO", metric="last_price", periods=5, provider="fmp")
    with pytest.raises(ValidationError):
        MacroSeriesRequest(metric="cpi_yoy", limit=5, root="/tmp")


def test_registry_registers_both_observation_tools_with_annotations() -> None:
    registry = build_registry()
    names = {tool.name for tool in registry.list_tools()}

    assert {"krw_market_series", "krw_macro_series"} <= names
    for name in ("krw_market_series", "krw_macro_series"):
        descriptor = registry.descriptor(name)
        assert descriptor.lane is CapabilityLane.BROAD
        wire_tool = descriptor.as_mcp_tool()
        assert wire_tool.annotations is not None
        assert wire_tool.annotations.readOnlyHint is True
        assert wire_tool.annotations.destructiveHint is False
        schema = descriptor.input_schema()
        assert schema["additionalProperties"] is False
        assert "root" not in schema["properties"]

    market_schema = registry.descriptor("krw_market_series").input_schema()
    assert market_schema["properties"]["periods"]["maximum"] == 260
    assert market_schema["properties"]["periods"]["minimum"] == 1
    macro_schema = registry.descriptor("krw_macro_series").input_schema()
    assert macro_schema["properties"]["limit"]["maximum"] == 24
    assert macro_schema["properties"]["limit"]["minimum"] == 1


def test_dispatch_serves_series_and_carries_the_presentation_meta_pack(
    fake_release: Path,
) -> None:
    registry = build_registry()

    market = asyncio.run(
        registry.dispatch("krw_market_series", {"ticker": "SO", "metric": "last_price", "periods": 5})
    )
    macro = asyncio.run(registry.dispatch("krw_macro_series", {"metric": "cpi_yoy", "limit": 2}))

    assert market.isError is not True
    assert market.structuredContent is not None
    assert market.structuredContent["format"] == MARKET_SERIES_FORMAT
    assert market.meta is not None
    market_pack = market.meta[PRESENTATION_META_KEY]
    assert market_pack["schema_version"] == 2
    assert market_pack["mode"] == "observation_series"
    series = market_pack["series"][0]
    assert series["ticker"] == "SO"
    assert series["unit"] == "USD_per_share"
    assert series["currency"] == "USD"
    assert series["scope"] == {"kind": "company_total", "key": "SO"}
    assert series["basis"] == "observation"
    assert series["advisory_only"] is True
    assert [point["period"] for point in series["points"]] == [
        point["date"] for point in market.structuredContent["points"]
    ]

    assert macro.isError is not True
    assert macro.structuredContent is not None
    assert macro.structuredContent["format"] == MACRO_SERIES_FORMAT
    assert macro.meta is not None
    macro_series = macro.meta[PRESENTATION_META_KEY]["series"][0]
    assert macro_series["scope"] == {"kind": "macro", "key": "cpi_yoy"}
    assert macro_series["factor"] == "inflation"


def test_dispatch_keeps_meta_absent_when_the_store_is_unavailable(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv("KRW_ONTOLOGY_ROOT", raising=False)
    monkeypatch.setenv("KRW_ONTOLOGY_RELEASE_ROOT", str(tmp_path))
    registry = build_registry()

    result = asyncio.run(
        registry.dispatch("krw_market_series", {"ticker": "SO", "metric": "last_price", "periods": 5})
    )

    assert result.isError is not True
    assert result.structuredContent is not None
    assert result.structuredContent["status"] == "unavailable"
    assert result.structuredContent["advisory_only"] is True
    assert result.meta is None


def test_dispatch_rejects_out_of_bounds_periods_as_input_correction(
    fake_release: Path,
) -> None:
    registry = build_registry()

    result = asyncio.run(
        registry.dispatch("krw_market_series", {"ticker": "SO", "metric": "last_price", "periods": 261})
    )

    assert result.isError is True
    assert result.structuredContent is not None
    assert result.structuredContent["code"] == "invalid_tool_input"
