"""Public v3 ontology store opener."""

from __future__ import annotations

from pathlib import Path

from krw_capability_runtime.agent_index.spine_router import OntologySpineRouter


def open_ontology_store(
    index_path: Path | str,
    *,
    check_same_thread: bool = True,
    routing: str = "spine",
) -> OntologySpineRouter:
    """Open a v3 global spine store.

    Production callers must enter through the verified v3 release topology. Legacy
    monolith and shard facade modes are intentionally not available from this public
    API because they hide route bugs and bypass the release trust boundary.
    """
    resolved_routing = str(routing or "spine").strip().lower()
    if resolved_routing != "spine":
        raise ValueError("open_ontology_store only supports v3 global spine routing")
    path = Path(index_path)
    if OntologySpineRouter.can_open(path):
        return OntologySpineRouter(path, check_same_thread=check_same_thread)
    raise FileNotFoundError(f"v3 global spine layout not found for index: {path}")
