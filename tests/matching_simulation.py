#!/usr/bin/env python3
"""Layer-1 matching simulation: diverse question types against the real ontology.

Runs without DeepSeek. Each scenario calls query_context_tool directly with
a hand-authored SearchPlan and reports answerability, coverage, and evidence
counts so we can see which clause types match and which still fail.

Usage:
  cd services/krw-ontology-runtime
  KRW_ONTOLOGY_ENV=prod \
  KRW_ONTOLOGY_RELEASE_ROOT=~/krw-ontology-data/releases/prod/current \
  .venv/bin/python ../../tests/matching_simulation.py
"""

from __future__ import annotations

import json
import sys
import traceback
from dataclasses import dataclass, field


@dataclass
class ScenarioResult:
    name: str
    description: str
    status: str = "?"
    covered: str = "?/?"
    evidence_count: int = 0
    clause_details: list[str] = field(default_factory=list)
    evidence_sample: list[str] = field(default_factory=list)
    error: str | None = None


def run_scenario(name: str, description: str, search_plan: dict) -> ScenarioResult:
    """Run one SearchPlan against the ontology and collect results."""
    from krw_capability_runtime.mcp_server.tools import query_context_tool

    result = ScenarioResult(name=name, description=description)
    try:
        state = query_context_tool(search_plan=search_plan)
        ans = state.answerability
        result.status = ans.status or "?"
        result.covered = f"{ans.covered_required_clause_count}/{ans.required_clause_count}"
        result.evidence_count = len(state.evidence_units)
        for c in state.clause_coverage:
            result.clause_details.append(
                f"  {c.clause_id[:35]}: {c.status} ev={len(c.evidence_ids)}"
            )
        for e in state.evidence_units[:3]:
            title = (e.title or "")[:40]
            result.evidence_sample.append(
                f"  {e.evidence_id} type={e.object_type} dir={e.directness} [{title}]"
            )
    except Exception as exc:
        result.error = f"{type(exc).__name__}: {exc}"
        traceback.print_exc()
    return result


def base_plan(
    question: str,
    intent: str,
    clauses: list[dict],
    *,
    tickers: list[str] | None = None,
    periods: list[str] | None = None,
    document_types: list[str] | None = None,
) -> dict:
    return {
        "question": question,
        "intent": intent,
        "tickers": tickers or ["AAPL"],
        "document_types": document_types or ["10-K"],
        "periods": periods or ["FY2023", "FY2024"],
        "answer_scope": "direct",
        "uncertainty": "medium",
        "clauses": clauses,
    }


def clause(
    cid: str,
    query: str,
    *,
    concepts: list[str] | None = None,
    predicates: list[str] | None = None,
    directness: str = "direct_preferred",
) -> dict:
    c = {
        "clause_id": cid,
        "retrieval_query": query,
        "required": True,
        "directness": directness,
    }
    if concepts is not None:
        c["required_concepts"] = concepts
    if predicates is not None:
        c["required_predicates"] = predicates
    return c


# ---------------------------------------------------------------------------
# Scenarios
# ---------------------------------------------------------------------------

SCENARIOS: list[tuple[str, str, dict]] = []


def scenario(name: str, description: str):
    def decorator(fn):
        SCENARIOS.append((name, description, fn()))
        return fn
    return decorator


@scenario("A_iphone_revenue", "iPhone segment revenue trend")
def _():
    return base_plan(
        "AAPL iPhone revenue trend",
        "iphone_revenue",
        [clause("iphone_rev", "AAPL iPhone revenue net sales segment revenue", concepts=["revenue"])],
    )


@scenario("B_gross_margin", "Gross margin trend across years")
def _():
    return base_plan(
        "AAPL gross margin trend",
        "gross_margin_trend",
        [clause("gm", "AAPL gross margin profitability gross margin", concepts=["gross margin"])],
    )


@scenario("C_causal_services_margin", "Services revenue impact on overall margin (causal)")
def _():
    return base_plan(
        "AAPL services revenue impact on overall margin",
        "services_margin_impact",
        [
            clause("svc_rev", "AAPL Services revenue net sales segment revenue", concepts=["revenue"]),
            clause(
                "svc_margin",
                "AAPL Services gross margin higher margin drives contributes",
                concepts=["gross margin"],
                predicates=["higher"],
            ),
        ],
    )


@scenario("D_qualitative_risk", "App Store regulation risk (no metric)")
def _():
    return base_plan(
        "AAPL App Store regulation risk",
        "app_store_regulation",
        [
            clause(
                "appstore_risk",
                "AAPL App Store regulation antitrust compliance legal risk",
                concepts=["App Store", "regulation"],
                predicates=["risk"],
            )
        ],
        periods=["FY2024", "FY2025"],
    )


@scenario("E_segment_comparison", "Services vs Products revenue comparison")
def _():
    return base_plan(
        "AAPL Services versus Products revenue comparison",
        "segment_comparison",
        [
            clause("svc", "AAPL Services revenue net sales", concepts=["revenue"]),
            clause("prod", "AAPL Products revenue net sales total products", concepts=["revenue"]),
        ],
    )


@scenario("F_invalid_metric", "Non-existent metric identifier (should fail validation)")
def _():
    return base_plan(
        "AAPL non-existent metric",
        "invalid_test",
        [clause("bad", "AAPL free_cash_flow_margin_xyz", concepts=["free_cash_flow_margin_xyz"])],
    )


@scenario("G_predicate_matching", "Clause with required_predicates")
def _():
    return base_plan(
        "AAPL services growth drivers",
        "growth_drivers",
        [
            clause(
                "drivers",
                "AAPL services revenue grew increased driven demand expansion",
                concepts=["services", "revenue"],
                predicates=["grew", "driven"],
            )
        ],
    )


@scenario("H_multi_clause", "Five simultaneous clauses (mix of metric + qualitative)")
def _():
    return base_plan(
        "AAPL comprehensive services analysis",
        "comprehensive",
        [
            clause("rev", "AAPL total revenue net sales", concepts=["revenue"]),
            clause("svc", "AAPL Services revenue segment revenue", concepts=["revenue"]),
            clause("margin", "AAPL gross margin profitability", concepts=["gross margin"]),
            clause("cost", "AAPL cost of revenue COGS", concepts=["cost"]),
            clause(
                "risk",
                "AAPL services risk competition regulatory",
                concepts=["services", "risk"],
                predicates=["competition"],
            ),
        ],
    )


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    print("=" * 70)
    print("LAYER-1 MATCHING SIMULATION")
    print(f"{len(SCENARIOS)} scenarios against real ontology data (no DeepSeek)")
    print("=" * 70)

    results: list[ScenarioResult] = []
    for name, description, plan in SCENARIOS:
        print(f"\n{'─' * 70}")
        print(f"Scenario: {name}")
        print(f"  {description}")
        r = run_scenario(name, description, plan)
        results.append(r)

        if r.error:
            print(f"  ❌ ERROR: {r.error}")
        else:
            icon = "✅" if r.status == "answerable" else ("⚠️ " if r.covered != "0/0" else "❌")
            print(f"  {icon} status={r.status}  covered={r.covered}  evidence={r.evidence_count}")
            for detail in r.clause_details:
                print(f"  {detail}")
            for sample in r.evidence_sample:
                print(f"  {sample}")

    # Summary table
    print(f"\n{'=' * 70}")
    print("SUMMARY")
    print(f"{'=' * 70}")
    print(f"{'Scenario':<30} {'Status':<16} {'Covered':<10} {'Evidence':<8} {'Result'}")
    print("─" * 70)
    for r in results:
        if r.error:
            mark = "ERROR"
        elif r.status == "answerable":
            mark = "PASS"
        elif r.covered != "0/0":
            mark = "PARTIAL"
        else:
            mark = "FAIL"
        print(f"{r.name:<30} {r.status:<16} {r.covered:<10} {r.evidence_count:<8} {mark}")

    failures = sum(1 for r in results if r.error or (r.covered == "0/0" and r.status != "answerable"))
    print(f"\n{len(results) - failures}/{len(results)} scenarios produced evidence.")


if __name__ == "__main__":
    main()
