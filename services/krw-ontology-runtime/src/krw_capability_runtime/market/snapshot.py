"""One fixed, bounded router for advisory per-ticker market snapshots.

This is deliberately not a plugin runner or a provider-selection surface.
The router owns one research-only FMP adapter and a compact per-ticker cache;
the model sees only its allow-listed projection. Volatile market values never
become filing evidence or strong-claim support.
"""

from __future__ import annotations

import json
import math
import os
import threading
import time
from collections import OrderedDict
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from datetime import UTC, datetime
from typing import Any, Protocol
from urllib.parse import urlencode, urlsplit
from urllib.request import HTTPRedirectHandler, Request, build_opener

from pydantic import BaseModel, ConfigDict, Field, field_validator

_MAX_TICKER_LENGTH = 32
_MAX_CACHE_ENTRIES = 128
_AVAILABLE_TTL_SECONDS = 60.0
_UNAVAILABLE_TTL_SECONDS = 15.0
_INFLIGHT_WAIT_SECONDS = 2.0
_MAX_METRIC_ABS = 1.0e18
_FMP_BASE_URL = "https://financialmodelingprep.com/stable"
_FMP_TIMEOUT_SECONDS = 1.25
_MAX_FMP_RESPONSE_BYTES = 128 * 1024
_MARKET_STORE_URL_ENV = "KRW_MARKET_SNAPSHOT_STORE_URL"
_MARKET_STORE_KEY_ENV = "KRW_MARKET_SNAPSHOT_STORE_SERVICE_ROLE_KEY"
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
        single result instead of multiplying an external FMP request.
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


class _RejectRedirects(HTTPRedirectHandler):
    """Keep the FMP key on the fixed HTTPS origin even if upstream redirects."""

    def redirect_request(self, *args: Any, **kwargs: Any) -> None:  # type: ignore[override]
        return None


def _open_fmp_request(request: Request, *, timeout: float) -> Any:
    return build_opener(_RejectRedirects()).open(request, timeout=timeout)


class FmpValuationStoreProvider:
    """Read the existing FMP-backed valuation store through one fixed query.

    The caller cannot select a table, column, ticker syntax, or query option.
    This is the preferred source because the frontend refresh job already
    normalizes FMP values per ticker; direct FMP is only a miss fallback.
    """

    source_name = "fmp"

    def __init__(
        self,
        *,
        store_url: str | None = None,
        service_role_key: str | None = None,
        opener: Callable[..., Any] = _open_fmp_request,
        timeout_seconds: float = _FMP_TIMEOUT_SECONDS,
    ) -> None:
        if not 0 < timeout_seconds <= 3.0:
            raise ValueError("market store timeout must be within 0..3 seconds")
        raw_url = (
            store_url if store_url is not None else os.getenv(_MARKET_STORE_URL_ENV, "")
        ).strip()
        parsed = urlsplit(raw_url)
        if raw_url and (
            parsed.scheme != "https"
            or not parsed.netloc
            or parsed.username is not None
            or parsed.password is not None
            or parsed.path not in ("", "/")
            or parsed.query
            or parsed.fragment
        ):
            raise ValueError("market store URL must be a bare HTTPS origin")
        self._store_url = raw_url.rstrip("/")
        self._service_role_key = (
            service_role_key
            if service_role_key is not None
            else os.getenv(_MARKET_STORE_KEY_ENV, "")
        ).strip()
        self._opener = opener
        self._timeout_seconds = timeout_seconds

    def fetch(self, ticker: str) -> Mapping[str, Any]:
        if not self._store_url or not self._service_role_key:
            raise RuntimeError("FMP valuation store provider is not configured")
        query = urlencode(
            {
                "select": "ticker,reference_price,currency,market_cap,pe_ttm,pb,computed_at,session_date",
                "ticker": f"eq.{ticker}",
                "order": "session_date.desc,computed_at.desc",
                "limit": "1",
            }
        )
        request = Request(
            f"{self._store_url}/rest/v1/valuation_snapshots?{query}",
            headers={
                "Accept": "application/json",
                "apikey": self._service_role_key,
                "Authorization": f"Bearer {self._service_role_key}",
            },
            method="GET",
        )
        with self._opener(request, timeout=self._timeout_seconds) as response:
            body = response.read(_MAX_FMP_RESPONSE_BYTES + 1)
        if not isinstance(body, bytes) or len(body) > _MAX_FMP_RESPONSE_BYTES:
            raise ValueError("market store response exceeds the bounded snapshot payload")
        decoded = json.loads(body.decode("utf-8"))
        if not isinstance(decoded, list) or not decoded or not isinstance(decoded[0], Mapping):
            raise ValueError("market store response has no usable valuation record")
        row = decoded[0]
        return {
            "ticker": row.get("ticker"),
            "source": self.source_name,
            "currency": row.get("currency"),
            "as_of": row.get("computed_at"),
            "metrics": {
                "last_price": row.get("reference_price"),
                "market_cap": row.get("market_cap"),
                "trailing_pe": row.get("pe_ttm"),
                "price_to_book": row.get("pb"),
            },
        }


class FmpResearchProvider:
    """Research-only FMP adapter with a closed endpoint and field allow-list.

    `quote` is the authoritative current-price source. `ratios-ttm` adds
    trailing P/E and P/B only when it is available; a ratio outage must not
    erase a valid current quote. FMP does not provide a directly labelled
    forward P/E in this endpoint family, so this adapter intentionally never
    infers one from PEG or another derived metric.
    """

    source_name = "fmp"

    def __init__(
        self,
        *,
        api_key: str | None = None,
        opener: Callable[..., Any] = _open_fmp_request,
        timeout_seconds: float = _FMP_TIMEOUT_SECONDS,
    ) -> None:
        if not 0 < timeout_seconds <= 3.0:
            raise ValueError("FMP timeout must be within 0..3 seconds")
        self._api_key = (api_key if api_key is not None else os.getenv("FMP_API_KEY", "")).strip()
        self._opener = opener
        self._timeout_seconds = timeout_seconds

    def fetch(self, ticker: str) -> Mapping[str, Any]:
        quote = self._fetch_first_record("quote", ticker)
        ratios = self._optional_first_record("ratios-ttm", ticker)
        if ratios and ratios.get("symbol") not in (None, ticker):
            ratios = {}
        return {
            "ticker": quote.get("symbol"),
            "source": self.source_name,
            # The stable quote response does not guarantee a currency field;
            # keep it absent rather than assuming USD for every ticker.
            "currency": quote.get("currency"),
            "as_of": quote.get("timestamp"),
            "metrics": {
                "last_price": quote.get("price"),
                "previous_close": quote.get("previousClose"),
                "market_cap": quote.get("marketCap"),
                "trailing_pe": ratios.get("priceToEarningsRatioTTM"),
                "price_to_book": ratios.get("priceToBookRatioTTM"),
            },
        }

    def _optional_first_record(self, endpoint: str, ticker: str) -> Mapping[str, Any]:
        try:
            return self._fetch_first_record(endpoint, ticker)
        except Exception:  # noqa: BLE001 - the quote remains usable without valuation ratios
            return {}

    def _fetch_first_record(self, endpoint: str, ticker: str) -> Mapping[str, Any]:
        if not self._api_key:
            raise RuntimeError("FMP market snapshot provider is not configured")
        query = urlencode({"symbol": ticker, "apikey": self._api_key})
        request = Request(
            f"{_FMP_BASE_URL}/{endpoint}?{query}",
            headers={"Accept": "application/json", "Accept-Encoding": "gzip, deflate"},
            method="GET",
        )
        with self._opener(request, timeout=self._timeout_seconds) as response:
            body = response.read(_MAX_FMP_RESPONSE_BYTES + 1)
        if not isinstance(body, bytes) or len(body) > _MAX_FMP_RESPONSE_BYTES:
            raise ValueError("FMP response exceeds the bounded market snapshot payload")
        decoded = json.loads(body.decode("utf-8"))
        if not isinstance(decoded, list) or not decoded or not isinstance(decoded[0], Mapping):
            raise ValueError("FMP response has no usable market record")
        return decoded[0]


class MarketSnapshotRouter:
    """Sanitize one source result and serve it through a short TTL cache."""

    def __init__(
        self,
        provider: MarketSnapshotProvider | None = None,
        store: MarketSnapshotStore | None = None,
        *,
        fallback_provider: MarketSnapshotProvider | None = None,
        monotonic_clock: Callable[[], float] = time.monotonic,
        utc_now: Callable[[], datetime] | None = None,
    ) -> None:
        if provider is not None:
            providers = (provider,) if fallback_provider is None else (provider, fallback_provider)
        else:
            providers = (FmpValuationStoreProvider(), FmpResearchProvider())
        if not providers or any(provider.source_name != "fmp" for provider in providers):
            raise ValueError("market snapshot providers must preserve FMP provenance")
        self._providers = providers
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
                self._providers[0].source_name,
                self._utc_now(),
            )

        payload = _unavailable_payload(
            canonical_ticker,
            self._providers[0].source_name,
            self._utc_now(),
        )
        for provider in self._providers:
            candidate: dict[str, Any] | None = None
            try:
                raw = provider.fetch(canonical_ticker)
                candidate = _sanitize_payload(
                    canonical_ticker,
                    provider.source_name,
                    raw,
                    self._utc_now(),
                )
            except Exception:  # noqa: BLE001 - source details never cross the MCP boundary
                candidate = None
            if candidate is not None and candidate["status"] == "available":
                payload = candidate
                break
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


def _normalize_timestamp(value: Any) -> str | None:
    if isinstance(value, datetime):
        timestamp = value.astimezone(UTC)
    elif isinstance(value, str) and 1 <= len(value) <= 64 and value.isascii():
        try:
            timestamp = datetime.fromisoformat(value)
        except ValueError:
            return None
        if timestamp.tzinfo is None:
            return None
        timestamp = timestamp.astimezone(UTC)
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
