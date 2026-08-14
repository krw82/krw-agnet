from __future__ import annotations

import sqlite3
from pathlib import Path

from krw_capability_runtime.agent_index.store import (
    OntologyStore,
    _metric_candidates_for_request,
    _normalize_period,
    _planned_metric_dimension_focus_score,
    _planned_metric_row_matches_periods,
    _prefer_member_focused_metric_rows,
)
from krw_capability_runtime.mcp_server.contracts import (
    ClauseEvidenceMatch,
    EvidenceSource,
    EvidenceUnit,
    QueryClause,
    SearchPlan,
    _document_role_priorities,
    _select_evidence_candidates,
    compile_research_state,
)
from krw_capability_runtime.mcp_server.tools import (
    _execute_search_plan,
    _planned_relation_verified,
    execution_scope_for_plan,
    normalize_execution_search_plan,
)


def _plan(*, object_types: list[str]) -> SearchPlan:
    return SearchPlan.model_validate(
        {
            "question": "What filing evidence supports ACME's principal risk?",
            "intent": "company_research",
            "tickers": ["ACME"],
            "clauses": [
                {
                    "clause_id": "principal_risk",
                    "retrieval_query": "ACME principal risk",
                    "required_concepts": ["principal risk"],
                    "object_types": object_types,
                }
            ],
        }
    )


def test_execution_plan_broadens_legacy_and_unknown_object_type_filters() -> None:
    plan, warnings = normalize_execution_search_plan(
        _plan(object_types=["NarrativeEvidence", "not_a_real_ontology_type"])
    )

    assert plan.clauses[0].object_types == [
        "BusinessFactor",
        "EvidenceQuote",
        "ResearchClaim",
    ]
    assert warnings == []


def test_execution_plan_preserves_numeric_evidence_as_metric_lineage_types() -> None:
    plan, warnings = normalize_execution_search_plan(_plan(object_types=["NumericEvidence"]))

    assert plan.clauses[0].object_types == ["Calculation", "MetricObservation", "XBRLFact"]
    assert warnings == []


def test_current_question_does_not_inherit_a_planner_guessed_historical_scope() -> None:
    plan = SearchPlan.model_validate(
        {
            "question": "Summarize ACME's recent revenue trend and principal risks.",
            "intent": "company_research",
            "tickers": ["ACME"],
            "document_types": ["10-K"],
            "periods": ["FY2025"],
            "clauses": [
                {
                    "clause_id": "recent_revenue",
                    "retrieval_query": "ACME revenue trend",
                    "required_concepts": ["revenue trend"],
                }
            ],
        }
    )

    scope = execution_scope_for_plan(plan)

    assert scope.document_types is None
    assert scope.periods is None
    assert scope.current_intent is True


def test_explicit_period_and_document_request_remain_exact_at_execution() -> None:
    plan = SearchPlan.model_validate(
        {
            "question": "Use ACME's FY2022 10-K to explain its principal risks.",
            "intent": "company_research",
            "tickers": ["ACME"],
            "document_types": ["10-K"],
            "periods": ["FY2022"],
            "clauses": [
                {
                    "clause_id": "historical_risk",
                    "retrieval_query": "ACME principal risks",
                    "required_concepts": ["principal risks"],
                }
            ],
        }
    )

    scope = execution_scope_for_plan(plan)

    assert scope.document_types == ["10-K"]
    assert scope.periods == ["FY2022"]
    assert scope.current_intent is False


def test_mixed_historical_and_recent_question_uses_scope_per_clause() -> None:
    """A historical revenue clause must not make a recent-risk clause historical.

    The physical plan has one external question, but its clauses are separate
    evidence obligations.  Applying a single FY2022 filter to both was a
    false-absence path whenever the requested current filing had the risk
    discussion but the old filing did not.
    """

    question = "Use ACME's FY2022 revenue and its recent principal risks."
    plan = SearchPlan.model_validate(
        {
            "question": question,
            "intent": "company_research",
            "tickers": ["ACME"],
            "periods": ["FY2022"],
            "clauses": [
                {
                    "clause_id": "historical_revenue",
                    "retrieval_query": "ACME FY2022 revenue",
                    "metrics": ["revenue"],
                },
                {
                    "clause_id": "recent_risks",
                    "retrieval_query": "ACME principal risks",
                    "required_concepts": ["principal risks"],
                },
            ],
        }
    )

    class BatchProbe:
        def __init__(self) -> None:
            self.clauses: list[dict[str, object]] = []

        def route_planned_tickers(self, **_kwargs: object) -> tuple[list[str], dict[str, object]]:
            return ["ACME"], {"resolved_tickers": ["ACME"]}

        def query_planned_batch_with_diagnostics(
            self, **kwargs: object
        ) -> tuple[dict[str, dict[str, object]], dict[str, object]]:
            self.clauses = list(kwargs["clauses"])
            return (
                {
                    str(clause["clause_id"]): {"rows": [], "diagnostics": {}}
                    for clause in self.clauses
                },
                {"routing": {}, "omitted_count": 0, "truncation_possible": False},
            )

    store = BatchProbe()
    _execute_search_plan(store=store, search_plan=plan)
    by_id = {str(clause["clause_id"]): clause for clause in store.clauses}

    assert by_id["historical_revenue"].get("periods") == ["FY2022"]
    assert by_id["historical_revenue"].get("query_intent_text") is None
    assert by_id["recent_risks"].get("periods") is None
    assert by_id["recent_risks"].get("query_intent_text") == question


def test_fiscal_quarter_is_not_silently_relabelled_as_the_same_calendar_quarter() -> None:
    plan = SearchPlan.model_validate(
        {
            "question": "Use ACME FY2024 Q3 revenue to explain the quarter.",
            "intent": "company_research",
            "tickers": ["ACME"],
            "periods": ["FY2024Q3"],
            "clauses": [
                {
                    "clause_id": "fiscal_quarter_revenue",
                    "retrieval_query": "ACME revenue",
                    "required_concepts": ["revenue"],
                }
            ],
        }
    )

    scope = execution_scope_for_plan(plan)

    assert scope.periods is None
    # Narrative objects use the index's calendar filing buckets, so applying
    # FY2024Q3 there would be an unsafe false-empty filter.  Structured
    # MetricObservation rows, however, carry the issuer fiscal coordinate and
    # must still receive the user's exact fiscal request; otherwise this
    # lookup silently falls through to the newest quarter.
    assert scope.metric_periods == ["FY2024Q3"]


def test_korean_fiscal_quarter_is_not_forced_into_a_calendar_period_filter() -> None:
    """Korean fiscal-quarter wording has the same FY/CY ambiguity as ``FY2024 Q3``.

    A company can end its fiscal Q3 in a different calendar quarter.  The
    execution layer must keep this lookup broad and let returned filing
    metadata establish the actual reporting basis instead of assuming that
    ``2024 회계연도 3분기`` means ``CY2024Q3``.
    """

    plan = SearchPlan.model_validate(
        {
            "question": "ACME의 2024 회계연도 3분기 매출을 설명해줘.",
            "intent": "company_research",
            "tickers": ["ACME"],
            "periods": ["FY2024Q3"],
            "clauses": [
                {
                    "clause_id": "fiscal_quarter_revenue",
                    "retrieval_query": "ACME revenue",
                    "required_concepts": ["revenue"],
                }
            ],
        }
    )

    scope = execution_scope_for_plan(plan)

    assert scope.periods is None
    assert scope.metric_periods == ["FY2024Q3"]


def test_fiscal_quarter_metric_scope_reaches_the_planned_query_separately() -> None:
    """A fiscal observation filter must not be dropped with the object filter."""

    class BatchProbe:
        def __init__(self) -> None:
            self.clauses: list[dict[str, object]] = []

        def route_planned_tickers(self, **_kwargs: object) -> tuple[list[str], dict[str, object]]:
            return ["ACME"], {"resolved_tickers": ["ACME"]}

        def query_planned_batch_with_diagnostics(
            self, **kwargs: object
        ) -> tuple[dict[str, dict[str, object]], dict[str, object]]:
            self.clauses = list(kwargs["clauses"])
            return (
                {
                    str(clause["clause_id"]): {"rows": [], "diagnostics": {}}
                    for clause in self.clauses
                },
                {"routing": {}, "omitted_count": 0, "truncation_possible": False},
            )

    plan = SearchPlan.model_validate(
        {
            "question": "Use ACME FY2024 Q3 revenue to explain the quarter.",
            "intent": "company_research",
            "tickers": ["ACME"],
            "periods": ["FY2024Q3"],
            "clauses": [
                {
                    "clause_id": "fiscal_quarter_revenue",
                    "retrieval_query": "ACME revenue",
                    "metrics": ["revenue"],
                }
            ],
        }
    )

    store = BatchProbe()
    _execute_search_plan(store=store, search_plan=plan)

    assert store.clauses[0]["periods"] is None
    assert store.clauses[0]["metric_periods"] == ["FY2024Q3"]


def test_user_fiscal_quarter_wins_over_a_planner_calendar_bucket() -> None:
    """The model must not be allowed to convert FY2024Q3 to CY2024Q3."""

    plan = SearchPlan.model_validate(
        {
            "question": "Use ACME FY2024 Q3 revenue to explain the quarter.",
            "intent": "company_research",
            "tickers": ["ACME"],
            # This is deliberately the wrong routing label.  The physical
            # metric filter must recover the user-specified fiscal coordinate
            # instead of trusting a proposal field that is only semantic.
            "periods": ["CY2024Q3"],
            "clauses": [
                {
                    "clause_id": "fiscal_quarter_revenue",
                    "retrieval_query": "ACME revenue",
                    "metrics": ["revenue"],
                }
            ],
        }
    )

    scope = execution_scope_for_plan(plan)

    assert scope.periods is None
    assert scope.metric_periods == ["FY2024Q3"]


def test_metric_period_matching_does_not_equate_fiscal_and_calendar_quarters() -> None:
    connection = sqlite3.connect(":memory:")
    connection.row_factory = sqlite3.Row
    row = connection.execute(
        """
        SELECT 'CY2024Q3' AS planned_metric_observation_period,
               2024 AS planned_metric_fiscal_year,
               3 AS planned_metric_fiscal_quarter
        """
    ).fetchone()
    assert row is not None

    assert not _planned_metric_row_matches_periods(row, ["FY2024Q3"])
    assert _planned_metric_row_matches_periods(row, ["CY2024Q3"])


def test_dimensioned_segment_revenue_reads_dimensioned_revenue_observations() -> None:
    """The index commonly stores product/region revenue under `revenue`."""

    assert _metric_candidates_for_request(
        ["segment_revenue"],
        metric_scope="dimensioned",
    ) == ["segment_revenue", "revenue"]


def test_named_dimensioned_metric_can_use_the_retrieval_query_as_member_focus() -> None:
    """Unknown issuer labels must not make a valid named-product lookup impossible.

    AAPL indexes this member as ``I Phone`` rather than the user's ``iPhone``.
    The dimensioned scope excludes company-total revenue; the literal query
    remains the deterministic member-ranking signal without pretending the
    planner knows the issuer's internal dimension spelling.
    """

    plan = SearchPlan.model_validate(
        {
            "question": "Summarize AAPL iPhone revenue.",
            "intent": "company_research",
            "tickers": ["AAPL"],
            "clauses": [
                {
                    "clause_id": "iphone_revenue",
                    "retrieval_query": "AAPL iPhone net sales segment revenue",
                    "metrics": ["segment_revenue"],
                    "metric_scope": "dimensioned",
                    "metric_dimensions": [],
                }
            ],
        }
    )

    assert plan.clauses[0].metric_scope == "dimensioned"
    assert plan.clauses[0].metric_dimensions == []


def test_company_total_segment_revenue_does_not_broaden_to_total_revenue() -> None:
    assert _metric_candidates_for_request(
        ["segment_revenue"],
        metric_scope="company_total",
    ) == ["segment_revenue"]


def test_named_product_focus_ranks_its_dimension_ahead_of_unrelated_segments() -> None:
    focus = ["AAPL", "iPhone", "net", "sales", "segment", "revenue"]
    iphone_score = _planned_metric_dimension_focus_score(
        '{"I Phone": "I Phone"}',
        focus,
    )
    americas_score = _planned_metric_dimension_focus_score(
        '{"Americas Segment": "Americas Segment"}',
        focus,
    )
    wearables_score = _planned_metric_dimension_focus_score(
        '{"Wearables Homeand Accessories": "Wearables Homeand Accessories"}',
        ["Wearables", "net", "sales"],
    )

    assert iphone_score > americas_score
    assert wearables_score > americas_score


def test_named_member_metric_query_keeps_matching_dimension_rows_without_false_empty_fallback() -> None:
    connection = sqlite3.connect(":memory:")
    connection.row_factory = sqlite3.Row
    rows = connection.execute(
        """
        SELECT 'iphone' AS id, '{"I Phone": "I Phone"}' AS planned_metric_dimensions_json
        UNION ALL
        SELECT 'americas', '{"Americas Segment": "Americas Segment"}'
        """
    ).fetchall()

    focused = _prefer_member_focused_metric_rows(
        rows,
        metric_scope="dimensioned",
        metric_focus_terms=["AAPL", "iPhone", "net", "sales", "segment", "revenue"],
    )
    fallback = _prefer_member_focused_metric_rows(
        rows[1:],
        metric_scope="dimensioned",
        metric_focus_terms=["AAPL", "iPhone", "net", "sales", "segment", "revenue"],
    )

    assert [row["id"] for row in focused] == ["iphone"]
    # Unknown issuer labels remain a broad lookup rather than becoming an
    # artificial no-result condition.
    assert [row["id"] for row in fallback] == ["americas"]


def test_planned_xbrl_metric_row_is_preserved_as_numeric_member_evidence() -> None:
    """Metric lookup can resolve a raw XBRL fact rather than a MetricObservation.

    The serving index represents AAPL product revenue this way.  Losing its
    value, unit, period, and member label makes a successful lookup look like
    an unanswerable generic text search to the research agent.
    """

    plan = SearchPlan.model_validate(
        {
            "question": "Summarize AAPL iPhone revenue.",
            "intent": "company_research",
            "tickers": ["AAPL"],
            "clauses": [
                {
                    "clause_id": "iphone_revenue",
                    "retrieval_query": "AAPL iPhone net sales segment revenue",
                    "metrics": ["segment_revenue"],
                    "metric_scope": "dimensioned",
                    "object_types": ["MetricObservation", "XBRLFact"],
                    "directness": "direct_preferred",
                }
            ],
        }
    )
    raw_payload = {
        "results_by_ticker": {
            "AAPL": [
                {
                    "id": "xbrl:AAPL:FY2025:10K:revenue:iphone",
                    "type": "XBRLFact",
                    "ticker": "AAPL",
                    "document_type": "10-K",
                    "period": "FY2025",
                    "planned_match_mode": "strict",
                    "_plan_clause_ids": ["iphone_revenue"],
                    "_plan_clause_matches": [
                        {
                            "clause_id": "iphone_revenue",
                            "planned_match_mode": "strict",
                            "planned_metric_terms": ["segment_revenue"],
                            "planned_metric_scope": "dimensioned",
                        }
                    ],
                    "object": {
                        "id": "xbrl:AAPL:FY2025:10K:revenue:iphone",
                        "type": "XBRLFact",
                        "value": 209_586_000_000,
                        "unit": "usd",
                        "canonical_metric": "revenue",
                        "dimensions": {"I Phone": "I Phone"},
                        "is_company_total": False,
                        "observation_period": "FY2025",
                        "period_type": "annual",
                        "period_end": "2025-09-27",
                        "observation_context_key": "iphone-2025",
                        "planned_metric_match": True,
                        "planned_metric_member_focus_matched": True,
                    },
                }
            ]
        }
    }

    state = compile_research_state(
        search_plan=plan,
        raw_payload=raw_payload,
        release_id="test-release",
    )

    unit = next(unit for unit in state.evidence_units if unit.object_type == "XBRLFact")
    assert unit.metric == "revenue"
    assert unit.dimensions == {"I Phone": "I Phone"}
    assert [(point.period, point.value, point.period_type) for point in unit.metric_points] == [
        ("FY2025", 209_586_000_000, "annual")
    ]
    coverage = state.clause_coverage[0]
    assert coverage.best_directness == "metric_lineage"
    assert coverage.evidence_ids == [unit.evidence_id]


def test_period_filter_does_not_expand_fiscal_quarter_to_calendar_quarter() -> None:
    assert _normalize_period("FY2024Q3") == ["FY2024Q3"]
    assert _normalize_period("CY2024Q3") == ["CY2024Q3"]
    # A year without a quarter is still intentionally broad because the
    # serving index uses both annual label conventions.
    assert _normalize_period("FY2024") == ["FY2024", "CY2024"]


def test_metric_lookup_keeps_source_object_inside_explicit_filing_scope(
    tmp_path: Path,
) -> None:
    """A derived metric row must not smuggle in an older source filing.

    The metric projection can carry a comparative observation period while its
    source object belongs to another filing.  The public query scope is the
    source filing scope, so the joined ``objects`` row is part of the
    contract, not merely an implementation detail.
    """

    db_path = tmp_path / "metric-scope.sqlite"
    conn = sqlite3.connect(db_path)
    conn.executescript(
        """
        CREATE TABLE objects (
            id TEXT PRIMARY KEY,
            ticker TEXT NOT NULL,
            document_type TEXT NOT NULL,
            period TEXT NOT NULL,
            type TEXT NOT NULL,
            review_status TEXT
        );
        CREATE TABLE metric_lookup (
            object_id TEXT NOT NULL,
            ticker TEXT NOT NULL,
            document_type TEXT NOT NULL,
            object_type TEXT NOT NULL,
            fiscal_year INTEGER,
            fiscal_quarter INTEGER,
            observation_period TEXT,
            observation_period_type TEXT,
            observation_context_key TEXT,
            canonical_metric TEXT,
            metric_name TEXT,
            metric_alias_text TEXT,
            text TEXT,
            value_text TEXT,
            value_numeric REAL,
            unit TEXT,
            segment_name TEXT,
            product_name TEXT,
            geography_name TEXT,
            is_company_total INTEGER,
            dimensions_json TEXT,
            filing_period TEXT,
            observation_start_date TEXT,
            observation_end_date TEXT
        );
        """
    )
    object_rows = [
        ("wanted-fy", "AAPL", "10-K", "FY2022"),
        ("wanted-cy", "AAPL", "10-K", "CY2022"),
        # The derived observation says FY2022, but this is an older source
        # filing.  It must not be returned for an explicit FY2022 filing read.
        ("stale-source", "AAPL", "10-K", "FY2021"),
        # The projection metadata says 10-K, but the source object is 10-Q.
        ("wrong-document", "AAPL", "10-Q", "FY2022"),
    ]
    conn.executemany(
        "INSERT INTO objects(id,ticker,document_type,period,type,review_status) VALUES(?,?,?,?,?,'')",
        [(*row, "MetricObservation") for row in object_rows],
    )
    conn.executemany(
        """
        INSERT INTO metric_lookup(
            object_id,ticker,document_type,object_type,fiscal_year,fiscal_quarter,
            observation_period,observation_period_type,observation_context_key,
            canonical_metric,metric_name,metric_alias_text,text,value_text,value_numeric,
            unit,segment_name,product_name,geography_name,is_company_total,
            dimensions_json,filing_period,observation_start_date,observation_end_date
        ) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
        """,
        [
            (
                object_id,
                "AAPL",
                # Keep projection metadata intentionally identical so only the
                # joined source-object scope can reject the bad rows.
                "10-K",
                "MetricObservation",
                2022,
                None,
                "FY2022",
                "annual",
                object_id,
                "revenue",
                "revenue",
                "revenue sales",
                "revenue",
                # Keep the derived rows distinct so the test exercises
                # scope filtering rather than metric deduplication.
                f"100-{object_id}",
                100.0,
                "USD",
                None,
                None,
                None,
                1,
                "{}",
                "FY2022",
                None,
                None,
            )
            for object_id, *_ in object_rows
        ],
    )
    conn.commit()
    conn.close()

    with OntologyStore(db_path) as store:
        rows, _strategy = store._query_metric_lookup_with_strategy(
            "revenue",
            tickers=["AAPL"],
            document_types=["10-K"],
            periods=["FY2022"],
            source_periods=["FY2022"],
            object_types=["MetricObservation"],
            include_rejected=False,
            limit=10,
            normalization={},
        )

    assert {row["id"] for row in rows} == {"wanted-fy", "wanted-cy"}


def test_predicate_aliases_do_not_require_every_wording_in_one_filing_sentence() -> None:
    row = {
        "quote_text": "Gross margin declined due to an unfavorable product mix.",
    }

    assert _planned_relation_verified(row, ["driven by", "due to"])


def test_predicate_verification_reads_compact_query_result_text() -> None:
    """QueryContext supplies compact bundles, not raw `quote_text` fields."""

    row = {
        "text": "Gross margin declined due to an unfavorable product mix.",
        "object": {
            "type": "EvidenceQuote",
            "quote_text": "Gross margin declined due to an unfavorable product mix.",
        },
    }

    assert _planned_relation_verified(row, ["due to"])


def test_exact_targeted_search_never_rewrites_to_partial_or_queries() -> None:
    """A known gap must not be filled with an unrelated generic result."""

    class StrategyProbe(OntologyStore):
        def __init__(self) -> None:
            self.queries: list[str] = []

        def _execute_fts(self, fts_query: str, **_kwargs: object) -> list[sqlite3.Row]:
            self.queries.append(fts_query)
            return []

    store = StrategyProbe()
    _rows, diagnostics = store._query_fts_with_strategy(
        "AAPL iPhone revenue segment",
        tickers=["AAPL"],
        document_types=["10-Q", "10-K"],
        periods=None,
        object_types=["EvidenceQuote", "ResearchClaim"],
        include_rejected=False,
        limit=20,
        strict_only=True,
    )

    assert [attempt["mode"] for attempt in diagnostics["attempts"]] == ["strict_and"]
    assert store.queries == ["aapl* iphone* revenue* segment*"]


def test_current_driver_wins_an_equivalent_clause_selection_without_explicit_period() -> None:
    """Opaque evidence IDs must not make an old 10-K beat the current filing."""

    clause = QueryClause(
        clause_id="greater_china",
        retrieval_query="AAPL Greater China revenue",
        required_concepts=["Greater China"],
        tickers=["AAPL"],
        directness="direct_preferred",
    )

    def candidate(evidence_id: str, period: str, document_type: str) -> dict[str, object]:
        return {
            "coverage_count": 1,
            "unit": EvidenceUnit(
                evidence_id=evidence_id,
                object_id=f"object:{evidence_id}",
                object_type="ResearchClaim",
                ticker="AAPL",
                period=period,
                document_type=document_type,
                title="Greater China filing fact",
                summary="Greater China filing fact",
                directness="related",
                evidence_grade="strong",
                supports_clause_ids=["greater_china"],
                clause_matches=[
                    ClauseEvidenceMatch(
                        clause_id="greater_china",
                        match_mode="strict",
                        directness="related",
                    )
                ],
                source=EvidenceSource(object_ids=[f"object:{evidence_id}"]),
            ),
        }

    priorities = _document_role_priorities(
        {
            "filing_document_roles": {
                "AAPL": {
                    "current_driver": {
                        "ticker": "AAPL",
                        "period": "CY2026Q1",
                        "document_type": "10-Q",
                    },
                    "annual_baseline": {
                        "ticker": "AAPL",
                        "period": "CY2025",
                        "document_type": "10-K",
                    },
                }
            }
        },
        requested_periods=[],
        requested_document_types=[],
    )
    selected, _protected = _select_evidence_candidates(
        [
            candidate("old-id-sorts-first", "CY2025", "10-K"),
            candidate("current-id-sorts-last", "CY2026Q1", "10-Q"),
        ],
        [clause],
        comparison_axes=["directness"],
        requested_periods=[],
        limit=1,
        document_role_priorities=priorities,
    )

    assert selected[0]["unit"].evidence_id == "current-id-sorts-last"


def test_metric_selection_protects_one_compatible_series_per_required_clause() -> None:
    """A broad metric lookup must not pin every unrelated member series.

    The state compiler may then compact non-essential rows under its wire
    budget while retaining one current/prior pair that answers each metric
    objective.  This is selection, not a new evidence-completeness rule.
    """

    clause = QueryClause(
        clause_id="product_revenue",
        retrieval_query="AAPL iPhone segment revenue",
        metrics=["segment_revenue"],
        metric_scope="dimensioned",
        calculation_window="year_over_year",
        directness="direct_required",
    )

    def candidate(member: str, period: str) -> dict[str, object]:
        evidence_id = f"{member}-{period}"
        return {
            "coverage_count": 1,
            "unit": EvidenceUnit(
                evidence_id=evidence_id,
                object_id=f"object:{evidence_id}",
                object_type="XBRLFact",
                ticker="AAPL",
                period=period,
                document_type="10-K",
                title=f"{member} revenue",
                summary=f"{member} revenue",
                directness="metric_lineage",
                evidence_grade="strong",
                metric="revenue",
                unit="usd",
                dimensions={member: member},
                metric_scope="dimensioned",
                metric_points=[
                    {
                        "period": period,
                        "value": 100,
                        "object_id": f"object:{evidence_id}",
                        "period_type": "annual",
                    }
                ],
                supports_clause_ids=["product_revenue"],
                clause_matches=[
                    ClauseEvidenceMatch(
                        clause_id="product_revenue",
                        match_mode="strict",
                        directness="metric_lineage",
                    )
                ],
                source=EvidenceSource(object_ids=[f"object:{evidence_id}"]),
            ),
        }

    candidates = [
        candidate("iphone", "FY2025"),
        candidate("iphone", "FY2024"),
        candidate("services", "FY2025"),
        candidate("services", "FY2024"),
    ]

    _selected, protected = _select_evidence_candidates(
        candidates,
        [clause],
        comparison_axes=["growth_rate", "value"],
        requested_periods=[],
        limit=20,
    )

    assert protected == {"iphone-FY2025", "iphone-FY2024"}


def test_raw_time_series_keeps_a_compatible_pair_ahead_of_narrative_fill() -> None:
    """A trend needs two observations even when it does not request a calculation.

    The calculation window is selection metadata here: the returned plan stays
    on the raw ``value`` axis, but the compact result must not keep only the
    latest point and fill the second slot with unrelated narrative prose.
    """

    clause = QueryClause(
        clause_id="revenue_trend",
        retrieval_query="AAPL revenue trend",
        metrics=["revenue"],
        calculation_window="period_over_period",
        directness="direct_preferred",
    )

    def metric_candidate(evidence_id: str, period: str) -> dict[str, object]:
        return {
            "coverage_count": 1,
            "unit": EvidenceUnit(
                evidence_id=evidence_id,
                object_id=f"object:{evidence_id}",
                object_type="MetricObservation",
                ticker="AAPL",
                period=period,
                document_type="10-K",
                title="Revenue observation",
                summary="Revenue observation",
                directness="metric_lineage",
                evidence_grade="strong",
                metric="revenue",
                unit="usd",
                metric_scope="company_total",
                metric_points=[
                    {
                        "period": period,
                        "value": 100,
                        "object_id": f"object:{evidence_id}",
                        "period_type": "annual",
                    }
                ],
                supports_clause_ids=["revenue_trend"],
                clause_matches=[
                    ClauseEvidenceMatch(
                        clause_id="revenue_trend",
                        match_mode="strict",
                        directness="metric_lineage",
                    )
                ],
                source=EvidenceSource(object_ids=[f"object:{evidence_id}"]),
            ),
        }

    narrative_fill = {
        "coverage_count": 1,
        "unit": EvidenceUnit(
            evidence_id="narrative-fill",
            object_id="object:narrative-fill",
            object_type="ResearchClaim",
            ticker="AAPL",
            period="FY2025",
            document_type="10-K",
            title="Nearby narrative",
            summary="Nearby narrative",
            directness="related",
            evidence_grade="medium",
            supports_clause_ids=["revenue_trend"],
            clause_matches=[
                ClauseEvidenceMatch(
                    clause_id="revenue_trend",
                    match_mode="strict",
                    directness="related",
                )
            ],
            source=EvidenceSource(object_ids=["object:narrative-fill"]),
        ),
    }

    selected, protected = _select_evidence_candidates(
        [
            metric_candidate("revenue-FY2025", "FY2025"),
            narrative_fill,
            metric_candidate("revenue-FY2024", "FY2024"),
        ],
        [clause],
        comparison_axes=["value"],
        requested_periods=[],
        limit=2,
    )

    assert [candidate["unit"].evidence_id for candidate in selected] == [
        "revenue-FY2025",
        "revenue-FY2024",
    ]
    assert protected == {"revenue-FY2025", "revenue-FY2024"}


def test_time_series_prefers_a_complete_annual_pair_over_an_unpaired_current_point() -> None:
    """A current singleton must not displace the only usable trend pair.

    A current 10-Q can be newer than the available annual observations while
    lacking an adjacent comparable quarter.  The first-pass required-clause
    witness should remain available, but a time-series request must reserve
    the complete FY2025/FY2024 pair before normal quality fill consumes the
    bounded response budget.
    """

    clause = QueryClause(
        clause_id="rnd_trend",
        retrieval_query="AAPL research and development trend",
        metrics=["research_and_development"],
        metric_scope="company_total",
        calculation_window="period_over_period",
        directness="direct_required",
    )

    def metric_candidate(
        evidence_id: str,
        *,
        period: str,
        document_type: str,
        value: int,
        period_type: str,
    ) -> dict[str, object]:
        return {
            "coverage_count": 1,
            "unit": EvidenceUnit(
                evidence_id=evidence_id,
                object_id=f"object:{evidence_id}",
                object_type="MetricObservation",
                ticker="AAPL",
                period=period,
                document_type=document_type,
                title="R&D observation",
                summary="R&D observation",
                directness="metric_lineage",
                evidence_grade="strong",
                metric="research_and_development",
                unit="usd",
                metric_scope="company_total",
                metric_points=[
                    {
                        "period": period,
                        "value": value,
                        "object_id": f"object:{evidence_id}",
                        "period_type": period_type,
                    }
                ],
                supports_clause_ids=["rnd_trend"],
                clause_matches=[
                    ClauseEvidenceMatch(
                        clause_id="rnd_trend",
                        match_mode="strict",
                        directness="metric_lineage",
                    )
                ],
                source=EvidenceSource(object_ids=[f"object:{evidence_id}"]),
            ),
        }

    def narrative_candidate(evidence_id: str) -> dict[str, object]:
        return {
            "coverage_count": 1,
            "unit": EvidenceUnit(
                evidence_id=evidence_id,
                object_id=f"object:{evidence_id}",
                object_type="ResearchClaim",
                ticker="AAPL",
                period="CY2026Q1",
                document_type="10-Q",
                title="Nearby narrative",
                summary="Nearby narrative",
                directness="related",
                evidence_grade="strong",
                supports_clause_ids=["rnd_trend"],
                clause_matches=[
                    ClauseEvidenceMatch(
                        clause_id="rnd_trend",
                        match_mode="strict",
                        directness="related",
                    )
                ],
                source=EvidenceSource(object_ids=[f"object:{evidence_id}"]),
            ),
        }

    selected, protected = _select_evidence_candidates(
        [
            metric_candidate(
                "rnd-current-q",
                period="CY2026Q1",
                document_type="10-Q",
                value=11_419,
                period_type="quarter",
            ),
            narrative_candidate("filler-one"),
            narrative_candidate("filler-two"),
            metric_candidate(
                "rnd-FY2025",
                period="FY2025",
                document_type="10-K",
                value=34_550,
                period_type="annual",
            ),
            metric_candidate(
                "rnd-FY2024",
                period="FY2024",
                document_type="10-K",
                value=31_370,
                period_type="annual",
            ),
        ],
        [clause],
        comparison_axes=["value"],
        requested_periods=[],
        limit=3,
        document_role_priorities={
            ("AAPL", "CY2026Q1", "10-Q"): 0,
            ("AAPL", "FY2025", "10-K"): 1,
            ("AAPL", "FY2024", "10-K"): 2,
        },
    )

    assert [candidate["unit"].evidence_id for candidate in selected] == [
        "rnd-current-q",
        "rnd-FY2025",
        "rnd-FY2024",
    ]
    assert protected == {"rnd-current-q", "rnd-FY2025", "rnd-FY2024"}


def test_explicit_period_disables_document_role_recency_preference() -> None:
    priorities = _document_role_priorities(
        {
            "filing_document_roles": {
                "AAPL": {
                    "current_driver": {
                        "ticker": "AAPL",
                        "period": "CY2026Q1",
                        "document_type": "10-Q",
                    }
                }
            }
        },
        requested_periods=["FY2022"],
        requested_document_types=["10-K"],
    )

    assert priorities == {}
