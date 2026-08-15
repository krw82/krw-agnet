"""Presentation-series pack contract tests.

Chart-ready numbers leave the capability through the private MCP ``_meta``
channel (``com.krwontology/presentationSeries``) instead of the model-visible
ResearchState. These tests pin that boundary.
"""

from __future__ import annotations

import asyncio
import sqlite3
from pathlib import Path

import pytest

from krw_capability_runtime.agent_index.chart_series import (
    create_chart_series_schema,
    query_chart_series_pack,
)
from krw_capability_runtime.mcp_server import tools as ontology_tools
from krw_capability_runtime.mcp_server.contracts import ResearchState
from krw_capability_runtime.transport.mcp.descriptors import DispatchOutcome, build_registry

PRESENTATION_META_KEY = "com.krwontology/presentationSeries"


def _create_sidecar(path: Path, *, insert_order: str = "asc") -> None:
    conn = sqlite3.connect(path)
    create_chart_series_schema(conn)
    series_rows = [
        # Deliberately not in canonical sort order so an ORDER BY-less LIMIT
        # would return different candidates depending on insertion order.
        ("AAPL:total_debt", "AAPL", "total_debt", "Total debt", "Total debt", "USD_millions",
         "company_total", "AAPL", "Apple", None, 0, "USD", "annual", "consolidated", "fy",
         "balance_sheet", "balance_sheet", 4, "FY2022", "FY2025", 2022, 2025, "[]"),
        ("AAPL:revenue", "AAPL", "revenue", "Revenue", "Revenue", "USD_millions",
         "company_total", "AAPL", "Apple", None, 0, "USD", "annual", "consolidated", "fy",
         "income_statement", "income_statement", 4, "FY2022", "FY2025", 2022, 2025, "[]"),
        ("MSFT:revenue", "MSFT", "revenue", "Revenue", "Revenue", "USD_millions",
         "company_total", "MSFT", "Microsoft", None, 0, "USD", "annual", "consolidated", "fy",
         "income_statement", "income_statement", 4, "FY2022", "FY2025", 2022, 2025, "[]"),
    ]
    if insert_order == "desc":
        series_rows = list(reversed(series_rows))
    conn.executemany(
        "INSERT INTO chart_series VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        series_rows,
    )
    point_rows = []
    for series_key, ticker in (("AAPL:revenue", "AAPL"), ("MSFT:revenue", "MSFT"),
                               ("AAPL:total_debt", "AAPL")):
        for index, (period, sort_key, value) in enumerate((
            ("FY2022", 2022, 394.3),
            ("FY2023", 2023, 383.3),
            ("FY2024", 2024, 391.0),
            ("FY2025", 2025, 416.2),
        )):
            point_rows.append(
                (series_key, ticker, period, "FY", None, f"{period}-12-31", sort_key, None,
                 sort_key, "10-K", period, f"doc-{series_key}-{index}", sort_key, value,
                 f"{value:.1f}", "USD", f"obj-{ticker}-{index}", "traceable", "complete")
            )
    if insert_order == "desc":
        point_rows = list(reversed(point_rows))
    conn.executemany(
        "INSERT INTO chart_series_points VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        point_rows,
    )
    conn.commit()
    conn.close()


def test_query_chart_series_pack_is_deterministic_and_metadata_rich(tmp_path: Path) -> None:
    ascending = tmp_path / "asc.sqlite"
    descending = tmp_path / "desc.sqlite"
    _create_sidecar(ascending, insert_order="asc")
    _create_sidecar(descending, insert_order="desc")

    kwargs = {
        "tickers": ["AAPL", "MSFT"],
        "release_id": "release-test-1",
        "source_manifest_hash": "sha256:manifest-test",
        "chart_clauses": [
            {
                "clause_id": "revenue_comparison",
                "required": True,
                "tickers": ["AAPL", "MSFT"],
                "metrics": ["revenue"],
                "metric_scope": "company_total",
                "metric_dimensions": [],
                "calculation_window": None,
            }
        ],
    }
    pack_one = query_chart_series_pack(ascending, **kwargs)  # type: ignore[arg-type]
    pack_two = query_chart_series_pack(descending, **kwargs)  # type: ignore[arg-type]

    assert pack_one is not None and pack_two is not None
    assert pack_one == pack_two, "candidate selection must not depend on row insertion order"
    assert pack_one["schema_version"] == 2
    assert pack_one["mode"] == "chart_series_sidecar"
    assert pack_one["release_id"] == "release-test-1"
    assert pack_one["source_manifest_hash"] == "sha256:manifest-test"
    # The sidecar does not predeclare a chart kind. Rust is the single
    # authority that decides trend/comparison/composition from this raw intent.
    assert "chart_kind" not in pack_one["chart_clauses"][0]
    series = pack_one["series"]
    assert [item["canonical_metric"] for item in series] == ["revenue", "revenue"]
    assert {item["ticker"] for item in series} == {"AAPL", "MSFT"}
    first = series[0]
    # Chart-critical metadata must survive to the presentation channel.
    assert first["basis"] == "consolidated"
    assert first["unit"] == "USD_millions"
    assert first["currency"] == "USD"
    assert first["scope"]["kind"] == "company_total"
    assert [point["period"] for point in first["points"]] == [
        "FY2022", "FY2023", "FY2024", "FY2025",
    ]
    assert first["points"][0]["period_basis"] == "FY"
    assert all(point["object_id"] for point in first["points"])


def test_clause_scoped_lookup_does_not_starve_later_ticker_or_metric(
    tmp_path: Path,
) -> None:
    path = tmp_path / "crowded.sqlite"
    _create_sidecar(path)
    conn = sqlite3.connect(path)
    # More than the old global candidate bound for one ticker/metric.  The
    # requested second metric must still be found because lookup is now
    # bounded per ticker/metric pair rather than across the whole query.
    for index in range(200):
        series_key = f"AAPL:revenue:product:segment-{index:03d}"
        conn.execute(
            "INSERT INTO chart_series VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (
                series_key, "AAPL", "revenue", "Revenue", "Revenue", "USD_millions",
                "product", f"segment-{index:03d}", f"Segment {index}", "product", 1,
                "USD", "annual", "consolidated", "fy", "income_statement",
                "income_statement", 1, "FY2025", "FY2025", 2025, 2025, "[]",
            ),
        )
        conn.execute(
            "INSERT INTO chart_series_points VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (
                series_key, "AAPL", "FY2025", "FY", None, "FY2025-12-31", 2025, None,
                2025, "10-K", "FY2025", f"doc-{index}", 2025, float(index + 1),
                str(index + 1), "USD", f"obj-segment-{index}", "traceable", "complete",
            ),
        )
    conn.execute(
        "INSERT INTO chart_series VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        (
            "MSFT:net_income", "MSFT", "net_income", "Net income", "Net income",
            "USD_millions", "company_total", "MSFT", "Microsoft", None, 0, "USD",
            "annual", "consolidated", "fy", "income_statement", "income_statement", 4,
            "FY2022", "FY2025", 2022, 2025, "[]",
        ),
    )
    for index, period in enumerate(("FY2022", "FY2023", "FY2024", "FY2025")):
        conn.execute(
            "INSERT INTO chart_series_points VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (
                "MSFT:net_income", "MSFT", period, "FY", None, f"{period}-12-31", 2022 + index,
                None, 2022 + index, "10-K", period, f"doc-msft-net-{index}", 2022 + index,
                float(50 + index), str(50 + index), "USD", f"obj-msft-net-{index}",
                "traceable", "complete",
            ),
        )
    conn.commit()
    conn.close()

    pack = query_chart_series_pack(
        path,
        tickers=["AAPL", "MSFT"],
        chart_clauses=[
            {
                "clause_id": "revenue_and_income",
                "required": True,
                "tickers": ["AAPL", "MSFT"],
                "metrics": ["revenue", "net_income"],
                "metric_scope": "company_total",
                "metric_dimensions": [],
                "calculation_window": None,
            }
        ],
        limit_series=4,
    )

    assert pack is not None
    assert {item["canonical_metric"] for item in pack["series"]} == {
        "revenue",
        "net_income",
    }
    assert {item["ticker"] for item in pack["series"]} == {"AAPL", "MSFT"}


def test_shared_segment_labels_do_not_collapse_across_tickers(tmp_path: Path) -> None:
    path = tmp_path / "shared-segment.sqlite"
    _create_sidecar(path)
    conn = sqlite3.connect(path)
    for ticker in ("AAPL", "MSFT"):
        series_key = f"{ticker}:revenue:product:other"
        conn.execute(
            "INSERT INTO chart_series VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (
                series_key, ticker, "revenue", "Revenue", "Revenue", "USD_millions",
                "product", "Other", "Other", "product", 1, "USD", "annual",
                "consolidated", "fy", "income_statement", "income_statement", 4,
                "FY2022", "FY2025", 2022, 2025, "[]",
            ),
        )
        for index, period in enumerate(("FY2022", "FY2023", "FY2024", "FY2025")):
            conn.execute(
                "INSERT INTO chart_series_points VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                (
                    series_key, ticker, period, "FY", None, f"{period}-12-31", 2022 + index,
                    None, 2022 + index, "10-K", period, f"doc-{ticker}-other-{index}",
                    2022 + index, float(10 + index), str(10 + index), "USD",
                    f"obj-{ticker}-other-{index}", "traceable", "complete",
                ),
            )
    conn.commit()
    conn.close()

    pack = query_chart_series_pack(
        path,
        tickers=["AAPL", "MSFT"],
        chart_clauses=[
            {
                "clause_id": "revenue_by_product",
                "required": True,
                "tickers": ["AAPL", "MSFT"],
                "metrics": ["revenue"],
                "metric_scope": "dimensioned",
                "metric_dimensions": ["product"],
                "calculation_window": None,
            }
        ],
        limit_series=8,
    )

    assert pack is not None
    shared_segments = [
        (item["ticker"], item["scope"]["key"])
        for item in pack["series"]
        if item["scope"]["key"] == "Other"
    ]
    assert set(shared_segments) == {("AAPL", "Other"), ("MSFT", "Other")}


def test_research_state_no_longer_carries_chart_pack() -> None:
    assert "metric_series_pack" not in ResearchState.model_fields


def test_dispatch_outcome_carries_presentation_meta() -> None:
    pack = {"schema_version": 2, "mode": "chart_series_sidecar", "series": []}
    outcome = DispatchOutcome(
        text="{}",
        structured_content={},
        meta={PRESENTATION_META_KEY: pack},
    )
    result = outcome.as_mcp_result()
    assert result.meta == {PRESENTATION_META_KEY: pack}
    wire = result.model_dump(mode="json", by_alias=True)
    assert wire["_meta"] == {PRESENTATION_META_KEY: pack}
    assert DispatchOutcome(text="{}", structured_content={}).meta is None


def _plan_arguments() -> dict[str, object]:
    return {
        "question": "How durable is ACME revenue?",
        "intent": "company_research",
        "clauses": [
            {
                "clause_id": "revenue_trend",
                "retrieval_query": "ACME revenue trend",
                "required_concepts": ["revenue"],
                "metrics": ["revenue"],
            }
        ],
        "tickers": ["ACME"],
    }


def test_query_context_dispatch_emits_meta_pack_and_keeps_state_clean(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    plan_arguments = _plan_arguments()
    state = ResearchState.model_validate(
        {
            "plan": plan_arguments,
            "resolved_scope": {},
            "answerability": {
                "status": "not_answerable",
                "strong_claim_allowed": False,
                "required_clause_count": 1,
                "covered_required_clause_count": 0,
                "requires_direct_evidence": True,
            },
            "clause_coverage": [],
            "evidence_units": [],
        }
    )
    pack = {"schema_version": 2, "mode": "chart_series_sidecar", "series": []}

    monkeypatch.setattr(
        ontology_tools,
        "query_context_from_search_plan",
        lambda _plan: ontology_tools.QueryContextResult(
            state=state,
            presentation_pack=pack,
        ),
    )
    registry = build_registry()
    outcome = asyncio.run(registry.dispatch("krw_ontology_query_context", plan_arguments))

    assert outcome.meta is not None
    assert outcome.meta[PRESENTATION_META_KEY] == pack
    # The model-visible payload carries no chart data of any spelling.
    dumped = str(outcome.structuredContent)
    assert "metric_series_pack" not in dumped
    assert "chart_series_pack" not in dumped
    assert "presentation" not in dumped


def test_sidecar_attach_is_available_when_the_sealed_sidecar_exists(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    indexes_dir = tmp_path / "indexes"
    indexes_dir.mkdir()
    sidecar = indexes_dir / "chart_series.sqlite"
    _create_sidecar(sidecar)
    monkeypatch.setattr(ontology_tools, "_runtime_root", lambda: tmp_path)
    # Production defaults to presentation-off. This test explicitly opts into
    # the dormant sidecar path so it continues to cover chart compilation.
    monkeypatch.setenv("KRW_CHART_SERIES_ENABLED", "1")

    raw_payload: dict[str, object] = {}
    ontology_tools._attach_chart_series_sidecar_to_search_plan_payload(
        raw_payload=raw_payload,
        requested_tickers=["AAPL"],
        chart_clauses=[
            {
                "clause_id": "revenue_trend",
                "required": True,
                "tickers": ["AAPL"],
                "metrics": ["revenue"],
                "metric_scope": "company_total",
                "metric_dimensions": [],
                "calculation_window": "year_over_year",
            }
        ],
    )

    presentation = raw_payload.get("presentation_series_pack")
    assert isinstance(presentation, dict)
    assert presentation["schema_version"] == 2
    research_pack = raw_payload.get("research_pack")
    assert not (isinstance(research_pack, dict) and "metric_series_pack" in research_pack)
    diagnostics = raw_payload["search_diagnostics"]["chart_series"]
    assert diagnostics["matched"] is True


def test_sidecar_attach_is_disabled_when_presentation_is_off(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    monkeypatch.setenv("KRW_CHART_SERIES_ENABLED", "0")
    monkeypatch.setattr(ontology_tools, "_runtime_root", lambda: tmp_path)
    raw_payload: dict[str, object] = {}

    ontology_tools._attach_chart_series_sidecar_to_search_plan_payload(
        raw_payload=raw_payload,
        requested_tickers=["AAPL"],
        chart_clauses=[],
    )

    assert "presentation_series_pack" not in raw_payload
    assert "search_diagnostics" not in raw_payload
