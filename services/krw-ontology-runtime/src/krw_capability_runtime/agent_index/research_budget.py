"""Budget policy for deterministic research-kernel routes."""

from __future__ import annotations

import time
from dataclasses import dataclass

from krw_capability_runtime.agent_index.research_types import ResearchBudget


@dataclass
class BudgetTimer:
    started_at: float
    max_ms: int
    exhausted: bool = False

    @classmethod
    def start(cls, max_ms: int) -> "BudgetTimer":
        return cls(started_at=time.perf_counter(), max_ms=max_ms)

    def elapsed_ms(self) -> int:
        elapsed = int((time.perf_counter() - self.started_at) * 1000)
        self.exhausted = elapsed >= self.max_ms
        return elapsed

    def remaining_ms(self) -> int:
        return max(0, self.max_ms - self.elapsed_ms())


def budget_for_context(context_name: str, depth: str = "shallow") -> ResearchBudget:
    if depth == "audit":
        return ResearchBudget(max_ms=30_000, max_topic_candidates=100, max_object_candidates=80)
    if depth == "deep":
        return ResearchBudget(max_ms=20_000, max_topic_candidates=80, max_object_candidates=50)
    budget_ms = {
        "metric_context": 4_000,
        "risk_context": 5_000,
        "direct_exposure_context": 3_000,
        "discovery_context": 7_000,
        "compare_context": 10_000,
        "company_overview_context": 4_000,
        "factual_lookup_context": 4_000,
        "valuation_stop_context": 500,
    }.get(context_name, 5_000)
    return ResearchBudget(max_ms=budget_ms)
