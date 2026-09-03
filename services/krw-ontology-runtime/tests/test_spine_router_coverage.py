"""SpineRouter coverage guards: uncovered and missing-shard tickers.

P7 live finding (2026-09-03): a run for a ticker the catalog never declared
died at turn zero — ``company_context`` raised ``KeyError: ticker shard not
found: VIPS`` in the capability handler, so the run never reached the
observation plane (openbb.filings FMP fallback). These tests pin the honest
payloads that keep such runs alive.
"""

from __future__ import annotations

import json
import sqlite3
from pathlib import Path

import pytest

from krw_capability_runtime.agent_index.spine_router import OntologySpineRouter


@pytest.fixture()
def release(tmp_path: Path) -> Path:
    """Synthetic v3 release: one declared+present shard (AAA), one declared
    but missing shard (ZZZ), and nothing else."""

    index_dir = tmp_path / "indexes"
    companies_dir = index_dir / "companies"
    companies_dir.mkdir(parents=True)
    # Empty sqlite files suffice: the uncovered/missing paths never open a
    # shard store, and the spine connection is only touched lazily.
    spine = index_dir / "global_spine.sqlite"
    spine.write_bytes(_empty_sqlite())
    aaa = companies_dir / "aaa.sqlite"
    aaa.write_bytes(_empty_sqlite())
    (index_dir / "shard_manifest.json").write_text(
        json.dumps(
            {
                "shards": {
                    "AAA": {"path": "companies/aaa.sqlite"},
                    "ZZZ": {"path": "companies/zzz.sqlite"},
                }
            }
        ),
        encoding="utf-8",
    )
    return tmp_path


def _empty_sqlite() -> bytes:
    conn = sqlite3.connect(":memory:")
    dump = conn.serialize()
    conn.close()
    return bytes(dump)


def test_company_context_for_uncovered_ticker_returns_honest_payload(release: Path) -> None:
    router = OntologySpineRouter(release / "indexes" / "global_spine.sqlite")
    payload = router.company_context(ticker="vips")
    assert payload["error"]["code"] == "ticker_not_covered"
    assert payload["ticker"] == "VIPS"
    assert payload["missing_parts"] == ["ticker_not_covered"]
    assert payload["fallback_used"] is False
    # The routing block names the ticker as unknown — the run can branch on
    # this instead of dying.
    assert payload["routing"]["unknown_tickers"] == ["VIPS"]


def test_topic_map_for_uncovered_ticker_returns_empty_topics(release: Path) -> None:
    router = OntologySpineRouter(release / "indexes" / "global_spine.sqlite")
    payload = router.topic_map(ticker="GRAB")
    assert payload["error"]["code"] == "ticker_not_covered"
    assert payload["topics"] == []


def test_missing_shard_behavior_is_unchanged(release: Path) -> None:
    router = OntologySpineRouter(release / "indexes" / "global_spine.sqlite")
    payload = router.company_context(ticker="ZZZ")
    assert payload["error"]["code"] == "ticker_shard_missing"
    assert "expected_shard_path" in payload["error"]["details"]


def test_not_covered_is_distinct_from_missing_shard(release: Path) -> None:
    router = OntologySpineRouter(release / "indexes" / "global_spine.sqlite")
    uncovered = router.company_context(ticker="VIPS")
    missing = router.company_context(ticker="ZZZ")
    assert uncovered["error"]["code"] != missing["error"]["code"]
    assert set(uncovered.keys()) <= set(missing.keys()) | {"missing_shards"}
