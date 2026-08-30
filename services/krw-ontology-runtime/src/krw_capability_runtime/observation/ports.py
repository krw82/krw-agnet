"""Provider ports for the observation data layer.

These Protocol interfaces are the closed boundary between external data
vendors and the observation store (built in a later task). Values crossing
this boundary are advisory-only market and macro context: never filing
evidence, never strong-claim support, never recommendation or price-target
grounds. Vendor names stay inside this layer.
"""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass, field
from typing import Literal, Protocol


@dataclass(frozen=True)
class RawObservation:
    """One normalized provider observation.

    phenomenon_time: ISO 8601 date/datetime string for the period the value
        describes (the trading day, the CPI reference month, ...).
    value: the observed number. Adapters skip rows whose value is missing or
        unparseable rather than emitting them with None; None stays in the
        contract for future sparse providers.
    result_time: ISO 8601 date string for when the value became known. This is
        the FRED/ALFRED vintage date; None for providers without a vintage
        concept (FMP and Polygon publish current values only).
    vintage: ISO 8601 vintage identifier for revised series. Two observations
        with the same phenomenon_time and different vintages (first release
        and revision) are both preserved as separate rows.
    provenance: collection provenance dict with at least ``endpoint``,
        ``params_hash`` (sha256 prefix of the public request parameters, secret
        parameters excluded), and ``fetched_at`` (ISO 8601 UTC placeholder).
    """

    series_key: str
    phenomenon_time: str
    value: float | None
    result_time: str | None = None
    vintage: str | None = None
    provenance: dict[str, str] = field(default_factory=dict)


@dataclass(frozen=True)
class SeriesFetchRequest:
    """One closed series request against a single provider.

    provider_series_id is the vendor-native identifier: the ticker for FMP
    historical endpoints, the series ID (for example ``CPIAUCSL``) for FRED.
    ticker carries the equity domain only (price and multiple series) and is
    None for macro series.
    """

    series_key: str
    provider_series_id: str
    ticker: str | None = None
    start: str | None = None
    end: str | None = None


@dataclass(frozen=True)
class SeriesFetchResult:
    """Bounded fetch outcome; external failures collapse to ``unavailable``."""

    series_key: str
    provider: str
    observations: Sequence[RawObservation]
    status: Literal["available", "unavailable"]


class ObservationProvider(Protocol):
    """Fixed provider boundary; no request may select an implementation."""

    provider_name: str  # "fmp" | "fred" | "polygon"

    def fetch_series(self, request: SeriesFetchRequest) -> SeriesFetchResult: ...
