"""Bounded, advisory current-market data capabilities.

The ontology store remains the durable source for filing-derived research.
This package intentionally holds only short-lived market snapshots, whose
source, freshness, and research-only status travel with every result.
"""

from .snapshot import MarketSnapshotRequest, market_snapshot_tool

__all__ = ["MarketSnapshotRequest", "market_snapshot_tool"]
