"""Read-only ontology index surface used by the capability runtime.

The upstream package initializer also exposed builders and a provider-backed
planner. Importing an initializer should not allocate build dependencies or
make a provider dependency reachable from an online read worker, so this
surface deliberately exports only the runtime functions used by MCP handlers.
"""

from krw_capability_runtime.agent_index.chart_series import (
    CHART_SERIES_RELATIVE_PATH,
    query_chart_series_pack,
)
from krw_capability_runtime.agent_index.retriever import AgentRetriever, QueryPlan
from krw_capability_runtime.agent_index.router import open_ontology_store
from krw_capability_runtime.agent_index.spine_schema import GLOBAL_SPINE_RELATIVE_PATH

__all__ = [
    "AgentRetriever",
    "CHART_SERIES_RELATIVE_PATH",
    "GLOBAL_SPINE_RELATIVE_PATH",
    "QueryPlan",
    "open_ontology_store",
    "query_chart_series_pack",
]
