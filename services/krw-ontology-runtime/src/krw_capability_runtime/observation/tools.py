"""MCP tool handlers over the immutable observations store.

Two bounded, read-only serving surfaces over the release-sidecar
``indexes/observations.sqlite``:

- ``krw_market_series``: latest-vintage price/valuation series for one
  canonical ticker;
- ``krw_macro_series``: latest-vintage macro indicator series.

Doctrine baked into this module (identical to the store's own invariants):

- observation values are ``advisory_only`` research context: never filing
  evidence, never strong-claim support, never recommendation or price-target
  grounds;
- vendor names are internal collection plumbing: every served payload is
  checked against the forbidden vendor tokens before it crosses the MCP
  boundary (fail closed, never a partial scrub);
- a missing or unreadable store is an ``unavailable`` payload, never an
  exception — the engine must keep running when a release predates the
  observation sidecar.
"""

from __future__ import annotations

import json
import sqlite3
from pathlib import Path
from typing import Any

from pydantic import BaseModel, ConfigDict, Field, field_validator

from krw_capability_runtime.config.paths import resolve_ontology_root
from krw_capability_runtime.observation.store import (
    MACRO_SERIES_FORMAT,
    MARKET_SERIES_FORMAT,
    MAX_MARKET_PERIODS,
    OBSERVATIONS_RELATIVE_PATH,
    SOURCE_USAGE_RESEARCH_ONLY,
    query_macro_series,
    query_market_series,
)

_MAX_TICKER_LENGTH = 32
_MAX_METRIC_LENGTH = 64
# A macro request serves at most 24 points (two years of monthly indicators);
# the store itself keeps a wider internal bound.
_MAX_MACRO_LIMIT = 24
# Internal collection-transport identifiers that must never cross the MCP
# boundary in any served payload.
_FORBIDDEN_VENDOR_TOKENS = ("fmp", "fred", "polygon")
# Chart-ready projection shipped on the private MCP ``_meta`` channel.
PRESENTATION_PACK_SCHEMA_VERSION = 2
PRESENTATION_PACK_MODE = "observation_series"


class MarketSeriesRequest(BaseModel):
    """The closed physical input contract for the market series capability."""

    model_config = ConfigDict(extra="forbid", str_strip_whitespace=True)

    ticker: str = Field(
        min_length=1,
        max_length=_MAX_TICKER_LENGTH,
        pattern=r"^[A-Z0-9][A-Z0-9.-]{0,31}$",
    )
    metric: str = Field(
        min_length=1,
        max_length=_MAX_METRIC_LENGTH,
        pattern=r"^[a-z0-9][a-z0-9_]{0,63}$",
        description="Canonical observation metric id (for example last_price).",
    )
    periods: int = Field(
        default=MAX_MARKET_PERIODS,
        ge=1,
        le=MAX_MARKET_PERIODS,
        description="Most recent observation points to serve.",
    )

    @field_validator("ticker")
    @classmethod
    def canonical_ticker(cls, value: str) -> str:
        if not value or len(value) > _MAX_TICKER_LENGTH:
            raise ValueError("ticker must be a bounded canonical market symbol")
        first, rest = value[0], value[1:]
        if not (first.isascii() and (first.isupper() or first.isdigit())):
            raise ValueError("ticker must begin with an uppercase letter or digit")
        if any(
            not (
                character.isascii()
                and (character.isupper() or character.isdigit() or character in ".-")
            )
            for character in rest
        ):
            raise ValueError("ticker must use canonical uppercase market-symbol characters")
        return value


class MacroSeriesRequest(BaseModel):
    """The closed physical input contract for the macro series capability."""

    model_config = ConfigDict(extra="forbid", str_strip_whitespace=True)

    metric: str = Field(
        min_length=1,
        max_length=_MAX_METRIC_LENGTH,
        pattern=r"^[a-z0-9][a-z0-9_]{0,63}$",
        description="Canonical macro metric id (for example cpi_yoy).",
    )
    limit: int = Field(
        default=_MAX_MACRO_LIMIT,
        ge=1,
        le=_MAX_MACRO_LIMIT,
        description="Most recent observation points to serve.",
    )


def observations_store_path() -> Path:
    """Resolve ``indexes/observations.sqlite`` under the configured release root."""

    root = resolve_ontology_root(None, fallback_to_cwd=False)
    return (root / OBSERVATIONS_RELATIVE_PATH).absolute()


def _canonical_json(payload: Any) -> str:
    return json.dumps(
        payload, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False
    )


def _unavailable_series_payload(
    format_id: str,
    *,
    ticker: str | None = None,
    canonical_metric: str | None = None,
) -> dict[str, Any]:
    """One stable no-store shape: the union of the served series keys."""

    return {
        "format": format_id,
        "series_key": None,
        "ticker": ticker,
        "canonical_metric": canonical_metric,
        "unit": None,
        "currency": None,
        "frequency": None,
        "factor": None,
        "status": "unavailable",
        "source_usage": SOURCE_USAGE_RESEARCH_ONLY,
        "advisory_only": True,
        "fetched_at": None,
        "as_of": None,
        "points": [],
    }


# Top-level payload keys that echo caller-validated request input verbatim.
_ECHO_FIELDS = ("ticker", "canonical_metric")


def _assert_doctrine_invariants(payload: dict[str, Any]) -> None:
    """Fail closed when a served payload breaks the observation doctrine.

    The vendor-token scan covers every store-derived value. The ``ticker``
    and ``canonical_metric`` keys are excluded: they echo the request
    verbatim after Pydantic validation, so a canonical ticker or metric id
    that happens to spell a transport name (``FRED``, ``fmp_last_price``) is
    the caller's own input, never a vendor leak.
    """

    if payload.get("advisory_only") is not True:
        raise RuntimeError("observation payload must stay advisory_only")
    if payload.get("source_usage") != SOURCE_USAGE_RESEARCH_ONLY:
        raise RuntimeError("observation payload must stay research_only")
    scanned = {key: value for key, value in payload.items() if key not in _ECHO_FIELDS}
    canonical = _canonical_json(scanned).lower()
    leaked = sorted(token for token in _FORBIDDEN_VENDOR_TOKENS if token in canonical)
    if leaked:
        raise RuntimeError(
            f"observation payload leaked internal collection identifiers: {leaked}"
        )


def market_series_tool(ticker: str, metric: str, periods: int = MAX_MARKET_PERIODS) -> dict[str, Any]:
    """Physical MCP handler: bounded latest-vintage market series for one ticker."""

    request = MarketSeriesRequest(ticker=ticker, metric=metric, periods=periods)
    path = observations_store_path()
    if not path.is_file():
        return _unavailable_series_payload(
            MARKET_SERIES_FORMAT,
            ticker=request.ticker,
            canonical_metric=request.metric,
        )
    try:
        payload = query_market_series(path, request.ticker, request.metric, request.periods)
    except sqlite3.Error:
        return _unavailable_series_payload(
            MARKET_SERIES_FORMAT,
            ticker=request.ticker,
            canonical_metric=request.metric,
        )
    _assert_doctrine_invariants(payload)
    return payload


def macro_series_tool(metric: str, limit: int = _MAX_MACRO_LIMIT) -> dict[str, Any]:
    """Physical MCP handler: bounded latest-vintage macro indicator series."""

    request = MacroSeriesRequest(metric=metric, limit=limit)
    path = observations_store_path()
    if not path.is_file():
        return _unavailable_series_payload(MACRO_SERIES_FORMAT, canonical_metric=request.metric)
    try:
        payload = query_macro_series(path, request.metric, request.limit)
    except sqlite3.Error:
        return _unavailable_series_payload(MACRO_SERIES_FORMAT, canonical_metric=request.metric)
    _assert_doctrine_invariants(payload)
    return payload


def observation_presentation_pack(payload: dict[str, Any]) -> dict[str, Any] | None:
    """Project one served series onto the private chart-ready ``_meta`` channel.

    Mirrors the chart-series presentation pack (schema version 2): chart
    metadata (unit, currency, frequency, scope, basis) plus the same
    latest-vintage points the model already sees in ``structured_content``.
    ``None`` when the series carries no points, so the ``_meta`` object stays
    absent instead of carrying an empty shell.
    """

    points = payload.get("points")
    if not isinstance(points, list) or not points or payload.get("status") != "available":
        return None
    format_id = str(payload.get("format") or "")
    ticker = payload.get("ticker")
    if format_id == MARKET_SERIES_FORMAT:
        scope_kind = "company_total"
        scope_key = str(ticker) if ticker else ""
    else:
        scope_kind = "macro"
        scope_key = str(payload.get("canonical_metric") or "")
    series: dict[str, Any] = {
        "ticker": ticker,
        "series_key": payload.get("series_key"),
        "canonical_metric": payload.get("canonical_metric"),
        "unit": payload.get("unit"),
        "currency": payload.get("currency"),
        "frequency": payload.get("frequency"),
        "factor": payload.get("factor"),
        "scope": {"kind": scope_kind, "key": scope_key},
        "basis": "observation",
        "advisory_only": True,
        "points": [
            {
                "period": point.get("date"),
                "value": point.get("value"),
                "object_id": point.get("observation_id"),
            }
            for point in points
            if isinstance(point, dict)
        ],
    }
    return {
        "schema_version": PRESENTATION_PACK_SCHEMA_VERSION,
        "mode": PRESENTATION_PACK_MODE,
        "series": [series],
    }
