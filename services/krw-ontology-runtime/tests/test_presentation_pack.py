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
         "company_total", "AAPL", "Apple", "annual", "consolidated", "fy", "balance_sheet",
         "balance_sheet", 4, "FY2022", "FY2025", 2022, 2025, "[]"),
        ("AAPL:revenue", "AAPL", "revenue", "Revenue", "Revenue", "USD_millions",
         "company_total", "AAPL", "Apple", "annual", "consolidated", "fy", "income_statement",
         "income_statement", 4, "FY2022", "FY2025", 2022, 2025, "[]"),
        ("MSFT:revenue", "MSFT", "revenue", "Revenue", "Revenue", "USD_millions",
         "company_total", "MSFT", "Microsoft", "annual", "consolidated", "fy", "income_statement",
         "income_statement", 4, "FY2022", "FY2025", 2022, 2025, "[]"),
    ]
    if insert_order == "desc":
        series_rows = list(reversed(series_rows))
    conn.executemany(
        "INSERT INTO chart_series VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
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
                (series_key, ticker, period, sort_key, None, sort_key, "10-K", period,
                 f"doc-{series_key}-{index}", sort_key, value, f"{value:.1f}",
                 f"obj-{ticker}-{index}", "traceable", "complete")
            )
    if insert_order == "desc":
        point_rows = list(reversed(point_rows))
    conn.executemany(
        "INSERT INTO chart_series_points VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
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
        "question": "compare revenue",
        "tickers": ["AAPL", "MSFT"],
        "metric_names": ["revenue"],
    }
    pack_one = query_chart_series_pack(ascending, **kwargs)  # type: ignore[arg-type]
    pack_two = query_chart_series_pack(descending, **kwargs)  # type: ignore[arg-type]

    assert pack_one is not None and pack_two is not None
    assert pack_one == pack_two, "candidate selection must not depend on row insertion order"
    assert pack_one["schema_version"] == 1
    assert pack_one["mode"] == "chart_series_sidecar"
    series = pack_one["series"]
    assert [item["canonical_metric"] for item in series] == ["revenue", "revenue"]
    assert {item["ticker"] for item in series} == {"AAPL", "MSFT"}
    first = series[0]
    # Chart-critical metadata must survive to the presentation channel.
    assert first["basis"] == "consolidated"
    assert first["unit"] == "USD_millions"
    assert first["scope"]["kind"] == "company_total"
    assert [point["period"] for point in first["points"]] == [
        "FY2022", "FY2023", "FY2024", "FY2025",
    ]
    assert all(point["object_id"] for point in first["points"])


def test_research_state_no_longer_carries_chart_pack() -> None:
    assert "metric_series_pack" not in ResearchState.model_fields


def test_dispatch_outcome_carries_presentation_meta() -> None:
    pack = {"schema_version": 1, "mode": "chart_series_sidecar", "series": []}
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
    pack = {"schema_version": 1, "mode": "chart_series_sidecar", "series": []}

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


def test_sidecar_attach_is_not_keyword_gated(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    indexes_dir = tmp_path / "indexes"
    indexes_dir.mkdir()
    sidecar = indexes_dir / "chart_series.sqlite"
    _create_sidecar(sidecar)
    monkeypatch.setattr(ontology_tools, "_chart_series_runtime_enabled", lambda: True)
    monkeypatch.setattr(ontology_tools, "_runtime_root", lambda: tmp_path)

    raw_payload: dict[str, object] = {}
    # A question with no metric keywords still receives the verified pack when
    # the plan names tickers; chart-worthiness is decided by the presentation
    # compiler, not by question routing.
    ontology_tools._attach_chart_series_sidecar_to_search_plan_payload(
        raw_payload=raw_payload,
        question="Tell me about this business",
        requested_tickers=["AAPL"],
        metric_names=["revenue"],
    )

    presentation = raw_payload.get("presentation_series_pack")
    assert isinstance(presentation, dict)
    assert presentation["schema_version"] == 1
    research_pack = raw_payload.get("research_pack")
    assert not (isinstance(research_pack, dict) and "metric_series_pack" in research_pack)
    diagnostics = raw_payload["search_diagnostics"]["chart_series"]
    assert diagnostics["matched"] is True
