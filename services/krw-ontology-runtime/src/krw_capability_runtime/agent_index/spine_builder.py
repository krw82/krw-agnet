"""v3 global spine + company shard build primitives."""

from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import sqlite3
import subprocess
import sys
import time
from concurrent.futures import ProcessPoolExecutor, ThreadPoolExecutor, as_completed
from concurrent.futures.process import BrokenProcessPool
from collections import defaultdict
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass, replace
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from krw_capability_runtime.agent_index.cross_company_links import (
    CROSS_COMPANY_LINK_BUILDER_VERSION,
    CrossCompanyLinkGenerationResult,
    generate_cross_company_links,
)
from krw_capability_runtime.agent_index.chart_series import (
    CHART_SERIES_RELATIVE_PATH,
    ChartSeriesBuildResult,
    build_chart_series_index,
)
from krw_capability_runtime.agent_index.cache_seal import (
    read_immutable_sqlite_cache_seal,
    record_immutable_sqlite_cache_sha256,
    remove_immutable_sqlite_cache_seal,
    write_immutable_sqlite_cache_seal,
)
from krw_capability_runtime.agent_index.router_sidecar import (
    ROUTER_SIDECAR_RELATIVE_PATH,
    RouterSidecarBuildResult,
    build_router_sidecar,
    load_router_ranking_profile,
)
from krw_capability_runtime.agent_index.router_coherence import (
    ROUTER_COHERENCE_RELATIVE_PATH,
    RouterCoherenceBuildResult,
    build_router_coherence,
    load_router_coherence_profile,
)
from krw_capability_runtime.agent_index.router_cache import (
    publish_router_sidecar_cache,
    restore_router_sidecar_cache,
    router_sidecar_cache_path,
    router_sidecar_semantic_cache_key,
    verify_router_sidecar_cache,
)
from krw_capability_runtime.agent_index.semantic_identity import (
    SEMANTIC_IDENTITY_POLICY_VERSION,
    effective_ticker,
    project_local_identity,
    project_object_identity,
    semantic_object_hash,
)
from krw_capability_runtime.agent_index.spine_preflight import (
    preflight_company_shard_identities,
    preflight_spine_fragments,
)
from krw_capability_runtime.agent_index.metric_dictionary import (
    metric_dictionary_binding,
    metric_dictionary_binding_errors,
)
from krw_capability_runtime.agent_index.source_artifact_sqlite import (
    SOURCE_ARTIFACT_SQLITE_BUILDER_VERSION,
    SOURCE_ARTIFACT_SQLITE_SCHEMA_VERSION,
    SourceArtifactSqlitePlan,
    SourceCompanyShardPlanItem,
    build_source_artifact_sqlite,
    build_source_artifact_manifest_payload,
    cleanup_sqlite_database_files,
    plan_source_artifact_sqlite_inputs,
    precompile_source_artifact_fragments,
    replace_sqlite_database,
    temporary_sqlite_path,
    verify_source_artifact_sqlite,
    write_source_artifact_manifest,
)
from krw_capability_runtime.agent_index.spine_schema import (
    GLOBAL_SPINE_BUILDER_VERSION,
    GLOBAL_SPINE_LAYOUT,
    GLOBAL_SPINE_SCHEMA_VERSION,
    GLOBAL_SPINE_TABLES,
    SPINE_FRAGMENT_SCHEMA_VERSION,
    SPINE_FRAGMENT_TABLES,
    create_global_spine_schema,
    create_spine_fragment_schema,
    read_global_spine_metadata,
    spine_verification_seal_path,
    verify_global_spine_schema,
    verify_spine_fragment_schema,
    write_spine_verification_seal,
    write_global_spine_metadata,
)

COMPANY_SHARD_SCHEMA_VERSION = "krw-ontology-company-shard/v3"
COMPANY_SHARD_CACHE_FORMAT_VERSION = "krw-ontology-company-shard-cache/v4"
SPINE_FRAGMENT_CACHE_FORMAT_VERSION = "krw-ontology-spine-fragment-cache/v8"
GLOBAL_SPINE_CACHE_FORMAT_VERSION = "krw-ontology-global-spine-cache/v1"
SPINE_FRAGMENT_FORMAT_VERSION = "krw-ontology-spine-fragment/v3"
SPINE_PROJECTION_VERSION = "spine-projection/v7"
V3_BUILD_SUMMARY_FORMAT_VERSION = "krw-ontology-v3-build-summary/v1"
V3_BUILD_PLAN_FORMAT_VERSION = "krw-ontology-v3-build-plan/v1"
V3_BUILD_PROGRESS_FORMAT_VERSION = "krw-ontology-v3-build-progress/v1"
V3_SHARD_MANIFEST_FORMAT_VERSION = "krw-ontology-shard-manifest/v3"
SHARD_QUALITY_SUMMARY_FORMAT_VERSION = "krw-ontology-shard-quality-summary/v1"
DEFAULT_COMPANY_WORKER_MEMORY_OVERHEAD_MIB = 2_048
DEFAULT_BUILD_MEMORY_RESERVE_MIB = 8_192
DEFAULT_GLOBAL_MERGE_SQLITE_THREADS = 8
DEFAULT_GLOBAL_MERGE_SQLITE_CACHE_KIB = 1_048_576
DEFAULT_SPINE_STALE_TMP_AGE_SECONDS = 24 * 60 * 60


def _validate_static_build_runtime() -> dict[str, Any]:
    """Fail before source planning when required SQLite features are unavailable."""
    try:
        with sqlite3.connect(":memory:") as conn:
            json_value = conn.execute("SELECT value FROM json_each('[1]')").fetchone()[0]
            conn.execute("CREATE VIRTUAL TABLE runtime_fts USING fts5(value, tokenize='unicode61')")
            conn.execute("INSERT INTO runtime_fts(value) VALUES('ontology runtime check')")
            fts_match = conn.execute(
                "SELECT COUNT(*) FROM runtime_fts WHERE runtime_fts MATCH 'ontology'"
            ).fetchone()[0]
    except sqlite3.Error as exc:
        raise RuntimeError(
            f"build_runtime_preflight_failed:sqlite={sqlite3.sqlite_version}:error={exc}"
        ) from exc
    if int(json_value) != 1 or int(fts_match) != 1:
        raise RuntimeError(
            "build_runtime_preflight_failed:required SQLite JSON1/FTS5 behavior mismatch"
        )
    return {
        "sqlite_version": sqlite3.sqlite_version,
        "json1": True,
        "fts5": True,
    }


class SpineFragmentCollisionError(RuntimeError):
    """Raised when fragments claim one key with incompatible semantic identity."""


class SpineSemanticPreflightError(SpineFragmentCollisionError):
    """Raised after collecting every incompatible fragment claim."""


def _semantic_preflight_error(result: Mapping[str, Any]) -> SpineSemanticPreflightError:
    conflicts = list(result.get("conflicts") or [])
    first = conflicts[0] if conflicts else {}
    key = json.dumps(
        first.get("key") or {}, ensure_ascii=False, sort_keys=True, separators=(",", ":")
    )
    kind = str(first.get("kind") or "semantic")
    prefix = (
        "spine_fragment_semantic_conflict"
        if kind == "semantic"
        else "spine_fragment_identity_scope_violation"
        if kind == "scope"
        else "company_identity_preflight_violation"
        if kind in {"reference", "sqlite", "json"}
        else "spine_fragment_pk_collision"
    )
    detail = (
        f"{prefix}:table={first.get('table', '<unknown>')}:key={key}:"
        f"fragment={first.get('conflicting_fragment', '<unknown>')}:"
        f"fields={','.join(first.get('fields') or ['<unknown>'])}"
    )
    report = result.get("report_path") or "<not-written>"
    return SpineSemanticPreflightError(
        "spine_semantic_preflight_failed:"
        f"conflict_count={len(conflicts)}:report={report}:first={detail}"
    )


@dataclass(frozen=True)
class CompanyShardBuildResult:
    ticker: str
    shard_path: Path
    cache_hit: bool
    artifact_count: int
    totals: Mapping[str, int]
    verification: Mapping[str, Any]
    cache_key: str | None = None


@dataclass(frozen=True)
class SpineFragmentResult:
    ticker: str
    fragment_path: Path
    shard_path: Path
    counts: Mapping[str, int]
    cache_hit: bool = False
    cache_key: str | None = None


@dataclass(frozen=True)
class GlobalSpineMergeResult:
    global_spine_path: Path
    fragment_count: int
    counts: Mapping[str, int]
    chain_links: CrossCompanyLinkGenerationResult
    verification: Mapping[str, Any]


@dataclass(frozen=True)
class BuildParallelism:
    """One global CPU/memory budget split across non-overlapping build stages."""

    requested_workers: int
    cpu_count: int
    physical_memory_mib: int | None
    artifact_compile_workers: int
    company_workers: int
    company_inner_artifact_workers: int
    spine_fragment_workers: int
    estimated_company_worker_mib: int
    memory_limited_company_workers: int | None

    def to_dict(self) -> dict[str, Any]:
        return {
            "requested_workers": self.requested_workers,
            "cpu_count": self.cpu_count,
            "physical_memory_mib": self.physical_memory_mib,
            "artifact_compile_workers": self.artifact_compile_workers,
            "company_workers": self.company_workers,
            "company_inner_artifact_workers": self.company_inner_artifact_workers,
            "spine_fragment_workers": self.spine_fragment_workers,
            "estimated_company_worker_mib": self.estimated_company_worker_mib,
            "memory_limited_company_workers": self.memory_limited_company_workers,
            "nested_process_pools": False,
        }


@dataclass(frozen=True)
class SpineShardReleaseBuildResult:
    release_root: Path
    release_id: str
    source_manifest_path: Path
    build_plan_path: Path
    shard_manifest_path: Path
    build_summary_path: Path
    global_spine_path: Path
    router_sidecar_path: Path
    router_coherence_path: Path
    shard_results: tuple[CompanyShardBuildResult, ...]
    fragment_results: tuple[SpineFragmentResult, ...]
    merge_result: GlobalSpineMergeResult
    shard_manifest: Mapping[str, Any]
    build_summary: Mapping[str, Any]
    router_sidecar_result: RouterSidecarBuildResult
    router_coherence_result: RouterCoherenceBuildResult
    chart_series_path: Path | None = None
    chart_series_result: ChartSeriesBuildResult | None = None
    progress_path: Path | None = None


def _global_spine_semantic_cache_key(
    fragment_results: Sequence[SpineFragmentResult],
    *,
    source_manifest_hash: str | None,
    generate_links: bool,
) -> str:
    ordered_fragments = [
        [result.ticker, str(result.cache_key or "")]
        for result in sorted(fragment_results, key=lambda item: item.ticker)
    ]
    if not ordered_fragments or any(not cache_key for _ticker, cache_key in ordered_fragments):
        raise ValueError("global spine cache requires every ordered fragment cache key")
    return _stable_hash(
        {
            "cache_format": GLOBAL_SPINE_CACHE_FORMAT_VERSION,
            "fragment_cache_keys": ordered_fragments,
            "source_manifest_hash": source_manifest_hash,
            "generate_links": bool(generate_links),
            "global_spine_schema_version": GLOBAL_SPINE_SCHEMA_VERSION,
            "global_spine_builder_version": GLOBAL_SPINE_BUILDER_VERSION,
            "spine_projection_version": SPINE_PROJECTION_VERSION,
            "semantic_identity_policy_version": SEMANTIC_IDENTITY_POLICY_VERSION,
            "cross_company_link_builder_version": CROSS_COMPANY_LINK_BUILDER_VERSION,
            "company_shard_schema_version": COMPANY_SHARD_SCHEMA_VERSION,
            "source_artifact_sqlite_schema_version": SOURCE_ARTIFACT_SQLITE_SCHEMA_VERSION,
            "source_artifact_sqlite_builder_version": SOURCE_ARTIFACT_SQLITE_BUILDER_VERSION,
            "metric_dictionary": metric_dictionary_binding(),
        }
    )


def _global_spine_cache_path(cache_root: Path, cache_key: str) -> Path:
    digest = cache_key.split(":", 1)[-1]
    return cache_root / "v3" / "global_spines" / digest[:2] / f"{digest}.sqlite"


def _verify_global_spine_cache(path: Path, *, cache_key: str) -> tuple[str, ...]:
    if not path.is_file():
        return ("global_spine_cache_missing",)
    verification = verify_global_spine_schema(path, deep=True, trust_seal=True)
    errors = [f"global_spine_cache:{error}" for error in verification.get("errors") or []]
    metadata = verification.get("metadata") or {}
    if metadata.get("global_spine_cache_format_version") != GLOBAL_SPINE_CACHE_FORMAT_VERSION:
        errors.append("global_spine_cache_format_mismatch")
    if metadata.get("global_spine_cache_key") != cache_key:
        errors.append("global_spine_cache_key_mismatch")
    return tuple(dict.fromkeys(errors))


def _publish_global_spine_cache(
    source_path: Path,
    cache_path: Path,
    *,
    cache_key: str,
    verification: Mapping[str, Any],
) -> str:
    if not _verify_global_spine_cache(cache_path, cache_key=cache_key):
        return "existing"
    cache_path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = cache_path.parent / f".{cache_path.name}.{os.getpid()}.{time.time_ns()}.tmp"
    _cleanup_spine_database_and_seal(tmp_path)
    try:
        copy_mode = clone_or_copy_immutable_file(source_path, tmp_path)
        os.replace(tmp_path, cache_path)
        write_spine_verification_seal(
            cache_path,
            verification,
            source_path=source_path,
            details={
                "cache_format": GLOBAL_SPINE_CACHE_FORMAT_VERSION,
                "cache_key": cache_key,
            },
        )
        errors = _verify_global_spine_cache(cache_path, cache_key=cache_key)
        if errors:
            raise RuntimeError("published global spine cache invalid: " + ", ".join(errors))
        return copy_mode
    finally:
        _cleanup_spine_database_and_seal(tmp_path)


def _bootstrap_global_spine_cache_from_current(
    release_root: Path,
    cache_path: Path,
    *,
    cache_key: str,
    source_manifest_hash: str | None,
    generate_links: bool,
    fragment_count: int,
) -> dict[str, Any]:
    """Adopt a compatible verified current release into the new semantic cache."""
    if not generate_links:
        return {"adopted": False, "reason": "generate_links_disabled"}
    current = release_root.parent / "current"
    if not current.is_symlink() or not current.exists():
        return {"adopted": False, "reason": "current_release_missing"}
    current_root = current.resolve()
    if current_root == release_root.resolve():
        return {"adopted": False, "reason": "current_is_candidate"}
    source_path = current_root / "indexes" / "global_spine.sqlite"
    verification = verify_global_spine_schema(source_path, deep=True, trust_seal=True)
    if not verification.get("ok"):
        return {
            "adopted": False,
            "reason": "current_global_spine_invalid",
            "errors": list(verification.get("errors") or []),
        }
    metadata = verification.get("metadata") or {}
    expected = {
        "source_manifest_hash": source_manifest_hash,
        "schema_version": GLOBAL_SPINE_SCHEMA_VERSION,
        "builder_version": GLOBAL_SPINE_BUILDER_VERSION,
        "spine_projection_version": SPINE_PROJECTION_VERSION,
        "semantic_identity_policy_version": SEMANTIC_IDENTITY_POLICY_VERSION,
        "source_artifact_sqlite_schema_version": SOURCE_ARTIFACT_SQLITE_SCHEMA_VERSION,
        "source_artifact_sqlite_builder_version": SOURCE_ARTIFACT_SQLITE_BUILDER_VERSION,
        "company_shard_schema_version": COMPANY_SHARD_SCHEMA_VERSION,
        "fragment_count": fragment_count,
    }
    mismatches = [key for key, value in expected.items() if metadata.get(key) != value]
    if mismatches:
        return {
            "adopted": False,
            "reason": "current_global_spine_binding_mismatch",
            "mismatches": mismatches,
        }
    cache_path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = cache_path.parent / f".{cache_path.name}.{os.getpid()}.{time.time_ns()}.tmp"
    _cleanup_spine_database_and_seal(tmp_path)
    try:
        copy_mode = clone_or_copy_immutable_file(source_path, tmp_path)
        with sqlite3.connect(tmp_path) as conn:
            write_global_spine_metadata(
                conn,
                {
                    "global_spine_cache_format_version": GLOBAL_SPINE_CACHE_FORMAT_VERSION,
                    "global_spine_cache_key": cache_key,
                },
            )
            conn.commit()
            conn.execute("PRAGMA journal_mode=DELETE").fetchall()
        _cleanup_spine_database_and_seal(cache_path)
        os.replace(tmp_path, cache_path)
        inherited = {
            **verification,
            "path": str(cache_path),
            "integrity_source": "immutable_seal",
            "verification_mode": "deep-sealed-inherited",
        }
        write_spine_verification_seal(
            cache_path,
            inherited,
            source_path=source_path,
            details={"bootstrap": "compatible-current-release", "cache_key": cache_key},
        )
        errors = _verify_global_spine_cache(cache_path, cache_key=cache_key)
        if errors:
            raise RuntimeError("bootstrapped global spine cache invalid: " + ", ".join(errors))
        return {
            "adopted": True,
            "source_release": current_root.name,
            "copy_mode": copy_mode,
        }
    finally:
        _cleanup_spine_database_and_seal(tmp_path)


def _restore_global_spine_cache(
    cache_path: Path,
    target_path: Path,
    *,
    cache_key: str,
    release_root: Path,
    release_id: str,
) -> tuple[GlobalSpineMergeResult, str]:
    errors = _verify_global_spine_cache(cache_path, cache_key=cache_key)
    if errors:
        raise ValueError("invalid global spine cache: " + ", ".join(errors))
    cache_verification = verify_global_spine_schema(cache_path, deep=True, trust_seal=True)
    target_path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = target_path.parent / f".{target_path.name}.{os.getpid()}.{time.time_ns()}.tmp"
    _cleanup_spine_database_and_seal(tmp_path)
    try:
        copy_mode = clone_or_copy_immutable_file(cache_path, tmp_path)
        _rebase_global_spine_release_paths(
            tmp_path,
            release_root=release_root,
            release_id=release_id,
        )
        restored = _verify_restored_global_spine(
            tmp_path,
            cache_key=cache_key,
            release_id=release_id,
        )
        if not restored["ok"]:
            raise RuntimeError(
                "restored global spine cache metadata invalid: " + ", ".join(restored["errors"])
            )
        _cleanup_spine_database_and_seal(target_path)
        os.replace(tmp_path, target_path)
        verification = {
            **cache_verification,
            "path": str(target_path),
            "metadata": restored["metadata"],
            "integrity_source": "immutable_seal",
            "verification_mode": "deep-sealed-inherited",
        }
        write_spine_verification_seal(
            target_path,
            verification,
            source_path=cache_path,
            details={"restore_mode": "metadata-rebind-from-immutable-cache"},
        )
        metadata = restored["metadata"]
        raw_counts = metadata.get("counts")
        counts = {
            str(key): int(value)
            for key, value in dict(raw_counts or {}).items()
            if isinstance(value, int) and not isinstance(value, bool)
        }
        raw_links = metadata.get("chain_links")
        links = dict(raw_links) if isinstance(raw_links, Mapping) else {}
        result = GlobalSpineMergeResult(
            global_spine_path=target_path,
            fragment_count=int(metadata.get("fragment_count") or 0),
            counts=counts,
            chain_links=CrossCompanyLinkGenerationResult(
                inserted=int(links.get("inserted") or 0),
                exact_links=int(links.get("exact_links") or 0),
                similarity_links=int(links.get("similarity_links") or 0),
                key_count=int(links.get("key_count") or 0),
                skipped_generic_keys=int(links.get("skipped_generic_keys") or 0),
            ),
            verification={
                **verification,
                "runtime": {
                    "cache_hit": True,
                    "cache_key": cache_key,
                    "copy_mode": copy_mode,
                },
            },
        )
        return result, copy_mode
    finally:
        _cleanup_spine_database_and_seal(tmp_path)


def _materialize_router_sidecar(
    *,
    global_spine_path: Path,
    router_sidecar_path: Path,
    release_id: str,
    fragment_results: Sequence[SpineFragmentResult],
    cache_root: Path,
    source_manifest_hash: str | None,
    generate_links: bool,
    no_cache: bool,
) -> tuple[RouterSidecarBuildResult, dict[str, Any]]:
    """Restore a semantically identical sidecar or build and publish one."""
    started_at = time.perf_counter()
    summary: dict[str, Any] = {
        "enabled": not no_cache,
        "hit": False,
        "copy_mode": None,
        "publish_mode": None,
        "probe_errors": [],
    }
    try:
        cache_key = router_sidecar_semantic_cache_key(
            fragment_cache_keys=[
                (result.ticker, str(result.cache_key or "")) for result in fragment_results
            ],
            source_manifest_hash=source_manifest_hash,
            generate_links=generate_links,
            global_spine_schema_version=GLOBAL_SPINE_SCHEMA_VERSION,
            global_spine_builder_version=GLOBAL_SPINE_BUILDER_VERSION,
            spine_projection_version=SPINE_PROJECTION_VERSION,
            cross_company_link_builder_version=CROSS_COMPANY_LINK_BUILDER_VERSION,
            metric_dictionary=metric_dictionary_binding(),
        )
    except (TypeError, ValueError) as exc:
        summary["enabled"] = False
        summary["key_error"] = f"{type(exc).__name__}:{exc}"
        cache_key = None

    cache_path = router_sidecar_cache_path(cache_root, cache_key) if cache_key is not None else None
    summary["key"] = cache_key
    summary["path"] = str(cache_path) if cache_path is not None else None

    if not no_cache and cache_key is not None and cache_path is not None:
        probe_errors = list(verify_router_sidecar_cache(cache_path, cache_key=cache_key))
        summary["probe_errors"] = probe_errors
        if not probe_errors:
            try:
                restored = restore_router_sidecar_cache(
                    cache_path,
                    router_sidecar_path,
                    cache_key=cache_key,
                    global_spine_path=global_spine_path,
                    release_id=release_id,
                    copy_file=clone_or_copy_immutable_file,
                )
            except (OSError, RuntimeError, TypeError, ValueError, sqlite3.Error) as exc:
                summary["restore_error"] = f"{type(exc).__name__}:{exc}"
            else:
                verification = restored.verification
                summary.update(
                    {
                        "hit": True,
                        "copy_mode": restored.copy_mode,
                        "elapsed_ms": int((time.perf_counter() - started_at) * 1000),
                    }
                )
                return (
                    RouterSidecarBuildResult(
                        path=restored.path,
                        counts=verification.get("counts") or {},
                        metadata=verification.get("metadata") or {},
                        verification=verification,
                        elapsed_ms=int((time.perf_counter() - started_at) * 1000),
                    ),
                    summary,
                )

    result = build_router_sidecar(
        global_spine_path,
        router_sidecar_path,
        release_id=release_id,
    )
    if not no_cache and cache_key is not None and cache_path is not None:
        try:
            summary["publish_mode"] = publish_router_sidecar_cache(
                result.path,
                cache_path,
                cache_key=cache_key,
                copy_file=clone_or_copy_immutable_file,
            )
        except (OSError, RuntimeError, TypeError, ValueError, sqlite3.Error) as exc:
            # Cache population is an optimization. The freshly built and deeply
            # verified release artifact remains authoritative.
            summary["publish_error"] = f"{type(exc).__name__}:{exc}"
    summary["elapsed_ms"] = int((time.perf_counter() - started_at) * 1000)
    return result, summary


def build_spine_shard_release_outputs(
    root: Path,
    *,
    release_id: str,
    workers: int | None = None,
    cache_root: Path | None = None,
    source_manifest_path: Path | None = None,
    progress_path: Path | None = None,
    no_cache: bool = False,
    generate_links: bool = True,
) -> SpineShardReleaseBuildResult:
    """Build all v3 production index outputs for an already-materialized release root."""
    resolved_root = root.expanduser().resolve()
    if not resolved_root.is_dir():
        raise FileNotFoundError(f"Release root not found: {resolved_root}")
    indexes_dir = resolved_root / "indexes"
    companies_dir = indexes_dir / "companies"
    fragments_dir = indexes_dir / "fragments" / "spine"
    global_spine_path = indexes_dir / "global_spine.sqlite"
    router_sidecar_path = resolved_root / ROUTER_SIDECAR_RELATIVE_PATH
    router_coherence_path = resolved_root / ROUTER_COHERENCE_RELATIVE_PATH
    shard_manifest_path = indexes_dir / "shard_manifest.json"
    chart_series_path = resolved_root / CHART_SERIES_RELATIVE_PATH
    build_plan_path = indexes_dir / "build_plan.json"
    build_summary_path = indexes_dir / "build_summary.json"
    company_identity_preflight_path = indexes_dir / "company_identity_preflight.json"
    semantic_preflight_path = indexes_dir / "semantic_preflight.json"
    resolved_source_manifest_path = (
        source_manifest_path.expanduser().resolve()
        if source_manifest_path is not None
        else resolved_root / "source_manifest.json"
    )
    resolved_progress_path = (
        progress_path.expanduser().resolve()
        if progress_path is not None
        else indexes_dir / "build_progress.jsonl"
    )

    indexes_dir.mkdir(parents=True, exist_ok=True)
    progress = _BuildProgressWriter(
        resolved_progress_path, release_id=release_id, release_root=resolved_root
    )
    build_started_at = time.perf_counter()
    progress.record(
        "build",
        "build",
        "started",
        details={
            "root": str(resolved_root),
            "index_layout": GLOBAL_SPINE_LAYOUT,
            "no_cache": no_cache,
        },
    )
    try:
        static_preflight_started_at = time.perf_counter()
        progress.record("static_preflight", "static_preflight", "started")
        runtime_capabilities = _validate_static_build_runtime()
        ranking_profile = load_router_ranking_profile()
        coherence_profile = load_router_coherence_profile()
        progress.record(
            "static_preflight",
            "static_preflight",
            "complete",
            details={
                "ranking_profile_id": ranking_profile.get("profile_id"),
                "ranking_profile_version": ranking_profile.get("version"),
                "coherence_profile_id": coherence_profile.get("profile_id"),
                "coherence_profile_version": coherence_profile.get("version"),
                "semantic_identity_policy_version": SEMANTIC_IDENTITY_POLICY_VERSION,
                "spine_projection_version": SPINE_PROJECTION_VERSION,
                "runtime_capabilities": runtime_capabilities,
            },
            started_at=static_preflight_started_at,
        )
        source_started_at = time.perf_counter()
        source_manifest = write_source_artifact_manifest(
            resolved_root,
            manifest_path=resolved_source_manifest_path,
        )
        progress.record(
            "source_manifest",
            "source_discovery",
            "complete",
            output=resolved_source_manifest_path,
            details={
                "artifact_count": source_manifest.get("artifact_count"),
                "manifest_hash": source_manifest.get("manifest_hash"),
            },
            started_at=source_started_at,
        )
        plan_started_at = time.perf_counter()
        progress.record(
            "build_plan",
            "build_plan",
            "started",
            output=build_plan_path,
        )
        plan = _plan_v3_artifact_inputs(
            resolved_root,
            sqlite_path=global_spine_path,
            cache_root=cache_root,
            workers=workers,
            source_manifest_path=resolved_source_manifest_path,
            source_manifest_payload=source_manifest,
        )
        if not plan.company_items:
            raise ValueError("No company artifacts found for v3 release build")

        cache_probe_started_at = time.perf_counter()
        progress.record(
            "cache_probe",
            "cache_probe",
            "started",
            details={
                "company_count": len(plan.company_items),
                "worker_count": min(plan.workers, len(plan.company_items)),
            },
        )
        company_specs = _company_build_specs(
            plan,
            companies_dir=companies_dir,
            no_cache=no_cache,
            artifact_workers=1,
        )
        parallelism = _resolve_build_parallelism(
            plan,
            artifact_count=len(plan.items),
            company_count=len(company_specs),
        )
        build_plan = {
            "format": V3_BUILD_PLAN_FORMAT_VERSION,
            "release_id": release_id,
            "index_layout": GLOBAL_SPINE_LAYOUT,
            "source_manifest_path": _path_label(resolved_root, resolved_source_manifest_path),
            "source_manifest_hash": plan.source_manifest_hash,
            "metric_dictionary": metric_dictionary_binding(),
            "progress_path": _path_label(resolved_root, resolved_progress_path),
            "no_cache": no_cache,
            "company_count": len(company_specs),
            "worker_count": parallelism.company_workers,
            "parallelism": parallelism.to_dict(),
            "plan": plan.to_dict(include_items=True),
            "companies": [
                {
                    "ticker": spec["ticker"],
                    "artifact_count": len(spec["company_plan"].items),
                    "shard_path": _path_label(resolved_root, spec["shard_path"]),
                    "cache_hit": spec["company_plan"].company_items[0].cache_hit,
                    "cache_key": spec["company_plan"].company_items[0].cache_key,
                    "estimated_cost": spec["estimated_cost"],
                }
                for spec in company_specs
            ],
        }
        _write_json(build_plan_path, build_plan)
        progress.record(
            "cache_probe",
            "cache_probe",
            "complete",
            details={
                "company_count": len(company_specs),
                "company_cache_hits": sum(
                    1 for spec in company_specs if spec["company_plan"].company_items[0].cache_hit
                ),
                "spine_fragment_cache_hits": sum(
                    1
                    for spec in company_specs
                    if _plan_spine_fragment_cache_hit(
                        spec,
                        release_id=release_id,
                        source_manifest_hash=plan.source_manifest_hash,
                        cache_root=plan.cache_root,
                        no_cache=no_cache,
                    )
                ),
            },
            started_at=cache_probe_started_at,
        )
        progress.record(
            "build_plan",
            "build_plan",
            "complete",
            output=build_plan_path,
            details={
                "company_count": len(company_specs),
                "worker_count": parallelism.company_workers,
                "parallelism": parallelism.to_dict(),
            },
            started_at=plan_started_at,
        )

        dirty_company_tickers = {
            str(spec["ticker"])
            for spec in company_specs
            if no_cache or not bool(spec["company_plan"].company_items[0].cache_hit)
        }
        precompile_items = tuple(
            item
            for item in plan.items
            if item.ticker in dirty_company_tickers and (no_cache or not item.cache_hit)
        )
        precompile_started_at = time.perf_counter()
        progress.record(
            "artifact_fragments",
            "artifact_fragment",
            "started",
            details={
                "total": len(precompile_items),
                "workers": min(parallelism.artifact_compile_workers, max(1, len(precompile_items))),
                "force": no_cache,
            },
        )
        precompile_summary = precompile_source_artifact_fragments(
            plan,
            items=precompile_items,
            workers=parallelism.artifact_compile_workers,
            force=no_cache,
        )
        progress.record(
            "artifact_fragments",
            "artifact_fragment",
            "complete",
            details={
                **precompile_summary,
                "workers": min(parallelism.artifact_compile_workers, max(1, len(precompile_items))),
                "force": no_cache,
            },
            started_at=precompile_started_at,
        )

        shard_stage_started_at = time.perf_counter()
        progress.record(
            "company_shards",
            "company_shard",
            "started",
            details={"total": len(company_specs), "workers": parallelism.company_workers},
        )
        shard_results = _build_company_shards_for_release(
            resolved_root,
            company_specs=company_specs,
            workers=parallelism.company_workers,
            no_cache=no_cache,
            fragments_prepared=True,
            progress=progress,
        )
        progress.record(
            "company_shards",
            "company_shard",
            "complete",
            details={
                "total": len(shard_results),
                "cache_hits": sum(1 for result in shard_results if result.cache_hit),
                "rebuilt": sum(1 for result in shard_results if not result.cache_hit),
            },
            started_at=shard_stage_started_at,
        )

        company_preflight_started_at = time.perf_counter()
        progress.record(
            "company_identity_preflight",
            "company_identity_preflight",
            "started",
            output=company_identity_preflight_path,
            details={"company_count": len(shard_results)},
        )
        company_identity_preflight_result = preflight_company_shard_identities(
            {result.ticker: result.shard_path for result in shard_results},
            report_path=company_identity_preflight_path,
            workers=parallelism.spine_fragment_workers,
        )
        if not company_identity_preflight_result["ok"]:
            error = _semantic_preflight_error(company_identity_preflight_result)
            progress.record(
                "company_identity_preflight",
                "company_identity_preflight",
                "failed",
                output=company_identity_preflight_path,
                error=str(error),
                details={
                    "conflict_count": company_identity_preflight_result["conflict_count"],
                    "warning_count": company_identity_preflight_result["warning_count"],
                },
                started_at=company_preflight_started_at,
            )
            raise error
        progress.record(
            "company_identity_preflight",
            "company_identity_preflight",
            "complete",
            output=company_identity_preflight_path,
            details={
                "claim_row_count": company_identity_preflight_result["claim_row_count"],
                "unique_claim_count": company_identity_preflight_result["unique_claim_count"],
                "conflict_count": 0,
                "warning_count": company_identity_preflight_result["warning_count"],
                "warning_kinds": company_identity_preflight_result["warning_kinds"],
            },
            started_at=company_preflight_started_at,
        )

        manifest_started_at = time.perf_counter()
        shard_manifest = _write_v3_shard_manifest(
            shard_manifest_path,
            release_root=resolved_root,
            release_id=release_id,
            source_manifest_hash=plan.source_manifest_hash,
            shard_results=shard_results,
        )
        progress.record(
            "shard_manifest",
            "manifest",
            "complete",
            output=shard_manifest_path,
            details={"ticker_count": shard_manifest.get("ticker_count")},
            started_at=manifest_started_at,
        )

        fragment_stage_started_at = time.perf_counter()
        progress.record(
            "spine_fragments",
            "spine_fragment",
            "started",
            details={"total": len(shard_results), "workers": parallelism.spine_fragment_workers},
        )
        fragment_results = _emit_spine_fragments_for_release(
            shard_results,
            fragments_dir=fragments_dir,
            release_id=release_id,
            source_manifest_hash=plan.source_manifest_hash,
            cache_root=plan.cache_root,
            workers=parallelism.spine_fragment_workers,
            no_cache=no_cache,
            progress=progress,
        )
        progress.record(
            "spine_fragments",
            "spine_fragment",
            "complete",
            details={
                "total": len(fragment_results),
                "cache_hits": sum(1 for result in fragment_results if result.cache_hit),
                "rebuilt": sum(1 for result in fragment_results if not result.cache_hit),
            },
            started_at=fragment_stage_started_at,
        )

        merge_started_at = time.perf_counter()
        global_spine_cache_summary: dict[str, Any] = {
            "enabled": not no_cache,
            "hit": False,
            "key": None,
            "path": None,
            "probe_errors": [],
        }
        global_spine_cache_key: str | None = None
        global_spine_cache_path: Path | None = None
        if not no_cache:
            try:
                global_spine_cache_key = _global_spine_semantic_cache_key(
                    fragment_results,
                    source_manifest_hash=plan.source_manifest_hash,
                    generate_links=generate_links,
                )
                global_spine_cache_path = _global_spine_cache_path(
                    plan.cache_root,
                    global_spine_cache_key,
                )
            except (TypeError, ValueError) as exc:
                global_spine_cache_summary["enabled"] = False
                global_spine_cache_summary["key_error"] = f"{type(exc).__name__}:{exc}"
        global_spine_cache_summary["key"] = global_spine_cache_key
        global_spine_cache_summary["path"] = (
            str(global_spine_cache_path) if global_spine_cache_path is not None else None
        )
        progress.record(
            "global_spine_merge",
            "global_spine_merge",
            "started",
            output=global_spine_path,
            details={
                "fragment_count": len(fragment_results),
                "generate_links": generate_links,
                "cache_key": global_spine_cache_key,
            },
        )
        merge_result: GlobalSpineMergeResult | None = None
        if global_spine_cache_path is not None and global_spine_cache_key is not None:
            cache_errors = _verify_global_spine_cache(
                global_spine_cache_path,
                cache_key=global_spine_cache_key,
            )
            if cache_errors == ("global_spine_cache_missing",):
                bootstrap = _bootstrap_global_spine_cache_from_current(
                    resolved_root,
                    global_spine_cache_path,
                    cache_key=global_spine_cache_key,
                    source_manifest_hash=plan.source_manifest_hash,
                    generate_links=generate_links,
                    fragment_count=len(fragment_results),
                )
                global_spine_cache_summary["bootstrap"] = bootstrap
                if bootstrap.get("adopted") is True:
                    cache_errors = _verify_global_spine_cache(
                        global_spine_cache_path,
                        cache_key=global_spine_cache_key,
                    )
            global_spine_cache_summary["probe_errors"] = list(cache_errors)
            if not cache_errors:
                try:
                    merge_result, copy_mode = _restore_global_spine_cache(
                        global_spine_cache_path,
                        global_spine_path,
                        cache_key=global_spine_cache_key,
                        release_root=resolved_root,
                        release_id=release_id,
                    )
                except (OSError, RuntimeError, TypeError, ValueError, sqlite3.Error) as exc:
                    global_spine_cache_summary["restore_error"] = f"{type(exc).__name__}:{exc}"
                else:
                    global_spine_cache_summary.update({"hit": True, "copy_mode": copy_mode})

        if merge_result is not None:
            semantic_preflight_result = {
                "ok": True,
                "cached": True,
                "cache_key": global_spine_cache_key,
                "claim_row_count": 0,
                "unique_claim_count": 0,
                "equivalent_collision_count": 0,
                "conflict_count": 0,
                "warning_count": 0,
            }
            progress.record(
                "semantic_preflight",
                "semantic_preflight",
                "cached",
                output=semantic_preflight_path,
                details={"global_spine_cache_key": global_spine_cache_key},
            )
        else:
            semantic_preflight_started_at = time.perf_counter()
            progress.record(
                "semantic_preflight",
                "semantic_preflight",
                "started",
                output=semantic_preflight_path,
                details={"fragment_count": len(fragment_results)},
            )
            semantic_preflight_result = preflight_spine_fragments(
                [result.fragment_path for result in fragment_results],
                report_path=semantic_preflight_path,
            )
            if not semantic_preflight_result["ok"]:
                progress.record(
                    "semantic_preflight",
                    "semantic_preflight",
                    "failed",
                    output=semantic_preflight_path,
                    error=str(_semantic_preflight_error(semantic_preflight_result)),
                    details={"conflict_count": semantic_preflight_result["conflict_count"]},
                    started_at=semantic_preflight_started_at,
                )
                raise _semantic_preflight_error(semantic_preflight_result)
            progress.record(
                "semantic_preflight",
                "semantic_preflight",
                "complete",
                output=semantic_preflight_path,
                details={
                    "claim_row_count": semantic_preflight_result["claim_row_count"],
                    "unique_claim_count": semantic_preflight_result["unique_claim_count"],
                    "equivalent_collision_count": semantic_preflight_result[
                        "equivalent_collision_count"
                    ],
                    "conflict_count": 0,
                },
                started_at=semantic_preflight_started_at,
            )
            merge_result = merge_spine_fragments(
                [result.fragment_path for result in fragment_results],
                global_spine_path,
                release_id=release_id,
                source_manifest_hash=plan.source_manifest_hash,
                generate_links=generate_links,
                semantic_preflight=False,
                semantic_cache_key=global_spine_cache_key,
                progress_callback=lambda event: progress.record(
                    str(event.get("node_id") or "global_spine_merge"),
                    str(event.get("stage") or "global_spine_merge"),
                    str(event.get("status") or "progress"),
                    output=event.get("output"),
                    details=(
                        event.get("details") if isinstance(event.get("details"), Mapping) else None
                    ),
                ),
            )
            if global_spine_cache_path is not None and global_spine_cache_key is not None:
                try:
                    global_spine_cache_summary["publish_mode"] = _publish_global_spine_cache(
                        merge_result.global_spine_path,
                        global_spine_cache_path,
                        cache_key=global_spine_cache_key,
                        verification=merge_result.verification,
                    )
                except (OSError, RuntimeError, TypeError, ValueError, sqlite3.Error) as exc:
                    global_spine_cache_summary["publish_error"] = f"{type(exc).__name__}:{exc}"
        progress.record(
            "global_spine_merge",
            "global_spine_merge",
            "complete",
            output=global_spine_path,
            details={
                "fragment_count": merge_result.fragment_count,
                "counts": dict(merge_result.counts),
                "chain_links_inserted": merge_result.chain_links.inserted,
                "cache": global_spine_cache_summary,
            },
            started_at=merge_started_at,
        )

        router_sidecar_started_at = time.perf_counter()
        progress.record(
            "router_sidecar",
            "router_sidecar",
            "started",
            output=router_sidecar_path,
            details={
                "source": _path_label(resolved_root, global_spine_path),
            },
        )
        router_sidecar_result, router_sidecar_cache_summary = _materialize_router_sidecar(
            global_spine_path=global_spine_path,
            router_sidecar_path=router_sidecar_path,
            release_id=release_id,
            fragment_results=fragment_results,
            cache_root=plan.cache_root,
            source_manifest_hash=plan.source_manifest_hash,
            generate_links=generate_links,
            no_cache=no_cache,
        )
        progress.record(
            "router_sidecar",
            "router_sidecar",
            "complete",
            output=router_sidecar_path,
            details={
                "counts": dict(router_sidecar_result.counts),
                "ranking_profile_sha256": router_sidecar_result.metadata.get(
                    "ranking_profile_sha256"
                ),
                "build_fingerprint_sha256": router_sidecar_result.metadata.get(
                    "build_fingerprint_sha256"
                ),
                "build_settings": router_sidecar_result.metadata.get("build_settings"),
                "ok": bool(router_sidecar_result.verification.get("ok")),
                "cache": router_sidecar_cache_summary,
            },
            started_at=router_sidecar_started_at,
        )

        router_coherence_started_at = time.perf_counter()
        progress.record(
            "router_coherence",
            "router_coherence",
            "started",
            output=router_coherence_path,
            details={"source": _path_label(resolved_root, global_spine_path)},
        )
        router_coherence_result = build_router_coherence(
            global_spine_path,
            router_coherence_path,
            release_id=release_id,
        )
        progress.record(
            "router_coherence",
            "router_coherence",
            "complete",
            output=router_coherence_path,
            details={
                "counts": dict(router_coherence_result.counts),
                "profile_sha256": router_coherence_result.metadata.get("profile_sha256"),
                "build_fingerprint_sha256": router_coherence_result.metadata.get(
                    "build_fingerprint_sha256"
                ),
                "ok": bool(router_coherence_result.verification.get("ok")),
            },
            started_at=router_coherence_started_at,
        )

        chart_series_started_at = time.perf_counter()
        chart_series_result: ChartSeriesBuildResult | None = None
        chart_series_summary: dict[str, Any]
        progress.record(
            "chart_series",
            "chart_series",
            "started",
            output=chart_series_path,
        )
        try:
            chart_series_result = build_chart_series_index(
                resolved_root,
                shard_manifest_path=shard_manifest_path,
                output_path=chart_series_path,
                release_id=release_id,
                source_manifest_hash=plan.source_manifest_hash,
            )
            chart_series_summary = {
                "status": "complete",
                "path": _path_label(resolved_root, chart_series_result.path),
                "counts": dict(chart_series_result.counts),
                "verification": dict(chart_series_result.verification),
                "elapsed_ms": chart_series_result.elapsed_ms,
            }
            progress.record(
                "chart_series",
                "chart_series",
                "complete",
                output=chart_series_result.path,
                details={
                    "counts": dict(chart_series_result.counts),
                    "ok": bool(chart_series_result.verification.get("ok")),
                },
                started_at=chart_series_started_at,
            )
        except Exception as exc:
            chart_series_summary = {
                "status": "failed",
                "path": _path_label(resolved_root, chart_series_path),
                "error": str(exc),
            }
            progress.record(
                "chart_series",
                "chart_series",
                "failed",
                output=chart_series_path,
                error=str(exc),
                started_at=chart_series_started_at,
            )
        cleanup_started_at = time.perf_counter()
        fragment_cleanup = _cleanup_release_spine_fragments(
            fragments_dir,
            release_root=resolved_root,
        )
        progress.record(
            "spine_fragments",
            "artifact_cleanup",
            "complete",
            output=fragments_dir,
            details=fragment_cleanup,
            started_at=cleanup_started_at,
        )
        build_summary = {
            "format": V3_BUILD_SUMMARY_FORMAT_VERSION,
            "release_id": release_id,
            "index_layout": GLOBAL_SPINE_LAYOUT,
            "source_manifest_path": _path_label(resolved_root, resolved_source_manifest_path),
            "source_manifest_hash": plan.source_manifest_hash,
            "progress_path": _path_label(resolved_root, resolved_progress_path),
            "artifact_count": len(plan.items),
            "company_count": len(shard_results),
            "parallelism": parallelism.to_dict(),
            "artifact_fragment_precompile": precompile_summary,
            "company_shard_cache": {
                "hits": sum(1 for result in shard_results if result.cache_hit),
                "misses": sum(1 for result in shard_results if not result.cache_hit),
            },
            "spine_fragment_cache": {
                "hits": sum(1 for result in fragment_results if result.cache_hit),
                "misses": sum(1 for result in fragment_results if not result.cache_hit),
            },
            "company_identity_preflight": dict(company_identity_preflight_result),
            "semantic_preflight": dict(semantic_preflight_result),
            "global_spine": {
                "path": _path_label(resolved_root, merge_result.global_spine_path),
                "counts": dict(merge_result.counts),
                "runtime": dict(merge_result.verification.get("runtime") or {}),
                "cache": global_spine_cache_summary,
                "chain_links": {
                    "inserted": merge_result.chain_links.inserted,
                    "exact_links": merge_result.chain_links.exact_links,
                    "similarity_links": merge_result.chain_links.similarity_links,
                    "key_count": merge_result.chain_links.key_count,
                    "skipped_generic_keys": merge_result.chain_links.skipped_generic_keys,
                },
            },
            "router_sidecar": {
                "path": _path_label(resolved_root, router_sidecar_result.path),
                "schema_version": router_sidecar_result.metadata.get("schema_version"),
                "counts": dict(router_sidecar_result.counts),
                "source_global_spine_sha256": router_sidecar_result.metadata.get(
                    "source_global_spine_sha256"
                ),
                "ranking_profile_id": router_sidecar_result.metadata.get("ranking_profile_id"),
                "ranking_profile_sha256": router_sidecar_result.metadata.get(
                    "ranking_profile_sha256"
                ),
                "content_sha256": router_sidecar_result.metadata.get("content_sha256"),
                "build_fingerprint_sha256": router_sidecar_result.metadata.get(
                    "build_fingerprint_sha256"
                ),
                "build_settings": router_sidecar_result.metadata.get("build_settings"),
                "verification": dict(router_sidecar_result.verification),
                "elapsed_ms": router_sidecar_result.elapsed_ms,
                "cache": router_sidecar_cache_summary,
            },
            "router_coherence": {
                "path": _path_label(resolved_root, router_coherence_result.path),
                "schema_version": router_coherence_result.metadata.get("schema_version"),
                "counts": dict(router_coherence_result.counts),
                "source_global_spine_sha256": router_coherence_result.metadata.get(
                    "source_global_spine_sha256"
                ),
                "profile_id": router_coherence_result.metadata.get("profile_id"),
                "profile_sha256": router_coherence_result.metadata.get("profile_sha256"),
                "build_fingerprint_sha256": router_coherence_result.metadata.get(
                    "build_fingerprint_sha256"
                ),
                "verification": dict(router_coherence_result.verification),
                "elapsed_ms": router_coherence_result.elapsed_ms,
            },
            "chart_series": chart_series_summary,
            "artifact_cleanup": {
                "spine_fragments": fragment_cleanup,
            },
            "shards": dict(shard_manifest.get("shards") or {}),
            "created_at": datetime.now(timezone.utc).isoformat(),
        }
        summary_started_at = time.perf_counter()
        _write_json(build_summary_path, build_summary)
        progress.record(
            "build_summary",
            "build_summary",
            "complete",
            output=build_summary_path,
            details={
                "artifact_count": len(plan.items),
                "company_count": len(shard_results),
            },
            started_at=summary_started_at,
        )
        progress.record(
            "build",
            "build",
            "complete",
            details={
                "artifact_count": len(plan.items),
                "company_count": len(shard_results),
                "global_spine": _path_label(resolved_root, global_spine_path),
                "router_sidecar": _path_label(resolved_root, router_sidecar_path),
                "router_coherence": _path_label(resolved_root, router_coherence_path),
                "chart_series": _path_label(resolved_root, chart_series_path)
                if chart_series_path.exists()
                else None,
            },
            started_at=build_started_at,
        )
        return SpineShardReleaseBuildResult(
            release_root=resolved_root,
            release_id=release_id,
            source_manifest_path=resolved_source_manifest_path,
            build_plan_path=build_plan_path,
            shard_manifest_path=shard_manifest_path,
            build_summary_path=build_summary_path,
            global_spine_path=global_spine_path,
            router_sidecar_path=router_sidecar_path,
            router_coherence_path=router_coherence_path,
            shard_results=tuple(sorted(shard_results, key=lambda result: result.ticker)),
            fragment_results=tuple(sorted(fragment_results, key=lambda result: result.ticker)),
            merge_result=merge_result,
            shard_manifest=shard_manifest,
            build_summary=build_summary,
            router_sidecar_result=router_sidecar_result,
            router_coherence_result=router_coherence_result,
            chart_series_path=chart_series_path if chart_series_path.exists() else None,
            chart_series_result=chart_series_result,
            progress_path=resolved_progress_path,
        )
    except Exception as exc:
        progress.record("build", "build", "failed", error=str(exc), started_at=build_started_at)
        raise


def plan_spine_shard_release_outputs(
    root: Path,
    *,
    release_id: str | None = None,
    workers: int | None = None,
    cache_root: Path | None = None,
    source_manifest_path: Path | None = None,
    no_cache: bool = False,
    generate_links: bool = True,
) -> dict[str, Any]:
    """Preview the v3 release build DAG without writing release outputs."""
    resolved_root = root.expanduser().resolve()
    if not resolved_root.is_dir():
        raise FileNotFoundError(f"Release root not found: {resolved_root}")
    load_router_ranking_profile()
    load_router_coherence_profile()
    indexes_dir = resolved_root / "indexes"
    companies_dir = indexes_dir / "companies"
    fragments_dir = indexes_dir / "fragments" / "spine"
    global_spine_path = indexes_dir / "global_spine.sqlite"
    router_sidecar_path = resolved_root / ROUTER_SIDECAR_RELATIVE_PATH
    router_coherence_path = resolved_root / ROUTER_COHERENCE_RELATIVE_PATH
    shard_manifest_path = indexes_dir / "shard_manifest.json"
    chart_series_path = resolved_root / CHART_SERIES_RELATIVE_PATH
    build_plan_path = indexes_dir / "build_plan.json"
    build_summary_path = indexes_dir / "build_summary.json"
    company_identity_preflight_path = indexes_dir / "company_identity_preflight.json"
    semantic_preflight_path = indexes_dir / "semantic_preflight.json"
    resolved_source_manifest_path = (
        source_manifest_path.expanduser().resolve()
        if source_manifest_path is not None
        else resolved_root / "source_manifest.json"
    )
    source_manifest = build_source_artifact_manifest_payload(
        resolved_root,
        manifest_path=resolved_source_manifest_path,
    )
    plan = _plan_v3_artifact_inputs(
        resolved_root,
        sqlite_path=global_spine_path,
        cache_root=cache_root,
        workers=workers,
        source_manifest_path=resolved_source_manifest_path,
        source_manifest_payload=source_manifest,
    )
    plan = replace(
        plan,
        source_manifest_path=resolved_source_manifest_path,
        source_manifest_hash=str(source_manifest["manifest_hash"]),
        discovery_mode="in-memory-source-manifest",
    )
    company_specs = _company_build_specs(
        plan,
        companies_dir=companies_dir,
        no_cache=no_cache,
        artifact_workers=1,
    )
    parallelism = _resolve_build_parallelism(
        plan,
        artifact_count=len(plan.items),
        company_count=len(company_specs),
    )

    companies: list[dict[str, Any]] = []
    dirty_tickers: set[str] = set()
    for spec in company_specs:
        ticker = str(spec["ticker"])
        company_plan = spec["company_plan"]
        company_item = company_plan.company_items[0]
        company_cache_hit = False if no_cache else bool(company_item.cache_hit)
        fragment_cache_key = _spine_fragment_cache_key(
            ticker=ticker,
            company_cache_key=company_item.cache_key,
            source_manifest_hash=plan.source_manifest_hash,
        )
        fragment_cache_path = _spine_fragment_cache_path(plan.cache_root, fragment_cache_key)
        fragment_cache_errors = (
            ("no_cache",)
            if no_cache
            else _verify_spine_fragment_cache(
                fragment_cache_path, ticker=ticker, cache_key=fragment_cache_key
            )
        )
        fragment_cache_hit = False if no_cache else not fragment_cache_errors
        if not company_cache_hit or not fragment_cache_hit:
            dirty_tickers.add(ticker)
        companies.append(
            {
                "ticker": ticker,
                "artifact_count": len(company_plan.items),
                "dirty_artifact_count": len(company_plan.dirty_items),
                "cached_artifact_count": len(company_plan.cached_items),
                "company_cache_hit": company_cache_hit,
                "company_cache_key": company_item.cache_key,
                "company_cache_errors": list(
                    company_item.cache_errors if not no_cache else ("no_cache",)
                ),
                "company_shard_path": _path_label(resolved_root, spec["shard_path"]),
                "spine_fragment_path": _path_label(
                    resolved_root, fragments_dir / f"{_safe_ticker_filename(ticker)}.sqlite"
                ),
                "spine_fragment_cache_hit": fragment_cache_hit,
                "spine_fragment_cache_key": fragment_cache_key,
                "spine_fragment_cache_errors": list(fragment_cache_errors),
                "estimated_cost": int(spec["estimated_cost"]),
            }
        )

    router_cache_key: str | None = None
    router_cache_path_value: Path | None = None
    router_cache_errors: tuple[str, ...] = ("no_cache",) if no_cache else ()
    if not no_cache:
        try:
            router_cache_key = router_sidecar_semantic_cache_key(
                fragment_cache_keys=[
                    (str(row["ticker"]), str(row["spine_fragment_cache_key"])) for row in companies
                ],
                source_manifest_hash=plan.source_manifest_hash,
                generate_links=generate_links,
                global_spine_schema_version=GLOBAL_SPINE_SCHEMA_VERSION,
                global_spine_builder_version=GLOBAL_SPINE_BUILDER_VERSION,
                spine_projection_version=SPINE_PROJECTION_VERSION,
                cross_company_link_builder_version=CROSS_COMPANY_LINK_BUILDER_VERSION,
                metric_dictionary=metric_dictionary_binding(),
            )
            router_cache_path_value = router_sidecar_cache_path(
                plan.cache_root,
                router_cache_key,
            )
            router_cache_errors = verify_router_sidecar_cache(
                router_cache_path_value,
                cache_key=router_cache_key,
            )
        except (OSError, TypeError, ValueError, sqlite3.Error) as exc:
            router_cache_errors = (f"{type(exc).__name__}:{exc}",)
    router_cache_hit = not no_cache and not router_cache_errors

    company_nodes = [
        {
            "id": f"company_shard:{row['ticker']}",
            "stage": "company_shard",
            "ticker": row["ticker"],
            "depends_on": ["artifact_fragments"],
            "output": row["company_shard_path"],
            "cache_key": row["company_cache_key"],
            "cache_hit": row["company_cache_hit"],
            "status": "cached" if row["company_cache_hit"] else "rebuild",
            "rebuild_reason": None
            if row["company_cache_hit"]
            else ",".join(row["company_cache_errors"] or ["cache_miss"]),
        }
        for row in companies
    ]
    fragment_nodes = [
        {
            "id": f"spine_fragment:{row['ticker']}",
            "stage": "spine_fragment",
            "ticker": row["ticker"],
            "depends_on": ["company_identity_preflight"],
            "output": row["spine_fragment_path"],
            "cache_key": row["spine_fragment_cache_key"],
            "cache_hit": row["spine_fragment_cache_hit"],
            "status": "cached" if row["spine_fragment_cache_hit"] else "rebuild",
            "rebuild_reason": None
            if row["spine_fragment_cache_hit"]
            else ",".join(row["spine_fragment_cache_errors"] or ["cache_miss"]),
        }
        for row in companies
    ]
    merge_dependencies = [node["id"] for node in fragment_nodes]
    nodes: list[dict[str, Any]] = [
        {
            "id": "static_preflight",
            "stage": "static_preflight",
            "depends_on": [],
            "output": None,
            "cache_hit": False,
            "status": "planned",
        },
        {
            "id": "source_manifest",
            "stage": "source_discovery",
            "depends_on": ["static_preflight"],
            "output": _path_label(resolved_root, resolved_source_manifest_path),
            "cache_hit": False,
            "status": "planned",
        },
        {
            "id": "artifact_fragments",
            "stage": "artifact_fragment",
            "depends_on": ["source_manifest"],
            "output": str(plan.cache_root / "fragments"),
            "cache_hit": False,
            "status": "planned",
            "workers": parallelism.artifact_compile_workers,
        },
        *company_nodes,
        {
            "id": "company_identity_preflight",
            "stage": "company_identity_preflight",
            "depends_on": [node["id"] for node in company_nodes],
            "output": _path_label(resolved_root, company_identity_preflight_path),
            "cache_hit": False,
            "status": "planned",
        },
        {
            "id": "shard_manifest",
            "stage": "manifest",
            "depends_on": ["company_identity_preflight"],
            "output": _path_label(resolved_root, shard_manifest_path),
            "cache_hit": False,
            "status": "planned",
        },
        *fragment_nodes,
        {
            "id": "semantic_preflight",
            "stage": "semantic_preflight",
            "depends_on": merge_dependencies,
            "output": _path_label(resolved_root, semantic_preflight_path),
            "cache_hit": False,
            "status": "planned",
        },
        {
            "id": "global_spine_merge",
            "stage": "global_spine_merge",
            "depends_on": ["semantic_preflight"],
            "output": _path_label(resolved_root, global_spine_path),
            "cache_hit": False,
            "status": "rebuild",
        },
        {
            "id": "cross_company_links",
            "stage": "cross_company_links",
            "depends_on": ["global_spine_merge"],
            "output": _path_label(resolved_root, global_spine_path),
            "cache_hit": False,
            "status": "planned",
        },
        {
            "id": "router_sidecar",
            "stage": "router_sidecar",
            "depends_on": ["global_spine_merge", "cross_company_links"],
            "output": _path_label(resolved_root, router_sidecar_path),
            "cache_key": router_cache_key,
            "cache_path": str(router_cache_path_value)
            if router_cache_path_value is not None
            else None,
            "cache_hit": router_cache_hit,
            "cache_errors": list(router_cache_errors),
            "status": "cached" if router_cache_hit else "rebuild",
        },
        {
            "id": "router_coherence",
            "stage": "router_coherence",
            "depends_on": ["global_spine_merge", "cross_company_links"],
            "output": _path_label(resolved_root, router_coherence_path),
            "cache_hit": False,
            "status": "rebuild",
        },
        {
            "id": "chart_series",
            "stage": "chart_series",
            "depends_on": ["shard_manifest"],
            "output": _path_label(resolved_root, chart_series_path),
            "cache_hit": False,
            "status": "planned",
        },
        {
            "id": "release_manifest",
            "stage": "manifest",
            "depends_on": [
                "global_spine_merge",
                "router_sidecar",
                "router_coherence",
                "shard_manifest",
                "chart_series",
            ],
            "output": "manifest.json",
            "cache_hit": False,
            "status": "planned",
        },
        {
            "id": "verification",
            "stage": "verification",
            "depends_on": ["release_manifest", "cross_company_links"],
            "output": "verify/release_verify.json",
            "cache_hit": False,
            "status": "planned",
        },
        {
            "id": "promote_current",
            "stage": "promotion",
            "depends_on": ["verification"],
            "output": "current",
            "cache_hit": False,
            "status": "conditional",
        },
    ]
    return {
        "format": V3_BUILD_PLAN_FORMAT_VERSION,
        "release_id": release_id,
        "index_layout": GLOBAL_SPINE_LAYOUT,
        "source_manifest_path": _path_label(resolved_root, resolved_source_manifest_path),
        "source_manifest_hash": plan.source_manifest_hash,
        "source_manifest": {
            "manifest_hash": source_manifest.get("manifest_hash"),
            "artifact_count": source_manifest.get("artifact_count"),
        },
        "cache_root": str(plan.cache_root),
        "no_cache": no_cache,
        "generate_links": generate_links,
        "artifact_count": len(plan.items),
        "company_count": len(companies),
        "worker_count": parallelism.company_workers if companies else 0,
        "parallelism": parallelism.to_dict(),
        "dirty_tickers": sorted(dirty_tickers),
        "dirty_company_count": sum(1 for row in companies if not row["company_cache_hit"]),
        "cached_company_count": sum(1 for row in companies if row["company_cache_hit"]),
        "dirty_spine_fragment_count": sum(
            1 for row in companies if not row["spine_fragment_cache_hit"]
        ),
        "cached_spine_fragment_count": sum(
            1 for row in companies if row["spine_fragment_cache_hit"]
        ),
        "router_sidecar_cache": {
            "hit": router_cache_hit,
            "key": router_cache_key,
            "path": str(router_cache_path_value) if router_cache_path_value is not None else None,
            "errors": list(router_cache_errors),
        },
        "outputs": {
            "build_plan": _path_label(resolved_root, build_plan_path),
            "build_summary": _path_label(resolved_root, build_summary_path),
            "company_identity_preflight": _path_label(
                resolved_root, company_identity_preflight_path
            ),
            "semantic_preflight": _path_label(resolved_root, semantic_preflight_path),
            "global_spine": _path_label(resolved_root, global_spine_path),
            "router_sidecar": _path_label(resolved_root, router_sidecar_path),
            "router_coherence": _path_label(resolved_root, router_coherence_path),
            "shard_manifest": _path_label(resolved_root, shard_manifest_path),
            "company_shards_dir": _path_label(resolved_root, companies_dir),
            "spine_fragments_dir": _path_label(resolved_root, fragments_dir),
        },
        "nodes": nodes,
        "companies": companies,
        "plan": plan.to_dict(include_items=True),
    }


def build_company_shard_direct(
    root: Path,
    *,
    ticker: str,
    shard_path: Path,
    cache_root: Path | None = None,
    workers: int | None = None,
    source_manifest_path: Path | None = None,
    no_cache: bool = False,
) -> CompanyShardBuildResult:
    """Build one company shard directly from source artifacts without a monolith."""
    normalized_ticker = _normalize_ticker(ticker)
    resolved_root = root.expanduser().resolve()
    resolved_shard_path = shard_path.expanduser().resolve()
    full_plan = _plan_v3_artifact_inputs(
        resolved_root,
        sqlite_path=resolved_shard_path,
        cache_root=cache_root,
        workers=workers,
        source_manifest_path=source_manifest_path,
    )
    company_plan = _filter_plan_for_company(
        full_plan,
        ticker=normalized_ticker,
        shard_path=resolved_shard_path,
        no_cache=no_cache,
    )
    if not company_plan.items:
        raise ValueError(f"No artifact inputs found for ticker: {normalized_ticker}")

    return _build_company_shard_from_plan(
        resolved_root,
        ticker=normalized_ticker,
        shard_path=resolved_shard_path,
        company_plan=company_plan,
        no_cache=no_cache,
    )


def _build_company_shard_from_plan(
    root: Path,
    *,
    ticker: str,
    shard_path: Path,
    company_plan: SourceArtifactSqlitePlan,
    no_cache: bool,
    fragments_prepared: bool = False,
) -> CompanyShardBuildResult:
    normalized_ticker = _normalize_ticker(ticker)
    resolved_root = root.expanduser().resolve()
    resolved_shard_path = shard_path.expanduser().resolve()
    if not company_plan.company_items:
        raise ValueError(f"company plan missing company item: {normalized_ticker}")
    company_item = company_plan.company_items[0]
    resolved_shard_path.parent.mkdir(parents=True, exist_ok=True)
    if not no_cache and company_item.cache_hit:
        try:
            _copy_sqlite_database(company_item.shard_cache_path, resolved_shard_path)
            _rebase_company_shard_release_paths(resolved_shard_path, release_root=resolved_root)
            verification = _verify_restored_company_shard(
                resolved_shard_path,
                ticker=normalized_ticker,
                cache_key=company_item.cache_key,
            )
            if not verification["ok"]:
                errors = ", ".join(verification["errors"])
                raise RuntimeError(
                    f"cached company shard failed verification: {normalized_ticker}: {errors}"
                )
            write_immutable_sqlite_cache_seal(
                resolved_shard_path,
                kind="company_shard",
                cache_key=company_item.cache_key,
                verification=verification,
                metadata=verification.get("metadata") or {},
                counts=verification.get("counts") or {},
                source_path=company_item.shard_cache_path,
            )
            return CompanyShardBuildResult(
                ticker=normalized_ticker,
                shard_path=resolved_shard_path,
                cache_hit=True,
                artifact_count=len(company_plan.items),
                totals=_company_shard_counts(resolved_shard_path),
                verification=verification,
                cache_key=company_item.cache_key,
            )
        except Exception:
            _quarantine_sqlite_cache(company_item.shard_cache_path)
            cleanup_sqlite_database_files(resolved_shard_path)

    if no_cache and not fragments_prepared:
        for item in company_plan.items:
            cleanup_sqlite_database_files(item.fragment_path)

    tmp_path = temporary_sqlite_path(resolved_shard_path)
    cleanup_sqlite_database_files(tmp_path)
    try:
        result = build_source_artifact_sqlite(
            resolved_root,
            sqlite_path=tmp_path,
            published_sqlite_path=resolved_shard_path,
            plan=company_plan,
        )
        _write_company_shard_metadata(
            tmp_path,
            ticker=normalized_ticker,
            plan=company_plan,
        )
        verification = verify_source_artifact_sqlite(tmp_path)
        if not verification["ok"]:
            errors = ", ".join(verification["errors"])
            raise RuntimeError(f"company shard failed verification: {normalized_ticker}: {errors}")
        replace_sqlite_database(tmp_path, resolved_shard_path)
        release_metadata = _read_company_shard_metadata(resolved_shard_path)
        write_immutable_sqlite_cache_seal(
            resolved_shard_path,
            kind="company_shard",
            cache_key=company_item.cache_key,
            verification=verification,
            metadata=release_metadata,
            counts=verification.get("counts") or {},
            source_path=tmp_path,
        )
        _store_sqlite_database_cache(
            resolved_shard_path,
            company_item.shard_cache_path,
            verification=verification,
            cache_kind="company_shard",
            cache_key=company_item.cache_key,
            cache_metadata=release_metadata,
            cache_counts=verification.get("counts") or {},
        )
        return CompanyShardBuildResult(
            ticker=normalized_ticker,
            shard_path=resolved_shard_path,
            cache_hit=False,
            artifact_count=len(company_plan.items),
            totals=result.get("totals") or {},
            verification=verification,
            cache_key=company_item.cache_key,
        )
    finally:
        cleanup_sqlite_database_files(tmp_path)


def _company_build_specs(
    plan: SourceArtifactSqlitePlan,
    *,
    companies_dir: Path,
    no_cache: bool,
    artifact_workers: int = 1,
) -> list[dict[str, Any]]:
    def build_spec(company: SourceCompanyShardPlanItem) -> dict[str, Any]:
        ticker = _normalize_ticker(company.ticker)
        shard_path = companies_dir / f"{_safe_ticker_filename(ticker)}.sqlite"
        company_plan = _filter_plan_for_company(
            plan,
            ticker=ticker,
            shard_path=shard_path,
            no_cache=no_cache,
            artifact_workers=artifact_workers,
        )
        return {
            "ticker": ticker,
            "shard_path": shard_path,
            "company_plan": company_plan,
            "estimated_cost": _company_build_cost(company_plan),
        }

    companies = tuple(plan.company_items)
    worker_count = min(max(1, int(plan.workers)), len(companies)) if companies else 1
    if worker_count > 1:
        with ThreadPoolExecutor(max_workers=worker_count) as executor:
            specs = list(executor.map(build_spec, companies))
    else:
        specs = [build_spec(company) for company in companies]
    return sorted(specs, key=lambda spec: (-int(spec["estimated_cost"]), str(spec["ticker"])))


def _plan_spine_fragment_cache_hit(
    spec: Mapping[str, Any],
    *,
    release_id: str,
    source_manifest_hash: str | None,
    cache_root: Path,
    no_cache: bool,
) -> bool:
    if no_cache:
        return False
    ticker = str(spec["ticker"])
    company_plan = spec["company_plan"]
    if not company_plan.company_items:
        return False
    company_item = company_plan.company_items[0]
    cache_key = _spine_fragment_cache_key(
        ticker=ticker,
        company_cache_key=company_item.cache_key,
        source_manifest_hash=source_manifest_hash,
    )
    return not _verify_spine_fragment_cache(
        _spine_fragment_cache_path(cache_root, cache_key),
        ticker=ticker,
        cache_key=cache_key,
    )


def _build_company_shards_for_release(
    root: Path,
    *,
    company_specs: Sequence[Mapping[str, Any]],
    workers: int,
    no_cache: bool,
    fragments_prepared: bool = False,
    progress: _BuildProgressWriter | None = None,
) -> list[CompanyShardBuildResult]:
    if not company_specs:
        return []
    worker_count = max(1, min(int(workers), len(company_specs)))
    total = len(company_specs)
    if worker_count <= 1:
        results: list[CompanyShardBuildResult] = []
        for spec in company_specs:
            ticker = str(spec["ticker"])
            shard_path = Path(spec["shard_path"])
            company_plan = spec["company_plan"]
            started_at = time.perf_counter()
            if progress:
                progress.record(
                    f"company_shard:{ticker}",
                    "company_shard",
                    "started",
                    ticker=ticker,
                    output=shard_path,
                    cache_hit=False if no_cache else bool(company_plan.company_items[0].cache_hit),
                    details={"completed": len(results), "total": total},
                )
            try:
                result = _build_company_shard_from_plan(
                    root,
                    ticker=ticker,
                    shard_path=shard_path,
                    company_plan=company_plan,
                    no_cache=no_cache,
                    fragments_prepared=fragments_prepared,
                )
            except Exception as exc:
                if progress:
                    progress.record(
                        f"company_shard:{ticker}",
                        "company_shard",
                        "failed",
                        ticker=ticker,
                        output=shard_path,
                        error=str(exc),
                        details={"completed": len(results), "total": total},
                        started_at=started_at,
                    )
                raise
            results.append(result)
            if progress:
                progress.record(
                    f"company_shard:{ticker}",
                    "company_shard",
                    "cached" if result.cache_hit else "rebuilt",
                    ticker=ticker,
                    output=result.shard_path,
                    cache_hit=result.cache_hit,
                    details={
                        "completed": len(results),
                        "total": total,
                        "artifact_count": result.artifact_count,
                        "cache_key": result.cache_key,
                    },
                    started_at=started_at,
                )
        return results
    results: list[CompanyShardBuildResult] = []
    try:
        with ProcessPoolExecutor(max_workers=worker_count) as executor:
            futures = {}
            for spec in company_specs:
                ticker = str(spec["ticker"])
                shard_path = Path(spec["shard_path"])
                company_plan = spec["company_plan"]
                started_at = time.perf_counter()
                if progress:
                    progress.record(
                        f"company_shard:{ticker}",
                        "company_shard",
                        "started",
                        ticker=ticker,
                        output=shard_path,
                        cache_hit=False
                        if no_cache
                        else bool(company_plan.company_items[0].cache_hit),
                        details={"completed": len(results), "total": total},
                    )
                future = executor.submit(
                    _build_company_shard_from_plan,
                    root,
                    ticker=ticker,
                    shard_path=shard_path,
                    company_plan=company_plan,
                    no_cache=no_cache,
                    fragments_prepared=fragments_prepared,
                )
                futures[future] = (ticker, shard_path, started_at)
            for future in as_completed(futures):
                ticker, shard_path, started_at = futures[future]
                try:
                    result = future.result()
                except Exception as exc:
                    if progress:
                        progress.record(
                            f"company_shard:{ticker}",
                            "company_shard",
                            "failed",
                            ticker=ticker,
                            output=shard_path,
                            error=str(exc),
                            details={"completed": len(results), "total": total},
                            started_at=started_at,
                        )
                    raise
                results.append(result)
                if progress:
                    progress.record(
                        f"company_shard:{ticker}",
                        "company_shard",
                        "cached" if result.cache_hit else "rebuilt",
                        ticker=ticker,
                        output=result.shard_path,
                        cache_hit=result.cache_hit,
                        details={
                            "completed": len(results),
                            "total": total,
                            "artifact_count": result.artifact_count,
                            "cache_key": result.cache_key,
                        },
                        started_at=started_at,
                    )
    except BrokenProcessPool as exc:
        completed_tickers = {result.ticker for result in results}
        remaining_specs = [
            spec for spec in company_specs if str(spec["ticker"]) not in completed_tickers
        ]
        retry_workers = max(1, worker_count // 2)
        if progress:
            progress.record(
                "company_shards",
                "company_shard",
                "fallback_sequential" if retry_workers == 1 else "retry_reduced_workers",
                error=str(exc),
                details={
                    "workers": worker_count,
                    "retry_workers": retry_workers,
                    "completed": len(completed_tickers),
                    "remaining": len(remaining_specs),
                    "total": total,
                },
            )
        retried = _build_company_shards_for_release(
            root,
            company_specs=remaining_specs,
            workers=retry_workers,
            no_cache=no_cache,
            fragments_prepared=fragments_prepared,
            progress=progress,
        )
        return sorted([*results, *retried], key=lambda result: result.ticker)
    return sorted(results, key=lambda result: result.ticker)


def _emit_spine_fragments_for_release(
    shard_results: Sequence[CompanyShardBuildResult],
    *,
    fragments_dir: Path,
    release_id: str,
    source_manifest_hash: str | None,
    cache_root: Path,
    workers: int,
    no_cache: bool,
    progress: _BuildProgressWriter | None = None,
) -> list[SpineFragmentResult]:
    if not shard_results:
        return []
    worker_count = max(1, min(int(workers), len(shard_results)))
    tasks = [
        {
            "shard_result": result,
            "fragment_path": fragments_dir / f"{_safe_ticker_filename(result.ticker)}.sqlite",
            "release_root": fragments_dir.parent.parent.parent,
            "release_id": release_id,
            "shard_path_in_release": f"indexes/companies/{_safe_ticker_filename(result.ticker)}.sqlite",
            "source_manifest_hash": source_manifest_hash,
            "cache_root": cache_root,
            "no_cache": no_cache,
        }
        for result in sorted(
            shard_results,
            key=lambda item: (-_file_size_or_zero(item.shard_path), item.ticker),
        )
    ]
    total = len(tasks)
    if worker_count <= 1:
        results: list[SpineFragmentResult] = []
        for task in tasks:
            shard_result = task["shard_result"]
            ticker = shard_result.ticker
            fragment_path = Path(task["fragment_path"])
            started_at = time.perf_counter()
            if progress:
                progress.record(
                    f"spine_fragment:{ticker}",
                    "spine_fragment",
                    "started",
                    ticker=ticker,
                    output=fragment_path,
                    cache_hit=False if no_cache else None,
                    details={"completed": len(results), "total": total},
                )
            try:
                result = _emit_spine_fragment_cached(**task)
            except Exception as exc:
                if progress:
                    progress.record(
                        f"spine_fragment:{ticker}",
                        "spine_fragment",
                        "failed",
                        ticker=ticker,
                        output=fragment_path,
                        error=str(exc),
                        details={"completed": len(results), "total": total},
                        started_at=started_at,
                    )
                raise
            results.append(result)
            if progress:
                progress.record(
                    f"spine_fragment:{ticker}",
                    "spine_fragment",
                    "cached" if result.cache_hit else "rebuilt",
                    ticker=ticker,
                    output=result.fragment_path,
                    cache_hit=result.cache_hit,
                    details={
                        "completed": len(results),
                        "total": total,
                        "cache_key": result.cache_key,
                        "counts": dict(result.counts),
                    },
                    started_at=started_at,
                )
        return results
    results: list[SpineFragmentResult] = []
    try:
        with ProcessPoolExecutor(max_workers=worker_count) as executor:
            futures = {}
            for task in tasks:
                shard_result = task["shard_result"]
                ticker = shard_result.ticker
                fragment_path = Path(task["fragment_path"])
                started_at = time.perf_counter()
                if progress:
                    progress.record(
                        f"spine_fragment:{ticker}",
                        "spine_fragment",
                        "started",
                        ticker=ticker,
                        output=fragment_path,
                        cache_hit=False if no_cache else None,
                        details={"completed": len(results), "total": total},
                    )
                futures[executor.submit(_emit_spine_fragment_cached, **task)] = (
                    ticker,
                    fragment_path,
                    started_at,
                )
            for future in as_completed(futures):
                ticker, fragment_path, started_at = futures[future]
                try:
                    result = future.result()
                except Exception as exc:
                    if progress:
                        progress.record(
                            f"spine_fragment:{ticker}",
                            "spine_fragment",
                            "failed",
                            ticker=ticker,
                            output=fragment_path,
                            error=str(exc),
                            details={"completed": len(results), "total": total},
                            started_at=started_at,
                        )
                    raise
                results.append(result)
                if progress:
                    progress.record(
                        f"spine_fragment:{ticker}",
                        "spine_fragment",
                        "cached" if result.cache_hit else "rebuilt",
                        ticker=ticker,
                        output=result.fragment_path,
                        cache_hit=result.cache_hit,
                        details={
                            "completed": len(results),
                            "total": total,
                            "cache_key": result.cache_key,
                            "counts": dict(result.counts),
                        },
                        started_at=started_at,
                    )
    except BrokenProcessPool as exc:
        completed_tickers = {result.ticker for result in results}
        remaining_shards = [
            result for result in shard_results if result.ticker not in completed_tickers
        ]
        retry_workers = max(1, worker_count // 2)
        if progress:
            progress.record(
                "spine_fragments",
                "spine_fragment",
                "fallback_sequential" if retry_workers == 1 else "retry_reduced_workers",
                error=str(exc),
                details={
                    "workers": worker_count,
                    "retry_workers": retry_workers,
                    "completed": len(completed_tickers),
                    "remaining": len(remaining_shards),
                    "total": total,
                },
            )
        retried = _emit_spine_fragments_for_release(
            remaining_shards,
            fragments_dir=fragments_dir,
            release_id=release_id,
            source_manifest_hash=source_manifest_hash,
            cache_root=cache_root,
            workers=retry_workers,
            no_cache=no_cache,
            progress=progress,
        )
        return sorted([*results, *retried], key=lambda result: result.ticker)
    return sorted(results, key=lambda result: result.ticker)


def _write_v3_shard_manifest(
    path: Path,
    *,
    release_root: Path,
    release_id: str,
    source_manifest_hash: str | None,
    shard_results: Sequence[CompanyShardBuildResult],
) -> dict[str, Any]:
    shards: dict[str, Any] = {}
    expected_metric_dictionary = metric_dictionary_binding()
    for result in sorted(shard_results, key=lambda item: item.ticker):
        counts = _company_shard_counts(result.shard_path)
        shard_metric_dictionary = _company_shard_metric_dictionary_binding(result.shard_path)
        binding_errors = metric_dictionary_binding_errors(
            shard_metric_dictionary,
            expected=expected_metric_dictionary,
        )
        if binding_errors:
            raise RuntimeError(
                f"company shard metric dictionary mismatch: {result.ticker}: "
                + ", ".join(binding_errors)
            )
        shard_sha256 = _file_sha256(result.shard_path)
        record_immutable_sqlite_cache_sha256(result.shard_path, shard_sha256)
        shards[result.ticker] = {
            "ticker": result.ticker,
            "path": _path_label(release_root / "indexes", result.shard_path),
            "schema_version": COMPANY_SHARD_SCHEMA_VERSION,
            "source_artifact_sqlite_schema_version": SOURCE_ARTIFACT_SQLITE_SCHEMA_VERSION,
            "source_artifact_sqlite_builder_version": SOURCE_ARTIFACT_SQLITE_BUILDER_VERSION,
            "document_count": counts.get("documents", 0),
            "object_count": counts.get("objects", 0),
            "edge_count": counts.get("edges", 0),
            "quality_event_count": counts.get("quality_events", 0),
            "row_counts": counts,
            "quality_summary": _company_shard_quality_summary(result.shard_path),
            "sha256": shard_sha256,
            "cache_hit": result.cache_hit,
            "cache_key": result.cache_key,
            "metric_dictionary": shard_metric_dictionary,
        }
    payload = {
        "format": V3_SHARD_MANIFEST_FORMAT_VERSION,
        "index_layout": GLOBAL_SPINE_LAYOUT,
        "release_id": release_id,
        "source_manifest_hash": source_manifest_hash,
        "company_shard_schema_version": COMPANY_SHARD_SCHEMA_VERSION,
        "source_artifact_sqlite_schema_version": SOURCE_ARTIFACT_SQLITE_SCHEMA_VERSION,
        "source_artifact_sqlite_builder_version": SOURCE_ARTIFACT_SQLITE_BUILDER_VERSION,
        "metric_dictionary": expected_metric_dictionary,
        "companies_dir": "companies",
        "ticker_count": len(shards),
        "shards": shards,
        "created_at": datetime.now(timezone.utc).isoformat(),
    }
    _write_json(path, payload)
    return payload


def _company_shard_metric_dictionary_binding(path: Path) -> dict[str, Any]:
    try:
        with sqlite3.connect(path) as conn:
            row = conn.execute("SELECT value FROM metadata WHERE key = 'build'").fetchone()
    except sqlite3.Error as exc:
        raise RuntimeError(f"cannot read company shard metadata: {path}: {exc}") from exc
    if row is None:
        return {}
    try:
        metadata = json.loads(str(row[0]))
    except json.JSONDecodeError:
        return {}
    binding = metadata.get("metric_dictionary")
    return dict(binding) if isinstance(binding, Mapping) else {}


def _sqlite_compile_max_worker_threads(conn: sqlite3.Connection) -> int:
    """Return the SQLite library's compiled auxiliary-worker ceiling."""
    for row in conn.execute("PRAGMA compile_options"):
        option = str(row[0] or "")
        if not option.startswith("MAX_WORKER_THREADS="):
            continue
        try:
            return max(0, int(option.split("=", 1)[1]))
        except ValueError:
            return 0
    return 0


def _global_merge_sqlite_thread_limit(
    conn: sqlite3.Connection,
    *,
    requested: int | None = None,
    cpu_count: int | None = None,
) -> int:
    """Bound SQLite sort workers by the request, CPU, and compile ceiling."""
    compiled_limit = _sqlite_compile_max_worker_threads(conn)
    resolved_cpu_count = max(1, int(cpu_count or os.cpu_count() or 1))
    auxiliary_cpu_limit = max(0, resolved_cpu_count - 1)
    if requested is None:
        raw = os.getenv("KRW_INDEX_SQLITE_THREADS")
        try:
            requested = int(raw) if raw is not None else DEFAULT_GLOBAL_MERGE_SQLITE_THREADS
        except ValueError:
            requested = DEFAULT_GLOBAL_MERGE_SQLITE_THREADS
    return max(0, min(int(requested), compiled_limit, auxiliary_cpu_limit))


def _configure_global_merge_connection(conn: sqlite3.Connection) -> dict[str, int | str]:
    """Configure the disposable global merge database for bounded bulk writes."""
    journal_mode = str(conn.execute("PRAGMA journal_mode=OFF").fetchone()[0])
    conn.execute("PRAGMA synchronous=OFF")
    conn.execute("PRAGMA temp_store=FILE")
    conn.execute(f"PRAGMA cache_size=-{DEFAULT_GLOBAL_MERGE_SQLITE_CACHE_KIB}")
    requested_threads = _global_merge_sqlite_thread_limit(conn)
    conn.execute(f"PRAGMA threads={requested_threads}")
    actual_threads = int(conn.execute("PRAGMA threads").fetchone()[0])
    return {
        "sqlite_version": sqlite3.sqlite_version,
        "journal_mode": journal_mode,
        "synchronous": int(conn.execute("PRAGMA synchronous").fetchone()[0]),
        "temp_store": int(conn.execute("PRAGMA temp_store").fetchone()[0]),
        "cache_size_kib": abs(int(conn.execute("PRAGMA cache_size").fetchone()[0])),
        "compiled_max_worker_threads": _sqlite_compile_max_worker_threads(conn),
        "threads": actual_threads,
    }


def _elapsed_ms(started_at: float) -> int:
    return max(0, int((time.perf_counter() - started_at) * 1000))


def _cleanup_stale_spine_temps(
    target_path: Path,
    *,
    older_than_seconds: int | None = None,
) -> list[str]:
    """Remove abandoned temp databases without touching a plausibly active build."""
    resolved = target_path.expanduser().resolve()
    raw_age = os.getenv("KRW_INDEX_STALE_TMP_AGE_SECONDS")
    if older_than_seconds is None:
        try:
            older_than_seconds = (
                int(raw_age) if raw_age is not None else DEFAULT_SPINE_STALE_TMP_AGE_SECONDS
            )
        except ValueError:
            older_than_seconds = DEFAULT_SPINE_STALE_TMP_AGE_SECONDS
    threshold = max(0, int(older_than_seconds))
    cutoff = time.time() - threshold
    candidates: set[Path] = set()
    for suffix in ("", "-wal", "-shm"):
        for path in resolved.parent.glob(f".{resolved.name}.*.tmp{suffix}"):
            base = Path(str(path)[: -len(suffix)]) if suffix else path
            candidates.add(base)
    seal_path = spine_verification_seal_path(resolved)
    candidates.update(resolved.parent.glob(f".{seal_path.name}.*.tmp"))
    removed: list[str] = []
    for candidate in sorted(candidates):
        members = (candidate, Path(str(candidate) + "-wal"), Path(str(candidate) + "-shm"))
        mtimes = []
        for member in members:
            try:
                mtimes.append(member.stat().st_mtime)
            except FileNotFoundError:
                continue
        if not mtimes:
            continue
        if max(mtimes) > cutoff:
            continue
        if candidate.name.startswith(f".{resolved.name}."):
            cleanup_sqlite_database_files(candidate)
        else:
            candidate.unlink(missing_ok=True)
        removed.append(candidate.name)
    return removed


def _cleanup_spine_database_and_seal(path: Path) -> None:
    cleanup_sqlite_database_files(path)
    spine_verification_seal_path(path).unlink(missing_ok=True)


def merge_spine_fragments(
    fragments: Sequence[Path],
    global_spine_path: Path,
    *,
    release_id: str | None = None,
    source_manifest_hash: str | None = None,
    replace: bool = True,
    generate_links: bool = True,
    created_at: str | None = None,
    progress_callback: Callable[[Mapping[str, Any]], None] | None = None,
    semantic_preflight: bool = True,
    semantic_preflight_report_path: Path | None = None,
    semantic_cache_key: str | None = None,
) -> GlobalSpineMergeResult:
    """Merge per-company spine fragments into one deterministic global spine."""
    merge_started_at = time.perf_counter()
    stage_timings_ms: dict[str, int] = {}
    resolved_fragments = tuple(
        sorted((path.expanduser().resolve() for path in fragments), key=lambda path: path.name)
    )
    if not resolved_fragments:
        raise ValueError("at least one spine fragment is required")
    if semantic_preflight:
        preflight_result = preflight_spine_fragments(
            resolved_fragments,
            report_path=semantic_preflight_report_path,
        )
        if not preflight_result["ok"]:
            raise _semantic_preflight_error(preflight_result)
    resolved_output = global_spine_path.expanduser().resolve()
    resolved_output.parent.mkdir(parents=True, exist_ok=True)
    stale_started_at = time.perf_counter()
    stale_temps_removed = _cleanup_stale_spine_temps(resolved_output)
    stage_timings_ms["stale_temp_cleanup"] = _elapsed_ms(stale_started_at)
    tmp_path = (
        resolved_output.parent / f".{resolved_output.name}.{os.getpid()}.{time.time_ns()}.tmp"
    )
    cleanup_sqlite_database_files(tmp_path)
    if resolved_output.exists() and not replace:
        raise FileExistsError(f"global spine already exists: {resolved_output}")
    sqlite_settings: dict[str, int | str] = {}
    try:
        with sqlite3.connect(tmp_path) as conn:
            conn.row_factory = sqlite3.Row
            sqlite_settings = _configure_global_merge_connection(conn)
            if progress_callback:
                progress_callback(
                    {
                        "node_id": "global_spine_merge:runtime",
                        "stage": "global_spine_merge",
                        "status": "configured",
                        "output": resolved_output,
                        "details": {
                            "sqlite": dict(sqlite_settings),
                            "stale_temps_removed": stale_temps_removed,
                        },
                    }
                )
            schema_started_at = time.perf_counter()
            create_global_spine_schema(conn, include_secondary_indexes=False)
            stage_timings_ms["schema_create"] = _elapsed_ms(schema_started_at)
            write_global_spine_metadata(
                conn,
                {
                    "builder_version": GLOBAL_SPINE_BUILDER_VERSION,
                    "index_layout": GLOBAL_SPINE_LAYOUT,
                    "schema_version": GLOBAL_SPINE_SCHEMA_VERSION,
                    "release_id": release_id,
                    "source_manifest_hash": source_manifest_hash,
                    "metric_dictionary": metric_dictionary_binding(),
                    "spine_projection_version": SPINE_PROJECTION_VERSION,
                    "semantic_identity_policy_version": SEMANTIC_IDENTITY_POLICY_VERSION,
                    "global_spine_cache_format_version": GLOBAL_SPINE_CACHE_FORMAT_VERSION,
                    "global_spine_cache_key": semantic_cache_key,
                    "source_artifact_sqlite_schema_version": SOURCE_ARTIFACT_SQLITE_SCHEMA_VERSION,
                    "source_artifact_sqlite_builder_version": SOURCE_ARTIFACT_SQLITE_BUILDER_VERSION,
                    "company_shard_schema_version": COMPANY_SHARD_SCHEMA_VERSION,
                    "fragment_count": len(resolved_fragments),
                    "created_at": created_at or datetime.now(timezone.utc).isoformat(),
                },
            )
            fragment_verification_ms = 0
            fragment_merge_ms = 0
            benign_duplicate_counts = {"objects": 0, "edges": 0, "total": 0}
            for index, fragment in enumerate(resolved_fragments, start=1):
                if progress_callback:
                    progress_callback(
                        {
                            "node_id": f"global_spine_merge:{fragment.stem}",
                            "stage": "global_spine_merge",
                            "status": "started",
                            "output": resolved_output,
                            "details": {
                                "completed": index - 1,
                                "total": len(resolved_fragments),
                                "fragment": fragment.name,
                            },
                        }
                    )
                verification_started_at = time.perf_counter()
                verification = verify_spine_fragment_schema(
                    fragment,
                    deep=True,
                    trust_seal=True,
                )
                fragment_verification_elapsed = _elapsed_ms(verification_started_at)
                fragment_verification_ms += fragment_verification_elapsed
                if not verification["ok"]:
                    errors = ", ".join(verification["errors"])
                    raise RuntimeError(f"spine fragment failed verification: {fragment}: {errors}")
                schema_name = f"frag_{index}"
                conn.execute(f"ATTACH DATABASE ? AS {schema_name}", (str(fragment),))
                fragment_merge_started_at = time.perf_counter()
                fragment_benign_duplicates = {"objects": 0, "edges": 0}
                try:
                    for table_name in _SPINE_FRAGMENT_MERGE_TABLES:
                        duplicate_count = _merge_spine_table(
                            conn,
                            schema_name,
                            table_name,
                            fragment_path=fragment,
                        )
                        if table_name == "global_object_locator":
                            fragment_benign_duplicates["objects"] += duplicate_count
                        elif table_name == "global_edge_spine":
                            fragment_benign_duplicates["edges"] += duplicate_count
                    conn.commit()
                except Exception:
                    conn.rollback()
                    raise
                finally:
                    conn.execute(f"DETACH DATABASE {schema_name}")
                fragment_merge_elapsed = _elapsed_ms(fragment_merge_started_at)
                fragment_merge_ms += fragment_merge_elapsed
                if progress_callback:
                    progress_callback(
                        {
                            "node_id": f"global_spine_merge:{fragment.stem}",
                            "stage": "global_spine_merge",
                            "status": "merged",
                            "output": resolved_output,
                            "details": {
                                "completed": index,
                                "total": len(resolved_fragments),
                                "fragment": fragment.name,
                                "verification_mode": verification.get("verification_mode"),
                                "integrity_source": verification.get("integrity_source"),
                                "verification_ms": fragment_verification_elapsed,
                                "merge_ms": fragment_merge_elapsed,
                                "benign_duplicate_counts": {
                                    **fragment_benign_duplicates,
                                    "total": sum(fragment_benign_duplicates.values()),
                                },
                            },
                        }
                    )
            stage_timings_ms["fragment_verification"] = fragment_verification_ms
            stage_timings_ms["fragment_merge"] = fragment_merge_ms
            # Derive the final figures from the occurrence tables rather than
            # summing per-fragment collisions.  This remains exact if the same
            # logical occurrence is supplied twice and ignored by its composite
            # primary key.
            benign_duplicate_counts = {
                "objects": max(
                    0,
                    _count_table(conn, "global_object_replica")
                    - _count_table(conn, "global_object_locator"),
                ),
                "edges": max(
                    0,
                    _count_table(conn, "global_edge_replica")
                    - _count_table(conn, "global_edge_spine"),
                ),
            }
            benign_duplicate_counts["total"] = sum(benign_duplicate_counts.values())
            if progress_callback:
                progress_callback(
                    {
                        "node_id": "global_chain_links",
                        "stage": "global_spine_merge",
                        "status": "started",
                        "output": resolved_output,
                        "details": {"generate_links": generate_links},
                    }
                )
            chain_started_at = time.perf_counter()
            chain_result = (
                generate_cross_company_links(conn, replace=True)
                if generate_links
                else CrossCompanyLinkGenerationResult(
                    inserted=0,
                    exact_links=0,
                    similarity_links=0,
                    key_count=_count_table(conn, "global_key_stats"),
                    skipped_generic_keys=0,
                )
            )
            stage_timings_ms["chain_generation"] = _elapsed_ms(chain_started_at)
            # Build every regenerable B-tree once, after the bulk inserts and
            # cross-company link generation.  Creating these indexes while 332
            # fragments stream in multiplies maintenance work substantially.
            secondary_index_started_at = time.perf_counter()
            create_global_spine_schema(conn, include_secondary_indexes=True)
            stage_timings_ms["secondary_index_build"] = _elapsed_ms(secondary_index_started_at)
            if progress_callback:
                progress_callback(
                    {
                        "node_id": "global_chain_links",
                        "stage": "global_spine_merge",
                        "status": "complete",
                        "output": resolved_output,
                        "details": {
                            "inserted": chain_result.inserted,
                            "exact_links": chain_result.exact_links,
                            "similarity_links": chain_result.similarity_links,
                            "chain_generation_ms": stage_timings_ms["chain_generation"],
                            "secondary_index_build_ms": stage_timings_ms["secondary_index_build"],
                        },
                    }
                )
            counts_started_at = time.perf_counter()
            counts = {
                table_name: _count_table(conn, table_name)
                for table_name in GLOBAL_SPINE_TABLES
                if table_name != "metadata"
            }
            counts.update(
                {
                    "benign_duplicate_objects": benign_duplicate_counts["objects"],
                    "benign_duplicate_edges": benign_duplicate_counts["edges"],
                    "benign_duplicate_total": benign_duplicate_counts["total"],
                }
            )
            stage_timings_ms["counts"] = _elapsed_ms(counts_started_at)
            optimize_started_at = time.perf_counter()
            conn.execute("PRAGMA optimize")
            stage_timings_ms["optimize"] = _elapsed_ms(optimize_started_at)
            runtime_metadata = {
                "sqlite": dict(sqlite_settings),
                "stage_timings_ms": dict(stage_timings_ms),
                "stale_temps_removed": stale_temps_removed,
                "benign_duplicate_counts": dict(benign_duplicate_counts),
            }
            metadata_started_at = time.perf_counter()
            write_global_spine_metadata(
                conn,
                {
                    "builder_version": GLOBAL_SPINE_BUILDER_VERSION,
                    "index_layout": GLOBAL_SPINE_LAYOUT,
                    "schema_version": GLOBAL_SPINE_SCHEMA_VERSION,
                    "release_id": release_id,
                    "source_manifest_hash": source_manifest_hash,
                    "metric_dictionary": metric_dictionary_binding(),
                    "spine_projection_version": SPINE_PROJECTION_VERSION,
                    "semantic_identity_policy_version": SEMANTIC_IDENTITY_POLICY_VERSION,
                    "global_spine_cache_format_version": GLOBAL_SPINE_CACHE_FORMAT_VERSION,
                    "global_spine_cache_key": semantic_cache_key,
                    "source_artifact_sqlite_schema_version": SOURCE_ARTIFACT_SQLITE_SCHEMA_VERSION,
                    "source_artifact_sqlite_builder_version": SOURCE_ARTIFACT_SQLITE_BUILDER_VERSION,
                    "company_shard_schema_version": COMPANY_SHARD_SCHEMA_VERSION,
                    "fragment_count": len(resolved_fragments),
                    "counts": counts,
                    "chain_links": {
                        "inserted": chain_result.inserted,
                        "exact_links": chain_result.exact_links,
                        "similarity_links": chain_result.similarity_links,
                        "key_count": chain_result.key_count,
                        "skipped_generic_keys": chain_result.skipped_generic_keys,
                    },
                    "benign_duplicate_counts": benign_duplicate_counts,
                    "merge_runtime": runtime_metadata,
                    "created_at": created_at or datetime.now(timezone.utc).isoformat(),
                },
            )
            conn.commit()
            stage_timings_ms["metadata_finalize"] = _elapsed_ms(metadata_started_at)
            sqlite_finalize_started_at = time.perf_counter()
            conn.execute("PRAGMA journal_mode=DELETE").fetchall()
            stage_timings_ms["sqlite_finalize"] = _elapsed_ms(sqlite_finalize_started_at)
        deep_verification_started_at = time.perf_counter()
        deep_verification = verify_global_spine_schema(tmp_path)
        stage_timings_ms["deep_verification"] = _elapsed_ms(deep_verification_started_at)
        stage_timings_ms["replica_invariant_verification"] = int(
            (deep_verification.get("replica_invariants") or {}).get("elapsed_ms") or 0
        )
        if not deep_verification["ok"]:
            errors = ", ".join(deep_verification["errors"])
            raise RuntimeError(f"merged global spine failed verification: {errors}")
        replace_started_at = time.perf_counter()
        replace_sqlite_database(tmp_path, resolved_output)
        stage_timings_ms["atomic_replace"] = _elapsed_ms(replace_started_at)
        seal_started_at = time.perf_counter()
        write_spine_verification_seal(
            resolved_output,
            deep_verification,
            source_path=tmp_path,
            details={
                "sqlite": dict(sqlite_settings),
                "pre_publish_stage_timings_ms": dict(stage_timings_ms),
            },
        )
        stage_timings_ms["seal_write"] = _elapsed_ms(seal_started_at)
        # The atomic replace preserves the bytes just verified at tmp_path.
        # Writing the stat-bound seal records that result; immediately reading
        # the same file again adds no independent evidence.
        stage_timings_ms["sealed_verification"] = 0
        stage_timings_ms["total"] = _elapsed_ms(merge_started_at)
        runtime = {
            "sqlite": dict(sqlite_settings),
            "stage_timings_ms": dict(stage_timings_ms),
            "stale_temps_removed": stale_temps_removed,
            "benign_duplicate_counts": dict(benign_duplicate_counts),
        }
        verification = {
            **deep_verification,
            "path": str(resolved_output),
            "integrity_source": "immutable_seal",
            "verification_mode": "deep-sealed",
            "deep_verification": {
                "integrity_check": deep_verification.get("integrity_check"),
                "integrity_source": deep_verification.get("integrity_source"),
                "verification_mode": deep_verification.get("verification_mode"),
            },
            "benign_duplicate_counts": dict(benign_duplicate_counts),
            "runtime": runtime,
        }
        if progress_callback:
            progress_callback(
                {
                    "node_id": "global_spine_merge:runtime",
                    "stage": "global_spine_merge",
                    "status": "complete",
                    "output": resolved_output,
                    "details": runtime,
                }
            )
        return GlobalSpineMergeResult(
            global_spine_path=resolved_output,
            fragment_count=len(resolved_fragments),
            counts=counts,
            chain_links=chain_result,
            verification=verification,
        )
    finally:
        cleanup_sqlite_database_files(tmp_path)


def emit_spine_fragment_from_company_shard(
    shard_path: Path,
    fragment_path: Path,
    *,
    ticker: str,
    release_id: str | None = None,
    shard_path_in_release: str | None = None,
    cache_key: str | None = None,
    company_source_hash: str | None = None,
    source_manifest_hash: str | None = None,
    replace: bool = True,
) -> SpineFragmentResult:
    """Project one company shard into a compact global spine fragment."""
    fragment_started_at = time.perf_counter()
    stage_timings_ms: dict[str, int] = {}
    normalized_ticker = _normalize_ticker(ticker)
    resolved_shard_path = shard_path.expanduser().resolve()
    resolved_fragment_path = fragment_path.expanduser().resolve()
    if not resolved_shard_path.is_file():
        raise FileNotFoundError(f"Company shard not found: {resolved_shard_path}")
    resolved_fragment_path.parent.mkdir(parents=True, exist_ok=True)
    stale_started_at = time.perf_counter()
    stale_temps_removed = _cleanup_stale_spine_temps(resolved_fragment_path)
    stage_timings_ms["stale_temp_cleanup"] = _elapsed_ms(stale_started_at)
    tmp_path = (
        resolved_fragment_path.parent
        / f".{resolved_fragment_path.name}.{os.getpid()}.{time.time_ns()}.tmp"
    )
    cleanup_sqlite_database_files(tmp_path)
    if resolved_fragment_path.exists() and not replace:
        raise FileExistsError(f"spine fragment already exists: {resolved_fragment_path}")
    release_shard_path = shard_path_in_release or f"indexes/companies/{normalized_ticker}.sqlite"
    counts: dict[str, int] = defaultdict(int)
    try:
        projection_started_at = time.perf_counter()
        with sqlite3.connect(resolved_shard_path) as source, sqlite3.connect(tmp_path) as target:
            source.row_factory = sqlite3.Row
            target.row_factory = sqlite3.Row
            create_spine_fragment_schema(target)
            write_global_spine_metadata(
                target,
                {
                    "format": SPINE_FRAGMENT_FORMAT_VERSION,
                    "fragment_schema_version": SPINE_FRAGMENT_SCHEMA_VERSION,
                    "builder_version": GLOBAL_SPINE_BUILDER_VERSION,
                    "index_layout": GLOBAL_SPINE_LAYOUT,
                    "schema_version": GLOBAL_SPINE_SCHEMA_VERSION,
                    "spine_projection_version": SPINE_PROJECTION_VERSION,
                    "semantic_identity_policy_version": SEMANTIC_IDENTITY_POLICY_VERSION,
                    "source_artifact_sqlite_schema_version": SOURCE_ARTIFACT_SQLITE_SCHEMA_VERSION,
                    "source_artifact_sqlite_builder_version": SOURCE_ARTIFACT_SQLITE_BUILDER_VERSION,
                    "company_shard_schema_version": COMPANY_SHARD_SCHEMA_VERSION,
                    "release_id": release_id,
                    "ticker": normalized_ticker,
                    "source_shard_path": str(resolved_shard_path),
                    "shard_path": release_shard_path,
                    "spine_fragment_cache_format_version": SPINE_FRAGMENT_CACHE_FORMAT_VERSION,
                    "spine_fragment_cache_key": cache_key,
                    "company_source_hash": company_source_hash,
                    "source_manifest_hash": source_manifest_hash,
                    "metric_dictionary": metric_dictionary_binding(),
                    "created_at": datetime.now(timezone.utc).isoformat(),
                },
            )
            object_rows = _source_rows(source, "objects")
            object_payload = {str(row["id"]): _json_loads(row["json"]) for row in object_rows}
            object_ticker = {
                str(row["id"]): effective_ticker(row["ticker"], fallback=normalized_ticker)
                for row in object_rows
            }
            object_type = {str(row["id"]): str(row["type"] or "") for row in object_rows}
            object_id_map = {
                raw_id: project_object_identity(
                    raw_id,
                    object_type.get(raw_id),
                    ticker=normalized_ticker,
                    payload=object_payload.get(raw_id),
                )
                for raw_id in object_type
            }
            projection_quality: dict[str, int] = {
                "dangling_edges_skipped": 0,
                "topic_source_refs_skipped": 0,
            }
            search_rows = _rows_by_key(_source_rows(source, "object_search_text"), "object_id")
            document_rows = _source_rows(source, "documents")
            counts["global_document_catalog"] = _emit_document_catalog(
                target,
                document_rows,
                ticker=normalized_ticker,
                shard_path=release_shard_path,
            )
            counts["global_object_locator"] = _emit_object_locator_and_search(
                target,
                object_rows,
                search_rows=search_rows,
                object_id_map=object_id_map,
                object_payload=object_payload,
                ticker=normalized_ticker,
                shard_path=release_shard_path,
            )
            counts["global_edge_spine"] = _emit_edge_spine(
                target,
                _source_rows(source, "edges"),
                object_ticker=object_ticker,
                object_type=object_type,
                object_id_map=object_id_map,
                ticker=normalized_ticker,
                projection_quality=projection_quality,
                shard_path=release_shard_path,
            )
            counts["global_factor_spine"] = _emit_factor_spine(
                target,
                _source_rows(source, "factor_lookup"),
                object_id_map=object_id_map,
                ticker=normalized_ticker,
            )
            counts["global_topic_spine"] = _emit_topic_spine(
                target,
                _source_rows(source, "company_topic_index"),
                object_id_map=object_id_map,
                ticker=normalized_ticker,
                projection_quality=projection_quality,
            )
            counts["global_metric_spine"] = _emit_metric_spine(
                target,
                _source_rows(source, "metric_lookup"),
                object_id_map=object_id_map,
                ticker=normalized_ticker,
            )
            counts["global_counterparty_spine"] = _emit_counterparty_spine(
                target,
                _source_rows(source, "agreement_lookup"),
                object_id_map=object_id_map,
                ticker=normalized_ticker,
            )
            counts["global_entity_spine"] = _emit_entity_spine_from_topics(
                target,
                _source_rows(source, "company_topic_index"),
                object_id_map=object_id_map,
                ticker=normalized_ticker,
            )
            write_global_spine_metadata(
                target,
                {
                    "projection_quality": {
                        **projection_quality,
                        "status": "degraded" if any(projection_quality.values()) else "ok",
                    }
                },
            )
        stage_timings_ms["projection"] = _elapsed_ms(projection_started_at)
        verification_started_at = time.perf_counter()
        verification = verify_spine_fragment_schema(
            tmp_path,
            deep=True,
            trust_seal=False,
        )
        stage_timings_ms["deep_verification"] = _elapsed_ms(verification_started_at)
        if not verification["ok"]:
            errors = ", ".join(verification["errors"])
            raise RuntimeError(
                f"built spine fragment failed temp verification: {normalized_ticker}: {errors}"
            )
        replace_started_at = time.perf_counter()
        replace_sqlite_database(tmp_path, resolved_fragment_path)
        stage_timings_ms["atomic_replace"] = _elapsed_ms(replace_started_at)
        seal_started_at = time.perf_counter()
        write_spine_verification_seal(
            resolved_fragment_path,
            verification,
            source_path=tmp_path,
            details={
                "pre_publish_stage_timings_ms": dict(stage_timings_ms),
                "stale_temps_removed": stale_temps_removed,
            },
        )
        stage_timings_ms["seal_write"] = _elapsed_ms(seal_started_at)
        # The published file is an atomic replacement of the verified temp DB.
        # The seal binds that verification to the destination stat, so a second
        # immediate schema/integrity pass would only repeat the same work.
        stage_timings_ms["sealed_verification"] = 0
        stage_timings_ms["total"] = _elapsed_ms(fragment_started_at)
        return SpineFragmentResult(
            ticker=normalized_ticker,
            fragment_path=resolved_fragment_path,
            shard_path=resolved_shard_path,
            counts=dict(counts),
            cache_hit=False,
            cache_key=cache_key,
        )
    finally:
        cleanup_sqlite_database_files(tmp_path)


def _emit_spine_fragment_cached(
    *,
    shard_result: CompanyShardBuildResult,
    fragment_path: Path,
    release_root: Path,
    release_id: str,
    shard_path_in_release: str,
    source_manifest_hash: str | None,
    cache_root: Path,
    no_cache: bool,
) -> SpineFragmentResult:
    cache_key = _spine_fragment_cache_key(
        ticker=shard_result.ticker,
        company_cache_key=shard_result.cache_key,
        source_manifest_hash=source_manifest_hash,
    )
    cache_path = _spine_fragment_cache_path(cache_root, cache_key)
    if not no_cache and not _verify_spine_fragment_cache(
        cache_path,
        ticker=shard_result.ticker,
        cache_key=cache_key,
    ):
        try:
            cache_verification = verify_spine_fragment_schema(
                cache_path,
                deep=True,
                trust_seal=True,
            )
            if not cache_verification["ok"]:
                raise RuntimeError(
                    "cached spine fragment seal verification failed: "
                    + ", ".join(cache_verification["errors"])
                )
            spine_verification_seal_path(fragment_path).unlink(missing_ok=True)
            _copy_sqlite_database(cache_path, fragment_path)
            _rebase_spine_fragment_release_paths(
                fragment_path,
                release_root=release_root,
                release_id=release_id,
                source_shard_path=shard_result.shard_path,
                shard_path_in_release=shard_path_in_release,
            )
            rebased_verification = _verify_restored_spine_fragment(
                fragment_path,
                ticker=shard_result.ticker,
                cache_key=cache_key,
                release_id=release_id,
                shard_path_in_release=shard_path_in_release,
            )
            if not rebased_verification["ok"]:
                errors = ", ".join(rebased_verification["errors"])
                raise RuntimeError(
                    f"cached spine fragment failed verification: {shard_result.ticker}: {errors}"
                )
            verification = {
                **cache_verification,
                "path": str(fragment_path.expanduser().resolve()),
                "metadata": rebased_verification["metadata"],
                "integrity_source": "immutable_seal",
                "verification_mode": "deep-sealed-inherited",
            }
            write_spine_verification_seal(
                fragment_path,
                verification,
                source_path=cache_path,
                details={"restore_mode": "metadata-rebind-from-immutable-cache"},
            )
            counts = _spine_counts(fragment_path)
            return SpineFragmentResult(
                ticker=shard_result.ticker,
                fragment_path=fragment_path.expanduser().resolve(),
                shard_path=shard_result.shard_path,
                counts=counts,
                cache_hit=True,
                cache_key=cache_key,
            )
        except Exception:
            _quarantine_sqlite_cache(cache_path)
            spine_verification_seal_path(cache_path).unlink(missing_ok=True)
            _cleanup_spine_database_and_seal(fragment_path)

    result = emit_spine_fragment_from_company_shard(
        shard_result.shard_path,
        fragment_path,
        ticker=shard_result.ticker,
        release_id=release_id,
        shard_path_in_release=shard_path_in_release,
        cache_key=cache_key,
        company_source_hash=shard_result.cache_key,
        source_manifest_hash=source_manifest_hash,
    )
    verification = verify_spine_fragment_schema(
        result.fragment_path,
        deep=True,
        trust_seal=True,
    )
    if not verification["ok"]:
        errors = ", ".join(verification["errors"])
        raise RuntimeError(f"built spine fragment failed verification: {result.ticker}: {errors}")
    _store_sqlite_database_cache(
        result.fragment_path,
        cache_path,
        verification=verification,
    )
    return result


def _spine_fragment_cache_key(
    *,
    ticker: str,
    company_cache_key: str | None,
    source_manifest_hash: str | None,
) -> str:
    return _stable_hash(
        {
            "spine_fragment_cache_format_version": SPINE_FRAGMENT_CACHE_FORMAT_VERSION,
            "spine_fragment_schema_version": SPINE_FRAGMENT_SCHEMA_VERSION,
            "spine_projection_version": SPINE_PROJECTION_VERSION,
            "semantic_identity_policy_version": SEMANTIC_IDENTITY_POLICY_VERSION,
            "global_spine_schema_version": GLOBAL_SPINE_SCHEMA_VERSION,
            "global_spine_builder_version": GLOBAL_SPINE_BUILDER_VERSION,
            "ticker": ticker,
            "company_cache_key": company_cache_key,
            "source_manifest_hash": source_manifest_hash,
        }
    )


def _spine_fragment_cache_path(cache_root: Path, cache_key: str) -> Path:
    digest = cache_key.split(":", 1)[-1]
    return cache_root / "v3" / "spine_fragments" / digest[:2] / f"{digest}.sqlite"


def _verify_spine_fragment_cache(path: Path, *, ticker: str, cache_key: str) -> tuple[str, ...]:
    if not path.exists():
        return ("spine_fragment_cache_missing",)
    verification = verify_spine_fragment_schema(path)
    errors = [f"spine_fragment_cache:{error}" for error in verification.get("errors") or []]
    try:
        with sqlite3.connect(path) as conn:
            metadata = read_global_spine_metadata(conn)
    except sqlite3.Error as exc:
        errors.append(f"spine_fragment_cache_metadata_error:{exc}")
        metadata = {}
    if metadata.get("format") != SPINE_FRAGMENT_FORMAT_VERSION:
        errors.append("spine_fragment_cache_format_mismatch")
    if metadata.get("ticker") != ticker:
        errors.append("spine_fragment_cache_ticker_mismatch")
    if metadata.get("spine_fragment_cache_format_version") != SPINE_FRAGMENT_CACHE_FORMAT_VERSION:
        errors.append("spine_fragment_cache_metadata_format_mismatch")
    if metadata.get("spine_fragment_cache_key") != cache_key:
        errors.append("spine_fragment_cache_key_mismatch")
    if not errors and verification.get("integrity_source") == "sqlite_integrity_check":
        try:
            write_spine_verification_seal(path, verification)
        except OSError:
            pass
    return tuple(errors)


def _spine_counts(path: Path) -> dict[str, int]:
    with sqlite3.connect(path) as conn:
        return {
            table_name: _count_table(conn, table_name)
            for table_name in SPINE_FRAGMENT_TABLES
            if table_name != "metadata"
        }


_SPINE_FRAGMENT_MERGE_TABLES = tuple(
    table_name for table_name in SPINE_FRAGMENT_TABLES if table_name != "metadata"
)

_SPINE_PRIMARY_KEY_COLUMNS: Mapping[str, tuple[str, ...]] = {
    "global_object_locator": ("object_id",),
    "global_document_catalog": ("document_id",),
    "global_edge_spine": ("edge_id",),
    "global_topic_spine": ("topic_id",),
}

_SPINE_SHARED_SEMANTIC_COLUMNS: Mapping[str, tuple[str, ...]] = {
    "global_object_locator": (
        "semantic_hash",
        "object_type",
        "local_object_key",
    ),
    "global_edge_spine": (
        "semantic_hash",
        "from_object_id",
        "to_object_id",
        "relation_type",
        "edge_scope",
        "source_object_type",
        "target_object_type",
    ),
}


def _merge_spine_table(
    conn: sqlite3.Connection,
    schema_name: str,
    table_name: str,
    *,
    fragment_path: Path,
) -> int:
    """Merge one table and return the number of equivalent shared-ID rows."""
    columns = _spine_table_columns(conn, "main", table_name)
    if not columns:
        return 0
    key_columns = _SPINE_PRIMARY_KEY_COLUMNS.get(table_name, ())
    semantic_columns = _SPINE_SHARED_SEMANTIC_COLUMNS.get(table_name)
    if semantic_columns:
        equivalent_collision_count = _assert_equivalent_spine_collisions(
            conn,
            schema_name,
            table_name,
            key_columns=key_columns,
            semantic_columns=semantic_columns,
            fragment_path=fragment_path,
        )
        _insert_spine_replicas(conn, schema_name, table_name)
        _upsert_deterministic_spine_canonical(
            conn,
            schema_name,
            table_name,
            columns=columns,
            key_columns=key_columns,
        )
        _synchronize_spine_occurrence_counts(
            conn,
            schema_name,
            table_name,
            key_columns=key_columns,
        )
        return equivalent_collision_count

    if key_columns:
        collision = _first_spine_collision(
            conn,
            schema_name,
            table_name,
            key_columns=key_columns,
        )
        if collision is not None:
            raise SpineFragmentCollisionError(
                "spine_fragment_pk_collision:"
                f"table={table_name}:key={_spine_collision_key_json(collision, key_columns)}:"
                f"fragment={fragment_path}"
            )
    column_sql = ", ".join(columns)
    try:
        conn.execute(
            f"""
            INSERT INTO {table_name}({column_sql})
            SELECT {column_sql}
            FROM {schema_name}.{table_name}
            """
        )
    except sqlite3.IntegrityError as exc:
        raise SpineFragmentCollisionError(
            "spine_fragment_insert_integrity_error:"
            f"table={table_name}:key=<unknown>:fragment={fragment_path}:sqlite={exc}"
        ) from exc
    return 0


def _first_spine_collision(
    conn: sqlite3.Connection,
    schema_name: str,
    table_name: str,
    *,
    key_columns: tuple[str, ...],
) -> sqlite3.Row | tuple[Any, ...] | None:
    join_sql = " AND ".join(
        f"main_table.{column_name} = fragment_table.{column_name}" for column_name in key_columns
    )
    key_sql = ", ".join(
        f"fragment_table.{column_name} AS {column_name}" for column_name in key_columns
    )
    return conn.execute(
        f"""
        SELECT {key_sql}
        FROM main.{table_name} AS main_table
        JOIN {schema_name}.{table_name} AS fragment_table
          ON {join_sql}
        ORDER BY {", ".join(f"fragment_table.{column}" for column in key_columns)}
        LIMIT 1
        """
    ).fetchone()


def _spine_collision_key_json(
    collision: sqlite3.Row | tuple[Any, ...],
    key_columns: tuple[str, ...],
) -> str:
    key_payload = {column_name: collision[index] for index, column_name in enumerate(key_columns)}
    return json.dumps(
        key_payload,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    )


def _assert_equivalent_spine_collisions(
    conn: sqlite3.Connection,
    schema_name: str,
    table_name: str,
    *,
    key_columns: tuple[str, ...],
    semantic_columns: tuple[str, ...],
    fragment_path: Path,
) -> int:
    join_sql = " AND ".join(
        f"main_table.{column_name} = fragment_table.{column_name}" for column_name in key_columns
    )
    mismatch_sql = " OR ".join(
        f"main_table.{column_name} IS NOT fragment_table.{column_name}"
        for column_name in semantic_columns
    )
    select_columns = [
        *(f"fragment_table.{column_name} AS key_{column_name}" for column_name in key_columns),
        *(f"main_table.{column_name} AS main_{column_name}" for column_name in semantic_columns),
        *(
            f"fragment_table.{column_name} AS fragment_{column_name}"
            for column_name in semantic_columns
        ),
    ]
    collision_summary = conn.execute(
        f"""
        SELECT
            COUNT(*) AS collision_count,
            COALESCE(SUM(CASE WHEN {mismatch_sql} THEN 1 ELSE 0 END), 0)
                AS conflict_count
        FROM main.{table_name} AS main_table
        JOIN {schema_name}.{table_name} AS fragment_table
          ON {join_sql}
        """
    ).fetchone()
    collision_count = int(collision_summary["collision_count"])
    conflict_count = int(collision_summary["conflict_count"])
    if conflict_count:
        conflict = conn.execute(
            f"""
            SELECT {", ".join(select_columns)}
            FROM main.{table_name} AS main_table
            JOIN {schema_name}.{table_name} AS fragment_table
              ON {join_sql}
            WHERE {mismatch_sql}
            ORDER BY {", ".join(f"fragment_table.{column}" for column in key_columns)}
            LIMIT 1
            """
        ).fetchone()
        if conflict is None:
            raise SpineFragmentCollisionError(
                "spine_fragment_semantic_conflict:"
                f"table={table_name}:key=<unknown>:fragment={fragment_path}:"
                "fields=<unknown>"
            )
        key_values = tuple(conflict[f"key_{column}"] for column in key_columns)
        conflicting_fields = [
            column
            for column in semantic_columns
            if conflict[f"main_{column}"] != conflict[f"fragment_{column}"]
        ]
        raise SpineFragmentCollisionError(
            "spine_fragment_semantic_conflict:"
            f"table={table_name}:key={_spine_collision_key_json(key_values, key_columns)}:"
            f"fragment={fragment_path}:fields={','.join(conflicting_fields)}"
        )
    return collision_count


def _insert_spine_replicas(
    conn: sqlite3.Connection,
    schema_name: str,
    table_name: str,
) -> None:
    if table_name == "global_object_locator":
        conn.execute(
            f"""
            INSERT OR IGNORE INTO global_object_replica(
                object_id, ticker, document_id, document_type, period,
                shard_id, shard_path, object_type, local_object_key,
                object_hash, semantic_hash, quality_status
            )
            SELECT
                object_id, ticker, COALESCE(document_id, ''),
                COALESCE(document_type, ''), COALESCE(period, ''),
                shard_id, shard_path, object_type, local_object_key,
                object_hash, semantic_hash, quality_status
            FROM {schema_name}.global_object_locator
            """
        )
        return
    if table_name == "global_edge_spine":
        conn.execute(
            f"""
            INSERT OR IGNORE INTO global_edge_replica(
                edge_id, ticker, document_id, document_type, period,
                shard_id, shard_path, from_object_id, to_object_id,
                relation_type, semantic_hash, confidence, evidence_grade
            )
            SELECT
                edge_id, ticker, COALESCE(document_id, ''),
                COALESCE(document_type, ''), COALESCE(period, ''),
                shard_id, shard_path, from_object_id, to_object_id,
                relation_type, semantic_hash, confidence, evidence_grade
            FROM {schema_name}.global_edge_spine
            """
        )
        return
    raise ValueError(f"unsupported spine replica table: {table_name}")


def _upsert_deterministic_spine_canonical(
    conn: sqlite3.Connection,
    schema_name: str,
    table_name: str,
    *,
    columns: Sequence[str],
    key_columns: tuple[str, ...],
) -> None:
    merge_columns = [column for column in columns if column != "occurrence_count"]
    update_columns = [column for column in merge_columns if column not in key_columns]
    rank_columns = tuple(update_columns)
    column_sql = ", ".join(merge_columns)
    update_sql = ", ".join(f"{column}=excluded.{column}" for column in update_columns)
    excluded_rank = ", ".join(
        f"COALESCE(quote(excluded.{column}), 'NULL')" for column in rank_columns
    )
    current_rank = ", ".join(
        f"COALESCE(quote({table_name}.{column}), 'NULL')" for column in rank_columns
    )
    conn.execute(
        f"""
        INSERT INTO {table_name}({column_sql})
        SELECT {column_sql}
        FROM {schema_name}.{table_name}
        WHERE 1
        ON CONFLICT({", ".join(key_columns)}) DO UPDATE SET
            {update_sql}
        WHERE ({excluded_rank}) < ({current_rank})
        """
    )


def _synchronize_spine_occurrence_counts(
    conn: sqlite3.Connection,
    schema_name: str,
    table_name: str,
    *,
    key_columns: tuple[str, ...],
) -> None:
    key_column = key_columns[0]
    replica_table = (
        "global_object_replica" if table_name == "global_object_locator" else "global_edge_replica"
    )
    conn.execute(
        f"""
        UPDATE {table_name}
        SET occurrence_count = (
            SELECT COUNT(*)
            FROM {replica_table} AS replica
            WHERE replica.{key_column} = {table_name}.{key_column}
        )
        WHERE {key_column} IN (
            SELECT fragment.{key_column}
            FROM {schema_name}.{table_name} AS fragment
            JOIN {replica_table} AS replica
              ON replica.{key_column} = fragment.{key_column}
            GROUP BY fragment.{key_column}
            HAVING COUNT(*) > 1
        )
        """
    )


def _spine_table_columns(conn: sqlite3.Connection, schema_name: str, table_name: str) -> list[str]:
    rows = conn.execute(f"PRAGMA {schema_name}.table_info({table_name})").fetchall()
    return [str(row[1]) for row in rows]


def _count_table(conn: sqlite3.Connection, table_name: str) -> int:
    if not _table_exists(conn, table_name):
        return 0
    return int(conn.execute(f"SELECT COUNT(*) FROM {table_name}").fetchone()[0])


def _plan_v3_artifact_inputs(
    root: Path,
    *,
    sqlite_path: Path,
    cache_root: Path | None,
    workers: int | None,
    source_manifest_path: Path | None,
    source_manifest_payload: Mapping[str, Any] | None = None,
) -> SourceArtifactSqlitePlan:
    """Build the source-artifact plan used to materialize v3 shards.

    The lower-level planner still owns artifact discovery, content hashing, and
    fragment cache keys. v3 always requests shard layout and supplies an
    explicit output SQLite path so the production builder never depends on the
    legacy monolith default path.
    """
    return plan_source_artifact_sqlite_inputs(
        root,
        sqlite_path=sqlite_path,
        cache_root=cache_root,
        workers=workers,
        source_manifest_path=source_manifest_path,
        source_manifest_payload=source_manifest_payload,
    )


def _filter_plan_for_company(
    plan: SourceArtifactSqlitePlan,
    *,
    ticker: str,
    shard_path: Path,
    no_cache: bool,
    artifact_workers: int | None = None,
) -> SourceArtifactSqlitePlan:
    items = tuple(item for item in plan.items if item.ticker == ticker)
    dirty_items = items if no_cache else tuple(item for item in items if not item.cache_hit)
    cached_items = () if no_cache else tuple(item for item in items if item.cache_hit)
    artifact_cache_keys = tuple(
        f"{item.relative_path}={item.cache_key}"
        for item in sorted(items, key=lambda item: item.relative_path)
    )
    input_hash = _stable_hash(
        {
            "ticker": ticker,
            "artifact_cache_keys": artifact_cache_keys,
            "company_shard_schema_version": COMPANY_SHARD_SCHEMA_VERSION,
            "company_shard_cache_format_version": COMPANY_SHARD_CACHE_FORMAT_VERSION,
            "index_layout": GLOBAL_SPINE_LAYOUT,
        }
    )
    cache_key = _stable_hash(
        {
            "company_shard_cache_format_version": COMPANY_SHARD_CACHE_FORMAT_VERSION,
            "input_hash": input_hash,
        }
    )
    cache_path = _v3_company_shard_cache_path(plan.cache_root, cache_key)
    cache_errors = _verify_company_shard_cache_v3(cache_path, ticker=ticker, cache_key=cache_key)
    build_settings = dict(plan.build_settings)
    build_settings["progress_log_path"] = str(
        plan.root
        / "indexes"
        / "progress"
        / "source_artifact_sqlite"
        / f"{_safe_ticker_filename(ticker)}.jsonl"
    )
    company_item = SourceCompanyShardPlanItem(
        ticker=ticker,
        artifact_count=len(items),
        artifact_cache_keys=artifact_cache_keys,
        input_hash=input_hash,
        cache_key=cache_key,
        shard_cache_path=cache_path,
        cache_hit=False if no_cache else not cache_errors,
        cache_errors=() if no_cache else cache_errors,
    )
    company_cache_hit = False if no_cache else not cache_errors
    return SourceArtifactSqlitePlan(
        root=plan.root,
        index_path=shard_path,
        cache_root=plan.cache_root,
        artifact_manifest_path=shard_path.parent / "artifact_manifest.json",
        source_manifest_path=plan.source_manifest_path,
        source_manifest_hash=plan.source_manifest_hash,
        discovery_mode=plan.discovery_mode,
        items=items,
        dirty_items=dirty_items,
        cached_items=cached_items,
        company_items=(company_item,),
        dirty_company_items=(company_item,) if not company_cache_hit else (),
        cached_company_items=(company_item,) if company_cache_hit else (),
        dirty_tickers=() if company_cache_hit else (ticker,),
        workers=max(1, int(artifact_workers if artifact_workers is not None else plan.workers)),
        layout="shards",
        build_settings=build_settings,
        builder_code_version=plan.builder_code_version,
        plan_format_version=plan.plan_format_version,
    )


def _write_company_shard_metadata(
    path: Path, *, ticker: str, plan: SourceArtifactSqlitePlan
) -> None:
    with sqlite3.connect(path) as conn:
        row = conn.execute("SELECT value FROM metadata WHERE key = 'build'").fetchone()
        metadata: dict[str, Any] = {}
        if row is not None:
            try:
                metadata = json.loads(str(row[0]))
            except json.JSONDecodeError:
                metadata = {}
        metadata.update(
            {
                "schema_version": SOURCE_ARTIFACT_SQLITE_SCHEMA_VERSION,
                "agent_index_schema_version": SOURCE_ARTIFACT_SQLITE_SCHEMA_VERSION,
                "source_artifact_sqlite_schema_version": SOURCE_ARTIFACT_SQLITE_SCHEMA_VERSION,
                "source_artifact_sqlite_builder_version": SOURCE_ARTIFACT_SQLITE_BUILDER_VERSION,
                "company_shard_schema_version": COMPANY_SHARD_SCHEMA_VERSION,
                "index_layout": GLOBAL_SPINE_LAYOUT,
                "index_role": "company_shard",
                "shard_ticker": ticker,
                "company_cache_format_version": COMPANY_SHARD_CACHE_FORMAT_VERSION,
                "company_cache_key": plan.company_items[0].cache_key
                if plan.company_items
                else None,
                "company_source_hash": _stable_hash(
                    {
                        "ticker": ticker,
                        "artifact_cache_keys": [
                            f"{item.relative_path}={item.cache_key}"
                            for item in sorted(plan.items, key=lambda item: item.relative_path)
                        ],
                    }
                ),
                "spine_projection_version": SPINE_PROJECTION_VERSION,
            }
        )
        conn.execute(
            "INSERT OR REPLACE INTO metadata(key, value) VALUES('build', ?)",
            (json.dumps(metadata, sort_keys=True),),
        )
        conn.commit()
        conn.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchall()
        conn.execute("PRAGMA journal_mode=DELETE").fetchall()


def _v3_company_shard_cache_path(cache_root: Path, cache_key: str) -> Path:
    digest = cache_key.split(":", 1)[-1]
    return cache_root / "v3" / "company_shards" / digest[:2] / f"{digest}.sqlite"


def _verify_company_shard_cache_v3(
    shard_path: Path,
    *,
    ticker: str,
    cache_key: str,
) -> tuple[str, ...]:
    if not shard_path.exists():
        return ("company_cache_missing",)
    seal, seal_status = read_immutable_sqlite_cache_seal(
        shard_path,
        kind="company_shard",
        cache_key=cache_key,
    )
    errors: list[str] = []
    if seal_status == "valid":
        raw_metadata = seal.get("metadata")
        metadata = dict(raw_metadata) if isinstance(raw_metadata, Mapping) else {}
    else:
        verification = verify_source_artifact_sqlite(shard_path)
        errors.extend(f"company_cache:{error}" for error in verification.get("errors") or [])
        try:
            metadata = _read_company_shard_metadata(shard_path)
        except (sqlite3.Error, json.JSONDecodeError) as exc:
            errors.append(f"company_cache_metadata_error:{exc}")
            metadata = {}
    if metadata.get("index_role") != "company_shard":
        errors.append("company_cache_metadata_role_mismatch")
    if str(metadata.get("shard_ticker") or "") != ticker:
        errors.append("company_cache_metadata_ticker_mismatch")
    if metadata.get("index_layout") != GLOBAL_SPINE_LAYOUT:
        errors.append("company_cache_metadata_layout_mismatch")
    if metadata.get("company_shard_schema_version") != COMPANY_SHARD_SCHEMA_VERSION:
        errors.append("company_cache_metadata_schema_mismatch")
    if metadata.get("company_cache_format_version") != COMPANY_SHARD_CACHE_FORMAT_VERSION:
        errors.append("company_cache_metadata_format_mismatch")
    if metadata.get("company_cache_key") != cache_key:
        errors.append("company_cache_metadata_key_mismatch")
    if not errors and seal_status != "valid":
        write_immutable_sqlite_cache_seal(
            shard_path,
            kind="company_shard",
            cache_key=cache_key,
            verification=verification,
            metadata=metadata,
            counts=verification.get("counts") or {},
        )
    return tuple(errors)


def _read_company_shard_metadata(path: Path) -> dict[str, Any]:
    with sqlite3.connect(path) as conn:
        row = conn.execute("SELECT value FROM metadata WHERE key = 'build'").fetchone()
    if row is None:
        return {}
    payload = json.loads(str(row[0]))
    return dict(payload) if isinstance(payload, Mapping) else {}


def _verify_restored_company_shard(
    path: Path,
    *,
    ticker: str,
    cache_key: str,
) -> dict[str, Any]:
    """Check a cache clone structurally; final release verification owns the deep pass."""
    errors: list[str] = []
    counts: dict[str, int] = {}
    metadata: dict[str, Any] = {}
    required_tables = {"metadata", "documents", "objects", "edges", "quality_events"}
    try:
        with sqlite3.connect(path) as conn:
            tables = {
                str(row[0])
                for row in conn.execute(
                    "SELECT name FROM sqlite_master WHERE type IN ('table', 'view')"
                )
            }
            errors.extend(f"table_missing:{table}" for table in sorted(required_tables - tables))
            row = conn.execute("SELECT value FROM metadata WHERE key = 'build'").fetchone()
            metadata = json.loads(str(row[0])) if row is not None else {}
            for table in ("documents", "objects", "edges", "quality_events"):
                if table in tables:
                    counts[table] = int(conn.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0])
    except (sqlite3.Error, json.JSONDecodeError) as exc:
        errors.append(f"sqlite_error:{exc}")
    expected = {
        "index_role": "company_shard",
        "shard_ticker": ticker,
        "index_layout": GLOBAL_SPINE_LAYOUT,
        "company_shard_schema_version": COMPANY_SHARD_SCHEMA_VERSION,
        "company_cache_format_version": COMPANY_SHARD_CACHE_FORMAT_VERSION,
        "company_cache_key": cache_key,
    }
    errors.extend(
        f"metadata_mismatch:{key}" for key, value in expected.items() if metadata.get(key) != value
    )
    return {
        "ok": not errors,
        "errors": errors,
        "index_path": str(path),
        "integrity_check": "ok" if not errors else None,
        "integrity_source": "inherited_immutable_cache_seal",
        "counts": counts,
        "metadata": metadata,
    }


def _copy_sqlite_database(source_path: Path, target_path: Path) -> None:
    cleanup_sqlite_database_files(target_path)
    target_path.parent.mkdir(parents=True, exist_ok=True)
    _clone_or_copy_file(source_path, target_path)


def clone_or_copy_immutable_file(source_path: Path, target_path: Path) -> str:
    """Public immutable-file copy boundary used by release materialization."""
    return _clone_or_copy_file(source_path, target_path)


def clone_or_copy_immutable_tree(
    source_path: Path,
    target_path: Path,
    *,
    ignored_names: Sequence[str] = (),
) -> str:
    """Clone an immutable directory tree when supported, with a safe copy fallback."""
    resolved_source = source_path.expanduser().resolve()
    resolved_target = target_path.expanduser().resolve()
    if not resolved_source.is_dir():
        raise NotADirectoryError(f"immutable tree source is not a directory: {resolved_source}")
    if resolved_target.exists() or resolved_target.is_symlink():
        raise FileExistsError(f"immutable tree target already exists: {resolved_target}")
    resolved_target.parent.mkdir(parents=True, exist_ok=True)
    configured_mode = os.getenv("KRW_INDEX_COPY_MODE", "auto").strip().lower()
    if _try_reflink_tree(resolved_source, resolved_target):
        _remove_ignored_tree_entries(resolved_target, ignored_names)
        return "reflink"
    if configured_mode in {"clone", "reflink", "reflink-required", "cow-required"}:
        raise RuntimeError(
            f"reflink_tree_required_but_unavailable:{resolved_source}:{resolved_target}"
        )
    ignore = shutil.ignore_patterns(*ignored_names) if ignored_names else None
    shutil.copytree(resolved_source, resolved_target, ignore=ignore)
    return "copy"


def _clone_or_copy_file(source_path: Path, target_path: Path) -> str:
    """Copy an immutable build artifact, preferring filesystem CoW clones."""
    configured_mode = os.getenv("KRW_INDEX_COPY_MODE", "auto").strip().lower()
    if _try_reflink_copy(source_path, target_path):
        try:
            shutil.copystat(source_path, target_path)
        except OSError:
            cleanup_sqlite_database_files(target_path)
        else:
            return "reflink"
    if configured_mode in {"clone", "reflink", "reflink-required", "cow-required"}:
        raise RuntimeError(f"reflink_required_but_unavailable:{source_path}:{target_path}")
    shutil.copy2(source_path, target_path)
    return "copy"


def _try_reflink_tree(source_path: Path, target_path: Path) -> bool:
    configured_mode = os.getenv("KRW_INDEX_COPY_MODE", "auto").strip().lower()
    if configured_mode in {"copy", "full-copy", "disabled", "off"}:
        return False
    copy_command = shutil.which("cp")
    if not copy_command:
        return False
    if sys.platform == "darwin":
        command = [copy_command, "-cR", str(source_path), str(target_path)]
    elif sys.platform.startswith("linux"):
        command = [
            copy_command,
            "--archive",
            "--reflink=always",
            str(source_path),
            str(target_path),
        ]
    else:
        return False
    try:
        result = subprocess.run(
            command,
            check=False,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    except OSError:
        return False
    if result.returncode == 0 and target_path.is_dir():
        return True
    shutil.rmtree(target_path, ignore_errors=True)
    return False


def _remove_ignored_tree_entries(root: Path, ignored_names: Sequence[str]) -> None:
    for ignored_name in sorted({str(value) for value in ignored_names if str(value)}):
        matches = sorted(root.rglob(ignored_name), key=lambda path: len(path.parts), reverse=True)
        for path in matches:
            if path.is_dir() and not path.is_symlink():
                shutil.rmtree(path)
            else:
                path.unlink(missing_ok=True)


def _try_reflink_copy(source_path: Path, target_path: Path) -> bool:
    configured_mode = os.getenv("KRW_INDEX_COPY_MODE", "auto").strip().lower()
    if configured_mode in {"copy", "full-copy", "disabled", "off"}:
        return False
    copy_command = shutil.which("cp")
    if not copy_command:
        return False
    if sys.platform == "darwin":
        command = [copy_command, "-c", str(source_path), str(target_path)]
    elif sys.platform.startswith("linux"):
        command = [
            copy_command,
            "--reflink=always",
            "--preserve=mode,timestamps",
            str(source_path),
            str(target_path),
        ]
    else:
        return False
    try:
        result = subprocess.run(
            command,
            check=False,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    except OSError:
        return False
    if result.returncode == 0 and target_path.is_file():
        return True
    cleanup_sqlite_database_files(target_path)
    return False


def _rebase_company_shard_release_paths(sqlite_path: Path, *, release_root: Path) -> None:
    resolved_root = release_root.expanduser().resolve()
    with sqlite3.connect(sqlite_path) as conn:
        _rebase_table_path_columns(
            conn,
            resolved_root,
            {
                "documents": ("artifact_index_path", "ontology_dir"),
                "objects": ("artifact_path",),
                "edges": ("artifact_path",),
            },
        )
        _rebase_metadata_json_values(conn, resolved_root)
        conn.commit()
        conn.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchall()
        conn.execute("PRAGMA journal_mode=DELETE").fetchall()


def _rebase_spine_fragment_release_paths(
    sqlite_path: Path,
    *,
    release_root: Path,
    release_id: str,
    source_shard_path: Path,
    shard_path_in_release: str,
) -> None:
    resolved_root = release_root.expanduser().resolve()
    with sqlite3.connect(sqlite_path) as conn:
        _rebase_table_path_columns(
            conn,
            resolved_root,
            {
                "global_document_catalog": ("source_path",),
            },
        )
        _rebase_metadata_json_values(
            conn,
            resolved_root,
            overrides={
                "release_id": release_id,
                "source_shard_path": str(source_shard_path.expanduser().resolve()),
                "shard_path": shard_path_in_release,
            },
        )
        conn.commit()
        conn.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchall()
        conn.execute("PRAGMA journal_mode=DELETE").fetchall()


def _rebase_global_spine_release_paths(
    sqlite_path: Path,
    *,
    release_root: Path,
    release_id: str,
) -> None:
    resolved_root = release_root.expanduser().resolve()
    with sqlite3.connect(sqlite_path) as conn:
        _rebase_table_path_columns(
            conn,
            resolved_root,
            {"global_document_catalog": ("source_path",)},
        )
        _rebase_metadata_json_values(
            conn,
            resolved_root,
            overrides={
                "release_id": release_id,
                "created_at": datetime.now(timezone.utc).isoformat(),
            },
        )
        conn.commit()
        conn.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchall()
        conn.execute("PRAGMA journal_mode=DELETE").fetchall()


def _verify_restored_global_spine(
    sqlite_path: Path,
    *,
    cache_key: str,
    release_id: str,
) -> dict[str, Any]:
    errors: list[str] = []
    metadata: dict[str, Any] = {}
    try:
        with sqlite3.connect(sqlite_path) as conn:
            tables = {
                str(row[0])
                for row in conn.execute(
                    "SELECT name FROM sqlite_master WHERE type IN ('table', 'view')"
                )
            }
            errors.extend(
                f"global_spine_table_missing:{table}"
                for table in sorted(set(GLOBAL_SPINE_TABLES) - tables)
            )
            metadata = read_global_spine_metadata(conn)
    except sqlite3.Error as exc:
        errors.append(f"sqlite_error:{exc}")
    expected = {
        "schema_version": GLOBAL_SPINE_SCHEMA_VERSION,
        "builder_version": GLOBAL_SPINE_BUILDER_VERSION,
        "index_layout": GLOBAL_SPINE_LAYOUT,
        "release_id": release_id,
        "global_spine_cache_format_version": GLOBAL_SPINE_CACHE_FORMAT_VERSION,
        "global_spine_cache_key": cache_key,
    }
    errors.extend(
        f"global_spine_metadata_mismatch:{key}"
        for key, value in expected.items()
        if metadata.get(key) != value
    )
    return {"ok": not errors, "errors": errors, "metadata": metadata}


def _verify_restored_spine_fragment(
    sqlite_path: Path,
    *,
    ticker: str,
    cache_key: str,
    release_id: str,
    shard_path_in_release: str,
) -> dict[str, Any]:
    """Validate only the metadata mutation applied to a deeply sealed cache clone."""
    errors: list[str] = []
    metadata: dict[str, Any] = {}
    try:
        with sqlite3.connect(sqlite_path) as conn:
            tables = {
                str(row[0])
                for row in conn.execute(
                    "SELECT name FROM sqlite_master WHERE type IN ('table', 'view')"
                )
            }
            errors.extend(
                f"spine_fragment_table_missing:{table}"
                for table in sorted(set(SPINE_FRAGMENT_TABLES) - tables)
            )
            metadata = read_global_spine_metadata(conn)
    except sqlite3.Error as exc:
        errors.append(f"sqlite_error:{exc}")
    expected = {
        "format": SPINE_FRAGMENT_FORMAT_VERSION,
        "fragment_schema_version": SPINE_FRAGMENT_SCHEMA_VERSION,
        "schema_version": GLOBAL_SPINE_SCHEMA_VERSION,
        "builder_version": GLOBAL_SPINE_BUILDER_VERSION,
        "index_layout": GLOBAL_SPINE_LAYOUT,
        "ticker": ticker,
        "spine_fragment_cache_format_version": SPINE_FRAGMENT_CACHE_FORMAT_VERSION,
        "spine_fragment_cache_key": cache_key,
        "release_id": release_id,
        "shard_path": shard_path_in_release,
    }
    errors.extend(
        f"spine_fragment_metadata_mismatch:{key}"
        for key, value in expected.items()
        if metadata.get(key) != value
    )
    return {
        "ok": not errors,
        "errors": errors,
        "metadata": metadata,
        "integrity_source": "inherited_immutable_seal",
    }


def _rebase_table_path_columns(
    conn: sqlite3.Connection,
    release_root: Path,
    table_columns: Mapping[str, Sequence[str]],
) -> None:
    for table_name, column_names in table_columns.items():
        if not _table_exists(conn, table_name):
            continue
        columns = _table_columns(conn, table_name)
        for column_name in column_names:
            if column_name not in columns:
                continue
            quoted_table = _quote_identifier(table_name)
            quoted_column = _quote_identifier(column_name)
            rows = conn.execute(
                f"SELECT rowid, {quoted_column} AS value FROM {quoted_table} WHERE {quoted_column} IS NOT NULL"
            ).fetchall()
            updates = []
            for rowid, raw_value in rows:
                if not isinstance(raw_value, str):
                    continue
                rebased = _rebase_release_path_string(raw_value, release_root)
                if rebased != raw_value:
                    updates.append((rebased, rowid))
            if updates:
                conn.executemany(
                    f"UPDATE {quoted_table} SET {quoted_column} = ? WHERE rowid = ?",
                    updates,
                )


def _rebase_metadata_json_values(
    conn: sqlite3.Connection,
    release_root: Path,
    *,
    overrides: Mapping[str, Any] | None = None,
) -> None:
    if not _table_exists(conn, "metadata"):
        return
    columns = _table_columns(conn, "metadata")
    value_column = (
        "value_json" if "value_json" in columns else "value" if "value" in columns else None
    )
    if value_column is None:
        return
    key_column = "key" if "key" in columns else None
    quoted_value_column = _quote_identifier(value_column)
    if key_column is None:
        rows = [
            (row[0], None, row[1])
            for row in conn.execute(f"SELECT rowid, {quoted_value_column} FROM metadata")
        ]
    else:
        quoted_key_column = _quote_identifier(key_column)
        rows = conn.execute(
            f"SELECT rowid, {quoted_key_column}, {quoted_value_column} FROM metadata"
        ).fetchall()
    updates = []
    for rowid, key, raw_value in rows:
        if not isinstance(raw_value, str):
            continue
        override_value = overrides.get(str(key)) if overrides and key is not None else None
        if override_value is not None:
            encoded = json.dumps(override_value, sort_keys=True)
            if encoded != raw_value:
                updates.append((encoded, rowid))
            continue
        try:
            decoded = json.loads(raw_value)
        except json.JSONDecodeError:
            rebased_text = _rebase_release_path_string(raw_value, release_root)
            if rebased_text != raw_value:
                updates.append((rebased_text, rowid))
            continue
        rebased = _rebase_release_paths(decoded, release_root)
        if isinstance(rebased, dict) and overrides:
            rebased.update(dict(overrides))
        encoded = json.dumps(rebased, sort_keys=True)
        if encoded != raw_value:
            updates.append((encoded, rowid))
    if updates:
        conn.executemany(
            f"UPDATE metadata SET {quoted_value_column} = ? WHERE rowid = ?",
            updates,
        )


def _rebase_release_paths(value: Any, release_root: Path) -> Any:
    if isinstance(value, str):
        return _rebase_release_path_string(value, release_root)
    if isinstance(value, list):
        return [_rebase_release_paths(item, release_root) for item in value]
    if isinstance(value, dict):
        return {key: _rebase_release_paths(item, release_root) for key, item in value.items()}
    return value


def _rebase_release_path_string(value: str, release_root: Path) -> str:
    if not value or not Path(value).is_absolute():
        return value
    path = Path(value)
    parts = path.parts
    for index in range(len(parts) - 2):
        if parts[index] != "releases":
            continue
        suffix = parts[index + 3 :]
        return str(release_root.joinpath(*suffix)) if suffix else str(release_root)
    for marker in ("companies", "indexes"):
        if marker in parts:
            index = parts.index(marker)
            return str(release_root.joinpath(*parts[index:]))
    return value


def _table_columns(conn: sqlite3.Connection, table_name: str) -> set[str]:
    return {
        str(row[1])
        for row in conn.execute(f"PRAGMA table_info({_quote_identifier(table_name)})").fetchall()
    }


def _quote_identifier(value: str) -> str:
    return '"' + value.replace('"', '""') + '"'


def _store_sqlite_database_cache(
    source_path: Path,
    cache_path: Path,
    *,
    verification: Mapping[str, Any] | None = None,
    cache_kind: str | None = None,
    cache_key: str | None = None,
    cache_metadata: Mapping[str, Any] | None = None,
    cache_counts: Mapping[str, Any] | None = None,
) -> None:
    cache_path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = cache_path.parent / f".{cache_path.name}.{os.getpid()}.{time.time_ns()}.tmp"
    cleanup_sqlite_database_files(tmp_path)
    try:
        _clone_or_copy_file(source_path, tmp_path)
        os.replace(tmp_path, cache_path)
        if verification is not None and cache_kind is not None and cache_key is not None:
            write_immutable_sqlite_cache_seal(
                cache_path,
                kind=cache_kind,
                cache_key=cache_key,
                verification=verification,
                metadata=cache_metadata,
                counts=cache_counts,
                source_path=source_path,
            )
        elif verification is not None:
            write_spine_verification_seal(
                cache_path,
                verification,
                source_path=source_path,
            )
    finally:
        cleanup_sqlite_database_files(tmp_path)


def _quarantine_sqlite_cache(cache_path: Path) -> Path | None:
    if not cache_path.exists():
        return None
    target = cache_path.with_name(f".corrupt-{cache_path.name}.{os.getpid()}.{time.time_ns()}")
    cleanup_sqlite_database_files(target)
    try:
        os.replace(cache_path, target)
    except OSError:
        cleanup_sqlite_database_files(cache_path)
        remove_immutable_sqlite_cache_seal(cache_path)
        spine_verification_seal_path(cache_path).unlink(missing_ok=True)
        return None
    remove_immutable_sqlite_cache_seal(cache_path)
    spine_verification_seal_path(cache_path).unlink(missing_ok=True)
    return target


def _company_shard_counts(path: Path) -> dict[str, int]:
    tables = ("documents", "objects", "edges", "quality_events")
    with sqlite3.connect(path) as conn:
        return {table: _count_table(conn, table) for table in tables}


def _company_shard_quality_summary(path: Path) -> dict[str, Any]:
    with sqlite3.connect(path) as conn:
        conn.row_factory = sqlite3.Row
        totals = {
            "documents": _count_table(conn, "documents"),
            "tickers": _count_distinct(conn, "documents", "ticker"),
            "objects": _count_table(conn, "objects"),
            "quality_events": _count_table(conn, "quality_events"),
        }
        section_status = (
            {
                str(row["section_quality_status"] or "unknown"): int(row["cnt"] or 0)
                for row in conn.execute(
                    """
                    SELECT section_quality_status, COUNT(*) AS cnt
                    FROM documents
                    GROUP BY section_quality_status
                    """
                )
            }
            if _table_exists(conn, "documents")
            else {}
        )
        event_counts = (
            [
                {
                    "category": str(row["category"] or ""),
                    "severity": str(row["severity"] or ""),
                    "stage": str(row["stage"] or ""),
                    "count": int(row["count"] or 0),
                }
                for row in conn.execute(
                    """
                    SELECT category, severity, COALESCE(stage, '') AS stage, COUNT(*) AS count
                    FROM quality_events
                    GROUP BY category, severity, stage
                    ORDER BY count DESC, category, severity, stage
                    """
                )
            ]
            if _table_exists(conn, "quality_events")
            else []
        )
        ticker_quality = _company_shard_ticker_quality_summary(conn)
        rejected_reasons = (
            [
                {"reason": str(row["reason"] or ""), "count": int(row["count"] or 0)}
                for row in conn.execute(
                    """
                    SELECT
                        CASE
                            WHEN message LIKE 'Unsupported numeric values:%'
                                THEN 'Unsupported numeric values'
                            WHEN message LIKE 'Dangling references:%'
                                THEN 'Dangling references'
                            ELSE message
                        END AS reason,
                        COUNT(*) AS count
                    FROM quality_events
                    WHERE category='rejected_object'
                    GROUP BY reason
                    ORDER BY count DESC, reason
                    LIMIT 20
                    """
                )
            ]
            if _table_exists(conn, "quality_events")
            else []
        )
    return {
        "format": SHARD_QUALITY_SUMMARY_FORMAT_VERSION,
        "totals": totals,
        "section_status": section_status,
        "event_counts": event_counts,
        "ticker_quality": ticker_quality,
        "rejected_reasons": rejected_reasons,
    }


def _company_shard_ticker_quality_summary(conn: sqlite3.Connection) -> list[dict[str, Any]]:
    if not _table_exists(conn, "documents"):
        return []
    if not _table_exists(conn, "quality_events"):
        sql = """
            SELECT ticker,
                   COUNT(*) AS docs,
                   SUM(CASE WHEN section_quality_status='fail' THEN 1 ELSE 0 END) AS section_fail,
                   SUM(CASE WHEN section_quality_status='warn' THEN 1 ELSE 0 END) AS section_warn,
                   0 AS batch_failure,
                   0 AS coverage_gap,
                   0 AS rejected_object
            FROM documents
            GROUP BY ticker
            ORDER BY ticker
        """
    else:
        sql = """
            WITH d AS (
                SELECT ticker,
                       COUNT(*) AS docs,
                       SUM(CASE WHEN section_quality_status='fail' THEN 1 ELSE 0 END) AS section_fail,
                       SUM(CASE WHEN section_quality_status='warn' THEN 1 ELSE 0 END) AS section_warn
                FROM documents
                GROUP BY ticker
            ),
            e AS (
                SELECT ticker,
                       SUM(CASE WHEN category='batch_failure' THEN 1 ELSE 0 END) AS batch_failure,
                       SUM(CASE WHEN category='coverage_gap' THEN 1 ELSE 0 END) AS coverage_gap,
                       SUM(CASE WHEN category='rejected_object' THEN 1 ELSE 0 END) AS rejected_object
                FROM quality_events
                GROUP BY ticker
            )
            SELECT d.ticker, d.docs, d.section_fail, d.section_warn,
                   COALESCE(e.batch_failure, 0) AS batch_failure,
                   COALESCE(e.coverage_gap, 0) AS coverage_gap,
                   COALESCE(e.rejected_object, 0) AS rejected_object
            FROM d
            LEFT JOIN e ON d.ticker=e.ticker
            ORDER BY d.ticker
        """
    return [
        {
            "ticker": str(row["ticker"] or ""),
            "docs": int(row["docs"] or 0),
            "section_fail": int(row["section_fail"] or 0),
            "section_warn": int(row["section_warn"] or 0),
            "batch_failure": int(row["batch_failure"] or 0),
            "coverage_gap": int(row["coverage_gap"] or 0),
            "rejected_object": int(row["rejected_object"] or 0),
        }
        for row in conn.execute(sql)
    ]


def _count_distinct(conn: sqlite3.Connection, table_name: str, column_name: str) -> int:
    if not _table_exists(conn, table_name):
        return 0
    return int(
        conn.execute(f"SELECT COUNT(DISTINCT {column_name}) FROM {table_name}").fetchone()[0]
    )


def _company_build_cost(plan: SourceArtifactSqlitePlan) -> int:
    return sum(max(int(item.estimated_bytes or 0), 1) for item in plan.items) + len(plan.items)


def _resolve_build_parallelism(
    plan: SourceArtifactSqlitePlan,
    *,
    artifact_count: int,
    company_count: int,
) -> BuildParallelism:
    """Resolve stage concurrency without ever nesting process pools.

    The source-artifact materializer reserves a large SQLite cache per company
    process, so CPU count alone is not a safe concurrency limit.  This resolver
    applies one CPU budget to every (non-overlapping) stage and additionally
    caps the company stage by physical memory.
    """
    cpu_count = max(1, int(os.cpu_count() or 1))
    requested = max(1, int(plan.workers))
    cpu_budget = min(requested, max(1, cpu_count - 1))
    physical_memory_mib = _physical_memory_mib()
    sqlite_cache_mib = max(1, int(plan.build_settings.get("sqlite_cache_mib") or 0))
    estimated_company_worker_mib = max(
        sqlite_cache_mib + DEFAULT_COMPANY_WORKER_MEMORY_OVERHEAD_MIB,
        DEFAULT_COMPANY_WORKER_MEMORY_OVERHEAD_MIB,
    )
    memory_limited_company_workers: int | None = None
    if physical_memory_mib is not None:
        reserve_mib = max(DEFAULT_BUILD_MEMORY_RESERVE_MIB, physical_memory_mib // 5)
        usable_mib = max(estimated_company_worker_mib, physical_memory_mib - reserve_mib)
        memory_limited_company_workers = max(1, usable_mib // estimated_company_worker_mib)

    artifact_default = min(cpu_budget, max(1, artifact_count))
    company_default = min(cpu_budget, max(1, company_count))
    if memory_limited_company_workers is not None:
        company_default = min(company_default, memory_limited_company_workers)
    spine_default = min(cpu_budget, max(1, company_count), 8)
    if memory_limited_company_workers is not None:
        spine_default = min(spine_default, memory_limited_company_workers)

    return BuildParallelism(
        requested_workers=requested,
        cpu_count=cpu_count,
        physical_memory_mib=physical_memory_mib,
        artifact_compile_workers=_stage_worker_override(
            "KRW_INDEX_ARTIFACT_WORKERS",
            artifact_default,
            maximum=min(max(1, artifact_count), cpu_budget),
        ),
        company_workers=_stage_worker_override(
            "KRW_INDEX_COMPANY_WORKERS",
            company_default,
            maximum=min(
                max(1, company_count),
                cpu_budget,
                memory_limited_company_workers or cpu_budget,
            ),
        ),
        company_inner_artifact_workers=1,
        spine_fragment_workers=_stage_worker_override(
            "KRW_INDEX_SPINE_WORKERS",
            spine_default,
            maximum=min(
                max(1, company_count),
                cpu_budget,
                8,
                memory_limited_company_workers or cpu_budget,
            ),
        ),
        estimated_company_worker_mib=estimated_company_worker_mib,
        memory_limited_company_workers=memory_limited_company_workers,
    )


def _stage_worker_override(name: str, default: int, *, maximum: int) -> int:
    raw = os.getenv(name)
    if not raw:
        return max(1, min(int(default), int(maximum)))
    try:
        value = int(raw)
    except ValueError:
        return max(1, min(int(default), int(maximum)))
    return max(1, min(value, int(maximum)))


def _physical_memory_mib() -> int | None:
    try:
        page_size = int(os.sysconf("SC_PAGE_SIZE"))
        page_count = int(os.sysconf("SC_PHYS_PAGES"))
    except (AttributeError, OSError, TypeError, ValueError):
        return None
    if page_size <= 0 or page_count <= 0:
        return None
    return (page_size * page_count) // (1024 * 1024)


def _file_size_or_zero(path: Path) -> int:
    try:
        return int(path.stat().st_size)
    except OSError:
        return 0


def _safe_ticker_filename(ticker: str) -> str:
    return re.sub(r"[^A-Z0-9._-]+", "_", _normalize_ticker(ticker))


class _BuildProgressWriter:
    def __init__(self, path: Path, *, release_id: str, release_root: Path) -> None:
        self.path = path.expanduser().resolve()
        self.release_id = release_id
        self.release_root = release_root.expanduser().resolve()
        self.path.parent.mkdir(parents=True, exist_ok=True)
        try:
            self.path.unlink()
        except FileNotFoundError:
            pass

    def record(
        self,
        node_id: str,
        stage: str,
        status: str,
        *,
        event: str = "node_status",
        ticker: str | None = None,
        output: Path | str | None = None,
        cache_hit: bool | None = None,
        details: Mapping[str, Any] | None = None,
        error: str | None = None,
        started_at: float | None = None,
    ) -> None:
        payload: dict[str, Any] = {
            "format": V3_BUILD_PROGRESS_FORMAT_VERSION,
            "release_id": self.release_id,
            "event": event,
            "node_id": node_id,
            "stage": stage,
            "status": status,
            "timestamp": datetime.now(timezone.utc).isoformat(),
        }
        if ticker:
            payload["ticker"] = ticker
        if output is not None:
            payload["output"] = _progress_output_label(self.release_root, output)
        if cache_hit is not None:
            payload["cache_hit"] = bool(cache_hit)
        if details:
            payload["details"] = dict(details)
        if error:
            payload["error"] = error
        if started_at is not None:
            payload["duration_ms"] = max(0, int((time.perf_counter() - started_at) * 1000))
        with self.path.open("a", encoding="utf-8") as handle:
            handle.write(
                json.dumps(payload, ensure_ascii=False, sort_keys=True, default=str) + "\n"
            )


def _progress_output_label(root: Path, output: Path | str) -> str:
    if isinstance(output, Path):
        return _path_label(root, output)
    return str(output)


def _path_label(root: Path, path: Path) -> str:
    resolved_root = root.expanduser().resolve()
    resolved_path = path.expanduser().resolve()
    try:
        return str(resolved_path.relative_to(resolved_root))
    except ValueError:
        return str(resolved_path)


def _file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _cleanup_release_spine_fragments(fragments_dir: Path, *, release_root: Path) -> dict[str, Any]:
    resolved_root = release_root.expanduser().resolve()
    resolved_fragments = fragments_dir.expanduser().resolve()
    try:
        resolved_fragments.relative_to(resolved_root)
    except ValueError as exc:
        raise ValueError(
            f"Refusing to clean spine fragments outside release root: {resolved_fragments}"
        ) from exc

    details: dict[str, Any] = {
        "path": _path_label(resolved_root, resolved_fragments),
        "removed": False,
        "file_count": 0,
        "size_bytes": 0,
    }
    if not resolved_fragments.exists():
        return details

    file_count = 0
    size_bytes = 0
    for path in resolved_fragments.rglob("*"):
        if not path.is_file():
            continue
        file_count += 1
        try:
            size_bytes += path.stat().st_size
        except OSError:
            continue
    shutil.rmtree(resolved_fragments)
    details.update(
        {
            "removed": True,
            "file_count": file_count,
            "size_bytes": size_bytes,
        }
    )

    parent = resolved_fragments.parent
    try:
        if parent != resolved_root and parent.exists() and not any(parent.iterdir()):
            parent.rmdir()
            details["parent_removed"] = _path_label(resolved_root, parent)
    except OSError:
        details["parent_removed"] = None
    return details


def _write_json(path: Path, payload: Mapping[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = path.with_suffix(path.suffix + ".tmp")
    tmp_path.write_text(
        json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True, default=str) + "\n",
        encoding="utf-8",
    )
    tmp_path.replace(path)


def _source_rows(conn: sqlite3.Connection, table_name: str) -> list[sqlite3.Row]:
    if not _table_exists(conn, table_name):
        return []
    return list(conn.execute(f"SELECT * FROM {table_name}").fetchall())


def _table_exists(conn: sqlite3.Connection, table_name: str) -> bool:
    row = conn.execute(
        "SELECT 1 FROM sqlite_master WHERE type IN ('table', 'view') AND name = ?",
        (table_name,),
    ).fetchone()
    return row is not None


def _rows_by_key(rows: Sequence[sqlite3.Row], key: str) -> dict[str, sqlite3.Row]:
    result: dict[str, sqlite3.Row] = {}
    for row in rows:
        value = row[key]
        if value is not None:
            result[str(value)] = row
    return result


def _emit_document_catalog(
    conn: sqlite3.Connection,
    rows: Sequence[sqlite3.Row],
    *,
    ticker: str,
    shard_path: str,
) -> int:
    payload: list[tuple[Any, ...]] = []
    for row in rows:
        document_id = _document_id(row, ticker=ticker)
        payload.append(
            (
                document_id,
                effective_ticker(row["ticker"], fallback=ticker),
                None,
                row["document_type"],
                row["period"],
                _fiscal_year(row["period"]),
                _fiscal_quarter(row["period"]),
                row["generated_at"],
                row["artifact_index_path"],
                ticker,
                shard_path,
                _stable_hash(
                    {
                        "artifact_index_path": row["artifact_index_path"],
                        "counts_json": row["counts_json"],
                        "section_quality_json": row["section_quality_json"],
                    }
                ),
                _json_count(row["counts_json"], default_key="objects"),
                _json_count(row["counts_json"], default_key="edges"),
                _json_count(row["counts_json"], default_key="quality_events"),
                row["section_quality_status"],
            )
        )
    conn.executemany(
        """
        INSERT INTO global_document_catalog(
            document_id, ticker, company_name, document_type, period,
            fiscal_year, fiscal_quarter, filing_date, source_path, shard_id,
            shard_path, document_hash, object_count, edge_count,
            quality_event_count, quality_status
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        payload,
    )
    return len(payload)


def _document_source_metadata(row: sqlite3.Row) -> dict[str, Any]:
    artifact_index_path = str(row["artifact_index_path"] or "")
    if not artifact_index_path:
        return {}
    try:
        doc_root = Path(artifact_index_path).parent
    except TypeError:
        return {}

    document_type = str(row["document_type"] or "")
    doc_type_key = document_type.replace("-", "").upper()
    period = str(row["period"] or "")
    candidates: list[tuple[Path, str]] = [
        (doc_root / "source_documents.jsonl", "jsonl"),
        (doc_root / "metadata.json", "json"),
    ]
    if len(doc_root.parents) >= 3:
        company_root = doc_root.parents[2]
        candidates.append(
            (company_root / "sources" / doc_type_key / period / "metadata.json", "json")
        )

    metadata: dict[str, Any] = {}
    for path, file_type in candidates:
        if not path.exists() or path.stat().st_size <= 0:
            continue
        try:
            if file_type == "jsonl":
                line = path.read_text(encoding="utf-8").splitlines()[0]
                data = _json_loads(line)
            else:
                data = _json_loads(path.read_text(encoding="utf-8"))
        except (OSError, IndexError, ValueError, TypeError):
            continue
        if isinstance(data, dict):
            for key, value in data.items():
                if value not in (None, ""):
                    metadata[key] = value
    return metadata


def _metadata_value(metadata: Mapping[str, Any], *keys: str) -> str | None:
    for key in keys:
        value = metadata.get(key)
        if value not in (None, ""):
            return str(value)
    return None


def _emit_object_locator_and_search(
    conn: sqlite3.Connection,
    rows: Sequence[sqlite3.Row],
    *,
    search_rows: Mapping[str, sqlite3.Row],
    object_id_map: Mapping[str, str],
    object_payload: Mapping[str, Mapping[str, Any]],
    ticker: str,
    shard_path: str,
) -> int:
    locator_rows: list[tuple[Any, ...]] = []
    for row in rows:
        local_object_id = str(row["id"])
        object_id = object_id_map[local_object_id]
        search = search_rows.get(local_object_id)
        obj = object_payload.get(local_object_id) or _json_loads(row["json"])
        compact_text = _row_value(search, "compact_text") or row["text"]
        compact_label = _compact_label(row, obj)
        semantic_hash = _semantic_object_hash(row, obj)
        locator_rows.append(
            (
                object_id,
                effective_ticker(row["ticker"], fallback=ticker),
                None,
                _object_document_id(row, ticker=ticker),
                row["document_type"],
                row["period"],
                None,
                row["type"],
                ticker,
                shard_path,
                local_object_id,
                _stable_hash(row["json"]),
                semantic_hash,
                compact_label,
                _truncate(compact_text, 700),
                row["review_status"] or row["confidence"],
            )
        )
    conn.executemany(
        """
        INSERT INTO global_object_locator(
            object_id, ticker, company_name, document_id, document_type, period,
            filing_date, object_type, shard_id, shard_path, local_object_key,
            object_hash, semantic_hash, compact_label, compact_summary,
            quality_status
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        locator_rows,
    )
    return len(locator_rows)


def _emit_edge_spine(
    conn: sqlite3.Connection,
    rows: Sequence[sqlite3.Row],
    *,
    object_ticker: Mapping[str, str],
    object_type: Mapping[str, str],
    object_id_map: Mapping[str, str],
    projection_quality: dict[str, int],
    ticker: str,
    shard_path: str,
) -> int:
    payload: list[tuple[Any, ...]] = []
    for row in rows:
        local_from_id = str(row["from_id"])
        local_to_id = str(row["to_id"])
        if local_from_id not in object_ticker or local_to_id not in object_ticker:
            projection_quality["dangling_edges_skipped"] += 1
            continue
        from_id = object_id_map[local_from_id]
        to_id = object_id_map[local_to_id]
        occurrence_ticker = effective_ticker(row["ticker"], fallback=ticker)
        from_ticker = object_ticker.get(local_from_id, occurrence_ticker)
        to_ticker = object_ticker.get(local_to_id, occurrence_ticker)
        edge = _json_loads(row["json"])
        relation_type = row["relation_name"] or row["relation_id"]
        edge_scope = "intra_company" if from_ticker == to_ticker else "cross_company"
        source_object_type = object_type.get(local_from_id) or None
        target_object_type = object_type.get(local_to_id) or None
        edge_id = project_local_identity(row["id"], ticker=ticker)
        semantic_hash = _semantic_edge_hash(
            row,
            edge,
            edge_id=edge_id,
            from_id=from_id,
            to_id=to_id,
            relation_type=relation_type,
            edge_scope=edge_scope,
            source_object_type=source_object_type,
            target_object_type=target_object_type,
        )
        payload.append(
            (
                edge_id,
                occurrence_ticker,
                _object_document_id(row, ticker=ticker),
                row["document_type"],
                row["period"],
                ticker,
                shard_path,
                from_id,
                to_id,
                from_ticker,
                to_ticker,
                relation_type,
                edge_scope,
                source_object_type,
                target_object_type,
                _confidence_score(row["confidence"]),
                edge.get("evidence_level") or edge.get("evidence_grade"),
                _parse_float(edge.get("materiality")),
                _parse_float(edge.get("recency_score")),
                shard_path,
                edge.get("rationale") or row["relation_name"],
                semantic_hash,
            )
        )
    conn.executemany(
        """
        INSERT INTO global_edge_spine(
            edge_id, ticker, document_id, document_type, period, shard_id,
            shard_path, from_object_id, to_object_id, from_ticker, to_ticker,
            relation_type, edge_scope, source_object_type, target_object_type,
            confidence, evidence_grade, materiality, recency_score, shard_hint,
            compact_reason, semantic_hash
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        payload,
    )
    return len(payload)


def _emit_factor_spine(
    conn: sqlite3.Connection,
    rows: Sequence[sqlite3.Row],
    *,
    object_id_map: Mapping[str, str],
    ticker: str,
) -> int:
    payload: list[tuple[Any, ...]] = []
    for row in rows:
        factor_key = _normalize_key(
            row["risk_or_driver"] or row["topic_label"] or row["factor_type"]
        )
        if not factor_key:
            continue
        payload.append(
            (
                factor_key,
                row["topic_label"] or row["risk_or_driver"] or row["factor_type"],
                row["topic_family"] or row["factor_type"],
                None,
                effective_ticker(row["ticker"], fallback=ticker),
                _mapped_object_id(row["object_id"], object_id_map, ticker=ticker),
                _lookup_document_id(row, ticker=ticker),
                row["impact_channel"],
                None,
                row["specificity_score"],
                row["evidence_strength"],
                ticker,
            )
        )
    conn.executemany(
        """
        INSERT INTO global_factor_spine(
            factor_key, factor_label, factor_family, benchmark, ticker,
            object_id, document_id, impact_channel, effect_direction,
            materiality, evidence_grade, shard_id
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        payload,
    )
    return len(payload)


def _emit_topic_spine(
    conn: sqlite3.Connection,
    rows: Sequence[sqlite3.Row],
    *,
    object_id_map: Mapping[str, str],
    ticker: str,
    projection_quality: dict[str, int],
) -> int:
    payload: list[tuple[Any, ...]] = []
    for row in rows:
        topic_key = _normalize_key(row["topic_label"] or row["topic_family"] or row["topic_id"])
        payload.append(
            (
                project_local_identity(row["topic_id"], ticker=ticker),
                topic_key,
                row["topic_label"],
                row["topic_family"],
                row["topic_summary"],
                effective_ticker(row["ticker"], fallback=ticker),
                _map_object_id_json_list(
                    row["source_object_ids"],
                    object_id_map,
                    projection_quality=projection_quality,
                ),
                row["factor_terms"],
                row["metric_terms"],
                row["entity_terms"],
                row["mechanism_terms"],
                row["impact_channels"],
                row["evidence_strength"],
                row["materiality_score"],
                ticker,
            )
        )
    conn.executemany(
        """
        INSERT INTO global_topic_spine(
            topic_id, topic_key, topic_label, topic_family, topic_summary,
            ticker, source_object_ids, factor_terms, metric_terms, entity_terms,
            mechanism_terms, impact_channels, evidence_grade, materiality, shard_id
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        payload,
    )
    return len(payload)


def _emit_metric_spine(
    conn: sqlite3.Connection,
    rows: Sequence[sqlite3.Row],
    *,
    object_id_map: Mapping[str, str],
    ticker: str,
) -> int:
    payload: list[tuple[Any, ...]] = []
    for row in rows:
        metric_key = _normalize_key(row["canonical_metric"] or row["metric_name"])
        if not metric_key:
            continue
        payload.append(
            (
                metric_key,
                row["metric_name"],
                row["unit"],
                _stable_hash(row["dimensions_json"] or ""),
                effective_ticker(row["ticker"], fallback=ticker),
                _mapped_object_id(row["object_id"], object_id_map, ticker=ticker),
                _lookup_document_id(row, ticker=ticker),
                row["observation_period"],
                row["document_type"],
                _parse_float(row["value_text"]),
                None,
                None,
                ticker,
            )
        )
    conn.executemany(
        """
        INSERT INTO global_metric_spine(
            canonical_metric_key, metric_name, unit, dimensions_hash, ticker,
            object_id, document_id, period, document_type, value_normalized,
            trend_direction, confidence, shard_id
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        payload,
    )
    return len(payload)


def _emit_counterparty_spine(
    conn: sqlite3.Connection,
    rows: Sequence[sqlite3.Row],
    *,
    object_id_map: Mapping[str, str],
    ticker: str,
) -> int:
    payload: list[tuple[Any, ...]] = []
    for row in rows:
        key = _normalize_key(row["counterparty"])
        if not key:
            continue
        payload.append(
            (
                key,
                row["counterparty"],
                row["agreement_subtype"] or row["agreement_type"],
                effective_ticker(row["ticker"], fallback=ticker),
                _mapped_object_id(row["object_id"], object_id_map, ticker=ticker),
                _lookup_document_id(row, ticker=ticker),
                row["agreement_type"],
                row["affected_channels"],
                row["specificity_score"],
                row["evidence_strength"],
                ticker,
            )
        )
    conn.executemany(
        """
        INSERT INTO global_counterparty_spine(
            counterparty_key, counterparty_name, relationship_type, ticker,
            object_id, document_id, agreement_type, affected_channels,
            materiality, evidence_grade, shard_id
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        payload,
    )
    return len(payload)


def _emit_entity_spine_from_topics(
    conn: sqlite3.Connection,
    rows: Sequence[sqlite3.Row],
    *,
    object_id_map: Mapping[str, str],
    ticker: str,
) -> int:
    payload: list[tuple[Any, ...]] = []
    seen: set[tuple[str, str, str]] = set()
    for row in rows:
        for term in _json_list(row["entity_terms"]):
            key = _normalize_key(term)
            if not key:
                continue
            occurrence_ticker = effective_ticker(row["ticker"], fallback=ticker)
            mapped_object_id = _mapped_object_id(
                row["primary_object_id"], object_id_map, ticker=ticker
            )
            marker = (key, occurrence_ticker, mapped_object_id)
            if marker in seen:
                continue
            seen.add(marker)
            payload.append(
                (
                    key,
                    "unknown",
                    term,
                    json.dumps([term], sort_keys=True),
                    occurrence_ticker,
                    occurrence_ticker,
                    mapped_object_id,
                    None,
                    None,
                    ticker,
                )
            )
    conn.executemany(
        """
        INSERT INTO global_entity_spine(
            entity_key, entity_type, canonical_name, aliases, ticker_scope,
            ticker, object_id, document_id, confidence, shard_id
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        payload,
    )
    return len(payload)


def _refresh_key_stats(conn: sqlite3.Connection) -> int:
    conn.execute("DELETE FROM global_key_stats")
    sources = (
        ("factor", "global_factor_spine", "factor_key", "ticker", "object_id", "document_id"),
        ("topic", "global_topic_spine", "topic_key", "ticker", "topic_id", "topic_id"),
        (
            "metric",
            "global_metric_spine",
            "canonical_metric_key",
            "ticker",
            "object_id",
            "document_id",
        ),
        ("entity", "global_entity_spine", "entity_key", "ticker", "object_id", "document_id"),
        (
            "counterparty",
            "global_counterparty_spine",
            "counterparty_key",
            "ticker",
            "object_id",
            "document_id",
        ),
    )
    rows: list[tuple[Any, ...]] = []
    for key_type, table, key_col, ticker_col, object_col, document_col in sources:
        if not _table_exists(conn, table):
            continue
        for row in conn.execute(
            f"""
            SELECT
                {key_col} AS key,
                COUNT(DISTINCT {ticker_col}) AS ticker_count,
                COUNT(DISTINCT {object_col}) AS object_count,
                COUNT(DISTINCT {document_col}) AS document_count
            FROM {table}
            WHERE {key_col} IS NOT NULL AND {key_col} != ''
            GROUP BY {key_col}
            """
        ):
            ticker_count = int(row["ticker_count"] or 0)
            object_count = int(row["object_count"] or 0)
            idf_score = 1.0 / max(ticker_count, 1)
            rows.append(
                (
                    key_type,
                    row["key"],
                    ticker_count,
                    object_count,
                    int(row["document_count"] or 0),
                    idf_score,
                    1 if ticker_count >= 25 else 0,
                )
            )
    conn.executemany(
        """
        INSERT OR REPLACE INTO global_key_stats(
            key_type, key, ticker_count, object_count, document_count,
            idf_score, generic
        )
        VALUES (?, ?, ?, ?, ?, ?, ?)
        """,
        rows,
    )
    return len(rows)


def _document_id(row: sqlite3.Row, *, ticker: str) -> str:
    occurrence_ticker = effective_ticker(row["ticker"], fallback=ticker)
    return f"{occurrence_ticker}:{row['doc_type_key']}:{row['period']}"


def _object_document_id(row: sqlite3.Row, *, ticker: str) -> str:
    source_document_id = row["source_document_id"]
    if source_document_id:
        return project_local_identity(source_document_id, ticker=ticker)
    occurrence_ticker = effective_ticker(row["ticker"], fallback=ticker)
    return f"{occurrence_ticker}:{row['doc_type_key']}:{row['period']}"


def _lookup_document_id(row: sqlite3.Row, *, ticker: str) -> str:
    filing_period = row["filing_period"] if "filing_period" in row.keys() else row["period"]
    occurrence_ticker = effective_ticker(row["ticker"], fallback=ticker)
    return f"{occurrence_ticker}:{row['doc_type_key']}:{filing_period}"


def _mapped_object_id(
    object_id: Any,
    object_id_map: Mapping[str, str],
    *,
    ticker: str,
) -> str:
    local_object_id = str(object_id or "")
    try:
        return object_id_map[local_object_id]
    except KeyError as exc:
        raise RuntimeError(
            f"spine_projection_missing_object_reference:ticker={ticker}:object_id={local_object_id}"
        ) from exc


def _map_object_id_json_list(
    value: Any,
    object_id_map: Mapping[str, str],
    *,
    projection_quality: dict[str, int],
) -> str:
    mapped: list[str] = []
    for object_id in _json_list(value):
        projected = object_id_map.get(object_id)
        if projected is None:
            projection_quality["topic_source_refs_skipped"] += 1
            continue
        mapped.append(projected)
    return json.dumps(mapped, ensure_ascii=False, sort_keys=True)


def _compact_label(row: sqlite3.Row, obj: Mapping[str, Any]) -> str:
    for key in ("metric_name", "label", "name", "topic_label", "title", "section_name"):
        value = row[key] if key in row.keys() else obj.get(key)
        if value:
            return _truncate(str(value), 180)
    return str(row["id"])


def _row_value(row: sqlite3.Row | None, key: str) -> Any:
    if row is None:
        return None
    return row[key] if key in row.keys() else None


def _json_loads(value: Any) -> dict[str, Any]:
    try:
        parsed = json.loads(str(value or "{}"))
    except json.JSONDecodeError:
        return {}
    return parsed if isinstance(parsed, dict) else {}


def _json_list(value: Any) -> list[str]:
    if value is None:
        return []
    try:
        parsed = json.loads(str(value))
    except json.JSONDecodeError:
        parsed = value
    if isinstance(parsed, list):
        return [str(item) for item in parsed if str(item).strip()]
    if isinstance(parsed, str) and parsed.strip():
        return [parsed.strip()]
    return []


def _json_count(value: Any, *, default_key: str) -> int:
    try:
        parsed = json.loads(str(value or "{}"))
    except json.JSONDecodeError:
        return 0
    if isinstance(parsed, Mapping):
        raw = parsed.get(default_key)
        return int(raw) if isinstance(raw, (int, float)) else 0
    return 0


def _normalize_key(value: Any) -> str:
    text = re.sub(r"[^A-Za-z0-9]+", "_", str(value or "").strip().lower()).strip("_")
    return re.sub(r"_+", "_", text)


def _normalize_ticker(ticker: str) -> str:
    value = str(ticker or "").strip().upper()
    if not value:
        raise ValueError("ticker is required")
    return value


def _semantic_object_hash(row: sqlite3.Row, obj: Mapping[str, Any]) -> str:
    return semantic_object_hash(
        obj,
        object_id=str(row["id"]),
        object_type=str(row["type"] or ""),
    )


def _semantic_edge_hash(
    row: sqlite3.Row,
    edge: Mapping[str, Any],
    *,
    edge_id: str,
    from_id: str,
    to_id: str,
    relation_type: Any,
    edge_scope: str,
    source_object_type: str | None,
    target_object_type: str | None,
) -> str:
    return _stable_hash(
        {
            "edge_class": edge.get("edge_class"),
            "edge_id": edge_id,
            "edge_scope": edge_scope,
            "from_object_id": from_id,
            "relation_id": row["relation_id"],
            "relation_type": relation_type,
            "source_object_type": source_object_type,
            "target_object_type": target_object_type,
            "to_object_id": to_id,
        }
    )


def _stable_hash(value: Any) -> str:
    payload = json.dumps(value, sort_keys=True, default=str)
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()


def _truncate(value: Any, limit: int) -> str:
    text = str(value or "")
    return text if len(text) <= limit else text[:limit]


def _confidence_score(value: Any) -> float | None:
    if value is None:
        return None
    text = str(value).strip().lower()
    mapping = {"high": 1.0, "medium": 0.6, "low": 0.3}
    if text in mapping:
        return mapping[text]
    return _parse_float(value)


def _parse_float(value: Any) -> float | None:
    if value is None:
        return None
    text = str(value).replace(",", "").strip()
    match = re.search(r"-?\d+(?:\.\d+)?", text)
    if not match:
        return None
    try:
        return float(match.group(0))
    except ValueError:
        return None


def _fiscal_year(period: Any) -> int | None:
    match = re.search(r"(20\d{2}|19\d{2})", str(period or ""))
    return int(match.group(1)) if match else None


def _fiscal_quarter(period: Any) -> int | None:
    match = re.search(r"Q([1-4])", str(period or ""), flags=re.IGNORECASE)
    return int(match.group(1)) if match else None
