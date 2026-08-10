from __future__ import annotations

import json
import threading
from datetime import UTC, datetime
from typing import Self
from urllib.request import Request

import pytest

from krw_capability_runtime.market.snapshot import (
    FmpResearchProvider,
    FmpValuationStoreProvider,
    MarketSnapshotRequest,
    MarketSnapshotRouter,
    MarketSnapshotStore,
)


class _FakeProvider:
    source_name = "fmp"

    def __init__(self, payload: dict[str, object] | None = None, *, fail: bool = False) -> None:
        self.payload = payload or {}
        self.fail = fail
        self.calls = 0

    def fetch(self, ticker: str) -> dict[str, object]:
        self.calls += 1
        if self.fail:
            raise RuntimeError("private upstream diagnostic")
        return {"ticker": ticker, **self.payload}


def test_snapshot_is_allow_list_sanitized_and_cached_per_ticker() -> None:
    now = [10.0]
    provider = _FakeProvider(
        {
            "currency": "usd",
            "as_of": 1_704_067_200,
            "metrics": {
                "last_price": 42.25,
                "trailing_pe": "18.5",
                "private_debug": "must not cross the boundary",
                "market_cap": float("nan"),
            },
            "router_path": "/private/market.db",
        }
    )
    router = MarketSnapshotRouter(
        provider,
        MarketSnapshotStore(),
        monotonic_clock=lambda: now[0],
        utc_now=lambda: datetime(2024, 1, 1, tzinfo=UTC),
    )

    first = router.snapshot("VG")
    now[0] += 5.0
    second = router.snapshot("VG")

    assert provider.calls == 1
    assert first == second
    assert first == {
        "format": "market-snapshot/v1",
        "ticker": "VG",
        "status": "available",
        "source": "fmp",
        "source_usage": "research_only",
        "fetched_at": "2024-01-01T00:00:00Z",
        "as_of": "2024-01-01T00:00:00Z",
        "currency": "USD",
        "metrics": {"last_price": 42.25, "trailing_pe": 18.5},
        "advisory_only": True,
    }
    assert "private" not in str(first)


def test_source_failure_returns_a_bounded_unavailable_control_result() -> None:
    router = MarketSnapshotRouter(
        _FakeProvider(fail=True),
        utc_now=lambda: datetime(2024, 1, 1, tzinfo=UTC),
    )

    result = router.snapshot("AAPL")

    assert result["status"] == "unavailable"
    assert result["metrics"] == {}
    assert result["advisory_only"] is True
    assert "private upstream diagnostic" not in str(result)


class _FakeFmpResponse:
    def __init__(self, payload: object) -> None:
        self._body = json.dumps(payload).encode("utf-8")

    def __enter__(self) -> Self:
        return self

    def __exit__(self, *_: object) -> None:
        return None

    def read(self, _: int) -> bytes:
        return self._body


def test_fmp_provider_combines_quote_with_ttm_valuation_metrics() -> None:
    requested_paths: list[str] = []

    def opener(request: Request, *, timeout: float) -> _FakeFmpResponse:
        assert timeout == 1.25
        path = request.full_url.split("?")[0]
        requested_paths.append(path)
        if path.endswith("/quote"):
            return _FakeFmpResponse(
                [
                    {
                        "symbol": "AAPL",
                        "price": 210.5,
                        "previousClose": 212.0,
                        "marketCap": 3_100_000_000_000,
                        "timestamp": 1_704_067_200,
                    }
                ]
            )
        if path.endswith("/ratios-ttm"):
            return _FakeFmpResponse(
                [
                    {
                        "symbol": "AAPL",
                        "priceToEarningsRatioTTM": 31.2,
                        "priceToBookRatioTTM": 45.4,
                    }
                ]
            )
        raise AssertionError(f"unexpected FMP endpoint: {path}")

    payload = FmpResearchProvider(api_key="test-key", opener=opener).fetch("AAPL")

    assert requested_paths == [
        "https://financialmodelingprep.com/stable/quote",
        "https://financialmodelingprep.com/stable/ratios-ttm",
    ]
    assert payload == {
        "ticker": "AAPL",
        "source": "fmp",
        "currency": None,
        "as_of": 1_704_067_200,
        "metrics": {
            "last_price": 210.5,
            "previous_close": 212.0,
            "market_cap": 3_100_000_000_000,
            "trailing_pe": 31.2,
            "price_to_book": 45.4,
        },
    }


def test_fmp_ratio_failure_keeps_a_valid_quote_but_never_uses_another_source() -> None:
    def opener(request: Request, *, timeout: float) -> _FakeFmpResponse:
        del timeout
        if request.full_url.split("?")[0].endswith("/quote"):
            return _FakeFmpResponse([{"symbol": "AAPL", "price": 210.5}])
        raise OSError("private FMP ratio failure")

    router = MarketSnapshotRouter(
        FmpResearchProvider(api_key="test-key", opener=opener),
        utc_now=lambda: datetime(2024, 1, 1, tzinfo=UTC),
    )

    payload = router.snapshot("AAPL")

    assert payload["source"] == "fmp"
    assert payload["status"] == "available"
    assert payload["metrics"] == {"last_price": 210.5}


def test_router_prefers_persisted_fmp_valuation_before_direct_fmp() -> None:
    requested_paths: list[str] = []

    def store_opener(request: Request, *, timeout: float) -> _FakeFmpResponse:
        assert timeout == 1.25
        requested_paths.append(request.full_url.split("?")[0])
        assert "apikey=" not in request.full_url
        return _FakeFmpResponse(
            [
                {
                    "ticker": "AAPL",
                    "reference_price": 210.5,
                    "currency": "USD",
                    "market_cap": 3_100_000_000_000,
                    "pe_ttm": 31.2,
                    "pb": 45.4,
                    "computed_at": "2026-08-07T20:00:00+00:00",
                    "session_date": "2026-08-07",
                }
            ]
        )

    direct = _FakeProvider({"metrics": {"last_price": 999.0}})
    router = MarketSnapshotRouter(
        FmpValuationStoreProvider(
            store_url="https://example.supabase.co",
            service_role_key="test-store-key",
            opener=store_opener,
        ),
        fallback_provider=direct,
        utc_now=lambda: datetime(2026, 8, 10, tzinfo=UTC),
    )

    payload = router.snapshot("AAPL")

    assert requested_paths == ["https://example.supabase.co/rest/v1/valuation_snapshots"]
    assert direct.calls == 0
    assert payload["source"] == "fmp"
    assert payload["as_of"] == "2026-08-07T20:00:00Z"
    assert payload["metrics"] == {
        "last_price": 210.5,
        "market_cap": 3_100_000_000_000.0,
        "trailing_pe": 31.2,
        "price_to_book": 45.4,
    }


def test_router_falls_back_to_direct_fmp_when_the_persisted_snapshot_is_unavailable() -> None:
    persisted = _FakeProvider(fail=True)
    direct = _FakeProvider({"metrics": {"last_price": 210.5}})
    router = MarketSnapshotRouter(
        persisted,
        fallback_provider=direct,
        utc_now=lambda: datetime(2024, 1, 1, tzinfo=UTC),
    )

    payload = router.snapshot("AAPL")

    assert persisted.calls == 1
    assert direct.calls == 1
    assert payload["status"] == "available"
    assert payload["metrics"] == {"last_price": 210.5}


def test_valuation_store_allows_only_https_or_loopback_http() -> None:
    FmpValuationStoreProvider(
        store_url="http://127.0.0.1:54321",
        service_role_key="test-store-key",
    )
    with pytest.raises(ValueError):
        FmpValuationStoreProvider(
            store_url="http://market.example.test",
            service_role_key="test-store-key",
        )


def test_cache_ttl_starts_after_the_source_fetch_completes() -> None:
    now = [10.0]

    class _SlowClockProvider(_FakeProvider):
        def fetch(self, ticker: str) -> dict[str, object]:
            now[0] += 30.0
            return super().fetch(ticker)

    provider = _SlowClockProvider(
        {
            "currency": "USD",
            "metrics": {"last_price": 42.25},
        }
    )
    router = MarketSnapshotRouter(
        provider,
        MarketSnapshotStore(),
        monotonic_clock=lambda: now[0],
        utc_now=lambda: datetime(2024, 1, 1, tzinfo=UTC),
    )

    router.snapshot("VG")
    now[0] = 95.0

    assert router.snapshot("VG")["status"] == "available"
    assert provider.calls == 1


def test_concurrent_cache_misses_share_one_source_fetch() -> None:
    class _BlockingProvider(_FakeProvider):
        def __init__(self) -> None:
            super().__init__({"metrics": {"last_price": 42.25}})
            self.entered = threading.Event()
            self.release = threading.Event()

        def fetch(self, ticker: str) -> dict[str, object]:
            self.calls += 1
            self.entered.set()
            assert self.release.wait(timeout=1.0)
            return {"ticker": ticker, **self.payload}

    provider = _BlockingProvider()
    router = MarketSnapshotRouter(
        provider,
        MarketSnapshotStore(),
        utc_now=lambda: datetime(2024, 1, 1, tzinfo=UTC),
    )
    results: list[dict[str, object]] = []
    errors: list[Exception] = []
    start = threading.Barrier(3)

    def call_snapshot() -> None:
        try:
            start.wait(timeout=1.0)
            results.append(router.snapshot("VG"))
        except (AssertionError, threading.BrokenBarrierError) as error:
            errors.append(error)

    first = threading.Thread(target=call_snapshot)
    second = threading.Thread(target=call_snapshot)
    first.start()
    second.start()
    start.wait(timeout=1.0)
    assert provider.entered.wait(timeout=1.0)
    provider.release.set()
    first.join(timeout=1.0)
    second.join(timeout=1.0)

    assert not first.is_alive()
    assert not second.is_alive()
    assert not errors
    assert provider.calls == 1
    assert [result["status"] for result in results] == ["available", "available"]


@pytest.mark.parametrize("ticker", ["aapl", "AAPL/../../", "", "AAPL_"])
def test_request_rejects_noncanonical_tickers(ticker: str) -> None:
    with pytest.raises(ValueError):
        MarketSnapshotRequest(ticker=ticker)
