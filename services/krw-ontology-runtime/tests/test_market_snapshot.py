from __future__ import annotations

import threading
from datetime import UTC, datetime

import pytest

from krw_capability_runtime.market.snapshot import (
    MarketSnapshotRequest,
    MarketSnapshotRouter,
    MarketSnapshotStore,
)


class _FakeProvider:
    source_name = "yahoo_finance"

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
        "source": "yahoo_finance",
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
