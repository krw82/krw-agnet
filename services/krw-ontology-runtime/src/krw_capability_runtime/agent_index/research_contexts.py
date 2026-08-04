"""Context executor interfaces for the research kernel.

The initial rollout keeps execution in the existing store methods and uses
these classes as explicit contracts for future migration. They intentionally do
not run agentic loops or write final answers.
"""

from __future__ import annotations

from typing import Any, Callable, Mapping, Sequence

from krw_capability_runtime.agent_index.research_types import ResearchRequest, ResearchRoute


class BaseContext:
    name = "base_context"

    def __init__(self, store: Any):
        self.store = store

    def run(self, request: ResearchRequest, route: ResearchRoute, **kwargs: Any) -> dict[str, Any]:
        raise NotImplementedError


class MetricContext(BaseContext):
    name = "metric_context"

    def run(
        self,
        request: ResearchRequest,
        route: ResearchRoute,
        *,
        search_topic: str | None,
        requested_tickers: Sequence[str] | None,
        document_types: Sequence[str] | None,
        periods: Sequence[str] | None,
        limit_results: int,
        metric_topic_builder: Callable[[str, str | None], str | None],
        metric_needs_denominator: Callable[[str], bool],
        metric_period_filters: Callable[[str, Sequence[str] | None], list[str] | None],
        annual_period_filter_check: Callable[[Sequence[str]], bool],
        metric_pack_builder: Callable[[Sequence[Mapping[str, Any]], Mapping[str, Any]], dict[str, Any]],
        **_: Any,
    ) -> dict[str, Any]:
        metric_topic = metric_topic_builder(request.question, search_topic)
        if not requested_tickers or not metric_topic:
            return {"metric_series_pack": None}
        compact_limit = max(3, min(int(limit_results), 12))
        metric_limit = max(compact_limit, 20) if metric_needs_denominator(metric_topic) else compact_limit
        metric_periods = metric_period_filters(metric_topic, periods)
        metric_document_types = document_types
        if not metric_document_types and metric_periods and annual_period_filter_check(metric_periods):
            metric_document_types = ["10-K"]
        metric_results, metric_diagnostics = self.store.query_compact_with_diagnostics(
            topic=metric_topic,
            tickers=requested_tickers,
            document_types=metric_document_types,
            periods=metric_periods or periods,
            object_types=["MetricObservation", "Calculation", "XBRLFact"],
            limit=metric_limit,
        )
        return {
            "metric_series_pack": metric_pack_builder(metric_results, metric_diagnostics),
            "metric_topic": metric_topic,
        }


class RiskContext(BaseContext):
    name = "risk_context"

    def run(
        self,
        request: ResearchRequest,
        route: ResearchRoute,
        **kwargs: Any,
    ) -> dict[str, Any]:
        return _run_projection_context(self.store, request, **kwargs)


class DirectExposureContext(BaseContext):
    name = "direct_exposure_context"

    def run(
        self,
        request: ResearchRequest,
        route: ResearchRoute,
        **kwargs: Any,
    ) -> dict[str, Any]:
        return _run_projection_context(self.store, request, **kwargs)


class CompanyOverviewContext(BaseContext):
    name = "company_overview_context"

    def run(
        self,
        request: ResearchRequest,
        route: ResearchRoute,
        **kwargs: Any,
    ) -> dict[str, Any]:
        return _run_projection_context(self.store, request, **kwargs)


class DiscoveryContext(BaseContext):
    name = "discovery_context"


class CompareContext(BaseContext):
    name = "compare_context"


class FactualLookupContext(BaseContext):
    name = "factual_lookup_context"


class ValuationStopContext(BaseContext):
    name = "valuation_stop_context"


def _run_projection_context(
    store: Any,
    request: ResearchRequest,
    *,
    document_types: Sequence[str] | None,
    periods: Sequence[str] | None,
    requested_tickers: Sequence[str] | None,
    limit_results: int,
    query_frame: Mapping[str, Any],
    answerability: Mapping[str, Any],
    default_object_types: Sequence[str],
    projection_pack_builder: Callable[..., dict[str, Any]],
    **_: Any,
) -> dict[str, Any]:
    compact_limit = max(3, min(int(limit_results), 12))
    typed_profile = store._typed_projection_profile(
        topic=request.question,
        object_types=default_object_types,
        explicit_object_types=False,
    )
    if not typed_profile.get("enabled"):
        return {"projection_pack": None, "typed_profile": typed_profile}
    projection_results, projection_diagnostics = store.query_compact_with_diagnostics(
        topic=request.question,
        tickers=requested_tickers,
        document_types=document_types,
        periods=periods,
        object_types=typed_profile.get("object_types") or default_object_types,
        limit=compact_limit,
    )
    return {
        "projection_pack": projection_pack_builder(
            projection_results,
            projection_diagnostics,
            query_frame=query_frame,
            answerability=answerability,
        ),
        "typed_profile": typed_profile,
    }
