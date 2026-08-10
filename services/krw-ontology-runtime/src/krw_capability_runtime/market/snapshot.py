"""One fixed, bounded router for advisory per-ticker market snapshots.

This is deliberately not a plugin runner or a provider-selection surface.
The only current provider is a research-only Yahoo Finance adapter, and the
router returns a compact allow-listed projection with a short per-ticker TTL.
Volatile market values never become filing evidence or strong-claim support.
"""

from __future__ import annotations

import math
import threading
import time
from collections import OrderedDict
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from datetime import UTC, datetime
from typing import Any, Protocol

from pydantic import BaseModel, ConfigDict, Field, field_validator

_MAX_TICKER_LENGTH = 32
_MAX_CACHE_ENTRIES = 128
_AVAILABLE_TTL_SECONDS = 60.0
_UNAVAILABLE_TTL_SECONDS = 15.0
_INFLIGHT_WAIT_SECONDS = 2.0
_MAX_METRIC_ABS = 1.0e18
_METRIC_FIELDS = {
    "last_price",
    "previous_close",
    "market_cap",
    "trailing_pe",
    "forward_pe",
    "price_to_book",
}


class MarketSnapshotRequest(BaseModel):
    """The closed physical and provider-visible input contract."""

    model_config = ConfigDict(extra="forbid", str_strip_whitespace=True)

    ticker: str = Field(
        min_length=1,
        max_length=_MAX_TICKER_LENGTH,
        pattern=r"^[A-Z0-9][A-Z0-9.-]{0,31}$",
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


class MarketSnapshotProvider(Protocol):
    """Fixed provider boundary; no request may select an implementation."""

    source_name: str

    def fetch(self, ticker: str) -> Mapping[str, Any]:
        """Return a raw bounded source projection or raise a private failure."""


@dataclass(frozen=True)
class _CacheEntry:
    expires_at: float
    payload: dict[str, Any]


@dataclass(frozen=True)
class _FetchReservation:
    cached: dict[str, Any] | None
    completed: threading.Event | None
    is_leader: bool


class MarketSnapshotStore:
    """Small process-local LRU store keyed only by canonical ticker.

    It is intentionally ephemeral. Filing-derived ontology data has its own
    durable per-ticker SQLite spine; current prices and multiples must retain
    their observed timestamp instead of pretending to be a release artifact.
    """

    def __init__(self, *, capacity: int = _MAX_CACHE_ENTRIES) -> None:
        if not 1 <= capacity <= _MAX_CACHE_ENTRIES:
            raise ValueError("market snapshot cache capacity is outside the supported bound")
        self._capacity = capacity
        self._entries: OrderedDict[str, _CacheEntry] = OrderedDict()
        self._inflight: dict[str, threading.Event] = {}
        self._lock = threading.RLock()

    def get(self, ticker: str, now: float) -> dict[str, Any] | None:
        with self._lock:
            return self._get_locked(ticker, now)

    def reserve_fetch(self, ticker: str, now: float) -> _FetchReservation:
        """Return one cache hit or elect exactly one source-fetch leader.

        Concurrent readers of the same missing ticker wait on the leader's
        single result instead of multiplying an external Yahoo request.
        """

        with self._lock:
            cached = self._get_locked(ticker, now)
            if cached is not None:
                return _FetchReservation(cached=cached, completed=None, is_leader=False)
            if completed := self._inflight.get(ticker):
                return _FetchReservation(cached=None, completed=completed, is_leader=False)
            completed = threading.Event()
            self._inflight[ticker] = completed
            return _FetchReservation(cached=None, completed=completed, is_leader=True)

    def put(
        self,
        ticker: str,
        payload: Mapping[str, Any],
        *,
        now: float,
        ttl_seconds: float,
    ) -> None:
        with self._lock:
            self._put_locked(ticker, payload, now=now, ttl_seconds=ttl_seconds)

    def complete_fetch(
        self,
        ticker: str,
        payload: Mapping[str, Any],
        *,
        now: float,
        ttl_seconds: float,
    ) -> None:
        """Publish the leader result before waking concurrent waiters."""

        with self._lock:
            self._put_locked(ticker, payload, now=now, ttl_seconds=ttl_seconds)
            completed = self._inflight.pop(ticker, None)
            if completed is not None:
                completed.set()

    def _get_locked(self, ticker: str, now: float) -> dict[str, Any] | None:
        entry = self._entries.get(ticker)
        if entry is None:
            return None
        if entry.expires_at <= now:
            self._entries.pop(ticker, None)
            return None
        self._entries.move_to_end(ticker)
        return dict(entry.payload)

    def _put_locked(
        self,
        ticker: str,
        payload: Mapping[str, Any],
        *,
        now: float,
        ttl_seconds: float,
    ) -> None:
        self._entries[ticker] = _CacheEntry(
            expires_at=now + ttl_seconds,
            payload=dict(payload),
        )
        self._entries.move_to_end(ticker)
        while len(self._entries) > self._capacity:
            self._entries.popitem(last=False)


class YahooFinanceResearchProvider:
    """Research-only, fixed Yahoo Finance adapter loaded only on first use."""

    source_name = "yahoo_finance"

    def fetch(self, ticker: str) -> Mapping[str, Any]:
        # Import lazily so capability-daemon startup and deterministic tests do
        # not depend on the optional network client being initialized.
        import yfinance as yf

        instrument = yf.Ticker(ticker)
        fast_info = _mapping_or_empty(getattr(instrument, "fast_info", None))
        try:
            info = _mapping_or_empty(instrument.get_info())
        except Exception:  # noqa: BLE001 - a price-only response is still useful
            info = {}
        market_time = _first_value(
            (info, fast_info),
            "regularMarketTime",
            "regular_market_time",
            "last_trade_time",
        )
        return {
            "ticker": ticker,
            "source": self.source_name,
            "currency": _first_value((info, fast_info), "currency"),
            "as_of": market_time,
            "metrics": {
                "last_price": _first_value(
                    (info, fast_info),
                    "regularMarketPrice",
                    "regular_market_price",
                    "last_price",
                ),
                "previous_close": _first_value(
                    (info, fast_info),
                    "regularMarketPreviousClose",
                    "regular_market_previous_close",
                    "previous_close",
                ),
                "market_cap": _first_value((info, fast_info), "marketCap", "market_cap"),
                "trailing_pe": _first_value((info, fast_info), "trailingPE", "trailing_pe"),
                "forward_pe": _first_value((info, fast_info), "forwardPE", "forward_pe"),
                "price_to_book": _first_value((info, fast_info), "priceToBook", "price_to_book"),
            },
        }


class MarketSnapshotRouter:
    """Sanitize one source result and serve it through a short TTL cache."""

    def __init__(
        self,
        provider: MarketSnapshotProvider | None = None,
        store: MarketSnapshotStore | None = None,
        *,
        monotonic_clock: Callable[[], float] = time.monotonic,
        utc_now: Callable[[], datetime] | None = None,
    ) -> None:
        self._provider = provider or YahooFinanceResearchProvider()
        self._store = store or MarketSnapshotStore()
        self._monotonic_clock = monotonic_clock
        self._utc_now = utc_now or (lambda: datetime.now(UTC))

    def snapshot(self, ticker: str) -> dict[str, Any]:
        canonical_ticker = MarketSnapshotRequest(ticker=ticker).ticker
        reservation = self._store.reserve_fetch(canonical_ticker, self._monotonic_clock())
        if reservation.cached is not None:
            return reservation.cached
        if not reservation.is_leader:
            assert reservation.completed is not None
            reservation.completed.wait(_INFLIGHT_WAIT_SECONDS)
            cached = self._store.get(canonical_ticker, self._monotonic_clock())
            if cached is not None:
                return cached
            return _unavailable_payload(
                canonical_ticker,
                self._provider.source_name,
                self._utc_now(),
            )

        try:
            raw = self._provider.fetch(canonical_ticker)
            payload = _sanitize_payload(
                canonical_ticker,
                self._provider.source_name,
                raw,
                self._utc_now(),
            )
        except Exception:  # noqa: BLE001 - source details never cross the MCP boundary
            payload = _unavailable_payload(
                canonical_ticker,
                self._provider.source_name,
                self._utc_now(),
            )
        ttl_seconds = (
            _AVAILABLE_TTL_SECONDS if payload["status"] == "available" else _UNAVAILABLE_TTL_SECONDS
        )
        # Start freshness at source completion, not before an unpredictable
        # external round-trip began.
        self._store.complete_fetch(
            canonical_ticker,
            payload,
            now=self._monotonic_clock(),
            ttl_seconds=ttl_seconds,
        )
        return payload


def _mapping_or_empty(value: Any) -> Mapping[str, Any]:
    return value if isinstance(value, Mapping) else {}


def _first_value(sources: tuple[Mapping[str, Any], ...], *keys: str) -> Any:
    for source in sources:
        if not isinstance(source, Mapping):
            continue
        for key in keys:
            value = source.get(key)
            if value is not None:
                return value
    return None


def _normalize_timestamp(value: Any) -> str | None:
    if isinstance(value, datetime):
        timestamp = value.astimezone(UTC)
    elif (
        isinstance(value, (int, float))
        and not isinstance(value, bool)
        and math.isfinite(float(value))
    ):
        try:
            timestamp = datetime.fromtimestamp(float(value), UTC)
        except (OverflowError, OSError, ValueError):
            return None
    else:
        return None
    return timestamp.isoformat(timespec="seconds").replace("+00:00", "Z")


def _safe_currency(value: Any) -> str | None:
    if not isinstance(value, str):
        return None
    value = value.strip().upper()
    if not 1 <= len(value) <= 8 or not value.isascii() or not value.isalpha():
        return None
    return value


def _safe_metric(value: Any) -> float | None:
    if isinstance(value, bool):
        return None
    try:
        number = float(value)
    except (TypeError, ValueError):
        return None
    if not math.isfinite(number) or abs(number) > _MAX_METRIC_ABS:
        return None
    return number


def _iso_now(value: datetime) -> str:
    return value.astimezone(UTC).isoformat(timespec="seconds").replace("+00:00", "Z")


def _sanitize_payload(
    expected_ticker: str,
    source_name: str,
    raw: Mapping[str, Any],
    fetched_at: datetime,
) -> dict[str, Any]:
    if raw.get("ticker") != expected_ticker:
        return _unavailable_payload(expected_ticker, source_name, fetched_at)
    raw_metrics = raw.get("metrics")
    metrics: dict[str, float] = {}
    if isinstance(raw_metrics, Mapping):
        for field in sorted(_METRIC_FIELDS):
            number = _safe_metric(raw_metrics.get(field))
            if number is not None:
                metrics[field] = number
    status = "available" if metrics else "unavailable"
    return {
        "format": "market-snapshot/v1",
        "ticker": expected_ticker,
        "status": status,
        "source": source_name,
        "source_usage": "research_only",
        "fetched_at": _iso_now(fetched_at),
        "as_of": _normalize_timestamp(raw.get("as_of")) if raw.get("as_of") is not None else None,
        "currency": _safe_currency(raw.get("currency")),
        "metrics": metrics,
        "advisory_only": True,
    }


def _unavailable_payload(ticker: str, source_name: str, fetched_at: datetime) -> dict[str, Any]:
    return {
        "format": "market-snapshot/v1",
        "ticker": ticker,
        "status": "unavailable",
        "source": source_name,
        "source_usage": "research_only",
        "fetched_at": _iso_now(fetched_at),
        "as_of": None,
        "currency": None,
        "metrics": {},
        "advisory_only": True,
    }


_DEFAULT_ROUTER = MarketSnapshotRouter()


def market_snapshot_tool(ticker: str) -> dict[str, Any]:
    """Physical MCP handler for the fixed market snapshot router."""

    return _DEFAULT_ROUTER.snapshot(ticker)
