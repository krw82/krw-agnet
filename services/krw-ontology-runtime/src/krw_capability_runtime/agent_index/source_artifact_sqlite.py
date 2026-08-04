"""Internal source-artifact SQLite materialization boundary for v3 builds."""

from __future__ import annotations

from pathlib import Path
from collections.abc import Mapping, Sequence
from typing import Any

from krw_capability_runtime.agent_index.builder import (
    AGENT_INDEX_SCHEMA_VERSION as SOURCE_ARTIFACT_SQLITE_SCHEMA_VERSION,
    CompanyPlanItem as SourceCompanyShardPlanItem,
    IndexBuildPlan as SourceArtifactSqlitePlan,
    SOURCE_ARTIFACT_SQLITE_BUILDER_VERSION,
    ArtifactPlanItem,
    _build_artifact_index_sqlite,
    _cleanup_sqlite_database_files,
    _plan_artifact_index_inputs,
    _replace_sqlite_database,
    _temporary_index_path,
    build_source_artifact_manifest_payload,
    compile_artifact_fragments,
    diff_source_artifact_manifests,
    verify_agent_index,
    verify_source_artifact_manifest,
    write_source_artifact_manifest,
)

__all__ = (
    "SOURCE_ARTIFACT_SQLITE_BUILDER_VERSION",
    "SOURCE_ARTIFACT_SQLITE_SCHEMA_VERSION",
    "SourceArtifactSqlitePlan",
    "SourceCompanyShardPlanItem",
    "build_source_artifact_manifest_payload",
    "build_source_artifact_sqlite",
    "cleanup_sqlite_database_files",
    "diff_source_artifact_manifests",
    "plan_source_artifact_sqlite_inputs",
    "precompile_source_artifact_fragments",
    "replace_sqlite_database",
    "temporary_sqlite_path",
    "verify_source_artifact_manifest",
    "verify_source_artifact_sqlite",
    "write_source_artifact_manifest",
)


def plan_source_artifact_sqlite_inputs(
    root: Path,
    *,
    sqlite_path: Path,
    cache_root: Path | None = None,
    workers: int | None = None,
    source_manifest_path: Path | None = None,
    source_manifest_payload: Mapping[str, Any] | None = None,
) -> SourceArtifactSqlitePlan:
    """Plan source artifact rows used to materialize v3 company shard SQLite files."""
    return _plan_artifact_index_inputs(
        root,
        index_path=sqlite_path,
        cache_root=cache_root,
        workers=workers,
        source_manifest_path=source_manifest_path,
        source_manifest_payload=source_manifest_payload,
    )


def build_source_artifact_sqlite(
    root: Path,
    *,
    sqlite_path: Path,
    published_sqlite_path: Path,
    plan: SourceArtifactSqlitePlan,
) -> dict[str, Any]:
    """Materialize one source-artifact SQLite database from a prepared v3 plan."""
    return _build_artifact_index_sqlite(
        root,
        index_path=sqlite_path,
        published_index_path=published_sqlite_path,
        plan=plan,
    )


def precompile_source_artifact_fragments(
    plan: SourceArtifactSqlitePlan,
    *,
    items: Sequence[ArtifactPlanItem] | None = None,
    workers: int | None = None,
    force: bool = False,
) -> dict[str, int]:
    """Compile a release's dirty artifact fragments in one global worker pool.

    Company shard workers subsequently consume these immutable cache entries
    with their own inner artifact worker count pinned to one.  Keeping the
    precompile pool at the release boundary prevents nested process pools from
    multiplying the requested concurrency.
    """
    selected_items = tuple(items if items is not None else plan.items)
    results = compile_artifact_fragments(
        selected_items,
        root=plan.root,
        workers=workers if workers is not None else plan.workers,
        force=force,
    )
    cache_hits = sum(1 for result in results if result.cache_hit)
    return {
        "total": len(results),
        "cache_hits": cache_hits,
        "cache_misses": len(results) - cache_hits,
    }


def verify_source_artifact_sqlite(sqlite_path: Path) -> dict[str, Any]:
    return verify_agent_index(sqlite_path)


def cleanup_sqlite_database_files(sqlite_path: Path) -> None:
    _cleanup_sqlite_database_files(sqlite_path)


def temporary_sqlite_path(sqlite_path: Path) -> Path:
    return _temporary_index_path(sqlite_path)


def replace_sqlite_database(source_path: Path, target_path: Path) -> None:
    _replace_sqlite_database(source_path, target_path)
