"""Build a global SQLite index for agent retrieval.

The JSONL ontology artifacts remain canonical. This module builds a
regenerable read index that lets agents retrieve evidence bundles without
reasoning over the filesystem layout.
"""

from __future__ import annotations

import json
import logging
import math
import os
import re
import resource
import shutil
import sqlite3
import hashlib
import sys
import time
from concurrent.futures import ProcessPoolExecutor, ThreadPoolExecutor, as_completed
from concurrent.futures.process import BrokenProcessPool
from collections import defaultdict
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from datetime import date, datetime, timezone
from pathlib import Path
from typing import Any, Iterable

from krw_capability_runtime.agent_index.cache_seal import (
    immutable_sqlite_cache_seal_path,
    read_immutable_sqlite_cache_seal,
    remove_immutable_sqlite_cache_seal,
    write_immutable_sqlite_cache_seal,
)
from krw_capability_runtime.config.constants import normalize_doc_type
from krw_capability_runtime.schema.objects import SCHEMA_VERSION
from krw_capability_runtime.agent_index.metric_dictionary import (
    canonical_metric_for_xbrl_tag,
    canonical_metric_name,
    metric_aliases,
    metric_dictionary_binding,
    metric_dictionary_binding_errors,
    normalize_dimension_key,
)
from krw_capability_runtime.agent_index.retrieval_text import (
    ObjectLookup,
    RETRIEVAL_TEXT_BUILDER_VERSION,
    build_retrieval_text,
    fallback_retrieval_text,
)
from krw_capability_runtime.agent_index.discovery import (
    COMPANY_TOPIC_OBJECT_TYPES,
    build_company_topic_profile,
)
from krw_capability_runtime.utils.io import read_jsonl

logger = logging.getLogger("krw_capability_runtime")

AGENT_INDEX_SCHEMA_VERSION = "1.0.0-alpha.4"
SOURCE_ARTIFACT_SQLITE_BUILDER_VERSION = "source-artifact-sqlite-builder/v2"
AGENT_INDEX_BUILDER_VERSION = SOURCE_ARTIFACT_SQLITE_BUILDER_VERSION
SOURCE_ARTIFACT_SQLITE_BUILD_STAGE = "source_artifact_sqlite"
DEFAULT_INDEX_RELATIVE_PATH = Path("indexes") / "agent_index.sqlite"
DEFAULT_INDEX_BUILD_WORKER_CAP = 12
INDEX_BUILD_PLAN_FORMAT_VERSION = "krw-agent-index-build-plan/v1"
SOURCE_ARTIFACT_MANIFEST_FORMAT_VERSION = "krw-agent-index-source-manifest/v1"
INDEX_BUILD_GRAPH_FORMAT_VERSION = "krw-agent-index-build-graph/v1"
INDEX_FRAGMENT_CACHE_FORMAT_VERSION = "krw-agent-index-fragment/v1"
INDEX_COMPANY_CACHE_FORMAT_VERSION = "krw-agent-index-company-cache/v1"
INDEX_LAYOUT_VERSION = "krw-agent-index-layout/v1"
SUPPORTED_INDEX_LAYOUTS = frozenset({"monolith", "monolith-and-shards", "shards"})
RELEASE_ENV_NAMES = frozenset({"dev", "staging", "prod"})
LEGACY_BUILDER_DISABLED_MESSAGE = (
    "legacy agent_index.sqlite builder is disabled by default; "
    "use v3 global spine + company shard builders"
)
LEGACY_SHARD_FACADE_DISABLED_MESSAGE = (
    "legacy v2 shard/catalog facade is disabled by default; "
    "use v3 global spine + company shard builders"
)


@dataclass(frozen=True)
class ArtifactPlanItem:
    artifact_index_path: Path
    relative_path: str
    ticker: str
    document_type: str
    doc_type_key: str
    period: str
    content_hash: str
    cache_key: str
    fragment_path: Path
    cache_hit: bool
    estimated_bytes: int
    estimated_rows: int | None
    input_paths: tuple[str, ...]
    missing_inputs: tuple[str, ...]
    cache_errors: tuple[str, ...]

    def to_dict(self) -> dict[str, Any]:
        return _json_safe(
            {
                "artifact_index_path": self.artifact_index_path,
                "relative_path": self.relative_path,
                "ticker": self.ticker,
                "document_type": self.document_type,
                "doc_type_key": self.doc_type_key,
                "period": self.period,
                "content_hash": self.content_hash,
                "cache_key": self.cache_key,
                "fragment_path": self.fragment_path,
                "cache_hit": self.cache_hit,
                "estimated_bytes": self.estimated_bytes,
                "estimated_rows": self.estimated_rows,
                "input_paths": self.input_paths,
                "missing_inputs": self.missing_inputs,
                "cache_errors": self.cache_errors,
            }
        )


@dataclass(frozen=True)
class CompanyPlanItem:
    ticker: str
    artifact_count: int
    artifact_cache_keys: tuple[str, ...]
    input_hash: str
    cache_key: str
    shard_cache_path: Path
    cache_hit: bool
    cache_errors: tuple[str, ...]

    def to_dict(self) -> dict[str, Any]:
        return _json_safe(
            {
                "ticker": self.ticker,
                "artifact_count": self.artifact_count,
                "artifact_cache_keys": self.artifact_cache_keys,
                "input_hash": self.input_hash,
                "cache_key": self.cache_key,
                "shard_cache_path": self.shard_cache_path,
                "cache_hit": self.cache_hit,
                "cache_errors": self.cache_errors,
            }
        )


@dataclass(frozen=True)
class IndexBuildPlan:
    root: Path
    index_path: Path
    cache_root: Path
    artifact_manifest_path: Path
    source_manifest_path: Path | None
    source_manifest_hash: str | None
    discovery_mode: str
    items: tuple[ArtifactPlanItem, ...]
    dirty_items: tuple[ArtifactPlanItem, ...]
    cached_items: tuple[ArtifactPlanItem, ...]
    company_items: tuple[CompanyPlanItem, ...]
    dirty_company_items: tuple[CompanyPlanItem, ...]
    cached_company_items: tuple[CompanyPlanItem, ...]
    dirty_tickers: tuple[str, ...]
    workers: int
    layout: str
    build_settings: Mapping[str, Any]
    builder_code_version: str = AGENT_INDEX_BUILDER_VERSION
    plan_format_version: str = INDEX_BUILD_PLAN_FORMAT_VERSION

    def to_dict(self, *, include_items: bool = True) -> dict[str, Any]:
        payload: dict[str, Any] = {
            "plan_format_version": self.plan_format_version,
            "root": self.root,
            "index_path": self.index_path,
            "cache_root": self.cache_root,
            "artifact_manifest_path": self.artifact_manifest_path,
            "source_manifest_path": self.source_manifest_path,
            "source_manifest_hash": self.source_manifest_hash,
            "discovery_mode": self.discovery_mode,
            "artifact_count": len(self.items),
            "dirty_artifact_count": len(self.dirty_items),
            "cached_artifact_count": len(self.cached_items),
            "company_count": len(self.company_items),
            "dirty_company_count": len(self.dirty_company_items),
            "cached_company_count": len(self.cached_company_items),
            "dirty_tickers": self.dirty_tickers,
            "workers": self.workers,
            "layout": self.layout,
            "builder_code_version": self.builder_code_version,
            "build_settings": dict(self.build_settings),
        }
        if include_items:
            payload["items"] = [item.to_dict() for item in self.items]
            payload["companies"] = [item.to_dict() for item in self.company_items]
        return _json_safe(payload)


@dataclass(frozen=True)
class FragmentCompileResult:
    item: ArtifactPlanItem
    fragment_path: Path
    stats: Mapping[str, int]
    cache_hit: bool
    verification: Mapping[str, Any]


@dataclass(frozen=True)
class IndexFragmentCacheEntry:
    fragment_path: Path
    ok: bool
    size_bytes: int
    referenced: bool | None
    errors: tuple[str, ...]
    metadata: Mapping[str, Any]
    counts: Mapping[str, int]

    def to_dict(self) -> dict[str, Any]:
        return _json_safe(
            {
                "fragment_path": self.fragment_path,
                "ok": self.ok,
                "size_bytes": self.size_bytes,
                "referenced": self.referenced,
                "errors": self.errors,
                "metadata": dict(self.metadata),
                "counts": dict(self.counts),
            }
        )


OBJECT_FILE_KEYS = {
    "run_manifests",
    "ontology_registry_snapshots",
    "validation_reports",
    "taxonomy_terms",
    "source_documents",
    "source_locations",
    "source_tables",
    "source_table_cells",
    "spans",
    "evidence_quotes",
    "language_signals",
    "support_links",
    "canonical_entities",
    "entity_mentions",
    "claims",
    "metric_observations",
    "calculations",
    "business_factors",
    "agreement_terms",
    "business_events",
    "business_activities",
    "external_factor_exposures",
    "assumption_candidates",
    "xbrl_facts",
    "company_business_profiles",
    "temporal_links",
    "trend_observations",
    "change_events",
}

BASE_FRAGMENT_TABLES = (
    "documents",
    "objects",
    "edges",
    "quality_events",
    "object_fts",
    "object_search_text",
    "object_text",
)

ANSWER_CANDIDATE_TYPES = {
    "ResearchClaim",
    "EvidenceQuote",
    "MetricObservation",
    "Calculation",
    "BusinessFactor",
    "AgreementTerm",
    "BusinessEvent",
    "BusinessActivity",
    "ExternalFactorExposure",
    "AssumptionCandidate",
    "TrendObservation",
    "ChangeEvent",
}

SEMANTIC_SUPPORT_TYPES = {
    "BusinessFactor",
    "AgreementTerm",
    "BusinessEvent",
    "BusinessActivity",
    "ExternalFactorExposure",
    "AssumptionCandidate",
    "ChangeEvent",
}

TEXT_KEYS_BY_TYPE = {
    "RunManifest": (
        "run_id",
        "pipeline_version",
        "ontology_schema_version",
        "ontology_registry_version",
        "model",
    ),
    "OntologyRegistrySnapshot": (
        "registry_version",
        "canonical_artifacts",
        "text_fields_by_type",
    ),
    "ValidationReport": ("validation_scope", "summary"),
    "TaxonomyTerm": ("taxonomy", "term_type", "canonical_name", "display_name", "aliases"),
    "SourceDocument": ("ticker", "company_name", "accession_number", "source_url"),
    "SourceLocation": ("source_boundary", "section_name", "section_path", "source_span_id"),
    "SourceTable": ("section_name", "caption"),
    "SourceTableCell": ("raw_text", "normalized_text"),
    "SourceSpan": ("text",),
    "EvidenceQuote": ("quote_text",),
    "LanguageSignal": ("signal_text",),
    "SupportLink": (
        "support_type",
        "support_role",
        "stance",
        "evidence_strength",
        "support_strength",
        "from_id",
        "to_id",
        "explanation",
    ),
    "CanonicalEntity": ("entity_type", "canonical_name", "aliases", "ticker_scope"),
    "EntityMention": ("mention_text", "source_object_type", "canonical_entity_id"),
    "ResearchClaim": (
        "claim_text",
        "theme_hint",
        "factor_hint",
        "activity_hint",
        "benchmark_hint",
        "impact_channels",
        "effect_direction",
        "materiality_hint",
        "time_horizon",
        "sector_hint",
        "object_type_hints",
    ),
    "MetricObservation": ("metric_name", "unit", "source_type", "normalization", "dimensions"),
    "Calculation": ("calculation_type", "formula", "calculation_method", "validation_status"),
    "BusinessFactor": (
        "name",
        "description",
        "factor_roles",
        "category",
        "affected_channels",
        "materiality_basis",
    ),
    "AgreementTerm": (
        "name",
        "agreement_type",
        "agreement_subtype",
        "economic_role",
        "affected_channels",
    ),
    "BusinessEvent": (
        "name",
        "event_type",
        "event_subtype",
        "event_status",
        "date_expression",
        "affected_channels",
    ),
    "BusinessActivity": (
        "name",
        "activity_type",
        "description",
        "revenue_relevance",
        "cost_relevance",
    ),
    "ExternalFactorExposure": (
        "factor",
        "factor_category",
        "benchmark",
        "direction",
        "impact_channel",
        "effect_direction",
        "mechanism",
        "evidence_grade",
        "materiality",
    ),
    "AssumptionCandidate": ("name", "assumption_text", "value_hint", "assumption_type"),
    "XBRLFact": ("taxonomy_tag", "safe_taxonomy_tag", "context_ref"),
    "CompanyBusinessProfile": (
        "sector",
        "business_model_summary",
        "primary_business_activities",
        "primary_revenue_sources",
        "primary_cost_sources",
        "key_external_factors",
        "key_uncertainties",
    ),
    "TemporalLink": (
        "from_object_id",
        "to_object_id",
        "relation",
        "rationale",
        "from_period",
        "to_period",
    ),
    "TrendObservation": (
        "subject",
        "metric_or_factor",
        "direction",
        "magnitude_text",
        "interpretation",
    ),
    "ChangeEvent": ("event_type", "event_date", "description"),
}

_NON_WORD_RE = re.compile(r"\s+")
DEFAULT_BUILD_RESOURCE_PROFILE = "max-local"
DEFAULT_BULK_INSERT_CHUNK_SIZE = 20_000
DEFAULT_COMPANY_TOPIC_BATCH_SIZE = 2_000
DEFAULT_SQLITE_CACHE_SIZE_KIB = 4_096 * 1024
DEFAULT_SQLITE_MMAP_SIZE_BYTES = 16 * 1024 * 1024 * 1024
BULK_INSERT_CHUNK_SIZE = DEFAULT_BULK_INSERT_CHUNK_SIZE
COMPANY_TOPIC_BATCH_SIZE = DEFAULT_COMPANY_TOPIC_BATCH_SIZE
SQLITE_CACHE_SIZE_KIB = DEFAULT_SQLITE_CACHE_SIZE_KIB
SQLITE_MMAP_SIZE_BYTES = DEFAULT_SQLITE_MMAP_SIZE_BYTES
COMPANY_TOPIC_TEXT_CHAR_LIMIT = 2_500
COMPANY_TOPIC_FIELD_CHAR_LIMIT = 700
COMPANY_TOPIC_FTS_CHAR_LIMIT = 1_800
COMPANY_TOPIC_BUILDER_VERSION = "0.3.0-rich-hardened"
TYPED_PROJECTION_BUILDER_VERSION = "0.1.0-serving-projections"
TYPED_PROJECTION_TABLES = (
    "exposure_lookup",
    "agreement_lookup",
    "event_lookup",
    "factor_lookup",
)
TYPED_PROJECTION_OBJECT_TYPES = {
    "exposure_lookup": ("ExternalFactorExposure",),
    "agreement_lookup": ("AgreementTerm",),
    "event_lookup": ("BusinessEvent", "ChangeEvent", "TrendObservation", "TemporalLink"),
    "factor_lookup": ("BusinessFactor",),
}
INDEX_SHARD_TABLES = (
    "documents",
    "objects",
    "edges",
    "quality_events",
    "object_fts",
    "object_search_text",
    "object_text",
    "object_traceability",
    "metric_lookup",
    "metric_dimension_lookup",
    "company_dimension_catalog",
    "exposure_lookup",
    "agreement_lookup",
    "event_lookup",
    "factor_lookup",
    "company_topic_index",
    "company_topic_fts",
    "company_topic_source_objects",
)
SHARD_FTS_TABLES = frozenset({"object_fts", "company_topic_fts"})
GLOBAL_TOPIC_TABLES = (
    "company_topic_index",
    "company_topic_fts",
    "company_topic_source_objects",
)

_SQLITE_SYNCHRONOUS_VALUES = {"OFF", "NORMAL", "FULL", "EXTRA"}
_ACTIVE_BUILD_PROGRESS_LOGGER: _BuildProgressLogger | None = None


def _env_int(name: str, default: int, *, min_value: int = 0) -> int:
    raw = os.getenv(name)
    if raw is None or str(raw).strip() == "":
        return default
    try:
        value = int(str(raw).strip())
    except ValueError:
        logger.warning(
            "source_artifact_sqlite: ignoring invalid integer env %s=%r",
            name,
            raw,
            extra={"stage": SOURCE_ARTIFACT_SQLITE_BUILD_STAGE},
        )
        return default
    return max(min_value, value)


def _env_float(name: str, default: float, *, min_value: float = 0.0) -> float:
    raw = os.getenv(name)
    if raw is None or str(raw).strip() == "":
        return default
    try:
        value = float(str(raw).strip())
    except ValueError:
        logger.warning(
            "source_artifact_sqlite: ignoring invalid float env %s=%r",
            name,
            raw,
            extra={"stage": SOURCE_ARTIFACT_SQLITE_BUILD_STAGE},
        )
        return default
    return max(min_value, value)


def _default_progress_log_path(index_path: Path | None = None) -> str:
    if index_path is None:
        return ""
    return str(index_path.parent / "build_progress.jsonl")


def _build_resource_settings(index_path: Path | None = None) -> dict[str, Any]:
    profile = (
        str(os.getenv("KRW_BUILD_RESOURCE_PROFILE") or DEFAULT_BUILD_RESOURCE_PROFILE)
        .strip()
        .lower()
        or DEFAULT_BUILD_RESOURCE_PROFILE
    )
    default_cache_mib = max(1, DEFAULT_SQLITE_CACHE_SIZE_KIB // 1024)
    default_mmap_gib = DEFAULT_SQLITE_MMAP_SIZE_BYTES / float(1024**3)
    default_batch_size = DEFAULT_BULK_INSERT_CHUNK_SIZE
    default_company_topic_batch_size = DEFAULT_COMPANY_TOPIC_BATCH_SIZE
    default_checkpoint_every_artifacts = 10

    synchronous = str(os.getenv("KRW_SQLITE_SYNCHRONOUS") or "OFF").strip().upper()
    if synchronous not in _SQLITE_SYNCHRONOUS_VALUES:
        logger.warning(
            "source_artifact_sqlite: ignoring invalid KRW_SQLITE_SYNCHRONOUS=%r",
            synchronous,
            extra={"stage": SOURCE_ARTIFACT_SQLITE_BUILD_STAGE},
        )
        synchronous = "OFF"

    cache_mib = _env_int("KRW_SQLITE_CACHE_MIB", default_cache_mib, min_value=1)
    mmap_gib = _env_float("KRW_SQLITE_MMAP_GIB", default_mmap_gib, min_value=0.0)
    return {
        "build_stage": SOURCE_ARTIFACT_SQLITE_BUILD_STAGE,
        "resource_profile": profile,
        "sqlite_synchronous": synchronous,
        "sqlite_cache_mib": cache_mib,
        "sqlite_cache_size_kib": cache_mib * 1024,
        "sqlite_mmap_gib": mmap_gib,
        "sqlite_mmap_size_bytes": int(mmap_gib * 1024**3),
        "sqlite_wal_autocheckpoint": _env_int("KRW_SQLITE_WAL_AUTOCHECKPOINT", 0, min_value=0),
        "bulk_insert_chunk_size": _env_int("KRW_BUILD_BATCH_SIZE", default_batch_size, min_value=1),
        "company_topic_batch_size": _env_int(
            "KRW_COMPANY_TOPIC_BATCH_SIZE",
            default_company_topic_batch_size,
            min_value=1,
        ),
        "checkpoint_every_artifacts": _env_int(
            "KRW_BUILD_CHECKPOINT_EVERY_ARTIFACTS",
            default_checkpoint_every_artifacts,
            min_value=0,
        ),
        "progress_log_path": os.getenv("KRW_BUILD_PROGRESS_LOG")
        or _default_progress_log_path(index_path),
        "progress_log_interval_sec": _env_float("KRW_BUILD_LOG_INTERVAL_SEC", 10.0, min_value=0.0),
    }


def _apply_build_resource_settings(settings: Mapping[str, Any]) -> None:
    global \
        BULK_INSERT_CHUNK_SIZE, \
        COMPANY_TOPIC_BATCH_SIZE, \
        SQLITE_CACHE_SIZE_KIB, \
        SQLITE_MMAP_SIZE_BYTES
    BULK_INSERT_CHUNK_SIZE = int(settings["bulk_insert_chunk_size"])
    COMPANY_TOPIC_BATCH_SIZE = int(settings["company_topic_batch_size"])
    SQLITE_CACHE_SIZE_KIB = int(settings["sqlite_cache_size_kib"])
    SQLITE_MMAP_SIZE_BYTES = int(settings["sqlite_mmap_size_bytes"])


def _json_safe(value: Any) -> Any:
    if isinstance(value, Path):
        return str(value)
    if isinstance(value, Mapping):
        return {str(key): _json_safe(item) for key, item in value.items()}
    if isinstance(value, (list, tuple, set, frozenset)):
        return [_json_safe(item) for item in value]
    return value


def _rss_mb() -> float | None:
    try:
        max_rss = float(resource.getrusage(resource.RUSAGE_SELF).ru_maxrss)
    except Exception:
        return None
    divisor = 1024 * 1024 if sys.platform == "darwin" else 1024
    return round(max_rss / divisor, 1)


class _BuildProgressLogger:
    def __init__(self, *, path: Path, index_path: Path, settings: Mapping[str, Any]) -> None:
        self.path = path
        self.index_path = index_path
        self.settings = dict(settings)
        self.started_at = time.perf_counter()
        self.last_write_at = 0.0
        self.interval_sec = float(settings.get("progress_log_interval_sec") or 0.0)
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.path.write_text("", encoding="utf-8")

    def write(self, phase: str, fields: Mapping[str, Any]) -> None:
        now = time.perf_counter()
        must_write = (
            self.last_write_at == 0.0
            or phase == "start"
            or phase.endswith("_start")
            or phase.endswith("_done")
            or phase == "finalize_done"
            or self.interval_sec == 0.0
            or now - self.last_write_at >= self.interval_sec
        )
        if not must_write:
            return
        self.last_write_at = now
        wal_path = Path(str(self.index_path) + "-wal")
        shm_path = Path(str(self.index_path) + "-shm")
        payload = {
            "event": "build_phase",
            "stage": self.settings.get("build_stage") or SOURCE_ARTIFACT_SQLITE_BUILD_STAGE,
            "phase": phase,
            "ts": datetime.now(timezone.utc).isoformat(),
            "pid": os.getpid(),
            "elapsed_sec": round(now - self.started_at, 3),
            "rss_mb": _rss_mb(),
            "db_size_mb": round(self.index_path.stat().st_size / 1024 / 1024, 1)
            if self.index_path.exists()
            else 0.0,
            "wal_size_mb": round(wal_path.stat().st_size / 1024 / 1024, 1)
            if wal_path.exists()
            else 0.0,
            "shm_size_mb": round(shm_path.stat().st_size / 1024 / 1024, 1)
            if shm_path.exists()
            else 0.0,
            "resource_profile": self.settings.get("resource_profile"),
            "sqlite_synchronous": self.settings.get("sqlite_synchronous"),
            "sqlite_cache_mib": self.settings.get("sqlite_cache_mib"),
            "sqlite_mmap_gib": self.settings.get("sqlite_mmap_gib"),
            "bulk_insert_chunk_size": self.settings.get("bulk_insert_chunk_size"),
            "checkpoint_every_artifacts": self.settings.get("checkpoint_every_artifacts"),
            "fields": _json_safe(dict(fields)),
        }
        with self.path.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(payload, ensure_ascii=False, sort_keys=True) + "\n")


def _elapsed(started_at: float) -> float:
    return round(time.perf_counter() - started_at, 3)


def _log_build_phase(phase: str, **fields: Any) -> None:
    rendered = " ".join(f"{key}={value}" for key, value in fields.items())
    message = f"source_artifact_sqlite: {phase}"
    if rendered:
        message = f"{message} {rendered}"
    logger.info(message, extra={"stage": SOURCE_ARTIFACT_SQLITE_BUILD_STAGE})
    if _ACTIVE_BUILD_PROGRESS_LOGGER is not None:
        _ACTIVE_BUILD_PROGRESS_LOGGER.write(phase, fields)


def _compact_space(value: str) -> str:
    return _NON_WORD_RE.sub(" ", str(value or "")).strip()


def build_agent_index(
    root: Path,
    *,
    index_path: Path | None = None,
    force: bool = True,
    cache_root: Path | None = None,
    workers: int | None = None,
    layout: str = "monolith-and-shards",
    source_manifest_path: Path | None = None,
    allow_internal_legacy_builder: bool = False,
) -> dict[str, Any]:
    """Build a legacy global SQLite agent index from discovered artifact indexes.

    Production builds must use the v3 global spine + company shard release
    builders. This legacy materializer remains only as an internal fixture and
    as the reusable table-writer implementation behind v3 company shard builds.
    """
    if not allow_internal_legacy_builder:
        raise ValueError(LEGACY_BUILDER_DISABLED_MESSAGE)
    root = root.resolve()
    published_index_path = (index_path or root / DEFAULT_INDEX_RELATIVE_PATH).resolve()
    _assert_not_active_release_path(root, "root")
    _assert_not_active_release_path(published_index_path, "index_path")
    published_index_path.parent.mkdir(parents=True, exist_ok=True)
    plan = plan_agent_index(
        root,
        index_path=published_index_path,
        cache_root=cache_root,
        workers=workers,
        layout=layout,
        source_manifest_path=source_manifest_path,
        allow_internal_legacy_builder=True,
    )
    _assert_not_active_release_path(plan.cache_root, "cache_root")
    progress_log_path = str(plan.build_settings.get("progress_log_path") or "")
    if progress_log_path:
        _assert_not_active_release_path(Path(progress_log_path), "progress_log_path")
    build_index_path = _temporary_index_path(published_index_path)
    _cleanup_sqlite_database_files(build_index_path)
    shard_stage: dict[str, Any] | None = None
    try:
        result = _build_agent_index_direct(
            root,
            index_path=build_index_path,
            published_index_path=published_index_path,
            force=force,
            plan=plan,
        )
        verification = verify_agent_index(build_index_path)
        if not verification["ok"]:
            errors = ", ".join(verification["errors"])
            raise RuntimeError(f"built agent index failed verification: {errors}")
        if _layout_builds_shards(plan.layout):
            shard_stage = _stage_index_shards(
                build_index_path,
                output_dir=published_index_path.parent,
                logical_index_path=published_index_path,
                plan=plan,
            )
        _replace_sqlite_database(build_index_path, published_index_path)
        if shard_stage is not None:
            result["shards"] = _publish_staged_index_shards(shard_stage)
        build_plan = _write_build_plan(published_index_path, plan)
        artifact_manifest = _write_artifact_manifest(plan)
        build_graph = _write_build_graph(
            published_index_path,
            plan=plan,
            result=result,
        )
        result["build_plan"] = build_plan
        result["build_plan_summary"] = _plan_summary(plan)
        result["build_plan_path"] = build_plan["path"]
        result["artifact_manifest"] = artifact_manifest
        result["artifact_manifest_path"] = artifact_manifest["path"]
        result["build_graph"] = build_graph
        result["build_graph_path"] = build_graph["path"]
        summary = _write_build_summary(
            published_index_path,
            result=result,
            verification=verification,
        )
        result["index_path"] = published_index_path
        result["build_index_path"] = build_index_path
        result["verification"] = verification
        result["build_summary"] = summary
        result["build_summary_path"] = summary["path"]
        return result
    finally:
        _cleanup_sqlite_database_files(build_index_path)
        if shard_stage is not None:
            shutil.rmtree(Path(shard_stage["stage_root"]), ignore_errors=True)


def _assert_not_active_release_path(path: Path, label: str) -> None:
    if _path_points_at_active_release(path):
        raise ValueError(
            f"Refusing {label}={path}: current is an immutable release pointer. "
            "Build a candidate release and promote it instead."
        )


def _path_points_at_active_release(path: Path) -> bool:
    expanded = path.expanduser().absolute()
    for candidate in (expanded, *expanded.parents):
        if candidate.name == "current" and candidate.parent.name in RELEASE_ENV_NAMES:
            return True
        env_root = candidate.parent
        if env_root.name not in RELEASE_ENV_NAMES:
            continue
        current = env_root / "current"
        try:
            if (
                current.is_symlink()
                and candidate.exists()
                and candidate.resolve() == current.resolve()
            ):
                return True
        except OSError:
            pass
    return False


def _temporary_index_path(index_path: Path) -> Path:
    return index_path.parent / f".{index_path.name}.{os.getpid()}.{time.time_ns()}.tmp"


def _sqlite_sidecar_paths(index_path: Path) -> tuple[Path, Path]:
    return (Path(str(index_path) + "-wal"), Path(str(index_path) + "-shm"))


def _cleanup_sqlite_database_files(index_path: Path) -> None:
    for path in (
        index_path,
        *_sqlite_sidecar_paths(index_path),
        immutable_sqlite_cache_seal_path(index_path),
    ):
        try:
            path.unlink()
        except FileNotFoundError:
            pass


def _replace_sqlite_database(source_path: Path, target_path: Path) -> None:
    # Direct index builds still replace the target atomically; release publish
    # avoids live-file replacement by swapping release pointers.
    for sidecar_path in _sqlite_sidecar_paths(target_path):
        try:
            sidecar_path.unlink()
        except FileNotFoundError:
            pass
    remove_immutable_sqlite_cache_seal(target_path)
    os.replace(source_path, target_path)


def verify_agent_index(index_path: Path, *, trust_seal: bool = True) -> dict[str, Any]:
    """Return verification metadata for a built agent SQLite index."""
    errors: list[str] = []
    required_tables = {
        "metadata",
        "documents",
        "objects",
        "edges",
        "quality_events",
        "object_fts",
        "object_text",
        "object_search_text",
        "object_traceability",
        "metric_lookup",
        "metric_dimension_lookup",
        "company_dimension_catalog",
        "exposure_lookup",
        "agreement_lookup",
        "event_lookup",
        "factor_lookup",
        "company_topic_index",
        "company_topic_fts",
        "company_topic_source_objects",
    }
    counts: dict[str, int] = {}
    integrity_check = None
    if not index_path.exists():
        return {
            "ok": False,
            "errors": ["index_missing"],
            "index_path": str(index_path),
            "integrity_check": None,
            "counts": counts,
        }
    seal, seal_status = read_immutable_sqlite_cache_seal(
        index_path,
        kind="company_shard",
    )
    if trust_seal and seal_status == "valid":
        raw_metadata = seal.get("metadata")
        metadata = dict(raw_metadata) if isinstance(raw_metadata, Mapping) else {}
        raw_counts = seal.get("counts")
        counts = {
            str(key): int(value)
            for key, value in dict(raw_counts or {}).items()
            if isinstance(value, int) and not isinstance(value, bool)
        }
        schema_version = metadata.get("agent_index_schema_version") or metadata.get(
            "schema_version"
        )
        if schema_version != AGENT_INDEX_SCHEMA_VERSION:
            errors.append("agent_index_schema_version_mismatch")
        if metadata.get("source_artifact_sqlite_schema_version") != AGENT_INDEX_SCHEMA_VERSION:
            errors.append("source_artifact_sqlite_schema_version_mismatch")
        if (
            metadata.get("source_artifact_sqlite_builder_version")
            != SOURCE_ARTIFACT_SQLITE_BUILDER_VERSION
        ):
            errors.append("source_artifact_sqlite_builder_version_mismatch")
        errors.extend(metric_dictionary_binding_errors(metadata.get("metric_dictionary")))
        return {
            "ok": not errors,
            "errors": errors,
            "index_path": str(index_path),
            "integrity_check": "ok",
            "integrity_source": "immutable_cache_seal",
            "seal_status": seal_status,
            "seal_trusted": True,
            "counts": counts,
            "metadata": metadata,
        }
    try:
        with sqlite3.connect(index_path) as conn:
            integrity_check = conn.execute("PRAGMA integrity_check").fetchone()[0]
            if integrity_check != "ok":
                errors.append(f"integrity_check_failed:{integrity_check}")
            table_rows = conn.execute(
                """
                SELECT name
                FROM sqlite_master
                WHERE type IN ('table', 'view')
                """
            ).fetchall()
            existing_tables = {str(row[0]) for row in table_rows}
            for table_name in sorted(required_tables - existing_tables):
                errors.append(f"table_missing:{table_name}")
            if "metric_lookup" in existing_tables:
                metric_columns = {
                    str(row[1])
                    for row in conn.execute("PRAGMA table_info(metric_lookup)").fetchall()
                }
                required_metric_columns = {
                    "filing_period",
                    "observation_period",
                    "observation_period_type",
                    "observation_start_date",
                    "observation_end_date",
                    "observation_context_key",
                    "value_numeric",
                }
                for column in sorted(required_metric_columns - metric_columns):
                    errors.append(f"metric_lookup_column_missing:{column}")
                if required_metric_columns.issubset(metric_columns):
                    invalid_contexts = int(
                        conn.execute(
                            """
                            SELECT COUNT(*)
                            FROM metric_lookup
                            WHERE filing_period = ''
                               OR observation_period = ''
                               OR observation_period_type = ''
                               OR observation_context_key = ''
                            """
                        ).fetchone()[0]
                    )
                    if invalid_contexts:
                        errors.append(
                            f"metric_lookup_observation_context_invalid:{invalid_contexts}"
                        )
            for table_name in ("documents", "objects", "edges", "quality_events"):
                if table_name in existing_tables:
                    counts[table_name] = int(
                        conn.execute(f"SELECT COUNT(*) FROM {table_name}").fetchone()[0]
                    )
            build_row = conn.execute("SELECT value FROM metadata WHERE key = 'build'").fetchone()
            if build_row is None:
                errors.append("metadata_build_missing")
            else:
                try:
                    build_metadata = json.loads(build_row[0])
                except json.JSONDecodeError:
                    errors.append("metadata_build_invalid_json")
                else:
                    schema_version = build_metadata.get(
                        "agent_index_schema_version"
                    ) or build_metadata.get("schema_version")
                    if schema_version != AGENT_INDEX_SCHEMA_VERSION:
                        errors.append("agent_index_schema_version_mismatch")
                    source_artifact_schema_version = build_metadata.get(
                        "source_artifact_sqlite_schema_version"
                    )
                    if source_artifact_schema_version != AGENT_INDEX_SCHEMA_VERSION:
                        errors.append("source_artifact_sqlite_schema_version_mismatch")
                    source_artifact_builder_version = build_metadata.get(
                        "source_artifact_sqlite_builder_version"
                    )
                    if source_artifact_builder_version != SOURCE_ARTIFACT_SQLITE_BUILDER_VERSION:
                        errors.append("source_artifact_sqlite_builder_version_mismatch")
                    errors.extend(
                        metric_dictionary_binding_errors(build_metadata.get("metric_dictionary"))
                    )
    except sqlite3.Error as exc:
        errors.append(f"sqlite_error:{exc}")
    return {
        "ok": not errors,
        "errors": errors,
        "index_path": str(index_path),
        "integrity_check": integrity_check,
        "counts": counts,
    }


def build_index_shards(
    index_path: Path,
    *,
    output_dir: Path | None = None,
    allow_internal_legacy_builder: bool = False,
) -> dict[str, Any]:
    """Build and publish global catalog/company shards from a verified monolith."""
    if not allow_internal_legacy_builder:
        raise ValueError(LEGACY_SHARD_FACADE_DISABLED_MESSAGE)
    source_index_path = index_path.expanduser().resolve()
    target_output_dir = (output_dir or source_index_path.parent).expanduser().resolve()
    stage = _stage_index_shards(
        source_index_path,
        output_dir=target_output_dir,
        logical_index_path=source_index_path,
    )
    try:
        return _publish_staged_index_shards(stage)
    finally:
        shutil.rmtree(Path(stage["stage_root"]), ignore_errors=True)


def verify_index_shards(
    index_dir: Path,
    *,
    catalog_path: Path | None = None,
    global_topics_path: Path | None = None,
    companies_dir: Path | None = None,
    monolith_index_path: Path | None = None,
    allow_internal_legacy_builder: bool = False,
) -> dict[str, Any]:
    """Verify a shard-aware index directory."""
    if not allow_internal_legacy_builder:
        raise ValueError(LEGACY_SHARD_FACADE_DISABLED_MESSAGE)
    resolved_index_dir = index_dir.expanduser().resolve()
    resolved_catalog_path = (
        (catalog_path or resolved_index_dir / "global_catalog.sqlite").expanduser().resolve()
    )
    resolved_global_topics_path = (
        (global_topics_path or resolved_index_dir / "global_topics.sqlite").expanduser().resolve()
    )
    resolved_companies_dir = (
        (companies_dir or resolved_index_dir / "companies").expanduser().resolve()
    )
    resolved_monolith_path = (
        monolith_index_path.expanduser().resolve() if monolith_index_path is not None else None
    )
    errors: list[str] = []
    counts = {
        "ticker_count": 0,
        "catalog_documents": 0,
        "documents": 0,
        "objects": 0,
        "edges": 0,
        "quality_events": 0,
        "global_topics": 0,
        "global_topic_sources": 0,
    }
    shard_results: dict[str, Any] = {}
    catalog_integrity_check = None
    global_topics_verification: dict[str, Any] | None = None
    if not resolved_catalog_path.exists():
        errors.append("global_catalog_missing")
    if not resolved_global_topics_path.exists():
        errors.append("global_topics_missing")
    if not resolved_companies_dir.exists():
        errors.append("companies_dir_missing")
    if errors:
        return {
            "ok": False,
            "errors": errors,
            "index_dir": str(resolved_index_dir),
            "global_catalog_path": str(resolved_catalog_path),
            "global_topics_path": str(resolved_global_topics_path),
            "companies_dir": str(resolved_companies_dir),
            "counts": counts,
            "shards": shard_results,
            "catalog_integrity_check": catalog_integrity_check,
            "global_topics_verification": global_topics_verification,
        }

    try:
        with sqlite3.connect(resolved_catalog_path) as catalog_conn:
            catalog_conn.row_factory = sqlite3.Row
            catalog_integrity_check = catalog_conn.execute("PRAGMA integrity_check").fetchone()[0]
            if catalog_integrity_check != "ok":
                errors.append(f"global_catalog_integrity_check_failed:{catalog_integrity_check}")
            table_rows = catalog_conn.execute(
                """
                SELECT name
                FROM sqlite_master
                WHERE type IN ('table', 'view')
                """
            ).fetchall()
            existing_tables = {str(row[0]) for row in table_rows}
            for table_name in sorted({"metadata", "shards", "documents"} - existing_tables):
                errors.append(f"global_catalog_table_missing:{table_name}")
            if "metadata" in existing_tables:
                catalog_metadata = _read_metadata_json_from_schema(catalog_conn, "main", "catalog")
                if catalog_metadata.get("index_layout_version") != INDEX_LAYOUT_VERSION:
                    errors.append("global_catalog_layout_version_mismatch")
            if "shards" not in existing_tables:
                shard_rows: list[sqlite3.Row] = []
            else:
                shard_rows = list(
                    catalog_conn.execute(
                        """
                        SELECT ticker, shard_path, document_count, object_count,
                               edge_count, quality_event_count, sha256
                        FROM shards
                        ORDER BY ticker
                        """
                    )
                )
            counts["ticker_count"] = len(shard_rows)
            if "documents" in existing_tables:
                counts["catalog_documents"] = int(
                    catalog_conn.execute("SELECT COUNT(*) FROM documents").fetchone()[0]
                )
    except sqlite3.Error as exc:
        errors.append(f"global_catalog_sqlite_error:{exc}")
        shard_rows = []

    for row in shard_rows:
        ticker = str(row["ticker"])
        shard_path = Path(str(row["shard_path"]))
        if not shard_path.is_absolute():
            shard_path = resolved_index_dir / shard_path
        shard_result: dict[str, Any] = {
            "path": str(shard_path),
            "verification": None,
            "counts": {},
        }
        shard_results[ticker] = shard_result
        if not shard_path.exists():
            errors.append(f"shard_missing:{ticker}")
            continue
        verification = verify_agent_index(shard_path)
        shard_result["verification"] = verification
        if not verification["ok"]:
            errors.append(f"shard_verify_failed:{ticker}:{','.join(verification['errors'])}")
        try:
            with sqlite3.connect(shard_path) as shard_conn:
                build_metadata = _read_metadata_json_from_schema(shard_conn, "main", "build")
                if build_metadata.get("index_role") != "company_shard":
                    errors.append(f"shard_metadata_role_mismatch:{ticker}")
                if str(build_metadata.get("shard_ticker") or "") != ticker:
                    errors.append(f"shard_metadata_ticker_mismatch:{ticker}")
                actual_counts = {
                    "documents": _table_count(shard_conn, "main", "documents"),
                    "objects": _table_count(shard_conn, "main", "objects"),
                    "edges": _table_count(shard_conn, "main", "edges"),
                    "quality_events": _table_count(shard_conn, "main", "quality_events"),
                }
        except sqlite3.Error as exc:
            errors.append(f"shard_sqlite_error:{ticker}:{exc}")
            continue
        shard_result["counts"] = actual_counts
        expected_counts = {
            "documents": int(row["document_count"]),
            "objects": int(row["object_count"]),
            "edges": int(row["edge_count"]),
            "quality_events": int(row["quality_event_count"]),
        }
        for key, expected in expected_counts.items():
            actual = actual_counts.get(key)
            if actual != expected:
                errors.append(f"shard_count_mismatch:{ticker}:{key}:{actual}!={expected}")
            counts[key] += int(actual or 0)
        actual_sha256 = _file_sha256(shard_path)
        if actual_sha256 != str(row["sha256"]):
            errors.append(f"shard_sha256_mismatch:{ticker}")
        shard_result["sha256"] = actual_sha256

    if counts["catalog_documents"] != counts["documents"]:
        errors.append(
            f"catalog_document_count_mismatch:{counts['catalog_documents']}!={counts['documents']}"
        )

    global_topics_verification = _verify_global_topics_index(
        resolved_global_topics_path,
        monolith_index_path=resolved_monolith_path,
    )
    if not global_topics_verification["ok"]:
        errors.extend(f"global_topics:{error}" for error in global_topics_verification["errors"])
    counts["global_topics"] = int(
        (global_topics_verification.get("counts") or {}).get("company_topic_index") or 0
    )
    counts["global_topic_sources"] = int(
        (global_topics_verification.get("counts") or {}).get("company_topic_source_objects") or 0
    )

    if resolved_monolith_path is not None and resolved_monolith_path.exists():
        try:
            with sqlite3.connect(resolved_monolith_path) as monolith_conn:
                for table_name in ("documents", "objects", "edges", "quality_events"):
                    monolith_count = _table_count(monolith_conn, "main", table_name)
                    if counts[table_name] != monolith_count:
                        errors.append(
                            f"shard_sum_mismatch:{table_name}:{counts[table_name]}!={monolith_count}"
                        )
        except sqlite3.Error as exc:
            errors.append(f"monolith_sqlite_error:{exc}")

    return {
        "ok": not errors,
        "errors": errors,
        "index_dir": str(resolved_index_dir),
        "global_catalog_path": str(resolved_catalog_path),
        "global_topics_path": str(resolved_global_topics_path),
        "companies_dir": str(resolved_companies_dir),
        "catalog_integrity_check": catalog_integrity_check,
        "global_topics_verification": global_topics_verification,
        "counts": counts,
        "shards": shard_results,
    }


def _verify_global_topics_index(
    topics_path: Path,
    *,
    monolith_index_path: Path | None = None,
) -> dict[str, Any]:
    errors: list[str] = []
    counts: dict[str, int] = {}
    integrity_check = None
    if not topics_path.exists():
        return {
            "ok": False,
            "errors": ["global_topics_missing"],
            "path": str(topics_path),
            "integrity_check": integrity_check,
            "counts": counts,
        }
    try:
        with sqlite3.connect(topics_path) as conn:
            integrity_check = conn.execute("PRAGMA integrity_check").fetchone()[0]
            if integrity_check != "ok":
                errors.append(f"integrity_check_failed:{integrity_check}")
            table_rows = conn.execute(
                """
                SELECT name
                FROM sqlite_master
                WHERE type IN ('table', 'view')
                """
            ).fetchall()
            existing_tables = {str(row[0]) for row in table_rows}
            for table_name in sorted({"metadata", *GLOBAL_TOPIC_TABLES} - existing_tables):
                errors.append(f"table_missing:{table_name}")
            metadata = (
                _read_metadata_json_from_schema(conn, "main", "topics")
                if "metadata" in existing_tables
                else {}
            )
            if metadata.get("index_layout_version") != INDEX_LAYOUT_VERSION:
                errors.append("metadata_layout_version_mismatch")
            if metadata.get("index_role") != "global_topics":
                errors.append("metadata_role_mismatch")
            for table_name in GLOBAL_TOPIC_TABLES:
                if table_name in existing_tables:
                    counts[table_name] = _table_count(conn, "main", table_name)
    except sqlite3.Error as exc:
        errors.append(f"sqlite_error:{exc}")

    if monolith_index_path is not None and monolith_index_path.exists():
        try:
            with sqlite3.connect(monolith_index_path) as monolith_conn:
                for table_name in GLOBAL_TOPIC_TABLES:
                    expected = _table_count(monolith_conn, "main", table_name)
                    actual = int(counts.get(table_name) or 0)
                    if actual != expected:
                        errors.append(f"count_mismatch:{table_name}:{actual}!={expected}")
        except sqlite3.Error as exc:
            errors.append(f"monolith_sqlite_error:{exc}")

    return {
        "ok": not errors,
        "errors": errors,
        "path": str(topics_path),
        "integrity_check": integrity_check,
        "counts": counts,
    }


def _write_build_summary(
    index_path: Path,
    *,
    result: Mapping[str, Any],
    verification: Mapping[str, Any],
) -> dict[str, Any]:
    build_settings = dict(result.get("build_settings") or {})
    progress_path = Path(
        str(build_settings.get("progress_log_path") or index_path.parent / "build_progress.jsonl")
    )
    progress_summary = _summarize_progress_log(progress_path)
    summary_path = index_path.parent / "build_summary.json"
    summary = {
        "path": str(summary_path),
        "index_path": str(index_path),
        "root": str(result.get("root")),
        "artifact_indexes": result.get("artifact_indexes"),
        "totals": _json_safe(result.get("totals") or {}),
        "build_settings": _json_safe(build_settings),
        "verification": _json_safe(dict(verification)),
        "build_plan": _json_safe(result.get("build_plan_summary") or {}),
        "artifact_manifest": _json_safe(result.get("artifact_manifest") or {}),
        "build_graph": _json_safe(result.get("build_graph") or {}),
        "fragment_cache": _json_safe(result.get("fragment_cache") or {}),
        "company_cache": _json_safe(result.get("company_cache") or {}),
        "shards": _json_safe(result.get("shards") or {}),
        **progress_summary,
    }
    summary_path.write_text(
        json.dumps(summary, ensure_ascii=False, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return summary


def _write_build_plan(index_path: Path, plan: IndexBuildPlan) -> dict[str, Any]:
    plan_path = index_path.parent / "build_plan.json"
    payload = plan.to_dict(include_items=True)
    payload["path"] = str(plan_path)
    tmp_path = plan_path.with_suffix(".json.tmp")
    tmp_path.write_text(
        json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    tmp_path.replace(plan_path)
    return payload


def _write_artifact_manifest(plan: IndexBuildPlan) -> dict[str, Any]:
    path = plan.artifact_manifest_path
    payload = {
        "format": "krw-agent-index-artifact-manifest/v1",
        "root": str(plan.root),
        "index_path": str(plan.index_path),
        "layout": plan.layout,
        "artifact_count": len(plan.items),
        "dirty_artifact_count": len(plan.dirty_items),
        "cached_artifact_count": len(plan.cached_items),
        "artifacts": [
            {
                "relative_path": item.relative_path,
                "ticker": item.ticker,
                "document_type": item.document_type,
                "doc_type_key": item.doc_type_key,
                "period": item.period,
                "content_hash": item.content_hash,
                "cache_key": item.cache_key,
                "input_paths": list(item.input_paths),
                "missing_inputs": list(item.missing_inputs),
                "estimated_bytes": item.estimated_bytes,
                "estimated_rows": item.estimated_rows,
            }
            for item in plan.items
        ],
    }
    payload["path"] = str(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = path.with_suffix(".json.tmp")
    tmp_path.write_text(
        json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    tmp_path.replace(path)
    return _json_safe(payload)


def _write_build_graph(
    index_path: Path,
    *,
    plan: IndexBuildPlan,
    result: Mapping[str, Any],
) -> dict[str, Any]:
    graph_path = index_path.parent / "build_graph.json"
    nodes: list[dict[str, Any]] = []
    edges: list[dict[str, str]] = []
    root_node = {
        "id": "source_manifest" if plan.source_manifest_path else "filesystem_discovery",
        "type": "source_manifest" if plan.source_manifest_path else "filesystem_discovery",
        "input_hash": plan.source_manifest_hash,
        "output_hash": plan.source_manifest_hash,
        "status": "verified" if plan.source_manifest_path else "scanned",
        "path": str(plan.source_manifest_path) if plan.source_manifest_path else None,
        "discovery_mode": plan.discovery_mode,
    }
    nodes.append(root_node)
    for item in plan.items:
        node_id = f"artifact:{item.relative_path}"
        nodes.append(
            {
                "id": node_id,
                "type": "artifact_fragment",
                "ticker": item.ticker,
                "input_hash": item.content_hash,
                "output_hash": item.cache_key,
                "cache_key": item.cache_key,
                "cache_hit": item.cache_hit,
                "status": "cached" if item.cache_hit else "rebuilt",
                "skip_reason": "fragment_cache_hit" if item.cache_hit else None,
                "rebuild_reason": None
                if item.cache_hit
                else ",".join(item.cache_errors or ("fragment_cache_miss",)),
                "path": item.relative_path,
                "fragment_path": str(item.fragment_path),
            }
        )
        edges.append({"from": root_node["id"], "to": node_id, "type": "selects"})
    company_nodes = {item.ticker: f"company:{item.ticker}" for item in plan.company_items}
    for company in plan.company_items:
        node_id = company_nodes[company.ticker]
        nodes.append(
            {
                "id": node_id,
                "type": "company_projection",
                "ticker": company.ticker,
                "input_hash": company.input_hash,
                "output_hash": company.cache_key,
                "cache_key": company.cache_key,
                "cache_hit": company.cache_hit,
                "status": "cached" if company.cache_hit else "rebuilt",
                "skip_reason": "company_cache_hit" if company.cache_hit else None,
                "rebuild_reason": None
                if company.cache_hit
                else ",".join(company.cache_errors or ("company_cache_miss",)),
                "shard_cache_path": str(company.shard_cache_path),
            }
        )
        for artifact in plan.items:
            if artifact.ticker == company.ticker:
                edges.append(
                    {"from": f"artifact:{artifact.relative_path}", "to": node_id, "type": "feeds"}
                )

    shard_entries = (
        ((result.get("shards") or {}).get("shards") or {})
        if isinstance(result.get("shards"), Mapping)
        else {}
    )
    for ticker, company_node_id in company_nodes.items():
        entry = shard_entries.get(ticker) if isinstance(shard_entries, Mapping) else None
        output_hash = entry.get("sha256") if isinstance(entry, Mapping) else None
        nodes.append(
            {
                "id": f"shard:{ticker}",
                "type": "company_shard",
                "ticker": ticker,
                "input_hash": next(
                    (item.input_hash for item in plan.company_items if item.ticker == ticker), None
                ),
                "output_hash": output_hash,
                "status": "cached"
                if isinstance(entry, Mapping) and entry.get("cache_hit")
                else "rebuilt",
                "path": entry.get("path") if isinstance(entry, Mapping) else None,
            }
        )
        edges.append({"from": company_node_id, "to": f"shard:{ticker}", "type": "materializes"})
    if result.get("shards"):
        nodes.extend(
            [
                {
                    "id": "global_catalog",
                    "type": "global_catalog",
                    "input_hash": _build_graph_hash(sorted(company_nodes)),
                    "output_hash": (result.get("shards") or {}).get("global_catalog_sha256"),
                    "status": "rebuilt",
                    "path": "global_catalog.sqlite",
                },
                {
                    "id": "global_topics",
                    "type": "global_topics",
                    "input_hash": _build_graph_hash(
                        [item.cache_key for item in plan.company_items]
                    ),
                    "output_hash": (result.get("shards") or {}).get("global_topics_sha256"),
                    "status": "rebuilt",
                    "path": "global_topics.sqlite",
                },
            ]
        )
        for ticker in company_nodes:
            edges.append({"from": f"shard:{ticker}", "to": "global_catalog", "type": "registers"})
            edges.append(
                {"from": company_nodes[ticker], "to": "global_topics", "type": "contributes"}
            )

    payload = {
        "format": INDEX_BUILD_GRAPH_FORMAT_VERSION,
        "path": str(graph_path),
        "root": str(plan.root),
        "index_path": str(index_path),
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "discovery_mode": plan.discovery_mode,
        "source_manifest_path": str(plan.source_manifest_path)
        if plan.source_manifest_path
        else None,
        "source_manifest_hash": plan.source_manifest_hash,
        "node_count": len(nodes),
        "edge_count": len(edges),
        "nodes": nodes,
        "edges": edges,
    }
    tmp_path = graph_path.with_suffix(".json.tmp")
    tmp_path.write_text(
        json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    tmp_path.replace(graph_path)
    return _json_safe(payload)


def _build_graph_hash(values: Sequence[str]) -> str:
    encoded = json.dumps(list(values), sort_keys=True, separators=(",", ":")).encode("utf-8")
    return f"sha256:{hashlib.sha256(encoded).hexdigest()}"


def _stage_index_shards(
    index_path: Path,
    *,
    output_dir: Path,
    logical_index_path: Path | None = None,
    plan: IndexBuildPlan | None = None,
) -> dict[str, Any]:
    source_index_path = index_path.expanduser().resolve()
    resolved_output_dir = output_dir.expanduser().resolve()
    logical_source_path = (logical_index_path or source_index_path).expanduser().resolve()
    resolved_output_dir.mkdir(parents=True, exist_ok=True)
    stage_root = resolved_output_dir / f".index_shards.{os.getpid()}.{time.time_ns()}.tmp"
    shutil.rmtree(stage_root, ignore_errors=True)
    companies_stage_dir = stage_root / "companies"
    companies_stage_dir.mkdir(parents=True, exist_ok=True)
    catalog_stage_path = stage_root / "global_catalog.sqlite"
    global_topics_stage_path = stage_root / "global_topics.sqlite"
    manifest_stage_path = stage_root / "shard_manifest.json"
    try:
        with sqlite3.connect(source_index_path) as source_conn:
            source_conn.row_factory = sqlite3.Row
            tickers = [
                str(row[0])
                for row in source_conn.execute(
                    "SELECT DISTINCT ticker FROM documents ORDER BY ticker"
                ).fetchall()
            ]
            source_build_metadata = _read_metadata_json_from_schema(source_conn, "main", "build")
            source_counts = {
                table_name: _table_count(source_conn, "main", table_name)
                for table_name in ("documents", "objects", "edges", "quality_events")
            }
            company_plan_by_ticker = {
                item.ticker: item for item in (plan.company_items if plan is not None else ())
            }
            shard_cache_hits = 0
            shard_cache_misses = 0
            shard_entries: dict[str, dict[str, Any]] = {}
            for ticker in tickers:
                shard_filename = f"{_safe_ticker_filename(ticker)}.sqlite"
                shard_path = companies_stage_dir / shard_filename
                company_plan = company_plan_by_ticker.get(ticker)
                cache_hit = False
                if company_plan is not None and company_plan.cache_hit:
                    shutil.copy2(company_plan.shard_cache_path, shard_path)
                    cache_hit = True
                    shard_cache_hits += 1
                else:
                    shard_cache_misses += 1
                    _build_company_index_shard(
                        source_index_path,
                        shard_path,
                        ticker,
                        logical_index_path=logical_source_path,
                        source_build_metadata=source_build_metadata,
                        company_cache_key=company_plan.cache_key
                        if company_plan is not None
                        else None,
                    )
                verification = verify_agent_index(shard_path)
                if not verification["ok"]:
                    errors = ", ".join(verification["errors"])
                    raise RuntimeError(f"company shard failed verification: {ticker}: {errors}")
                expected_counts = _expected_shard_counts(source_conn, ticker)
                actual_counts = _sqlite_table_counts(shard_path, INDEX_SHARD_TABLES)
                count_mismatches = {
                    table_name: {"expected": expected, "actual": actual_counts.get(table_name)}
                    for table_name, expected in expected_counts.items()
                    if actual_counts.get(table_name) != expected
                }
                if count_mismatches:
                    raise RuntimeError(
                        f"company shard count mismatch: {ticker}: "
                        + json.dumps(count_mismatches, ensure_ascii=False, sort_keys=True)
                    )
                if not cache_hit and company_plan is not None:
                    _store_company_shard_cache(shard_path, company_plan.shard_cache_path)
                shard_entries[ticker] = {
                    "ticker": ticker,
                    "path": f"companies/{shard_filename}",
                    "document_count": actual_counts["documents"],
                    "object_count": actual_counts["objects"],
                    "edge_count": actual_counts["edges"],
                    "quality_event_count": actual_counts["quality_events"],
                    "row_counts": actual_counts,
                    "sha256": _file_sha256(shard_path),
                    "cache_hit": cache_hit,
                    "cache_key": company_plan.cache_key if company_plan is not None else None,
                    "shard_cache_path": str(company_plan.shard_cache_path)
                    if company_plan is not None
                    else None,
                }

            _build_global_catalog(
                source_conn,
                catalog_stage_path,
                shard_entries=shard_entries,
                source_counts=source_counts,
                logical_index_path=logical_source_path,
            )
            _build_global_topics_index(
                source_index_path,
                global_topics_stage_path,
                logical_index_path=logical_source_path,
                source_build_metadata=source_build_metadata,
            )

        verification = verify_index_shards(
            stage_root,
            monolith_index_path=source_index_path,
            allow_internal_legacy_builder=True,
        )
        if not verification["ok"]:
            errors = ", ".join(verification["errors"])
            raise RuntimeError(f"index shard layout failed verification: {errors}")
        manifest = {
            "index_layout_version": INDEX_LAYOUT_VERSION,
            "source_index_path": str(logical_source_path),
            "global_catalog": "global_catalog.sqlite",
            "global_catalog_sha256": _file_sha256(catalog_stage_path),
            "global_topics": "global_topics.sqlite",
            "global_topics_sha256": _file_sha256(global_topics_stage_path),
            "global_topics_counts": (verification.get("global_topics_verification") or {}).get(
                "counts"
            )
            or {},
            "companies_dir": "companies",
            "ticker_count": len(shard_entries),
            "source_counts": source_counts,
            "company_cache": {
                "format": INDEX_COMPANY_CACHE_FORMAT_VERSION,
                "hits": shard_cache_hits,
                "misses": shard_cache_misses,
                "cache_root": str(plan.cache_root) if plan is not None else None,
            },
            "shards": shard_entries,
            "verification": verification,
        }
        manifest_stage_path.write_text(
            json.dumps(manifest, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        return {
            "stage_root": stage_root,
            "output_dir": resolved_output_dir,
            "global_catalog_path": catalog_stage_path,
            "global_topics_path": global_topics_stage_path,
            "companies_dir": companies_stage_dir,
            "manifest_path": manifest_stage_path,
            "manifest": manifest,
            "verification": verification,
        }
    except Exception:
        shutil.rmtree(stage_root, ignore_errors=True)
        raise


def _publish_staged_index_shards(stage: Mapping[str, Any]) -> dict[str, Any]:
    output_dir = Path(stage["output_dir"])
    final_catalog_path = output_dir / "global_catalog.sqlite"
    final_global_topics_path = output_dir / "global_topics.sqlite"
    final_companies_dir = output_dir / "companies"
    final_manifest_path = output_dir / "shard_manifest.json"
    _replace_sqlite_database(Path(stage["global_catalog_path"]), final_catalog_path)
    _replace_sqlite_database(Path(stage["global_topics_path"]), final_global_topics_path)
    _replace_directory(Path(stage["companies_dir"]), final_companies_dir)
    os.replace(Path(stage["manifest_path"]), final_manifest_path)
    manifest = dict(stage["manifest"])
    result = {
        **manifest,
        "global_catalog_path": str(final_catalog_path),
        "global_topics_path": str(final_global_topics_path),
        "companies_dir": str(final_companies_dir),
        "manifest_path": str(final_manifest_path),
    }
    return _json_safe(result)


def _store_company_shard_cache(source_path: Path, cache_path: Path) -> None:
    cache_path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = cache_path.parent / f".{cache_path.name}.{os.getpid()}.{time.time_ns()}.tmp"
    _cleanup_sqlite_database_files(tmp_path)
    try:
        shutil.copy2(source_path, tmp_path)
        os.replace(tmp_path, cache_path)
    finally:
        _cleanup_sqlite_database_files(tmp_path)


def _build_company_index_shard(
    source_index_path: Path,
    target_path: Path,
    ticker: str,
    *,
    logical_index_path: Path,
    source_build_metadata: Mapping[str, Any],
    company_cache_key: str | None = None,
) -> None:
    _cleanup_sqlite_database_files(target_path)
    target_path.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(target_path, timeout=60)
    try:
        conn.row_factory = sqlite3.Row
        _configure_connection(conn)
        _create_schema(conn)
        conn.execute("ATTACH DATABASE ? AS source", (str(source_index_path),))
        try:
            with conn:
                for table_name in INDEX_SHARD_TABLES:
                    _copy_company_shard_table(conn, table_name, ticker)
                _create_base_secondary_indexes(conn)
                _create_serving_secondary_indexes(conn)
                row_counts = {
                    table_name: _table_count(conn, "main", table_name)
                    for table_name in INDEX_SHARD_TABLES
                }
                metadata = {
                    **dict(source_build_metadata),
                    "index_layout_version": INDEX_LAYOUT_VERSION,
                    "index_layout": "company-shard",
                    "index_role": "company_shard",
                    "source_index_layout": source_build_metadata.get("index_layout"),
                    "source_index_path": str(logical_index_path),
                    "shard_ticker": ticker,
                    "shard_generated_at": datetime.now(timezone.utc).isoformat(),
                    "row_counts": row_counts,
                    "company_cache_format_version": INDEX_COMPANY_CACHE_FORMAT_VERSION,
                    "company_cache_key": company_cache_key,
                }
                conn.execute(
                    """
                    INSERT OR REPLACE INTO metadata(key, value)
                    VALUES('build', ?)
                    """,
                    (json.dumps(metadata, ensure_ascii=False, sort_keys=True),),
                )
        finally:
            conn.execute("DETACH DATABASE source")
        conn.execute("PRAGMA optimize")
        _checkpoint_wal(conn, truncate=True)
    finally:
        conn.close()
        for sidecar_path in _sqlite_sidecar_paths(target_path):
            try:
                sidecar_path.unlink()
            except FileNotFoundError:
                pass


def _build_global_topics_index(
    source_index_path: Path,
    target_path: Path,
    *,
    logical_index_path: Path,
    source_build_metadata: Mapping[str, Any],
) -> None:
    _cleanup_sqlite_database_files(target_path)
    target_path.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(target_path, timeout=60)
    try:
        conn.row_factory = sqlite3.Row
        _configure_connection(conn)
        _create_schema(conn)
        conn.execute("ATTACH DATABASE ? AS source", (str(source_index_path),))
        try:
            with conn:
                for table_name in GLOBAL_TOPIC_TABLES:
                    _copy_global_topic_table(conn, table_name)
                _create_base_secondary_indexes(conn)
                _create_serving_secondary_indexes(conn)
                row_counts = {
                    table_name: _table_count(conn, "main", table_name)
                    for table_name in GLOBAL_TOPIC_TABLES
                }
                metadata = {
                    "schema_version": AGENT_INDEX_SCHEMA_VERSION,
                    "agent_index_schema_version": AGENT_INDEX_SCHEMA_VERSION,
                    "index_layout_version": INDEX_LAYOUT_VERSION,
                    "index_role": "global_topics",
                    "source_index_layout": source_build_metadata.get("index_layout"),
                    "source_index_path": str(logical_index_path),
                    "generated_at": datetime.now(timezone.utc).isoformat(),
                    "row_counts": row_counts,
                    "company_topic_builder_version": COMPANY_TOPIC_BUILDER_VERSION,
                }
                conn.execute(
                    """
                    INSERT OR REPLACE INTO metadata(key, value)
                    VALUES('topics', ?)
                    """,
                    (json.dumps(metadata, ensure_ascii=False, sort_keys=True),),
                )
        finally:
            conn.execute("DETACH DATABASE source")
        conn.execute("PRAGMA optimize")
        _checkpoint_wal(conn, truncate=True)
    finally:
        conn.close()
        for sidecar_path in _sqlite_sidecar_paths(target_path):
            try:
                sidecar_path.unlink()
            except FileNotFoundError:
                pass


def _copy_global_topic_table(conn: sqlite3.Connection, table_name: str) -> None:
    columns = _table_columns(conn, "main", table_name)
    if not columns:
        return
    column_sql = ", ".join(columns)
    insert_prefix = "INSERT INTO" if table_name in SHARD_FTS_TABLES else "INSERT OR REPLACE INTO"
    conn.execute(
        f"""
        {insert_prefix} {table_name}({column_sql})
        SELECT {column_sql}
        FROM source.{table_name}
        """
    )


def _copy_company_shard_table(conn: sqlite3.Connection, table_name: str, ticker: str) -> None:
    columns = _table_columns(conn, "main", table_name)
    if not columns:
        return
    column_sql = ", ".join(columns)
    insert_prefix = "INSERT INTO" if table_name in SHARD_FTS_TABLES else "INSERT OR REPLACE INTO"
    if table_name == "company_topic_source_objects":
        conn.execute(
            f"""
            {insert_prefix} {table_name}({column_sql})
            SELECT {column_sql}
            FROM source.{table_name}
            WHERE topic_id IN (SELECT topic_id FROM company_topic_index)
            """
        )
        return
    if "ticker" not in _table_columns(conn, "source", table_name):
        raise RuntimeError(f"cannot shard table without ticker column: {table_name}")
    conn.execute(
        f"""
        {insert_prefix} {table_name}({column_sql})
        SELECT {column_sql}
        FROM source.{table_name}
        WHERE ticker = ?
        """,
        (ticker,),
    )


def _build_global_catalog(
    source_conn: sqlite3.Connection,
    catalog_path: Path,
    *,
    shard_entries: Mapping[str, Mapping[str, Any]],
    source_counts: Mapping[str, int],
    logical_index_path: Path,
) -> None:
    _cleanup_sqlite_database_files(catalog_path)
    catalog_conn = sqlite3.connect(catalog_path, timeout=60)
    try:
        catalog_conn.row_factory = sqlite3.Row
        _configure_connection(catalog_conn)
        _create_global_catalog_schema(catalog_conn)
        document_rows = list(
            source_conn.execute(
                """
                SELECT ticker, document_type, doc_type_key, period, artifact_index_path,
                       ontology_dir, section_quality_status, generated_at, schema_version
                FROM documents
                ORDER BY ticker, doc_type_key, period
                """
            )
        )
        with catalog_conn:
            catalog_conn.executemany(
                """
                INSERT OR REPLACE INTO shards(
                    ticker, shard_path, document_count, object_count,
                    edge_count, quality_event_count, sha256, row_counts_json
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                """,
                [
                    (
                        ticker,
                        entry["path"],
                        int(entry["document_count"]),
                        int(entry["object_count"]),
                        int(entry["edge_count"]),
                        int(entry["quality_event_count"]),
                        entry["sha256"],
                        json.dumps(
                            entry.get("row_counts") or {}, ensure_ascii=False, sort_keys=True
                        ),
                    )
                    for ticker, entry in sorted(shard_entries.items())
                ],
            )
            catalog_conn.executemany(
                """
                INSERT OR REPLACE INTO documents(
                    ticker, document_type, doc_type_key, period, shard_path,
                    artifact_index_path, ontology_dir, section_quality_status,
                    generated_at, schema_version
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                """,
                [
                    (
                        row["ticker"],
                        row["document_type"],
                        row["doc_type_key"],
                        row["period"],
                        shard_entries[str(row["ticker"])]["path"],
                        row["artifact_index_path"],
                        row["ontology_dir"],
                        row["section_quality_status"],
                        row["generated_at"],
                        row["schema_version"],
                    )
                    for row in document_rows
                ],
            )
            metadata = {
                "schema_version": AGENT_INDEX_SCHEMA_VERSION,
                "agent_index_schema_version": AGENT_INDEX_SCHEMA_VERSION,
                "index_layout_version": INDEX_LAYOUT_VERSION,
                "index_role": "global_catalog",
                "source_index_path": str(logical_index_path),
                "generated_at": datetime.now(timezone.utc).isoformat(),
                "ticker_count": len(shard_entries),
                "source_counts": dict(source_counts),
            }
            catalog_conn.execute(
                "INSERT OR REPLACE INTO metadata(key, value) VALUES('catalog', ?)",
                (json.dumps(metadata, ensure_ascii=False, sort_keys=True),),
            )
        catalog_conn.execute("PRAGMA optimize")
        _checkpoint_wal(catalog_conn, truncate=True)
    finally:
        catalog_conn.close()
        for sidecar_path in _sqlite_sidecar_paths(catalog_path):
            try:
                sidecar_path.unlink()
            except FileNotFoundError:
                pass


def _create_global_catalog_schema(conn: sqlite3.Connection) -> None:
    conn.executescript(
        """
        CREATE TABLE IF NOT EXISTS metadata (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS shards (
            ticker TEXT PRIMARY KEY,
            shard_path TEXT NOT NULL,
            document_count INTEGER NOT NULL,
            object_count INTEGER NOT NULL,
            edge_count INTEGER NOT NULL,
            quality_event_count INTEGER NOT NULL,
            sha256 TEXT NOT NULL,
            row_counts_json TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS documents (
            ticker TEXT NOT NULL,
            document_type TEXT NOT NULL,
            doc_type_key TEXT NOT NULL,
            period TEXT NOT NULL,
            shard_path TEXT NOT NULL,
            artifact_index_path TEXT NOT NULL,
            ontology_dir TEXT NOT NULL,
            section_quality_status TEXT,
            generated_at TEXT,
            schema_version TEXT,
            PRIMARY KEY (ticker, doc_type_key, period)
        );

        CREATE INDEX IF NOT EXISTS idx_global_catalog_documents_ticker
            ON documents(ticker);
        CREATE INDEX IF NOT EXISTS idx_global_catalog_documents_scope
            ON documents(ticker, doc_type_key, period);
        """
    )


def _expected_shard_counts(source_conn: sqlite3.Connection, ticker: str) -> dict[str, int]:
    counts: dict[str, int] = {}
    for table_name in INDEX_SHARD_TABLES:
        if table_name == "company_topic_source_objects":
            counts[table_name] = int(
                source_conn.execute(
                    """
                    SELECT COUNT(*)
                    FROM company_topic_source_objects
                    WHERE topic_id IN (
                        SELECT topic_id
                        FROM company_topic_index
                        WHERE ticker = ?
                    )
                    """,
                    (ticker,),
                ).fetchone()[0]
            )
        elif "ticker" in _table_columns(source_conn, "main", table_name):
            counts[table_name] = int(
                source_conn.execute(
                    f"SELECT COUNT(*) FROM {table_name} WHERE ticker = ?",
                    (ticker,),
                ).fetchone()[0]
            )
        else:
            raise RuntimeError(f"cannot count shard table without ticker column: {table_name}")
    return counts


def _sqlite_table_counts(index_path: Path, table_names: Sequence[str]) -> dict[str, int]:
    with sqlite3.connect(index_path) as conn:
        return {table_name: _table_count(conn, "main", table_name) for table_name in table_names}


def _table_count(conn: sqlite3.Connection, schema_name: str, table_name: str) -> int:
    row = conn.execute(
        f"SELECT COUNT(*) FROM {_qualified_table_name(schema_name, table_name)}"
    ).fetchone()
    return int(row[0]) if row is not None else 0


def _read_metadata_json_from_schema(
    conn: sqlite3.Connection, schema_name: str, key: str
) -> dict[str, Any]:
    try:
        row = conn.execute(
            f"SELECT value FROM {_qualified_table_name(schema_name, 'metadata')} WHERE key = ?",
            (key,),
        ).fetchone()
    except sqlite3.Error:
        return {}
    if row is None:
        return {}
    try:
        return json.loads(row[0])
    except (TypeError, json.JSONDecodeError):
        return {}


def _qualified_table_name(schema_name: str, table_name: str) -> str:
    return f"{_quote_identifier(schema_name)}.{_quote_identifier(table_name)}"


def _quote_identifier(value: str) -> str:
    return '"' + str(value).replace('"', '""') + '"'


def _file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _safe_ticker_filename(ticker: str) -> str:
    normalized = re.sub(r"[^A-Z0-9._-]+", "_", str(ticker).upper()).strip("._")
    if normalized:
        return normalized
    return hashlib.sha256(str(ticker).encode("utf-8")).hexdigest()[:16]


def _replace_directory(source_dir: Path, target_dir: Path) -> None:
    backup_dir = target_dir.parent / f".{target_dir.name}.{os.getpid()}.{time.time_ns()}.bak"
    _remove_path(backup_dir)
    if target_dir.exists():
        os.replace(target_dir, backup_dir)
    try:
        os.replace(source_dir, target_dir)
    except Exception:
        if backup_dir.exists() and not target_dir.exists():
            os.replace(backup_dir, target_dir)
        raise
    else:
        _remove_path(backup_dir)


def _remove_path(path: Path) -> None:
    if not path.exists() and not path.is_symlink():
        return
    if path.is_dir() and not path.is_symlink():
        shutil.rmtree(path)
    else:
        path.unlink()


def _summarize_progress_log(path: Path) -> dict[str, Any]:
    if not path.exists():
        return {"total_elapsed_sec": None, "slow_phases": [], "slow_artifacts": []}
    rows: list[dict[str, Any]] = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    slow_phases: list[dict[str, Any]] = []
    slow_artifacts: list[dict[str, Any]] = []
    total_elapsed_sec = None
    for row in rows:
        phase = str(row.get("phase") or "")
        fields = row.get("fields") if isinstance(row.get("fields"), Mapping) else {}
        elapsed = fields.get("elapsed_seconds") if isinstance(fields, Mapping) else None
        if phase == "finalize_done":
            total_elapsed_sec = (
                fields.get("total_elapsed_seconds") if isinstance(fields, Mapping) else None
            )
        if not isinstance(elapsed, (int, float)):
            continue
        if phase == "index_artifact_done":
            slow_artifacts.append(
                {
                    "artifact_index": fields.get("artifact_index"),
                    "elapsed_seconds": elapsed,
                    "stats": fields.get("stats"),
                }
            )
        elif phase.endswith("_done"):
            slow_phases.append({"phase": phase.removesuffix("_done"), "elapsed_seconds": elapsed})
    slow_phases.sort(key=lambda item: float(item.get("elapsed_seconds") or 0), reverse=True)
    slow_artifacts.sort(key=lambda item: float(item.get("elapsed_seconds") or 0), reverse=True)
    return {
        "total_elapsed_sec": total_elapsed_sec,
        "slow_phases": slow_phases[:10],
        "slow_artifacts": slow_artifacts[:20],
    }


def write_source_artifact_manifest(
    root: Path,
    *,
    manifest_path: Path | None = None,
) -> dict[str, Any]:
    """Write the canonical artifact source manifest used by production builds."""
    payload = build_source_artifact_manifest_payload(
        root,
        manifest_path=manifest_path,
    )
    resolved_manifest_path = Path(str(payload["path"]))
    resolved_manifest_path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = resolved_manifest_path.with_suffix(".json.tmp")
    tmp_path.write_text(
        json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    tmp_path.replace(resolved_manifest_path)
    return _json_safe(payload)


def build_source_artifact_manifest_payload(
    root: Path,
    *,
    manifest_path: Path | None = None,
) -> dict[str, Any]:
    """Build a canonical source-manifest payload without writing release files."""
    resolved_root = root.expanduser().resolve()
    resolved_manifest_path = (
        manifest_path.expanduser().resolve()
        if manifest_path is not None
        else resolved_root / "indexes" / "source_manifest.json"
    )
    artifacts = [
        _source_manifest_artifact_entry(resolved_root, artifact_index_path)
        for artifact_index_path in discover_artifact_indexes(resolved_root)
    ]
    payload: dict[str, Any] = {
        "format": SOURCE_ARTIFACT_MANIFEST_FORMAT_VERSION,
        "root": str(resolved_root),
        "builder_code_version": AGENT_INDEX_BUILDER_VERSION,
        "source_artifact_sqlite_builder_version": AGENT_INDEX_BUILDER_VERSION,
        "agent_index_schema_version": AGENT_INDEX_SCHEMA_VERSION,
        "source_artifact_sqlite_schema_version": AGENT_INDEX_SCHEMA_VERSION,
        "ontology_schema_version": SCHEMA_VERSION,
        "retrieval_text_builder_version": RETRIEVAL_TEXT_BUILDER_VERSION,
        "company_topic_builder_version": COMPANY_TOPIC_BUILDER_VERSION,
        "typed_projection_builder_version": TYPED_PROJECTION_BUILDER_VERSION,
        "metric_dictionary": metric_dictionary_binding(),
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "artifact_count": len(artifacts),
        "artifacts": artifacts,
    }
    payload["manifest_hash"] = _source_manifest_payload_hash(payload)
    payload["path"] = str(resolved_manifest_path)
    return _json_safe(payload)


def verify_source_artifact_manifest(
    root: Path,
    *,
    manifest_path: Path | None = None,
) -> dict[str, Any]:
    """Verify a source manifest against the current artifact bytes."""
    resolved_root = root.expanduser().resolve()
    resolved_manifest_path = (
        manifest_path.expanduser().resolve()
        if manifest_path is not None
        else resolved_root / "indexes" / "source_manifest.json"
    )
    errors: list[str] = []
    manifest: dict[str, Any] = {}
    try:
        manifest = json.loads(resolved_manifest_path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        errors.append("source_manifest_missing")
    except json.JSONDecodeError:
        errors.append("source_manifest_invalid_json")
    except OSError as exc:
        errors.append(f"source_manifest_read_error:{exc}")
    if errors:
        return {
            "ok": False,
            "errors": errors,
            "root": str(resolved_root),
            "path": str(resolved_manifest_path),
            "artifact_count": 0,
            "manifest_hash": None,
        }

    artifact_paths: list[Path] = []
    errors.extend(metric_dictionary_binding_errors(manifest.get("metric_dictionary")))
    try:
        artifact_paths, manifest_hash = _artifact_paths_from_source_manifest(
            resolved_root,
            resolved_manifest_path,
            manifest=manifest,
        )
    except ValueError as exc:
        errors.append(str(exc))
        manifest_hash = (
            _source_manifest_payload_hash(manifest) if isinstance(manifest, Mapping) else None
        )
    return {
        "ok": not errors,
        "errors": errors,
        "root": str(resolved_root),
        "path": str(resolved_manifest_path),
        "artifact_count": len(artifact_paths),
        "manifest_hash": manifest_hash,
    }


def diff_source_artifact_manifests(
    left_path: Path,
    right_path: Path,
) -> dict[str, Any]:
    """Diff two canonical source manifests by artifact relative path and content hash."""
    left = json.loads(left_path.expanduser().read_text(encoding="utf-8"))
    right = json.loads(right_path.expanduser().read_text(encoding="utf-8"))
    left_artifacts = _source_manifest_artifact_map(left)
    right_artifacts = _source_manifest_artifact_map(right)
    added = sorted(set(right_artifacts) - set(left_artifacts))
    removed = sorted(set(left_artifacts) - set(right_artifacts))
    changed = sorted(
        path
        for path in set(left_artifacts) & set(right_artifacts)
        if left_artifacts[path].get("content_hash") != right_artifacts[path].get("content_hash")
    )
    unchanged = sorted(
        path
        for path in set(left_artifacts) & set(right_artifacts)
        if left_artifacts[path].get("content_hash") == right_artifacts[path].get("content_hash")
    )
    return {
        "format": "krw-agent-index-source-manifest-diff/v1",
        "left_path": str(left_path.expanduser()),
        "right_path": str(right_path.expanduser()),
        "left_manifest_hash": left.get("manifest_hash") or _source_manifest_payload_hash(left),
        "right_manifest_hash": right.get("manifest_hash") or _source_manifest_payload_hash(right),
        "added": added,
        "removed": removed,
        "changed": changed,
        "unchanged_count": len(unchanged),
        "summary": {
            "added": len(added),
            "removed": len(removed),
            "changed": len(changed),
            "unchanged": len(unchanged),
        },
    }


def _source_manifest_artifact_entry(root: Path, artifact_index_path: Path) -> dict[str, Any]:
    artifact_index = json.loads(artifact_index_path.read_text(encoding="utf-8"))
    ticker, document_type, doc_type_key, period = _artifact_identity(
        root, artifact_index_path, artifact_index
    )
    content_hash, input_paths, missing_inputs, estimated_bytes = _artifact_content_hash(
        root,
        artifact_index_path,
        artifact_index,
    )
    return {
        "relative_path": _path_label(root, artifact_index_path),
        "ticker": ticker,
        "document_type": document_type,
        "doc_type_key": doc_type_key,
        "period": period,
        "content_hash": content_hash,
        "input_paths": list(input_paths),
        "missing_inputs": list(missing_inputs),
        "estimated_bytes": estimated_bytes,
        "estimated_rows": _estimated_artifact_rows(artifact_index),
    }


def _source_manifest_payload_hash(payload: Mapping[str, Any]) -> str:
    canonical = {
        key: value
        for key, value in payload.items()
        if key not in {"generated_at", "manifest_hash", "path", "root"}
    }
    encoded = json.dumps(
        canonical, ensure_ascii=False, sort_keys=True, separators=(",", ":")
    ).encode("utf-8")
    return f"sha256:{hashlib.sha256(encoded).hexdigest()}"


def _source_manifest_artifact_map(payload: Mapping[str, Any]) -> dict[str, Mapping[str, Any]]:
    artifacts = payload.get("artifacts")
    if not isinstance(artifacts, Sequence) or isinstance(artifacts, (str, bytes, bytearray)):
        return {}
    result: dict[str, Mapping[str, Any]] = {}
    for artifact in artifacts:
        if not isinstance(artifact, Mapping):
            continue
        relative_path = artifact.get("relative_path")
        if isinstance(relative_path, str) and relative_path:
            result[relative_path] = artifact
    return result


def _artifact_paths_from_source_manifest(
    root: Path,
    manifest_path: Path,
    *,
    manifest: Mapping[str, Any] | None = None,
) -> tuple[list[Path], str]:
    payload = dict(manifest or json.loads(manifest_path.read_text(encoding="utf-8")))
    if payload.get("format") != SOURCE_ARTIFACT_MANIFEST_FORMAT_VERSION:
        raise ValueError("source_manifest_format_mismatch")
    actual_manifest_hash = _source_manifest_payload_hash(payload)
    expected_manifest_hash = payload.get("manifest_hash")
    if expected_manifest_hash and expected_manifest_hash != actual_manifest_hash:
        raise ValueError("source_manifest_hash_mismatch")
    artifacts = payload.get("artifacts")
    if not isinstance(artifacts, Sequence) or isinstance(artifacts, (str, bytes, bytearray)):
        raise ValueError("source_manifest_artifacts_invalid")
    expected_count = payload.get("artifact_count")
    if isinstance(expected_count, int) and expected_count != len(artifacts):
        raise ValueError("source_manifest_artifact_count_mismatch")
    seen: set[str] = set()
    paths: list[Path] = []
    for index, artifact in enumerate(artifacts):
        if not isinstance(artifact, Mapping):
            raise ValueError(f"source_manifest_artifact_invalid:{index}")
        relative_path = artifact.get("relative_path")
        if not isinstance(relative_path, str) or not relative_path:
            raise ValueError(f"source_manifest_artifact_path_missing:{index}")
        if relative_path in seen:
            raise ValueError(f"source_manifest_artifact_duplicate:{relative_path}")
        seen.add(relative_path)
        artifact_path = (root / relative_path).resolve()
        try:
            artifact_path.relative_to(root)
        except ValueError as exc:
            raise ValueError(f"source_manifest_artifact_escapes_root:{relative_path}") from exc
        if not artifact_path.is_file():
            raise ValueError(f"source_manifest_artifact_missing:{relative_path}")
        artifact_index = json.loads(artifact_path.read_text(encoding="utf-8"))
        actual_content_hash, _input_paths, missing_inputs, _estimated_bytes = (
            _artifact_content_hash(
                root,
                artifact_path,
                artifact_index,
            )
        )
        if missing_inputs:
            raise ValueError(
                f"source_manifest_artifact_inputs_missing:{relative_path}:{','.join(missing_inputs)}"
            )
        expected_content_hash = artifact.get("content_hash")
        if expected_content_hash != actual_content_hash:
            raise ValueError(f"source_manifest_content_hash_mismatch:{relative_path}")
        paths.append(artifact_path)
    return sorted(paths), actual_manifest_hash


def _validated_source_manifest_entries(
    root: Path,
    manifest: Mapping[str, Any],
) -> tuple[tuple[Mapping[str, Any], ...], str]:
    """Validate an in-process manifest envelope without rereading artifact bytes."""
    payload = dict(manifest)
    if payload.get("format") != SOURCE_ARTIFACT_MANIFEST_FORMAT_VERSION:
        raise ValueError("source_manifest_format_mismatch")
    actual_manifest_hash = _source_manifest_payload_hash(payload)
    expected_manifest_hash = payload.get("manifest_hash")
    if expected_manifest_hash != actual_manifest_hash:
        raise ValueError("source_manifest_hash_mismatch")
    artifacts = payload.get("artifacts")
    if not isinstance(artifacts, Sequence) or isinstance(artifacts, (str, bytes, bytearray)):
        raise ValueError("source_manifest_artifacts_invalid")
    if payload.get("artifact_count") != len(artifacts):
        raise ValueError("source_manifest_artifact_count_mismatch")
    seen: set[str] = set()
    entries: list[Mapping[str, Any]] = []
    for index, raw_artifact in enumerate(artifacts):
        if not isinstance(raw_artifact, Mapping):
            raise ValueError(f"source_manifest_artifact_invalid:{index}")
        artifact = dict(raw_artifact)
        relative_path = artifact.get("relative_path")
        if not isinstance(relative_path, str) or not relative_path:
            raise ValueError(f"source_manifest_artifact_path_missing:{index}")
        if relative_path in seen:
            raise ValueError(f"source_manifest_artifact_duplicate:{relative_path}")
        seen.add(relative_path)
        artifact_path = (root / relative_path).resolve()
        try:
            artifact_path.relative_to(root)
        except ValueError as exc:
            raise ValueError(f"source_manifest_artifact_escapes_root:{relative_path}") from exc
        if not artifact_path.is_file():
            raise ValueError(f"source_manifest_artifact_missing:{relative_path}")
        missing_inputs = artifact.get("missing_inputs")
        if missing_inputs:
            raise ValueError(
                f"source_manifest_artifact_inputs_missing:{relative_path}:"
                + ",".join(str(value) for value in missing_inputs)
            )
        entries.append(artifact)
    return tuple(entries), actual_manifest_hash


def _artifact_plan_item_from_manifest_entry(
    *,
    root: Path,
    artifact: Mapping[str, Any],
    cache_root: Path,
) -> ArtifactPlanItem:
    """Build one plan item from bytes hashed by the immediately preceding manifest stage."""
    relative_path = str(artifact.get("relative_path") or "")
    artifact_index_path = (root / relative_path).resolve()
    required_strings = {
        key: str(artifact.get(key) or "").strip()
        for key in (
            "ticker",
            "document_type",
            "doc_type_key",
            "period",
            "content_hash",
        )
    }
    missing = [key for key, value in required_strings.items() if not value]
    if missing:
        raise ValueError(
            f"source_manifest_artifact_fields_missing:{relative_path}:{','.join(missing)}"
        )
    input_paths_raw = artifact.get("input_paths")
    if not isinstance(input_paths_raw, Sequence) or isinstance(
        input_paths_raw, (str, bytes, bytearray)
    ):
        raise ValueError(f"source_manifest_artifact_input_paths_invalid:{relative_path}")
    input_paths = tuple(str(value) for value in input_paths_raw)
    cache_key = _artifact_fragment_cache_key(required_strings["content_hash"])
    fragment_path = _fragment_path(cache_root, cache_key)
    expected_fragment = _expected_fragment_metadata(
        relative_path=relative_path,
        ticker=required_strings["ticker"].upper(),
        document_type=required_strings["document_type"],
        doc_type_key=required_strings["doc_type_key"],
        period=required_strings["period"],
        content_hash=required_strings["content_hash"],
        cache_key=cache_key,
    )
    fragment_verification = verify_index_fragment(
        fragment_path,
        expected=expected_fragment,
        seal_on_success=True,
    )
    cache_errors = tuple(fragment_verification["errors"])
    estimated_rows = artifact.get("estimated_rows")
    return ArtifactPlanItem(
        artifact_index_path=artifact_index_path,
        relative_path=relative_path,
        ticker=required_strings["ticker"].upper(),
        document_type=required_strings["document_type"],
        doc_type_key=required_strings["doc_type_key"],
        period=required_strings["period"],
        content_hash=required_strings["content_hash"],
        cache_key=cache_key,
        fragment_path=fragment_path,
        cache_hit=bool(fragment_verification["ok"]),
        estimated_bytes=int(artifact.get("estimated_bytes") or 0),
        estimated_rows=(
            int(estimated_rows)
            if isinstance(estimated_rows, int) and not isinstance(estimated_rows, bool)
            else None
        ),
        input_paths=input_paths,
        missing_inputs=(),
        cache_errors=cache_errors,
    )


def plan_agent_index(
    root: Path,
    *,
    index_path: Path | None = None,
    cache_root: Path | None = None,
    layout: str = "monolith-and-shards",
    workers: int | None = None,
    source_manifest_path: Path | None = None,
    source_manifest_payload: Mapping[str, Any] | None = None,
    allow_internal_legacy_builder: bool = False,
) -> IndexBuildPlan:
    """Create a deterministic build plan for the legacy agent index.

    Normal production planning must use plan_spine_shard_release_outputs().
    """
    if not allow_internal_legacy_builder:
        raise ValueError(LEGACY_BUILDER_DISABLED_MESSAGE)
    resolved_root = root.expanduser().resolve()
    resolved_index_path = (
        (index_path or resolved_root / DEFAULT_INDEX_RELATIVE_PATH).expanduser().resolve()
    )
    resolved_layout = _normalize_index_layout(layout)
    resolved_cache_root = (
        cache_root.expanduser().resolve()
        if cache_root is not None
        else _default_fragment_cache_root(resolved_root)
    )
    resolved_workers = _resolve_build_workers(workers)
    build_settings = _build_resource_settings(resolved_index_path)
    resolved_source_manifest_path = (
        source_manifest_path.expanduser().resolve() if source_manifest_path is not None else None
    )
    manifest_entries: tuple[Mapping[str, Any], ...] | None = None
    if source_manifest_payload is not None:
        if resolved_source_manifest_path is None:
            raise ValueError("source_manifest_payload_requires_path")
        manifest_entries, source_manifest_hash = _validated_source_manifest_entries(
            resolved_root,
            source_manifest_payload,
        )
        artifact_indexes = []
        discovery_mode = "trusted-in-process-source-manifest"
    elif resolved_source_manifest_path is None:
        artifact_indexes = discover_artifact_indexes(resolved_root)
        discovery_mode = "filesystem-scan"
        source_manifest_hash = None
    else:
        artifact_indexes, source_manifest_hash = _artifact_paths_from_source_manifest(
            resolved_root,
            resolved_source_manifest_path,
        )
        discovery_mode = "source-manifest"
    if manifest_entries is not None:
        item_inputs = tuple(manifest_entries)

        def build_item(entry: Mapping[str, Any]) -> ArtifactPlanItem:
            return _artifact_plan_item_from_manifest_entry(
                root=resolved_root,
                artifact=entry,
                cache_root=resolved_cache_root,
            )

    else:
        item_inputs = tuple(artifact_indexes)

        def build_item(entry: Mapping[str, Any] | Path) -> ArtifactPlanItem:
            return _artifact_plan_item(
                root=resolved_root,
                artifact_index_path=Path(entry),
                cache_root=resolved_cache_root,
            )

    if resolved_workers > 1 and len(item_inputs) > 1:
        with ThreadPoolExecutor(max_workers=min(resolved_workers, len(item_inputs))) as executor:
            items = tuple(executor.map(build_item, item_inputs))
    else:
        items = tuple(build_item(entry) for entry in item_inputs)
    cached_items = tuple(item for item in items if item.cache_hit)
    dirty_items = tuple(item for item in items if not item.cache_hit)
    company_items = _plan_company_items(items, cache_root=resolved_cache_root)
    cached_company_items = tuple(item for item in company_items if item.cache_hit)
    dirty_company_items = tuple(item for item in company_items if not item.cache_hit)
    dirty_tickers = tuple(sorted({item.ticker for item in dirty_items}))
    return IndexBuildPlan(
        root=resolved_root,
        index_path=resolved_index_path,
        cache_root=resolved_cache_root,
        artifact_manifest_path=resolved_index_path.parent / "artifact_manifest.json",
        source_manifest_path=resolved_source_manifest_path,
        source_manifest_hash=source_manifest_hash,
        discovery_mode=discovery_mode,
        items=items,
        dirty_items=dirty_items,
        cached_items=cached_items,
        company_items=company_items,
        dirty_company_items=dirty_company_items,
        cached_company_items=cached_company_items,
        dirty_tickers=dirty_tickers,
        workers=resolved_workers,
        layout=resolved_layout,
        build_settings=build_settings,
        builder_code_version=AGENT_INDEX_BUILDER_VERSION,
    )


def _normalize_index_layout(layout: str) -> str:
    normalized = str(layout or "monolith").strip().lower()
    if normalized not in SUPPORTED_INDEX_LAYOUTS:
        supported = ", ".join(sorted(SUPPORTED_INDEX_LAYOUTS))
        raise ValueError(f"unsupported index layout {layout!r}; expected one of: {supported}")
    return normalized


def _layout_builds_shards(layout: str) -> bool:
    return _normalize_index_layout(layout) in {"monolith-and-shards", "shards"}


def _default_fragment_cache_root(root: Path) -> Path:
    configured = os.getenv("KRW_INDEX_FRAGMENT_CACHE_ROOT")
    if configured:
        return Path(configured).expanduser().resolve()
    return (root.parent / ".index_fragment_cache").resolve()


def _plan_company_items(
    items: Sequence[ArtifactPlanItem],
    *,
    cache_root: Path,
) -> tuple[CompanyPlanItem, ...]:
    by_ticker: dict[str, list[ArtifactPlanItem]] = defaultdict(list)
    for item in items:
        by_ticker[item.ticker].append(item)
    return tuple(
        _company_plan_item(ticker, by_ticker[ticker], cache_root=cache_root)
        for ticker in sorted(by_ticker)
    )


def _company_plan_item(
    ticker: str,
    items: Sequence[ArtifactPlanItem],
    *,
    cache_root: Path,
) -> CompanyPlanItem:
    artifact_cache_keys = tuple(
        f"{item.relative_path}={item.cache_key}"
        for item in sorted(items, key=lambda item: item.relative_path)
    )
    input_hash = _company_cache_input_hash(ticker, artifact_cache_keys)
    cache_key = _company_shard_cache_key(input_hash)
    shard_cache_path = _company_shard_cache_path(cache_root, cache_key)
    cache_errors = _verify_company_shard_cache(shard_cache_path, ticker=ticker, cache_key=cache_key)
    return CompanyPlanItem(
        ticker=ticker,
        artifact_count=len(items),
        artifact_cache_keys=artifact_cache_keys,
        input_hash=input_hash,
        cache_key=cache_key,
        shard_cache_path=shard_cache_path,
        cache_hit=not cache_errors,
        cache_errors=cache_errors,
    )


def _company_cache_input_hash(ticker: str, artifact_cache_keys: Sequence[str]) -> str:
    payload = {
        "ticker": ticker,
        "artifact_cache_keys": list(artifact_cache_keys),
        "builder_code_version": AGENT_INDEX_BUILDER_VERSION,
        "source_artifact_sqlite_builder_version": AGENT_INDEX_BUILDER_VERSION,
        "agent_index_schema_version": AGENT_INDEX_SCHEMA_VERSION,
        "source_artifact_sqlite_schema_version": AGENT_INDEX_SCHEMA_VERSION,
        "ontology_schema_version": SCHEMA_VERSION,
        "retrieval_text_builder_version": RETRIEVAL_TEXT_BUILDER_VERSION,
        "company_topic_builder_version": COMPANY_TOPIC_BUILDER_VERSION,
        "typed_projection_builder_version": TYPED_PROJECTION_BUILDER_VERSION,
        "metric_dictionary": metric_dictionary_binding(),
        "index_layout_version": INDEX_LAYOUT_VERSION,
        "index_shard_tables": list(INDEX_SHARD_TABLES),
    }
    encoded = json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return f"sha256:{hashlib.sha256(encoded).hexdigest()}"


def _company_shard_cache_key(input_hash: str) -> str:
    payload = {
        "company_cache_format_version": INDEX_COMPANY_CACHE_FORMAT_VERSION,
        "input_hash": input_hash,
    }
    encoded = json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return f"sha256:{hashlib.sha256(encoded).hexdigest()}"


def _company_shard_cache_path(cache_root: Path, cache_key: str) -> Path:
    digest = cache_key.split(":", 1)[-1]
    return cache_root / "companies" / digest[:2] / f"{digest}.sqlite"


def _verify_company_shard_cache(
    shard_path: Path,
    *,
    ticker: str,
    cache_key: str,
) -> tuple[str, ...]:
    if not shard_path.exists():
        return ("company_cache_missing",)
    verification = verify_agent_index(shard_path)
    errors = [f"company_cache:{error}" for error in verification.get("errors") or []]
    try:
        with sqlite3.connect(shard_path) as conn:
            metadata = _read_metadata_json_from_schema(conn, "main", "build")
    except sqlite3.Error as exc:
        errors.append(f"company_cache_sqlite_error:{exc}")
        metadata = {}
    if metadata.get("index_role") != "company_shard":
        errors.append("company_cache_metadata_role_mismatch")
    if str(metadata.get("shard_ticker") or "") != ticker:
        errors.append("company_cache_metadata_ticker_mismatch")
    if metadata.get("company_cache_key") != cache_key:
        errors.append("company_cache_metadata_key_mismatch")
    if metadata.get("company_cache_format_version") != INDEX_COMPANY_CACHE_FORMAT_VERSION:
        errors.append("company_cache_metadata_format_mismatch")
    return tuple(errors)


def _resolve_build_workers(workers: int | None) -> int:
    if workers is not None:
        return max(1, int(workers))
    configured = os.getenv("KRW_INDEX_BUILD_WORKERS")
    if configured:
        try:
            return max(1, int(configured))
        except ValueError:
            return 1
    return min(max((os.cpu_count() or 1) - 1, 1), DEFAULT_INDEX_BUILD_WORKER_CAP)


def _artifact_plan_item(
    *, root: Path, artifact_index_path: Path, cache_root: Path
) -> ArtifactPlanItem:
    artifact_index = json.loads(artifact_index_path.read_text(encoding="utf-8"))
    ticker, document_type, doc_type_key, period = _artifact_identity(
        root, artifact_index_path, artifact_index
    )
    content_hash, input_paths, missing_inputs, estimated_bytes = _artifact_content_hash(
        root,
        artifact_index_path,
        artifact_index,
    )
    cache_key = _artifact_fragment_cache_key(content_hash)
    fragment_path = _fragment_path(cache_root, cache_key)
    expected_fragment = _expected_fragment_metadata(
        relative_path=_path_label(root, artifact_index_path),
        ticker=ticker,
        document_type=document_type,
        doc_type_key=doc_type_key,
        period=period,
        content_hash=content_hash,
        cache_key=cache_key,
    )
    fragment_verification = verify_index_fragment(
        fragment_path,
        expected=expected_fragment,
        seal_on_success=True,
    )
    cache_errors = tuple(fragment_verification["errors"])
    cache_hit = not missing_inputs and fragment_verification["ok"]
    return ArtifactPlanItem(
        artifact_index_path=artifact_index_path,
        relative_path=_path_label(root, artifact_index_path),
        ticker=ticker,
        document_type=document_type,
        doc_type_key=doc_type_key,
        period=period,
        content_hash=content_hash,
        cache_key=cache_key,
        fragment_path=fragment_path,
        cache_hit=cache_hit,
        estimated_bytes=estimated_bytes,
        estimated_rows=_estimated_artifact_rows(artifact_index),
        input_paths=input_paths,
        missing_inputs=missing_inputs,
        cache_errors=cache_errors,
    )


def _artifact_identity(
    root: Path,
    artifact_index_path: Path,
    artifact_index: Mapping[str, Any],
) -> tuple[str, str, str, str]:
    required_fields = ("ticker", "document_type", "doc_type_key", "period")
    values = {field: str(artifact_index.get(field) or "").strip() for field in required_fields}
    missing_fields = tuple(field for field, value in values.items() if not value)
    if missing_fields:
        missing = ",".join(missing_fields)
        raise KeyError(
            f"artifact_index_missing_required:{missing}:{_path_label(root, artifact_index_path)}"
        )
    return (
        values["ticker"].upper(),
        values["document_type"],
        values["doc_type_key"],
        values["period"],
    )


def _artifact_content_hash(
    root: Path,
    artifact_index_path: Path,
    artifact_index: Mapping[str, Any],
) -> tuple[str, tuple[str, ...], tuple[str, ...], int]:
    digest = hashlib.sha256()
    digest.update(INDEX_BUILD_PLAN_FORMAT_VERSION.encode("utf-8"))
    digest.update(b"\0")
    input_paths = _artifact_input_paths(root, artifact_index_path, artifact_index)
    existing_labels: list[str] = []
    missing_labels: list[str] = []
    estimated_bytes = 0
    optional_missing = {_path_label(root, artifact_index_path.parent / "section_quality.json")}
    for path in input_paths:
        label = _path_label(root, path)
        digest.update(label.encode("utf-8"))
        digest.update(b"\0")
        try:
            data = path.read_bytes()
        except OSError:
            digest.update(b"MISSING")
            if label not in optional_missing:
                missing_labels.append(label)
            continue
        digest.update(str(len(data)).encode("ascii"))
        digest.update(b"\0")
        digest.update(data)
        existing_labels.append(label)
        estimated_bytes += len(data)
    return (
        f"sha256:{digest.hexdigest()}",
        tuple(existing_labels),
        tuple(missing_labels),
        estimated_bytes,
    )


def _artifact_input_paths(
    root: Path,
    artifact_index_path: Path,
    artifact_index: Mapping[str, Any],
) -> tuple[Path, ...]:
    paths: dict[str, Path] = {}

    def add(path: Path | None) -> None:
        if path is None:
            return
        resolved = path.expanduser().resolve()
        paths[_path_label(root, resolved)] = resolved

    add(artifact_index_path)
    files = artifact_index.get("files") if isinstance(artifact_index.get("files"), Mapping) else {}
    indexed_file_keys = OBJECT_FILE_KEYS | {
        "edges",
        "quality_events",
        "rejected_objects",
        "batch_failures",
    }
    for artifact_key, rel_path in files.items():
        if artifact_key in indexed_file_keys:
            add(_resolve_artifact_path(root, rel_path))
    add(artifact_index_path.parent / "section_quality.json")
    return tuple(paths[key] for key in sorted(paths))


def _artifact_fragment_cache_key(content_hash: str) -> str:
    digest = hashlib.sha256()
    payload = {
        "fragment_cache_format_version": INDEX_FRAGMENT_CACHE_FORMAT_VERSION,
        "content_hash": content_hash,
        "builder_code_version": AGENT_INDEX_BUILDER_VERSION,
        "source_artifact_sqlite_builder_version": AGENT_INDEX_BUILDER_VERSION,
        "agent_index_schema_version": AGENT_INDEX_SCHEMA_VERSION,
        "source_artifact_sqlite_schema_version": AGENT_INDEX_SCHEMA_VERSION,
        "ontology_schema_version": SCHEMA_VERSION,
        "retrieval_text_builder_version": RETRIEVAL_TEXT_BUILDER_VERSION,
        "company_topic_builder_version": COMPANY_TOPIC_BUILDER_VERSION,
        "typed_projection_builder_version": TYPED_PROJECTION_BUILDER_VERSION,
    }
    digest.update(json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("utf-8"))
    return f"sha256:{digest.hexdigest()}"


def _fragment_path(cache_root: Path, cache_key: str) -> Path:
    digest = cache_key.split(":", 1)[-1]
    return cache_root / "fragments" / digest[:2] / f"{digest}.sqlite"


def _expected_fragment_metadata(
    *,
    relative_path: str,
    ticker: str,
    document_type: str,
    doc_type_key: str,
    period: str,
    content_hash: str,
    cache_key: str,
) -> dict[str, Any]:
    return {
        "fragment_cache_format_version": INDEX_FRAGMENT_CACHE_FORMAT_VERSION,
        "cache_key": cache_key,
        "content_hash": content_hash,
        "builder_code_version": AGENT_INDEX_BUILDER_VERSION,
        "source_artifact_sqlite_builder_version": AGENT_INDEX_BUILDER_VERSION,
        "agent_index_schema_version": AGENT_INDEX_SCHEMA_VERSION,
        "source_artifact_sqlite_schema_version": AGENT_INDEX_SCHEMA_VERSION,
        "ontology_schema_version": SCHEMA_VERSION,
        "retrieval_text_builder_version": RETRIEVAL_TEXT_BUILDER_VERSION,
        "company_topic_builder_version": COMPANY_TOPIC_BUILDER_VERSION,
        "typed_projection_builder_version": TYPED_PROJECTION_BUILDER_VERSION,
        "artifact_index_relative_path": relative_path,
        "ticker": ticker,
        "document_type": document_type,
        "doc_type_key": doc_type_key,
        "period": period,
    }


def write_index_fragment_metadata(
    fragment_path: Path,
    item: ArtifactPlanItem,
    *,
    row_counts: Mapping[str, int] | None = None,
) -> dict[str, Any]:
    """Write the minimum valid SQLite fragment metadata atomically."""
    normalized_row_counts = {
        table_name: int((row_counts or {}).get(table_name) or 0)
        for table_name in BASE_FRAGMENT_TABLES
    }
    metadata = {
        **_expected_fragment_metadata(
            relative_path=item.relative_path,
            ticker=item.ticker,
            document_type=item.document_type,
            doc_type_key=item.doc_type_key,
            period=item.period,
            content_hash=item.content_hash,
            cache_key=item.cache_key,
        ),
        "input_paths": list(item.input_paths),
        "row_counts": normalized_row_counts,
        "compile_complete": True,
        "generated_at": datetime.now(timezone.utc).isoformat(),
    }
    fragment_path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = fragment_path.parent / f".{fragment_path.name}.{os.getpid()}.{time.time_ns()}.tmp"
    _cleanup_sqlite_database_files(tmp_path)
    try:
        with sqlite3.connect(tmp_path) as conn:
            conn.execute("PRAGMA journal_mode=DELETE")
            _create_schema(conn)
            conn.execute(
                """
                CREATE TABLE IF NOT EXISTS fragment_metadata(
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                )
                """
            )
            conn.execute(
                "INSERT INTO fragment_metadata(key, value) VALUES('fragment', ?)",
                (json.dumps(metadata, ensure_ascii=False, sort_keys=True),),
            )
        os.replace(tmp_path, fragment_path)
    finally:
        _cleanup_sqlite_database_files(tmp_path)
    return metadata


def verify_index_fragment(
    fragment_path: Path,
    *,
    expected: Mapping[str, Any] | ArtifactPlanItem | None = None,
    trust_seal: bool = True,
    seal_on_success: bool = False,
) -> dict[str, Any]:
    """Verify a cached artifact fragment before treating it as a cache hit."""
    errors: list[str] = []
    metadata: dict[str, Any] = {}
    counts: dict[str, int] = {}
    integrity_check = None
    integrity_source = "none"
    expected_metadata = _coerce_expected_fragment_metadata(expected)
    expected_cache_key = expected_metadata.get("cache_key")
    seal, seal_status = read_immutable_sqlite_cache_seal(
        fragment_path,
        kind="artifact_fragment",
        cache_key=str(expected_cache_key) if expected_cache_key else None,
    )
    if not fragment_path.exists():
        return {
            "ok": False,
            "errors": ["fragment_missing"],
            "fragment_path": str(fragment_path),
            "integrity_check": None,
            "integrity_source": integrity_source,
            "seal_status": seal_status,
            "seal_trusted": False,
            "metadata": metadata,
            "counts": counts,
        }
    if trust_seal and seal_status == "valid":
        integrity_check = "ok"
        integrity_source = "immutable_cache_seal"
        raw_metadata = seal.get("metadata")
        raw_counts = seal.get("counts")
        metadata = dict(raw_metadata) if isinstance(raw_metadata, Mapping) else {}
        counts = {
            str(key): int(value)
            for key, value in dict(raw_counts or {}).items()
            if isinstance(value, int) and not isinstance(value, bool)
        }
    else:
        try:
            with sqlite3.connect(fragment_path) as conn:
                integrity_check = conn.execute("PRAGMA integrity_check").fetchone()[0]
                integrity_source = "sqlite_integrity_check"
                if integrity_check != "ok":
                    errors.append(f"integrity_check_failed:{integrity_check}")
                table_rows = conn.execute(
                    """
                    SELECT name
                    FROM sqlite_master
                    WHERE type IN ('table', 'view')
                    """
                ).fetchall()
                existing_tables = {str(row[0]) for row in table_rows}
                required_tables = {"fragment_metadata", *BASE_FRAGMENT_TABLES}
                for table_name in sorted(required_tables - existing_tables):
                    errors.append(f"table_missing:{table_name}")
                table_row = conn.execute(
                    """
                    SELECT name
                    FROM sqlite_master
                    WHERE type = 'table' AND name = 'fragment_metadata'
                    """
                ).fetchone()
                if table_row is None:
                    errors.append("table_missing:fragment_metadata")
                else:
                    metadata_row = conn.execute(
                        "SELECT value FROM fragment_metadata WHERE key = 'fragment'"
                    ).fetchone()
                    if metadata_row is None:
                        errors.append("metadata_fragment_missing")
                    else:
                        try:
                            metadata = json.loads(metadata_row[0])
                        except json.JSONDecodeError:
                            errors.append("metadata_fragment_invalid_json")
                for table_name in BASE_FRAGMENT_TABLES:
                    if table_name in existing_tables:
                        counts[table_name] = int(
                            conn.execute(f"SELECT COUNT(*) FROM {table_name}").fetchone()[0]
                        )
        except sqlite3.Error as exc:
            errors.append(f"sqlite_error:{exc}")

    if metadata:
        required_versions = {
            "fragment_cache_format_version": INDEX_FRAGMENT_CACHE_FORMAT_VERSION,
            "builder_code_version": AGENT_INDEX_BUILDER_VERSION,
            "source_artifact_sqlite_builder_version": AGENT_INDEX_BUILDER_VERSION,
            "agent_index_schema_version": AGENT_INDEX_SCHEMA_VERSION,
            "source_artifact_sqlite_schema_version": AGENT_INDEX_SCHEMA_VERSION,
            "ontology_schema_version": SCHEMA_VERSION,
            "retrieval_text_builder_version": RETRIEVAL_TEXT_BUILDER_VERSION,
            "company_topic_builder_version": COMPANY_TOPIC_BUILDER_VERSION,
            "typed_projection_builder_version": TYPED_PROJECTION_BUILDER_VERSION,
        }
        for key, value in required_versions.items():
            if metadata.get(key) != value:
                errors.append(f"metadata_mismatch:{key}")
        if metadata.get("compile_complete") is not True:
            errors.append("metadata_compile_incomplete")
        for key, value in expected_metadata.items():
            if metadata.get(key) != value:
                errors.append(f"metadata_mismatch:{key}")
        row_counts = metadata.get("row_counts")
        if not isinstance(row_counts, Mapping):
            errors.append("metadata_row_counts_missing")
        else:
            for table_name in BASE_FRAGMENT_TABLES:
                expected_count = int(row_counts.get(table_name) or 0)
                actual_count = int(counts.get(table_name) or 0)
                if expected_count != actual_count:
                    errors.append(f"row_count_mismatch:{table_name}")

    result = {
        "ok": not errors,
        "errors": errors,
        "fragment_path": str(fragment_path),
        "integrity_check": integrity_check,
        "integrity_source": integrity_source,
        "seal_status": seal_status,
        "seal_trusted": bool(trust_seal and seal_status == "valid"),
        "metadata": metadata,
        "counts": counts,
    }
    if result["ok"] and seal_on_success and seal_status != "valid":
        write_immutable_sqlite_cache_seal(
            fragment_path,
            kind="artifact_fragment",
            cache_key=str(metadata.get("cache_key") or expected_cache_key or ""),
            verification=result,
            metadata=metadata,
            counts=counts,
        )
        result["seal_status"] = "written"
    return result


def _coerce_expected_fragment_metadata(
    expected: Mapping[str, Any] | ArtifactPlanItem | None,
) -> dict[str, Any]:
    if expected is None:
        return {}
    if isinstance(expected, ArtifactPlanItem):
        return _expected_fragment_metadata(
            relative_path=expected.relative_path,
            ticker=expected.ticker,
            document_type=expected.document_type,
            doc_type_key=expected.doc_type_key,
            period=expected.period,
            content_hash=expected.content_hash,
            cache_key=expected.cache_key,
        )
    return dict(expected)


def inspect_index_fragment_cache(
    cache_root: Path,
    *,
    referenced_fragments: Iterable[Path] | None = None,
) -> dict[str, Any]:
    """Inspect fragment cache health without mutating cached artifacts."""
    resolved_cache_root = cache_root.expanduser().resolve()
    referenced_paths = _referenced_fragment_path_set(referenced_fragments)
    entries: list[IndexFragmentCacheEntry] = []
    total_counts = {table_name: 0 for table_name in BASE_FRAGMENT_TABLES}
    tickers: set[str] = set()
    valid_count = 0
    invalid_count = 0
    referenced_count = 0
    unreferenced_count = 0
    total_size_bytes = 0

    for fragment_path in _iter_fragment_cache_sqlite_files(resolved_cache_root):
        verification = verify_index_fragment(fragment_path)
        metadata = (
            verification.get("metadata")
            if isinstance(verification.get("metadata"), Mapping)
            else {}
        )
        counts = (
            verification.get("counts") if isinstance(verification.get("counts"), Mapping) else {}
        )
        referenced = (
            None if referenced_paths is None else str(fragment_path.resolve()) in referenced_paths
        )
        size_bytes = _sqlite_database_file_size(fragment_path)
        total_size_bytes += size_bytes
        if verification["ok"]:
            valid_count += 1
            for table_name in BASE_FRAGMENT_TABLES:
                total_counts[table_name] += int(counts.get(table_name) or 0)
            ticker = str(metadata.get("ticker") or "").strip().upper()
            if ticker:
                tickers.add(ticker)
        else:
            invalid_count += 1
        if referenced is True:
            referenced_count += 1
        elif referenced is False:
            unreferenced_count += 1
        entries.append(
            IndexFragmentCacheEntry(
                fragment_path=fragment_path,
                ok=bool(verification["ok"]),
                size_bytes=size_bytes,
                referenced=referenced,
                errors=tuple(str(error) for error in verification.get("errors") or ()),
                metadata=dict(metadata),
                counts={str(key): int(value) for key, value in counts.items()},
            )
        )

    return _json_safe(
        {
            "cache_root": resolved_cache_root,
            "fragment_count": len(entries),
            "valid_fragment_count": valid_count,
            "invalid_fragment_count": invalid_count,
            "referenced_fragment_count": None if referenced_paths is None else referenced_count,
            "unreferenced_fragment_count": None if referenced_paths is None else unreferenced_count,
            "referenced_plan_fragment_count": None
            if referenced_paths is None
            else len(referenced_paths),
            "total_size_bytes": total_size_bytes,
            "counts": total_counts,
            "tickers": sorted(tickers),
            "entries": [entry.to_dict() for entry in entries],
        }
    )


def gc_index_fragment_cache(
    cache_root: Path | None = None,
    *,
    root: Path | None = None,
    index_path: Path | None = None,
    layout: str = "monolith-and-shards",
    workers: int | None = None,
    dry_run: bool = True,
    include_invalid: bool = True,
    include_unreferenced: bool = True,
) -> dict[str, Any]:
    """Garbage-collect invalid or no-longer-referenced cached fragments.

    The collector only deletes SQLite fragment files located below cache_root.
    By default it performs a dry run; callers must pass dry_run=False to remove
    candidates.
    """
    if cache_root is None:
        if root is None:
            raise ValueError("cache_root is required when root is not provided")
        resolved_cache_root = _default_fragment_cache_root(root.expanduser().resolve())
    else:
        resolved_cache_root = cache_root.expanduser().resolve()
    plan = None
    referenced_fragments: tuple[Path, ...] | None = None
    if root is not None:
        plan = plan_agent_index(
            root.expanduser().resolve(),
            index_path=index_path,
            cache_root=resolved_cache_root,
            layout=layout,
            workers=workers,
            allow_internal_legacy_builder=True,
        )
        referenced_fragments = tuple(item.fragment_path for item in plan.items)

    status = inspect_index_fragment_cache(
        resolved_cache_root,
        referenced_fragments=referenced_fragments,
    )
    candidates: list[dict[str, Any]] = []
    for entry in status["entries"]:
        reason = None
        if include_invalid and not entry["ok"]:
            reason = "invalid"
        elif (
            include_unreferenced
            and referenced_fragments is not None
            and entry["ok"]
            and entry["referenced"] is False
        ):
            reason = "unreferenced"
        if reason is None:
            continue
        candidates.append(
            {
                "fragment_path": entry["fragment_path"],
                "reason": reason,
                "size_bytes": entry["size_bytes"],
                "errors": entry["errors"],
                "metadata": entry["metadata"],
            }
        )

    deleted: list[dict[str, Any]] = []
    deleted_bytes = 0
    if not dry_run:
        for candidate in candidates:
            fragment_path = Path(str(candidate["fragment_path"]))
            removed_bytes = _delete_fragment_cache_file(fragment_path, resolved_cache_root)
            deleted_bytes += removed_bytes
            deleted.append({**candidate, "deleted_bytes": removed_bytes})
        _prune_empty_cache_dirs(resolved_cache_root / "fragments", stop=resolved_cache_root)

    return _json_safe(
        {
            "cache_root": resolved_cache_root,
            "dry_run": dry_run,
            "root": None if plan is None else plan.root,
            "layout": None if plan is None else plan.layout,
            "fragment_count": status["fragment_count"],
            "valid_fragment_count": status["valid_fragment_count"],
            "invalid_fragment_count": status["invalid_fragment_count"],
            "referenced_fragment_count": status["referenced_fragment_count"],
            "unreferenced_fragment_count": status["unreferenced_fragment_count"],
            "candidate_count": len(candidates),
            "candidate_bytes": sum(int(candidate["size_bytes"]) for candidate in candidates),
            "deleted_count": len(deleted),
            "deleted_bytes": deleted_bytes,
            "candidates": candidates,
            "deleted": deleted,
        }
    )


def _referenced_fragment_path_set(referenced_fragments: Iterable[Path] | None) -> set[str] | None:
    if referenced_fragments is None:
        return None
    return {str(path.expanduser().resolve()) for path in referenced_fragments}


def _iter_fragment_cache_sqlite_files(cache_root: Path) -> list[Path]:
    if not cache_root.exists():
        return []
    search_root = cache_root / "fragments"
    if not search_root.exists():
        search_root = cache_root
    paths: list[Path] = []
    for path in search_root.rglob("*.sqlite"):
        try:
            resolved = path.expanduser().resolve()
        except OSError:
            continue
        if not resolved.is_file():
            continue
        if not _path_is_inside(resolved, cache_root):
            continue
        paths.append(resolved)
    return sorted(paths)


def _path_is_inside(path: Path, parent: Path) -> bool:
    try:
        path.relative_to(parent)
    except ValueError:
        return False
    return True


def _sqlite_database_file_size(index_path: Path) -> int:
    total = 0
    for path in (index_path, *_sqlite_sidecar_paths(index_path)):
        try:
            total += path.stat().st_size
        except FileNotFoundError:
            continue
    return total


def _delete_fragment_cache_file(fragment_path: Path, cache_root: Path) -> int:
    resolved_fragment_path = fragment_path.expanduser().resolve()
    resolved_cache_root = cache_root.expanduser().resolve()
    if resolved_fragment_path.suffix != ".sqlite":
        raise RuntimeError(f"refusing to delete non-sqlite cache file: {resolved_fragment_path}")
    if not _path_is_inside(resolved_fragment_path, resolved_cache_root):
        raise RuntimeError(
            f"refusing to delete cache file outside cache root: {resolved_fragment_path}"
        )
    removed_bytes = _sqlite_database_file_size(resolved_fragment_path)
    _cleanup_sqlite_database_files(resolved_fragment_path)
    return removed_bytes


def _prune_empty_cache_dirs(path: Path, *, stop: Path) -> None:
    if not path.exists() or not path.is_dir():
        return
    resolved_stop = stop.expanduser().resolve()
    for child in sorted(path.rglob("*"), reverse=True):
        if not child.is_dir():
            continue
        try:
            resolved_child = child.resolve()
        except OSError:
            continue
        if resolved_child == resolved_stop or not _path_is_inside(resolved_child, resolved_stop):
            continue
        try:
            child.rmdir()
        except OSError:
            pass


def compile_artifact_fragment(
    item: ArtifactPlanItem,
    *,
    root: Path,
    force: bool = False,
) -> FragmentCompileResult:
    """Compile one artifact into a cacheable SQLite fragment."""
    fragment_path = item.fragment_path
    existing_verification = verify_index_fragment(
        fragment_path,
        expected=item,
        seal_on_success=True,
    )
    if existing_verification["ok"] and not force:
        return FragmentCompileResult(
            item=item,
            fragment_path=fragment_path,
            stats=_fragment_stats_from_verification(existing_verification),
            cache_hit=True,
            verification=existing_verification,
        )

    fragment_path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = fragment_path.parent / f".{fragment_path.name}.{os.getpid()}.{time.time_ns()}.tmp"
    _cleanup_sqlite_database_files(tmp_path)
    try:
        with sqlite3.connect(tmp_path, timeout=60) as conn:
            conn.row_factory = sqlite3.Row
            conn.execute("PRAGMA journal_mode=DELETE")
            conn.execute("PRAGMA temp_store=MEMORY")
            _create_schema(conn)
            stats = _index_artifact(
                conn,
                root.expanduser().resolve(),
                item.artifact_index_path,
                replace_fts_entries=False,
            )
            row_counts = _base_fragment_table_counts(conn)
            _write_fragment_metadata_row(
                conn,
                item=item,
                row_counts=row_counts,
            )
            conn.execute("PRAGMA optimize")
        verification = verify_index_fragment(
            tmp_path,
            expected=item,
            trust_seal=False,
        )
        if not verification["ok"]:
            errors = ", ".join(verification["errors"])
            raise RuntimeError(f"compiled fragment failed verification: {errors}")
        _replace_sqlite_database(tmp_path, fragment_path)
        write_immutable_sqlite_cache_seal(
            fragment_path,
            kind="artifact_fragment",
            cache_key=item.cache_key,
            verification=verification,
            metadata=verification.get("metadata") or {},
            counts=verification.get("counts") or {},
            source_path=tmp_path,
        )
        final_verification = verify_index_fragment(fragment_path, expected=item)
        if not final_verification["ok"]:
            errors = ", ".join(final_verification["errors"])
            raise RuntimeError(f"stored fragment failed verification: {errors}")
        return FragmentCompileResult(
            item=item,
            fragment_path=fragment_path,
            stats=stats,
            cache_hit=False,
            verification=final_verification,
        )
    finally:
        _cleanup_sqlite_database_files(tmp_path)


def _compile_fragment_worker(args: tuple[ArtifactPlanItem, Path, bool]) -> FragmentCompileResult:
    item, root, force = args
    return compile_artifact_fragment(item, root=root, force=force)


def compile_artifact_fragments(
    items: Sequence[ArtifactPlanItem],
    *,
    root: Path,
    workers: int = 1,
    force: bool = False,
) -> list[FragmentCompileResult]:
    """Compile/cache artifact fragments, optionally in worker processes."""
    if not items:
        return []
    resolved_root = root.expanduser().resolve()
    worker_count = max(1, min(int(workers), len(items)))
    if worker_count <= 1 or len(items) <= 1:
        return [compile_artifact_fragment(item, root=resolved_root, force=force) for item in items]

    results_by_relative_path: dict[str, FragmentCompileResult] = {}
    submission_items = _fragment_compile_submission_order(items)
    try:
        with ProcessPoolExecutor(max_workers=worker_count) as executor:
            futures = {
                executor.submit(_compile_fragment_worker, (item, resolved_root, force)): item
                for item in submission_items
            }
            for future in as_completed(futures):
                item = futures[future]
                try:
                    result = future.result()
                except BrokenProcessPool:
                    raise
                except Exception as exc:
                    raise RuntimeError(
                        f"fragment compile failed for {item.relative_path}: {exc}"
                    ) from exc
                results_by_relative_path[item.relative_path] = result
    except BrokenProcessPool as exc:
        retry_workers = max(1, worker_count // 2)
        remaining_items = [
            item for item in items if item.relative_path not in results_by_relative_path
        ]
        logger.warning(
            "fragment worker pool failed; retrying %d unfinished artifact(s) with %d worker(s): %s",
            len(remaining_items),
            retry_workers,
            exc,
        )
        retried = compile_artifact_fragments(
            remaining_items,
            root=resolved_root,
            workers=retry_workers,
            force=force,
        )
        results_by_relative_path.update({result.item.relative_path: result for result in retried})
        return [results_by_relative_path[item.relative_path] for item in items]

    return [results_by_relative_path[item.relative_path] for item in items]


def _fragment_compile_submission_order(items: Sequence[ArtifactPlanItem]) -> list[ArtifactPlanItem]:
    """Dispatch large artifacts first while preserving deterministic tie breaks."""
    return sorted(items, key=lambda item: (-item.estimated_bytes, item.relative_path))


def _write_fragment_metadata_row(
    conn: sqlite3.Connection,
    *,
    item: ArtifactPlanItem,
    row_counts: Mapping[str, int],
) -> dict[str, Any]:
    metadata = {
        **_expected_fragment_metadata(
            relative_path=item.relative_path,
            ticker=item.ticker,
            document_type=item.document_type,
            doc_type_key=item.doc_type_key,
            period=item.period,
            content_hash=item.content_hash,
            cache_key=item.cache_key,
        ),
        "input_paths": list(item.input_paths),
        "row_counts": {
            table_name: int(row_counts.get(table_name) or 0) for table_name in BASE_FRAGMENT_TABLES
        },
        "compile_complete": True,
        "generated_at": datetime.now(timezone.utc).isoformat(),
    }
    conn.execute(
        """
        CREATE TABLE IF NOT EXISTS fragment_metadata(
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        )
        """
    )
    conn.execute(
        "INSERT OR REPLACE INTO fragment_metadata(key, value) VALUES('fragment', ?)",
        (json.dumps(metadata, ensure_ascii=False, sort_keys=True),),
    )
    return metadata


def _base_fragment_table_counts(conn: sqlite3.Connection) -> dict[str, int]:
    return {
        table_name: int(conn.execute(f"SELECT COUNT(*) FROM {table_name}").fetchone()[0])
        for table_name in BASE_FRAGMENT_TABLES
    }


def _fragment_stats_from_verification(verification: Mapping[str, Any]) -> dict[str, int]:
    counts = verification.get("counts") if isinstance(verification.get("counts"), Mapping) else {}
    return {
        "documents": int(counts.get("documents") or 0),
        "objects": int(counts.get("objects") or 0),
        "edges": int(counts.get("edges") or 0),
        "quality_events": int(counts.get("quality_events") or 0),
    }


def merge_fragments(
    conn: sqlite3.Connection,
    fragments: Sequence[Path],
) -> dict[str, int]:
    """Merge base rows from verified SQLite fragments into the open index DB."""
    stats = {"documents": 0, "objects": 0, "edges": 0, "quality_events": 0}
    for fragment_number, fragment_path in enumerate(fragments, start=1):
        schema_name = f"frag_{fragment_number}"
        verification = verify_index_fragment(fragment_path)
        if not verification["ok"]:
            errors = ", ".join(verification["errors"])
            raise RuntimeError(f"fragment verify failed before merge: {fragment_path}: {errors}")
        conn.execute(f"ATTACH DATABASE ? AS {schema_name}", (str(fragment_path),))
        try:
            with conn:
                for table_name in BASE_FRAGMENT_TABLES:
                    _merge_fragment_table(conn, schema_name, table_name)
            fragment_stats = _fragment_stats_from_verification(verification)
            for key, value in fragment_stats.items():
                stats[key] += value
        finally:
            conn.execute(f"DETACH DATABASE {schema_name}")
    return stats


def _merge_fragment_table(conn: sqlite3.Connection, schema_name: str, table_name: str) -> None:
    columns = _table_columns(conn, "main", table_name)
    if not columns:
        return
    column_sql = ", ".join(columns)
    if table_name == "object_fts":
        conn.execute(
            f"""
            INSERT INTO {table_name}({column_sql})
            SELECT {column_sql}
            FROM {schema_name}.{table_name}
            """
        )
    else:
        conn.execute(
            f"""
            INSERT OR REPLACE INTO {table_name}({column_sql})
            SELECT {column_sql}
            FROM {schema_name}.{table_name}
            """
        )


def _table_columns(conn: sqlite3.Connection, schema_name: str, table_name: str) -> list[str]:
    rows = conn.execute(f"PRAGMA {schema_name}.table_info({table_name})").fetchall()
    return [str(row[1]) for row in rows]


def _estimated_artifact_rows(artifact_index: Mapping[str, Any]) -> int | None:
    counts = artifact_index.get("counts")
    if not isinstance(counts, Mapping):
        return None
    total = 1
    found_numeric = False
    for value in counts.values():
        if isinstance(value, bool):
            continue
        if isinstance(value, (int, float)):
            total += int(value)
            found_numeric = True
    return total if found_numeric else None


def _path_label(root: Path, path: Path) -> str:
    try:
        return path.expanduser().resolve().relative_to(root).as_posix()
    except ValueError:
        return str(path.expanduser().resolve())


def _plan_summary(plan: IndexBuildPlan) -> dict[str, Any]:
    return {
        "plan_format_version": plan.plan_format_version,
        "cache_root": str(plan.cache_root),
        "discovery_mode": plan.discovery_mode,
        "source_manifest_path": str(plan.source_manifest_path)
        if plan.source_manifest_path
        else None,
        "source_manifest_hash": plan.source_manifest_hash,
        "artifact_count": len(plan.items),
        "dirty_artifact_count": len(plan.dirty_items),
        "cached_artifact_count": len(plan.cached_items),
        "company_count": len(plan.company_items),
        "dirty_company_count": len(plan.dirty_company_items),
        "cached_company_count": len(plan.cached_company_items),
        "dirty_tickers": list(plan.dirty_tickers),
        "workers": plan.workers,
        "layout": plan.layout,
    }


def _build_agent_index_direct(
    root: Path,
    *,
    index_path: Path,
    published_index_path: Path | None = None,
    force: bool = True,
    plan: IndexBuildPlan | None = None,
) -> dict[str, Any]:
    """Build a global SQLite agent index at a specific database path."""
    global _ACTIVE_BUILD_PROGRESS_LOGGER
    root = root.resolve()
    index_path = index_path.resolve()
    published_index_path = (published_index_path or index_path).resolve()
    plan = plan or plan_agent_index(
        root,
        index_path=published_index_path,
        allow_internal_legacy_builder=True,
    )
    if force and index_path.exists():
        index_path.unlink()
    index_path.parent.mkdir(parents=True, exist_ok=True)
    build_settings = dict(plan.build_settings)
    _apply_build_resource_settings(build_settings)

    artifact_items = plan.items
    artifact_indexes = [item.artifact_index_path for item in artifact_items]
    progress_log_path = str(build_settings.get("progress_log_path") or "").strip()
    previous_progress_logger = _ACTIVE_BUILD_PROGRESS_LOGGER
    _ACTIVE_BUILD_PROGRESS_LOGGER = (
        _BuildProgressLogger(
            path=Path(progress_log_path), index_path=index_path, settings=build_settings
        )
        if progress_log_path
        else None
    )
    conn = sqlite3.connect(index_path, timeout=60)
    build_started_at = time.perf_counter()
    try:
        conn.row_factory = sqlite3.Row
        _configure_connection(conn, build_settings)
        _create_schema(conn)
        _log_build_phase(
            "start",
            root=str(root),
            index_path=str(index_path),
            force=force,
            artifact_indexes=len(artifact_indexes),
            build_settings=build_settings,
        )
        totals = {
            "documents": 0,
            "objects": 0,
            "edges": 0,
            "quality_events": 0,
            "object_traceability": 0,
            "metric_lookup": 0,
            "metric_dimension_lookup": 0,
            "company_dimension_catalog": 0,
            "exposure_lookup": 0,
            "agreement_lookup": 0,
            "event_lookup": 0,
            "factor_lookup": 0,
            "company_topics": 0,
        }

        compile_started_at = time.perf_counter()
        _log_build_phase(
            "compile_fragments_start",
            artifact_indexes=len(artifact_items),
            workers=plan.workers,
            cache_root=str(plan.cache_root),
        )
        fragment_results = compile_artifact_fragments(
            artifact_items,
            root=root,
            workers=plan.workers,
        )
        fragment_cache_hits = sum(1 for result in fragment_results if result.cache_hit)
        fragment_cache_misses = len(fragment_results) - fragment_cache_hits
        _log_build_phase(
            "compile_fragments_done",
            elapsed_seconds=_elapsed(compile_started_at),
            artifact_indexes=len(artifact_items),
            workers=plan.workers,
            cache_hits=fragment_cache_hits,
            cache_misses=fragment_cache_misses,
        )

        for artifact_number, (item, fragment_result) in enumerate(
            zip(artifact_items, fragment_results, strict=True),
            start=1,
        ):
            artifact_started_at = time.perf_counter()
            _log_build_phase(
                "merge_fragment_start",
                artifact_number=artifact_number,
                artifact_indexes=len(artifact_items),
                artifact_index=str(item.artifact_index_path),
                fragment_path=str(item.fragment_path),
                cache_hit=fragment_result.cache_hit,
            )
            stats = merge_fragments(conn, [fragment_result.fragment_path])
            for key, value in stats.items():
                totals[key] += value
            _checkpoint_wal(conn, settings=build_settings, artifact_number=artifact_number)
            _log_build_phase(
                "merge_fragment_done",
                artifact_number=artifact_number,
                artifact_indexes=len(artifact_items),
                artifact_index=str(item.artifact_index_path),
                fragment_path=str(fragment_result.fragment_path),
                cache_hit=fragment_result.cache_hit,
                elapsed_seconds=_elapsed(artifact_started_at),
                stats=stats,
                totals=totals,
            )

        secondary_started_at = time.perf_counter()
        _log_build_phase("create_base_secondary_indexes_start", totals=totals)
        with conn:
            _create_base_secondary_indexes(conn)
        _checkpoint_wal(conn)
        _log_build_phase(
            "create_base_secondary_indexes_done",
            elapsed_seconds=_elapsed(secondary_started_at),
            totals=totals,
        )

        trace_started_at = time.perf_counter()
        _log_build_phase("object_traceability_start", objects=totals["objects"])
        with conn:
            totals["object_traceability"] = _rebuild_object_traceability(conn)
        _checkpoint_wal(conn)
        _log_build_phase(
            "object_traceability_done",
            elapsed_seconds=_elapsed(trace_started_at),
            object_traceability=totals["object_traceability"],
        )

        metric_started_at = time.perf_counter()
        _log_build_phase("metric_lookup_start", objects=totals["objects"])
        with conn:
            totals["metric_lookup"] = _rebuild_metric_lookup(conn)
            totals["metric_dimension_lookup"] = _rebuild_metric_dimension_lookup(conn)
            totals["company_dimension_catalog"] = _rebuild_company_dimension_catalog(conn)
        _checkpoint_wal(conn)
        _log_build_phase(
            "metric_lookup_done",
            elapsed_seconds=_elapsed(metric_started_at),
            metric_lookup=totals["metric_lookup"],
            metric_dimension_lookup=totals["metric_dimension_lookup"],
            company_dimension_catalog=totals["company_dimension_catalog"],
        )

        projection_started_at = time.perf_counter()
        _log_build_phase("typed_projection_lookup_start", objects=totals["objects"])
        with conn:
            for table_name in TYPED_PROJECTION_TABLES:
                totals[table_name] = _rebuild_typed_projection_lookup(conn, table_name)
        _checkpoint_wal(conn)
        _log_build_phase(
            "typed_projection_lookup_done",
            elapsed_seconds=_elapsed(projection_started_at),
            exposure_lookup=totals["exposure_lookup"],
            agreement_lookup=totals["agreement_lookup"],
            event_lookup=totals["event_lookup"],
            factor_lookup=totals["factor_lookup"],
        )

        topic_started_at = time.perf_counter()
        _log_build_phase("company_topic_index_start", objects=totals["objects"])
        totals["company_topics"] = _rebuild_company_topic_index(conn)
        _checkpoint_wal(conn)
        _log_build_phase(
            "company_topic_index_done",
            elapsed_seconds=_elapsed(topic_started_at),
            company_topics=totals["company_topics"],
        )

        finalize_started_at = time.perf_counter()
        _log_build_phase("finalize_start", totals=totals)
        with conn:
            _create_serving_secondary_indexes(conn)
            conn.execute(
                """
                INSERT OR REPLACE INTO metadata(key, value)
                VALUES (?, ?)
                """,
                (
                    "build",
                    json.dumps(
                        {
                            "schema_version": AGENT_INDEX_SCHEMA_VERSION,
                            "agent_index_schema_version": AGENT_INDEX_SCHEMA_VERSION,
                            "source_artifact_sqlite_schema_version": AGENT_INDEX_SCHEMA_VERSION,
                            "builder_code_version": AGENT_INDEX_BUILDER_VERSION,
                            "source_artifact_sqlite_builder_version": AGENT_INDEX_BUILDER_VERSION,
                            "ontology_schema_version": SCHEMA_VERSION,
                            "ontology_registry_version": _registry_version(conn),
                            "retrieval_text_builder_version": RETRIEVAL_TEXT_BUILDER_VERSION,
                            "root": str(root),
                            "artifact_root": str(root),
                            "artifact_manifest_hash": _artifact_manifest_hash(artifact_indexes),
                            "source_manifest_path": str(plan.source_manifest_path)
                            if plan.source_manifest_path
                            else None,
                            "source_manifest_hash": plan.source_manifest_hash,
                            "discovery_mode": plan.discovery_mode,
                            "generated_at": datetime.now(timezone.utc).isoformat(),
                            "artifact_indexes": len(artifact_indexes),
                            "index_build_plan_format_version": plan.plan_format_version,
                            "index_layout_version": INDEX_LAYOUT_VERSION,
                            "index_layout": plan.layout,
                            "company_shards_enabled": _layout_builds_shards(plan.layout),
                            "fragment_cache_format_version": INDEX_FRAGMENT_CACHE_FORMAT_VERSION,
                            "fragment_cache_root": str(plan.cache_root),
                            "fragment_compile_workers": plan.workers,
                            "fragment_cache_hits": fragment_cache_hits,
                            "fragment_cache_misses": fragment_cache_misses,
                            "company_cache_format_version": INDEX_COMPANY_CACHE_FORMAT_VERSION,
                            "company_cache_hits": len(plan.cached_company_items),
                            "company_cache_misses": len(plan.dirty_company_items),
                            "company_topic_builder_version": COMPANY_TOPIC_BUILDER_VERSION,
                            "company_topic_profile_mode": "rich_materialized",
                            "object_search_text_enabled": True,
                            "metric_lookup_enabled": True,
                            "metric_dictionary": metric_dictionary_binding(),
                            "metric_dimension_lookup_enabled": True,
                            "company_dimension_catalog_enabled": True,
                            "typed_projection_builder_version": TYPED_PROJECTION_BUILDER_VERSION,
                            "typed_projection_lookup_enabled": True,
                            "company_topic_fts_enabled": True,
                            "build_settings": build_settings,
                            "totals": totals,
                        },
                        ensure_ascii=False,
                    ),
                ),
            )
        conn.execute("PRAGMA optimize")
        _checkpoint_wal(conn, truncate=True)
        _log_build_phase(
            "finalize_done",
            elapsed_seconds=_elapsed(finalize_started_at),
            total_elapsed_seconds=_elapsed(build_started_at),
            totals=totals,
        )

        logger.info(
            "source_artifact_sqlite: indexed %d documents, %d objects, %d edges",
            totals["documents"],
            totals["objects"],
            totals["edges"],
            extra={"stage": SOURCE_ARTIFACT_SQLITE_BUILD_STAGE},
        )
        return {
            "index_path": index_path,
            "root": root,
            "artifact_indexes": len(artifact_indexes),
            "totals": totals,
            "build_settings": build_settings,
            "fragment_cache": {
                "hits": fragment_cache_hits,
                "misses": fragment_cache_misses,
                "cache_root": str(plan.cache_root),
                "workers": plan.workers,
            },
            "company_cache": {
                "format": INDEX_COMPANY_CACHE_FORMAT_VERSION,
                "hits": len(plan.cached_company_items),
                "misses": len(plan.dirty_company_items),
                "cache_root": str(plan.cache_root),
            },
        }
    finally:
        conn.close()
        _ACTIVE_BUILD_PROGRESS_LOGGER = previous_progress_logger


def _plan_artifact_index_inputs(
    root: Path,
    *,
    index_path: Path,
    cache_root: Path | None = None,
    workers: int | None = None,
    source_manifest_path: Path | None = None,
    source_manifest_payload: Mapping[str, Any] | None = None,
) -> IndexBuildPlan:
    """Plan source artifact inputs for v3 shard materialization."""
    return plan_agent_index(
        root,
        index_path=index_path,
        cache_root=cache_root,
        workers=workers,
        layout="shards",
        source_manifest_path=source_manifest_path,
        source_manifest_payload=source_manifest_payload,
        allow_internal_legacy_builder=True,
    )


def _build_artifact_index_sqlite(
    root: Path,
    *,
    index_path: Path,
    published_index_path: Path,
    plan: IndexBuildPlan,
) -> dict[str, Any]:
    """Materialize one SQLite database from a prepared artifact input plan."""
    return _build_agent_index_direct(
        root,
        index_path=index_path,
        published_index_path=published_index_path,
        force=True,
        plan=plan,
    )


def discover_artifact_indexes(root: Path) -> list[Path]:
    """Return artifact indexes known through company indexes or direct glob."""
    found: dict[Path, Path] = {}

    for company_index_path in sorted(root.glob("companies/*/indexes/company_artifact_index.json")):
        try:
            company_index = json.loads(company_index_path.read_text())
        except (json.JSONDecodeError, OSError):
            continue
        for doc in company_index.get("documents", {}).values():
            rel = doc.get("artifact_index")
            if not rel:
                continue
            path = (root / rel).resolve()
            if path.exists():
                found[path] = path

    for path in sorted(root.glob("companies/*/ontology/*/*/artifact_index.json")):
        found[path.resolve()] = path.resolve()

    for path in sorted(root.glob("companies/*/context/artifact_index.json")):
        found[path.resolve()] = path.resolve()

    return sorted(found)


def _configure_connection(
    conn: sqlite3.Connection, settings: Mapping[str, Any] | None = None
) -> None:
    settings = dict(settings or _build_resource_settings())
    conn.execute("PRAGMA journal_mode=WAL")
    conn.execute(f"PRAGMA synchronous={settings['sqlite_synchronous']}")
    conn.execute("PRAGMA temp_store=MEMORY")
    conn.execute(f"PRAGMA cache_size=-{int(settings['sqlite_cache_size_kib'])}")
    conn.execute(f"PRAGMA mmap_size={int(settings['sqlite_mmap_size_bytes'])}")
    conn.execute(f"PRAGMA wal_autocheckpoint={int(settings['sqlite_wal_autocheckpoint'])}")


def _checkpoint_wal(
    conn: sqlite3.Connection,
    *,
    truncate: bool = False,
    settings: Mapping[str, Any] | None = None,
    artifact_number: int | None = None,
) -> None:
    if artifact_number is not None and settings is not None:
        checkpoint_every = int(settings.get("checkpoint_every_artifacts") or 0)
        if checkpoint_every <= 0 or artifact_number % checkpoint_every != 0:
            return
    mode = "TRUNCATE" if truncate else "PASSIVE"
    try:
        conn.execute(f"PRAGMA wal_checkpoint({mode})").fetchall()
    except sqlite3.OperationalError as exc:
        logger.debug(
            "source_artifact_sqlite: WAL checkpoint skipped: %s",
            exc,
            extra={"stage": SOURCE_ARTIFACT_SQLITE_BUILD_STAGE},
        )


def _create_schema(conn: sqlite3.Connection) -> None:
    conn.executescript(
        """
        CREATE TABLE IF NOT EXISTS metadata (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS documents (
            ticker TEXT NOT NULL,
            document_type TEXT NOT NULL,
            doc_type_key TEXT NOT NULL,
            period TEXT NOT NULL,
            artifact_index_path TEXT NOT NULL,
            ontology_dir TEXT NOT NULL,
            sources_json TEXT NOT NULL,
            reports_json TEXT NOT NULL,
            counts_json TEXT NOT NULL,
            section_quality_status TEXT,
            section_quality_json TEXT NOT NULL,
            generated_at TEXT,
            schema_version TEXT,
            PRIMARY KEY (ticker, doc_type_key, period)
        );

        CREATE TABLE IF NOT EXISTS objects (
            id TEXT PRIMARY KEY,
            type TEXT NOT NULL,
            ticker TEXT NOT NULL,
            document_type TEXT NOT NULL,
            doc_type_key TEXT NOT NULL,
            period TEXT NOT NULL,
            source_document_id TEXT,
            section_name TEXT,
            metric_name TEXT,
            review_status TEXT,
            confidence TEXT,
            text TEXT NOT NULL,
            json TEXT NOT NULL,
            artifact_key TEXT NOT NULL,
            artifact_path TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS edges (
            id TEXT PRIMARY KEY,
            ticker TEXT NOT NULL,
            document_type TEXT NOT NULL,
            doc_type_key TEXT NOT NULL,
            period TEXT NOT NULL,
            source_document_id TEXT,
            from_id TEXT NOT NULL,
            to_id TEXT NOT NULL,
            relation_id TEXT NOT NULL,
            relation_name TEXT NOT NULL,
            confidence TEXT,
            review_status TEXT,
            json TEXT NOT NULL,
            artifact_path TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS quality_events (
            id TEXT PRIMARY KEY,
            ticker TEXT NOT NULL,
            document_type TEXT NOT NULL,
            doc_type_key TEXT NOT NULL,
            period TEXT NOT NULL,
            severity TEXT NOT NULL,
            category TEXT NOT NULL,
            object_id TEXT,
            stage TEXT,
            message TEXT NOT NULL,
            json TEXT NOT NULL
        );

        DROP TABLE IF EXISTS object_fts;
        DROP TABLE IF EXISTS object_text;
        DROP TABLE IF EXISTS object_search_text;
        DROP TABLE IF EXISTS object_traceability;
        DROP TABLE IF EXISTS metric_lookup;
        DROP TABLE IF EXISTS metric_dimension_lookup;
        DROP TABLE IF EXISTS company_dimension_catalog;
        DROP TABLE IF EXISTS exposure_lookup;
        DROP TABLE IF EXISTS agreement_lookup;
        DROP TABLE IF EXISTS event_lookup;
        DROP TABLE IF EXISTS factor_lookup;
        DROP TABLE IF EXISTS company_topic_source_objects;
        DROP TABLE IF EXISTS company_topic_fts;
        DROP TABLE IF EXISTS company_topic_index;

        CREATE VIRTUAL TABLE object_fts USING fts5(
            object_id UNINDEXED,
            type UNINDEXED,
            ticker UNINDEXED,
            document_type UNINDEXED,
            period UNINDEXED,
            text_self,
            text_support,
            text_related,
            text_entities,
            text_aliases,
            compact_text,
            tokenize = 'unicode61'
        );

        CREATE TABLE IF NOT EXISTS object_search_text (
            object_id TEXT PRIMARY KEY,
            type TEXT NOT NULL,
            ticker TEXT NOT NULL,
            document_type TEXT NOT NULL,
            period TEXT NOT NULL,
            text_self TEXT,
            text_support TEXT,
            text_related TEXT,
            text_entities TEXT,
            text_aliases TEXT,
            compact_text TEXT
        );

        CREATE TABLE IF NOT EXISTS object_text (
            object_id TEXT PRIMARY KEY,
            type TEXT NOT NULL,
            ticker TEXT NOT NULL,
            document_type TEXT NOT NULL,
            period TEXT NOT NULL,
            text_self TEXT,
            text_support TEXT,
            text_related TEXT,
            text_entities TEXT,
            text_aliases TEXT,
            compact_text TEXT
        );

        CREATE TABLE IF NOT EXISTS object_traceability (
            object_id TEXT PRIMARY KEY,
            object_type TEXT NOT NULL,
            ticker TEXT NOT NULL,
            document_type TEXT NOT NULL,
            period TEXT NOT NULL,
            trace_status TEXT NOT NULL,
            evidence_chain_count INTEGER DEFAULT 0,
            support_depth INTEGER,
            support_quote_count INTEGER DEFAULT 0,
            support_claim_count INTEGER DEFAULT 0,
            support_link_count INTEGER DEFAULT 0,
            trace_method TEXT,
            metric_lineage_status TEXT,
            answer_candidate INTEGER DEFAULT 0,
            json TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS metric_lookup (
            object_id TEXT PRIMARY KEY,
            object_type TEXT NOT NULL,
            ticker TEXT NOT NULL,
            document_type TEXT NOT NULL,
            doc_type_key TEXT NOT NULL,
            period TEXT NOT NULL,
            filing_period TEXT NOT NULL,
            observation_period TEXT NOT NULL,
            observation_period_type TEXT NOT NULL,
            observation_start_date TEXT,
            observation_end_date TEXT,
            observation_context_key TEXT NOT NULL,
            fiscal_year INTEGER,
            fiscal_quarter INTEGER,
            metric_name TEXT,
            canonical_metric TEXT,
            metric_alias_text TEXT,
            value_text TEXT,
            value_numeric REAL,
            unit TEXT,
            dimensions_json TEXT NOT NULL,
            is_company_total INTEGER DEFAULT 0,
            segment_name TEXT,
            product_name TEXT,
            geography_name TEXT,
            trace_status TEXT,
            metric_lineage_status TEXT,
            text TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS metric_dimension_lookup (
            object_id TEXT NOT NULL,
            ticker TEXT NOT NULL,
            period TEXT NOT NULL,
            filing_period TEXT NOT NULL,
            observation_period TEXT NOT NULL,
            fiscal_year INTEGER,
            fiscal_quarter INTEGER,
            canonical_metric TEXT NOT NULL,
            dimension_kind TEXT,
            dimension_key TEXT NOT NULL,
            dimension_label TEXT NOT NULL,
            axis_key TEXT,
            member_key TEXT,
            is_company_total INTEGER DEFAULT 0,
            trace_status TEXT,
            metric_lineage_status TEXT,
            PRIMARY KEY (object_id, dimension_key)
        );

        CREATE TABLE IF NOT EXISTS company_dimension_catalog (
            ticker TEXT NOT NULL,
            dimension_key TEXT NOT NULL,
            dimension_label TEXT NOT NULL,
            dimension_kind TEXT,
            source_metric_count INTEGER DEFAULT 0,
            periods_json TEXT NOT NULL,
            aliases_text TEXT,
            PRIMARY KEY (ticker, dimension_key)
        );

        CREATE TABLE IF NOT EXISTS exposure_lookup (
            object_id TEXT PRIMARY KEY,
            object_type TEXT NOT NULL,
            ticker TEXT NOT NULL,
            document_type TEXT NOT NULL,
            doc_type_key TEXT NOT NULL,
            period TEXT NOT NULL,
            factor TEXT,
            benchmark TEXT,
            impact_channel TEXT,
            mechanism TEXT,
            scenario_terms TEXT,
            direct_anchor_text TEXT,
            related_context_text TEXT,
            negative_guard_terms TEXT,
            trace_status TEXT,
            evidence_strength TEXT,
            specificity_score REAL,
            generic_score REAL,
            boilerplate_score REAL,
            support_quote_count INTEGER DEFAULT 0,
            support_claim_count INTEGER DEFAULT 0,
            evidence_chain_count INTEGER DEFAULT 0,
            lookup_text TEXT NOT NULL,
            source_text TEXT
        );

        CREATE TABLE IF NOT EXISTS agreement_lookup (
            object_id TEXT PRIMARY KEY,
            object_type TEXT NOT NULL,
            ticker TEXT NOT NULL,
            document_type TEXT NOT NULL,
            doc_type_key TEXT NOT NULL,
            period TEXT NOT NULL,
            agreement_type TEXT,
            agreement_subtype TEXT,
            counterparty TEXT,
            amount TEXT,
            maturity_date TEXT,
            termination_terms TEXT,
            covenant_terms TEXT,
            collateral_terms TEXT,
            affected_channels TEXT,
            trace_status TEXT,
            evidence_strength TEXT,
            specificity_score REAL,
            generic_score REAL,
            boilerplate_score REAL,
            support_quote_count INTEGER DEFAULT 0,
            support_claim_count INTEGER DEFAULT 0,
            evidence_chain_count INTEGER DEFAULT 0,
            lookup_text TEXT NOT NULL,
            source_text TEXT
        );

        CREATE TABLE IF NOT EXISTS event_lookup (
            object_id TEXT PRIMARY KEY,
            object_type TEXT NOT NULL,
            ticker TEXT NOT NULL,
            document_type TEXT NOT NULL,
            doc_type_key TEXT NOT NULL,
            period TEXT NOT NULL,
            event_type TEXT,
            event_status TEXT,
            event_date TEXT,
            date_expression TEXT,
            date_sort_key TEXT,
            project_or_product TEXT,
            regulatory_body TEXT,
            affected_channels TEXT,
            trace_status TEXT,
            evidence_strength TEXT,
            specificity_score REAL,
            generic_score REAL,
            boilerplate_score REAL,
            support_quote_count INTEGER DEFAULT 0,
            support_claim_count INTEGER DEFAULT 0,
            evidence_chain_count INTEGER DEFAULT 0,
            lookup_text TEXT NOT NULL,
            source_text TEXT
        );

        CREATE TABLE IF NOT EXISTS factor_lookup (
            object_id TEXT PRIMARY KEY,
            object_type TEXT NOT NULL,
            ticker TEXT NOT NULL,
            document_type TEXT NOT NULL,
            doc_type_key TEXT NOT NULL,
            period TEXT NOT NULL,
            factor_type TEXT,
            topic_family TEXT,
            impact_channel TEXT,
            business_area TEXT,
            risk_or_driver TEXT,
            topic_label TEXT,
            topic_summary TEXT,
            trace_status TEXT,
            evidence_strength TEXT,
            specificity_score REAL,
            generic_score REAL,
            boilerplate_score REAL,
            support_quote_count INTEGER DEFAULT 0,
            support_claim_count INTEGER DEFAULT 0,
            evidence_chain_count INTEGER DEFAULT 0,
            lookup_text TEXT NOT NULL,
            source_text TEXT
        );

        CREATE TABLE IF NOT EXISTS company_topic_index (
            topic_id TEXT PRIMARY KEY,
            ticker TEXT NOT NULL,
            period TEXT,
            document_type TEXT,
            doc_type_key TEXT,
            filing_type TEXT,
            topic_label TEXT,
            topic_summary TEXT,
            topic_type TEXT,
            topic_family TEXT,
            topic_text TEXT,
            facet_text TEXT,
            primary_object_id TEXT NOT NULL,
            primary_object_type TEXT NOT NULL,
            source_object_ids TEXT NOT NULL,
            top_traceable_object_ids TEXT NOT NULL,
            untraced_object_ids TEXT NOT NULL,
            dominant_object_types TEXT NOT NULL,
            impact_channels TEXT NOT NULL,
            factor_terms TEXT NOT NULL,
            metric_terms TEXT NOT NULL,
            entity_terms TEXT NOT NULL,
            mechanism_terms TEXT NOT NULL,
            scenario_terms TEXT NOT NULL,
            evidence_strength TEXT,
            materiality_hint TEXT,
            materiality_score REAL,
            specificity_score REAL,
            generic_score REAL,
            boilerplate_score REAL,
            support_quote_count INTEGER DEFAULT 0,
            support_claim_count INTEGER DEFAULT 0,
            support_metric_count INTEGER DEFAULT 0,
            trace_status TEXT DEFAULT 'unknown',
            evidence_chain_count INTEGER DEFAULT 0,
            support_depth INTEGER,
            support_link_count INTEGER DEFAULT 0,
            trace_method TEXT,
            metric_lineage_status TEXT,
            answer_candidate INTEGER DEFAULT 0,
            created_from TEXT,
            builder_version TEXT
        );

        CREATE TABLE IF NOT EXISTS company_topic_source_objects (
            topic_id TEXT NOT NULL,
            object_id TEXT NOT NULL,
            object_type TEXT NOT NULL,
            role TEXT,
            rank INTEGER,
            trace_status TEXT,
            evidence_chain_count INTEGER DEFAULT 0,
            PRIMARY KEY (topic_id, object_id)
        );

        CREATE VIRTUAL TABLE company_topic_fts USING fts5(
            topic_id UNINDEXED,
            ticker UNINDEXED,
            period UNINDEXED,
            text_label,
            text_summary,
            text_evidence,
            text_entities,
            text_channels,
            text_aliases,
            compact_text,
            tokenize = 'unicode61'
        );
        """
    )


def _create_base_secondary_indexes(conn: sqlite3.Connection) -> None:
    conn.executescript(
        """
        CREATE INDEX IF NOT EXISTS idx_objects_scope
            ON objects(ticker, doc_type_key, period, type);
        CREATE INDEX IF NOT EXISTS idx_objects_metric
            ON objects(metric_name, ticker, doc_type_key, period);
        CREATE INDEX IF NOT EXISTS idx_objects_ticker_type_period
            ON objects(ticker, type, period);
        CREATE INDEX IF NOT EXISTS idx_objects_ticker_metric_period
            ON objects(ticker, metric_name, period);
        CREATE INDEX IF NOT EXISTS idx_edges_from
            ON edges(from_id, relation_id);
        CREATE INDEX IF NOT EXISTS idx_edges_to
            ON edges(to_id, relation_id);
        CREATE INDEX IF NOT EXISTS idx_quality_scope
            ON quality_events(ticker, doc_type_key, period, category);
        CREATE INDEX IF NOT EXISTS idx_object_search_text_ticker
            ON object_search_text(ticker);
        CREATE INDEX IF NOT EXISTS idx_object_search_text_type
            ON object_search_text(type);
        CREATE INDEX IF NOT EXISTS idx_object_search_text_ticker_type
            ON object_search_text(ticker, type);
        """
    )


def _create_serving_secondary_indexes(conn: sqlite3.Connection) -> None:
    conn.executescript(
        """
        CREATE INDEX IF NOT EXISTS idx_object_traceability_status
            ON object_traceability(trace_status, object_type);
        CREATE INDEX IF NOT EXISTS idx_object_traceability_ticker_type_status
            ON object_traceability(ticker, object_type, trace_status);
        CREATE INDEX IF NOT EXISTS idx_metric_lookup_ticker_period_metric
            ON metric_lookup(ticker, observation_period, canonical_metric);
        CREATE INDEX IF NOT EXISTS idx_metric_lookup_ticker_filing_period
            ON metric_lookup(ticker, filing_period, canonical_metric);
        CREATE INDEX IF NOT EXISTS idx_metric_lookup_ticker_year_metric
            ON metric_lookup(ticker, fiscal_year, canonical_metric);
        CREATE INDEX IF NOT EXISTS idx_metric_lookup_ticker_type_period
            ON metric_lookup(ticker, object_type, observation_period);
        CREATE INDEX IF NOT EXISTS idx_metric_lookup_total
            ON metric_lookup(ticker, is_company_total, canonical_metric);
        CREATE INDEX IF NOT EXISTS idx_metric_lookup_ticker_metric_year
            ON metric_lookup(ticker, canonical_metric, fiscal_year, observation_period);
        CREATE INDEX IF NOT EXISTS idx_metric_lookup_total_metric_year
            ON metric_lookup(ticker, canonical_metric, is_company_total, fiscal_year, observation_period);
        CREATE INDEX IF NOT EXISTS idx_metric_dim_ticker_key_metric_year
            ON metric_dimension_lookup(ticker, dimension_key, canonical_metric, fiscal_year, observation_period);
        CREATE INDEX IF NOT EXISTS idx_metric_dim_ticker_metric_year_key
            ON metric_dimension_lookup(ticker, canonical_metric, fiscal_year, dimension_key);
        CREATE INDEX IF NOT EXISTS idx_metric_dim_ticker_kind_key_metric_year
            ON metric_dimension_lookup(ticker, dimension_kind, dimension_key, canonical_metric, fiscal_year, observation_period);
        CREATE INDEX IF NOT EXISTS idx_company_dimension_catalog_ticker
            ON company_dimension_catalog(ticker);
        CREATE INDEX IF NOT EXISTS idx_company_dimension_catalog_lookup
            ON company_dimension_catalog(ticker, dimension_key, dimension_kind);
        CREATE INDEX IF NOT EXISTS idx_exposure_lookup_scope
            ON exposure_lookup(ticker, period, object_type);
        CREATE INDEX IF NOT EXISTS idx_exposure_lookup_factor
            ON exposure_lookup(ticker, factor, benchmark);
        CREATE INDEX IF NOT EXISTS idx_agreement_lookup_scope
            ON agreement_lookup(ticker, period, object_type);
        CREATE INDEX IF NOT EXISTS idx_agreement_lookup_type
            ON agreement_lookup(ticker, agreement_type, maturity_date);
        CREATE INDEX IF NOT EXISTS idx_event_lookup_scope
            ON event_lookup(ticker, period, object_type);
        CREATE INDEX IF NOT EXISTS idx_event_lookup_type
            ON event_lookup(ticker, event_type, event_status, date_sort_key);
        CREATE INDEX IF NOT EXISTS idx_factor_lookup_scope
            ON factor_lookup(ticker, period, object_type);
        CREATE INDEX IF NOT EXISTS idx_factor_lookup_family
            ON factor_lookup(ticker, topic_family, risk_or_driver);
        CREATE INDEX IF NOT EXISTS idx_company_topic_ticker
            ON company_topic_index(ticker);
        CREATE INDEX IF NOT EXISTS idx_company_topic_scope
            ON company_topic_index(ticker, period, document_type, doc_type_key);
        CREATE INDEX IF NOT EXISTS idx_company_topic_ticker_family
            ON company_topic_index(ticker, topic_family);
        CREATE INDEX IF NOT EXISTS idx_company_topic_answerability
            ON company_topic_index(ticker, answer_candidate, trace_status, evidence_strength);
        CREATE INDEX IF NOT EXISTS idx_company_topic_source_object
            ON company_topic_source_objects(object_id);
        CREATE INDEX IF NOT EXISTS idx_company_topic_source_topic
            ON company_topic_source_objects(topic_id);
        CREATE INDEX IF NOT EXISTS idx_company_topic_source_topic_rank
            ON company_topic_source_objects(topic_id, rank);
        """
    )


def _index_artifact(
    conn: sqlite3.Connection,
    root: Path,
    artifact_index_path: Path,
    *,
    replace_fts_entries: bool,
) -> dict[str, int]:
    artifact_index = json.loads(artifact_index_path.read_text())
    ticker = artifact_index["ticker"].upper()
    document_type = artifact_index["document_type"]
    doc_type_key = artifact_index.get("doc_type_key") or normalize_doc_type(document_type)
    period = artifact_index["period"]
    ontology_dir = artifact_index_path.parent
    files = artifact_index.get("files", {})
    object_rows_by_key: dict[str, list[dict[str, Any]]] = {}
    for artifact_key, rel_path in files.items():
        if artifact_key not in OBJECT_FILE_KEYS:
            continue
        path = _resolve_artifact_path(root, rel_path)
        object_rows_by_key[artifact_key] = read_jsonl(path) if path else []

    edge_path = _resolve_artifact_path(root, files.get("edges"))
    edge_rows = read_jsonl(edge_path) if edge_path else []
    objects_by_id = {
        obj["id"]: obj for rows in object_rows_by_key.values() for obj in rows if obj.get("id")
    }
    retrieval_lookup = ObjectLookup(
        objects_by_id=objects_by_id,
        support_links=object_rows_by_key.get("support_links", []),
        edges=edge_rows,
    )
    taxonomy_by_id = _taxonomy_by_id(object_rows_by_key.get("taxonomy_terms", []))

    section_quality = _read_section_quality(ontology_dir)
    conn.execute(
        """
        INSERT OR REPLACE INTO documents(
            ticker, document_type, doc_type_key, period, artifact_index_path,
            ontology_dir, sources_json, reports_json, counts_json,
            section_quality_status, section_quality_json, generated_at, schema_version
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        (
            ticker,
            document_type,
            doc_type_key,
            period,
            str(artifact_index_path),
            str(ontology_dir),
            json.dumps(artifact_index.get("sources", {}), ensure_ascii=False),
            json.dumps(artifact_index.get("reports", {}), ensure_ascii=False),
            json.dumps(artifact_index.get("counts", {}), ensure_ascii=False),
            section_quality.get("status"),
            json.dumps(section_quality, ensure_ascii=False),
            artifact_index.get("generated_at"),
            artifact_index.get("schema_version"),
        ),
    )

    stats = {"documents": 1, "objects": 0, "edges": 0, "quality_events": 0}
    if section_quality.get("status") in {"warn", "fail"}:
        _insert_quality_event(
            conn,
            ticker=ticker,
            document_type=document_type,
            doc_type_key=doc_type_key,
            period=period,
            severity=section_quality["status"],
            category="section_quality",
            object_id=None,
            stage="extract_sections",
            message=_section_quality_message(section_quality),
            payload=section_quality,
        )
        stats["quality_events"] += 1

    for artifact_key, rel_path in files.items():
        path = _resolve_artifact_path(root, rel_path)
        if artifact_key == "edges":
            stats["edges"] += _index_edges(
                conn,
                path,
                ticker,
                document_type,
                doc_type_key,
                period,
                rows=edge_rows,
            )
        elif artifact_key == "quality_events":
            stats["quality_events"] += _index_quality_events(
                conn, path, ticker, document_type, doc_type_key, period
            )
        elif artifact_key in OBJECT_FILE_KEYS:
            stats["objects"] += _index_objects(
                conn,
                path,
                artifact_key,
                ticker,
                document_type,
                doc_type_key,
                period,
                replace_fts_entries=replace_fts_entries,
                rows=object_rows_by_key.get(artifact_key),
                retrieval_lookup=retrieval_lookup,
                taxonomy_by_id=taxonomy_by_id,
            )

    rejected_path = _resolve_artifact_path(root, files.get("rejected_objects"))
    rejected_objects = read_jsonl(rejected_path) if rejected_path else []
    for obj in rejected_objects:
        _insert_object(
            conn,
            obj,
            artifact_key="rejected_objects",
            artifact_path=rejected_path or ontology_dir / "rejected_objects.jsonl",
            ticker=ticker,
            document_type=document_type,
            doc_type_key=doc_type_key,
            period=period,
            forced_review_status="rejected",
            replace_fts_entries=replace_fts_entries,
            retrieval_lookup=None,
            taxonomy_by_id=None,
        )
        stats["objects"] += 1
        _insert_quality_event(
            conn,
            ticker=ticker,
            document_type=document_type,
            doc_type_key=doc_type_key,
            period=period,
            severity="warn",
            category="rejected_object",
            object_id=obj.get("id"),
            stage=obj.get("rejection_stage") or obj.get("stage"),
            message=obj.get("rejection_reason") or obj.get("reason") or "Rejected object",
            payload=obj,
        )
        stats["quality_events"] += 1

    failures_path = _resolve_artifact_path(root, files.get("batch_failures"))
    failures = read_jsonl(failures_path) if failures_path else []
    for failure in failures:
        _insert_quality_event(
            conn,
            ticker=ticker,
            document_type=document_type,
            doc_type_key=doc_type_key,
            period=period,
            severity="error",
            category="batch_failure",
            object_id=failure.get("id"),
            stage=failure.get("stage"),
            message=(
                failure.get("error_message")
                or failure.get("error")
                or failure.get("message")
                or "Batch failure"
            ),
            payload=failure,
        )
        stats["quality_events"] += 1

    return stats


def _index_objects(
    conn: sqlite3.Connection,
    path: Path | None,
    artifact_key: str,
    ticker: str,
    document_type: str,
    doc_type_key: str,
    period: str,
    *,
    replace_fts_entries: bool,
    rows: list[dict[str, Any]] | None = None,
    retrieval_lookup: ObjectLookup | None = None,
    taxonomy_by_id: dict[str, dict[str, Any]] | None = None,
) -> int:
    if not path and rows is None:
        return 0
    count = 0
    object_rows: list[tuple[Any, ...]] = []
    fts_rows: list[tuple[Any, ...]] = []
    for obj in rows if rows is not None else read_jsonl(path):
        if not obj.get("id") or not obj.get("type"):
            continue
        object_row, fts_row = _build_object_insert_rows(
            obj,
            artifact_key=artifact_key,
            artifact_path=path,
            ticker=ticker,
            document_type=document_type,
            doc_type_key=doc_type_key,
            period=period,
            retrieval_lookup=retrieval_lookup,
            taxonomy_by_id=taxonomy_by_id,
        )
        object_rows.append(object_row)
        if fts_row is not None:
            fts_rows.append(fts_row)
        count += 1
        if len(object_rows) >= BULK_INSERT_CHUNK_SIZE:
            _flush_object_insert_batch(
                conn,
                object_rows,
                fts_rows,
                replace_fts_entries=replace_fts_entries,
            )
    _flush_object_insert_batch(
        conn,
        object_rows,
        fts_rows,
        replace_fts_entries=replace_fts_entries,
    )
    return count


def _index_quality_events(
    conn: sqlite3.Connection,
    path: Path | None,
    ticker: str,
    document_type: str,
    doc_type_key: str,
    period: str,
) -> int:
    if not path:
        return 0
    count = 0
    for event in read_jsonl(path):
        _insert_quality_event(
            conn,
            ticker=str(event.get("ticker") or ticker).upper(),
            document_type=str(event.get("document_type") or document_type),
            doc_type_key=str(event.get("doc_type_key") or doc_type_key),
            period=str(event.get("period") or period),
            severity=str(event.get("severity") or "warn"),
            category=str(event.get("category") or "quality_event"),
            object_id=event.get("object_id"),
            stage=event.get("stage"),
            message=str(event.get("message") or "Quality event"),
            payload=event,
        )
        count += 1
    return count


def _rebuild_object_traceability(conn: sqlite3.Connection) -> int:
    """Compute serving-layer evidence traceability without mutating artifacts."""
    conn.execute("DELETE FROM object_traceability")
    rows = conn.execute(
        """
        SELECT id, type, ticker, document_type, period, json
        FROM objects
        WHERE review_status IS NULL OR review_status != 'rejected'
        """
    ).fetchall()
    objects: dict[str, dict[str, Any]] = {}
    for row in rows:
        obj = json.loads(row["json"])
        obj.setdefault("id", row["id"])
        obj.setdefault("type", row["type"])
        obj.setdefault("ticker", row["ticker"])
        obj.setdefault("document_type", row["document_type"])
        obj.setdefault("period", row["period"])
        objects[row["id"]] = obj

    incoming_support: dict[str, list[dict[str, Any]]] = defaultdict(list)
    calculations_by_output: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for obj in objects.values():
        if obj.get("type") == "SupportLink":
            support_id = obj.get("support_object_id") or obj.get("from_id")
            target_id = obj.get("target_object_id") or obj.get("to_id")
            if support_id in objects and target_id in objects:
                incoming_support[str(target_id)].append(obj)
        if obj.get("type") == "Calculation" and obj.get("output_metric_id"):
            calculations_by_output[str(obj["output_metric_id"])].append(obj)

    count = 0
    trace_rows: list[tuple[Any, ...]] = []
    for obj in objects.values():
        info = _traceability_for_object(obj, objects, incoming_support, calculations_by_output)
        trace_rows.append(
            (
                obj["id"],
                obj.get("type") or "Unknown",
                obj.get("ticker") or "",
                obj.get("document_type") or "",
                obj.get("period") or "",
                info["trace_status"],
                info["evidence_chain_count"],
                info.get("support_depth"),
                info["support_quote_count"],
                info["support_claim_count"],
                info["support_link_count"],
                info.get("trace_method"),
                info.get("metric_lineage_status"),
                1 if info.get("answer_candidate") else 0,
                json.dumps(info, ensure_ascii=False),
            )
        )
        count += 1
        if len(trace_rows) >= BULK_INSERT_CHUNK_SIZE:
            _flush_traceability_batch(conn, trace_rows)
    _flush_traceability_batch(conn, trace_rows)
    return count


def _flush_traceability_batch(conn: sqlite3.Connection, rows: list[tuple[Any, ...]]) -> None:
    if not rows:
        return
    conn.executemany(
        """
        INSERT OR REPLACE INTO object_traceability(
            object_id, object_type, ticker, document_type, period,
            trace_status, evidence_chain_count, support_depth,
            support_quote_count, support_claim_count, support_link_count,
            trace_method, metric_lineage_status, answer_candidate, json
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        rows,
    )
    rows.clear()


def _traceability_for_object(
    obj: dict[str, Any],
    objects: dict[str, dict[str, Any]],
    incoming_support: dict[str, list[dict[str, Any]]],
    calculations_by_output: dict[str, list[dict[str, Any]]],
) -> dict[str, Any]:
    obj_id = str(obj.get("id") or "")
    obj_type = str(obj.get("type") or "")
    incoming = incoming_support.get(obj_id, [])
    answer_candidate = obj_type in ANSWER_CANDIDATE_TYPES

    if not answer_candidate:
        return {
            "trace_status": "index_only",
            "evidence_chain_count": 0,
            "support_depth": None,
            "support_quote_count": 0,
            "support_claim_count": 0,
            "support_link_count": len(incoming),
            "trace_method": "index_only",
            "metric_lineage_status": None,
            "answer_candidate": False,
        }

    if obj_type == "EvidenceQuote":
        span_id = obj.get("source_span_id")
        traceable = bool(span_id and span_id in objects)
        return {
            "trace_status": "traceable" if traceable else "untraced",
            "evidence_chain_count": 1 if traceable else 0,
            "support_depth": 1 if traceable else None,
            "support_quote_count": 1,
            "support_claim_count": 0,
            "support_link_count": len(incoming),
            "trace_method": "source_span" if traceable else "none",
            "metric_lineage_status": None,
            "answer_candidate": True,
        }

    if obj_type == "ResearchClaim":
        quote_ids = _support_ids(
            obj,
            objects,
            incoming,
            field_names=("supported_by_quotes",),
            support_types={"EvidenceQuote"},
        )
        return {
            "trace_status": "traceable" if quote_ids else "untraced",
            "evidence_chain_count": len(quote_ids),
            "support_depth": 1 if quote_ids else None,
            "support_quote_count": len(quote_ids),
            "support_claim_count": 1,
            "support_link_count": len(incoming),
            "trace_method": _trace_method(obj, incoming, quote_ids),
            "metric_lineage_status": None,
            "answer_candidate": True,
        }

    if obj_type == "MetricObservation":
        fact_ids = _support_ids(
            obj,
            objects,
            incoming,
            field_names=("source_fact_ids",),
            support_types={"XBRLFact"},
        )
        source_metric_ids = _support_ids(
            obj,
            objects,
            incoming,
            field_names=("source_metric_ids",),
            support_types={"MetricObservation"},
        )
        calculation_ids = _unique_list(
            [
                *([obj["calculation_id"]] if obj.get("calculation_id") else []),
                *[
                    calc.get("id")
                    for calc in calculations_by_output.get(obj_id, [])
                    if calc.get("id")
                ],
                *[
                    _support_object_id(link)
                    for link in incoming
                    if _object_type(objects, _support_object_id(link)) == "Calculation"
                ],
            ]
        )
        traceable = bool(fact_ids or source_metric_ids or calculation_ids)
        return {
            "trace_status": "traceable_metric_lineage"
            if traceable
            else "untraced_metric_candidate",
            "evidence_chain_count": len(fact_ids) + len(source_metric_ids) + len(calculation_ids),
            "support_depth": 1
            if fact_ids
            else (2 if source_metric_ids or calculation_ids else None),
            "support_quote_count": 0,
            "support_claim_count": 0,
            "support_link_count": len(incoming),
            "trace_method": "metric_lineage" if traceable else "none",
            "metric_lineage_status": "traceable_metric_lineage"
            if traceable
            else "missing_metric_lineage",
            "answer_candidate": True,
        }

    if obj_type in SEMANTIC_SUPPORT_TYPES:
        claim_ids = _support_ids(
            obj,
            objects,
            incoming,
            field_names=("supported_by_claims",),
            support_types={"ResearchClaim"},
        )
        direct_quote_ids = _support_ids(
            obj,
            objects,
            incoming,
            field_names=("supported_by_quotes",),
            support_types={"EvidenceQuote"},
        )
        claim_quote_ids = _unique_list(
            quote_id
            for claim_id in claim_ids
            for quote_id in _quote_ids_for_claim(objects.get(claim_id), objects, incoming_support)
        )
        quote_ids = _unique_list([*direct_quote_ids, *claim_quote_ids])
        traceable = bool(claim_ids or quote_ids)
        return {
            "trace_status": "traceable" if traceable else "orphan",
            "evidence_chain_count": len(claim_ids) + len(quote_ids),
            "support_depth": 2
            if claim_quote_ids
            else (1 if direct_quote_ids or claim_ids else None),
            "support_quote_count": len(quote_ids),
            "support_claim_count": len(claim_ids),
            "support_link_count": len(incoming),
            "trace_method": _trace_method(obj, incoming, [*claim_ids, *quote_ids]),
            "metric_lineage_status": None,
            "answer_candidate": True,
        }

    if obj_type == "Calculation":
        metric_ids = _unique_list(
            [*(obj.get("input_metric_ids") or []), *(obj.get("source_metric_ids") or [])]
        )
        output_id = obj.get("output_metric_id")
        traceable = bool(metric_ids or output_id)
        return {
            "trace_status": "traceable_metric_lineage"
            if traceable
            else "untraced_metric_candidate",
            "evidence_chain_count": len(metric_ids) + (1 if output_id else 0),
            "support_depth": 1 if traceable else None,
            "support_quote_count": 0,
            "support_claim_count": 0,
            "support_link_count": len(incoming),
            "trace_method": "metric_lineage" if traceable else "none",
            "metric_lineage_status": "traceable_metric_lineage"
            if traceable
            else "missing_metric_lineage",
            "answer_candidate": True,
        }

    return {
        "trace_status": "untraced",
        "evidence_chain_count": 0,
        "support_depth": None,
        "support_quote_count": 0,
        "support_claim_count": 0,
        "support_link_count": len(incoming),
        "trace_method": "none",
        "metric_lineage_status": None,
        "answer_candidate": True,
    }


def _support_ids(
    obj: dict[str, Any],
    objects: dict[str, dict[str, Any]],
    incoming: list[dict[str, Any]],
    *,
    field_names: tuple[str, ...],
    support_types: set[str],
) -> list[str]:
    values: list[str] = []
    for field_name in field_names:
        values.extend(obj.get(field_name) or [])
    for link in incoming:
        support_id = _support_object_id(link)
        if _object_type(objects, support_id) in support_types:
            values.append(support_id)
    return _unique_list(value for value in values if value in objects)


def _support_object_id(link: dict[str, Any]) -> str:
    return str(link.get("support_object_id") or link.get("from_id") or "")


def _object_type(objects: dict[str, dict[str, Any]], object_id: str) -> str | None:
    return (objects.get(object_id) or {}).get("type")


def _quote_ids_for_claim(
    claim: dict[str, Any] | None,
    objects: dict[str, dict[str, Any]],
    incoming_support: dict[str, list[dict[str, Any]]],
) -> list[str]:
    if not claim:
        return []
    quote_ids = list(claim.get("supported_by_quotes") or [])
    for link in incoming_support.get(str(claim.get("id") or ""), []):
        support_id = _support_object_id(link)
        if _object_type(objects, support_id) == "EvidenceQuote":
            quote_ids.append(support_id)
    return _unique_list(quote_id for quote_id in quote_ids if quote_id in objects)


def _trace_method(
    obj: dict[str, Any], incoming: list[dict[str, Any]], support_ids: list[str]
) -> str:
    if incoming and support_ids:
        return "explicit_support_link"
    if support_ids:
        return "reference_fields"
    return "none"


def _unique_list(values: Iterable[Any]) -> list[str]:
    seen: set[str] = set()
    result: list[str] = []
    for value in values:
        if value is None:
            continue
        text = str(value)
        if not text or text in seen:
            continue
        seen.add(text)
        result.append(text)
    return result


def _insert_object(
    conn: sqlite3.Connection,
    obj: dict[str, Any],
    *,
    artifact_key: str,
    artifact_path: Path,
    ticker: str,
    document_type: str,
    doc_type_key: str,
    period: str,
    forced_review_status: str | None = None,
    replace_fts_entries: bool,
    retrieval_lookup: ObjectLookup | None,
    taxonomy_by_id: dict[str, dict[str, Any]] | None,
) -> None:
    object_row, fts_row = _build_object_insert_rows(
        obj,
        artifact_key=artifact_key,
        artifact_path=artifact_path,
        ticker=ticker,
        document_type=document_type,
        doc_type_key=doc_type_key,
        period=period,
        forced_review_status=forced_review_status,
        retrieval_lookup=retrieval_lookup,
        taxonomy_by_id=taxonomy_by_id,
    )
    _flush_object_insert_batch(
        conn,
        [object_row],
        [fts_row] if fts_row is not None else [],
        replace_fts_entries=replace_fts_entries,
    )


def _build_object_insert_rows(
    obj: dict[str, Any],
    *,
    artifact_key: str,
    artifact_path: Path | None,
    ticker: str,
    document_type: str,
    doc_type_key: str,
    period: str,
    forced_review_status: str | None = None,
    retrieval_lookup: ObjectLookup | None,
    taxonomy_by_id: dict[str, dict[str, Any]] | None,
) -> tuple[tuple[Any, ...], tuple[Any, ...] | None]:
    retrieval_text = (
        build_retrieval_text(obj, retrieval_lookup, taxonomy_by_id)
        if retrieval_lookup is not None
        else fallback_retrieval_text(obj)
    )
    text = retrieval_text.compact_text or retrieval_text.text_self or _object_text(obj)
    review_status = forced_review_status or obj.get("review_status")
    object_row = (
        obj["id"],
        obj["type"],
        obj.get("ticker", ticker),
        obj.get("document_type", document_type),
        doc_type_key,
        obj.get("period", period),
        obj.get("source_document_id"),
        obj.get("section_name") or obj.get("section_key"),
        obj.get("metric_name"),
        review_status,
        obj.get("confidence"),
        text,
        json.dumps(obj, ensure_ascii=False),
        artifact_key,
        str(artifact_path or ""),
    )
    fts_row = None
    if retrieval_text.joined and obj.get("type") != "SupportLink":
        fts_row = (
            obj["id"],
            obj["type"],
            obj.get("ticker", ticker),
            obj.get("document_type", document_type),
            obj.get("period", period),
            retrieval_text.text_self,
            retrieval_text.text_support,
            retrieval_text.text_related,
            retrieval_text.text_entities,
            retrieval_text.text_aliases,
            retrieval_text.compact_text,
        )
    return object_row, fts_row


def _flush_object_insert_batch(
    conn: sqlite3.Connection,
    object_rows: list[tuple[Any, ...]],
    fts_rows: list[tuple[Any, ...]],
    *,
    replace_fts_entries: bool,
) -> None:
    if object_rows:
        conn.executemany(
            """
            INSERT OR REPLACE INTO objects(
                id, type, ticker, document_type, doc_type_key, period,
                source_document_id, section_name, metric_name, review_status,
                confidence, text, json, artifact_key, artifact_path
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            object_rows,
        )
    if fts_rows:
        fts_rows = [_object_fts_row_with_scope_tokens(row) for row in fts_rows]
        if replace_fts_entries:
            conn.executemany(
                "DELETE FROM object_fts WHERE object_id = ?", ((row[0],) for row in fts_rows)
            )
            conn.executemany(
                "DELETE FROM object_search_text WHERE object_id = ?",
                ((row[0],) for row in fts_rows),
            )
            conn.executemany(
                "DELETE FROM object_text WHERE object_id = ?", ((row[0],) for row in fts_rows)
            )
        conn.executemany(
            """
            INSERT INTO object_fts(
                object_id, type, ticker, document_type, period,
                text_self, text_support, text_related, text_entities,
                text_aliases, compact_text
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            fts_rows,
        )
        conn.executemany(
            """
            INSERT OR REPLACE INTO object_search_text(
                object_id, type, ticker, document_type, period,
                text_self, text_support, text_related, text_entities,
                text_aliases, compact_text
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            fts_rows,
        )
        conn.executemany(
            """
            INSERT OR REPLACE INTO object_text(
                object_id, type, ticker, document_type, period,
                text_self, text_support, text_related, text_entities,
                text_aliases, compact_text
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            fts_rows,
        )
    object_rows.clear()
    fts_rows.clear()


def _rebuild_metric_lookup(conn: sqlite3.Connection) -> int:
    """Materialize metric-oriented objects for structured numeric lookup."""
    conn.execute("DELETE FROM metric_lookup")
    inserted_count = 0
    rows = conn.execute(
        """
        SELECT
            objects.id,
            objects.type,
            objects.ticker,
            objects.document_type,
            objects.doc_type_key,
            objects.period,
            objects.metric_name,
            objects.text,
            objects.json,
            object_traceability.trace_status,
            object_traceability.metric_lineage_status
        FROM objects
        LEFT JOIN object_traceability
          ON object_traceability.object_id = objects.id
        WHERE objects.type IN ('MetricObservation', 'Calculation')
          AND (objects.review_status IS NULL OR objects.review_status != 'rejected')
        """
    ).fetchall()
    lookup_rows: list[tuple[Any, ...]] = []
    for row in rows:
        try:
            obj = json.loads(row["json"] or "{}")
        except json.JSONDecodeError:
            obj = {}
        metric_name = str(row["metric_name"] or _metric_lookup_metric_name(obj, row["type"]) or "")
        metric_text = obj.get("text") or row["text"]
        canonical_metric = _metric_lookup_canonical(metric_name)
        dimensions = _metric_lookup_normalize_dimensions(
            obj.get("dimensions") or obj.get("dimension") or {}
        )
        inferred_dimension = _metric_lookup_infer_dimension_from_text(
            metric_name=metric_name,
            text=metric_text,
            dimensions=dimensions,
        )
        if inferred_dimension and inferred_dimension[1] not in set(dimensions.values()):
            dimensions[f"inferred_{inferred_dimension[0]}"] = inferred_dimension[1]
        segment_name = _metric_lookup_dimension(
            dimensions, ("segment", "segment_name", "business_segment")
        )
        product_name = _metric_lookup_dimension(
            dimensions, ("product", "product_name", "product_line")
        )
        geography_name = _metric_lookup_dimension(
            dimensions, ("geography", "geography_name", "region", "country")
        )
        observation = _metric_lookup_observation_context(obj, row["period"])
        fiscal_year = observation["fiscal_year"]
        fiscal_quarter = observation["fiscal_quarter"]
        value_text = _metric_lookup_value_text(obj)
        alias_text = _metric_lookup_alias_text(
            metric_name=metric_name,
            canonical_metric=canonical_metric,
            obj=obj,
            dimensions=dimensions,
            text=metric_text,
        )
        lookup_rows.append(
            (
                row["id"],
                row["type"],
                row["ticker"],
                row["document_type"],
                row["doc_type_key"],
                observation["filing_period"],
                observation["filing_period"],
                observation["observation_period"],
                observation["period_type"],
                observation["start_date"],
                observation["end_date"],
                observation["context_key"],
                fiscal_year,
                fiscal_quarter,
                metric_name or None,
                canonical_metric or None,
                alias_text,
                value_text,
                _metric_lookup_numeric_value(value_text),
                _metric_lookup_unit(obj),
                json.dumps(dimensions, ensure_ascii=False, sort_keys=True),
                1
                if _metric_lookup_is_company_total(
                    metric_name=metric_name,
                    dimensions=dimensions,
                    text=metric_text,
                )
                else 0,
                segment_name,
                product_name,
                geography_name,
                row["trace_status"],
                row["metric_lineage_status"],
                metric_text,
            )
        )
        inserted_count += 1
        if len(lookup_rows) >= BULK_INSERT_CHUNK_SIZE:
            _flush_metric_lookup_batch(conn, lookup_rows)

    xbrl_rows = conn.execute(
        """
        SELECT
            objects.id,
            objects.type,
            objects.ticker,
            objects.document_type,
            objects.doc_type_key,
            objects.period,
            objects.text,
            objects.json,
            object_traceability.trace_status,
            object_traceability.metric_lineage_status
        FROM objects
        LEFT JOIN object_traceability
          ON object_traceability.object_id = objects.id
        WHERE objects.type = 'XBRLFact'
          AND (objects.review_status IS NULL OR objects.review_status != 'rejected')
        """
    ).fetchall()
    for row in xbrl_rows:
        try:
            obj = json.loads(row["json"] or "{}")
        except json.JSONDecodeError:
            obj = {}
        dimensions = _metric_lookup_xbrl_dimensions(obj)
        if not dimensions:
            continue
        metric_name = _metric_lookup_xbrl_metric_name(obj)
        if not metric_name:
            continue
        metric_text = _metric_lookup_xbrl_text(
            obj=obj,
            fallback_text=row["text"],
            metric_name=metric_name,
            period=row["period"],
        )
        canonical_metric = _metric_lookup_canonical(metric_name)
        segment_name = _metric_lookup_dimension(
            dimensions, ("segment", "segment_name", "business_segment")
        )
        product_name = _metric_lookup_dimension(
            dimensions, ("product", "product_name", "product_line")
        )
        geography_name = _metric_lookup_dimension(
            dimensions, ("geography", "geography_name", "region", "country")
        )
        observation = _metric_lookup_observation_context(obj, row["period"])
        fiscal_year = observation["fiscal_year"]
        fiscal_quarter = observation["fiscal_quarter"]
        value_text = _metric_lookup_value_text(obj)
        alias_text = _metric_lookup_alias_text(
            metric_name=metric_name,
            canonical_metric=canonical_metric,
            obj=obj,
            dimensions=dimensions,
            text=metric_text,
        )
        lookup_rows.append(
            (
                row["id"],
                row["type"],
                row["ticker"],
                row["document_type"],
                row["doc_type_key"],
                observation["filing_period"],
                observation["filing_period"],
                observation["observation_period"],
                observation["period_type"],
                observation["start_date"],
                observation["end_date"],
                observation["context_key"],
                fiscal_year,
                fiscal_quarter,
                metric_name,
                canonical_metric or None,
                alias_text,
                value_text,
                _metric_lookup_numeric_value(value_text),
                _metric_lookup_unit(obj),
                json.dumps(dimensions, ensure_ascii=False, sort_keys=True),
                1
                if _metric_lookup_is_company_total(
                    metric_name=metric_name,
                    dimensions=dimensions,
                    text=metric_text,
                )
                else 0,
                segment_name,
                product_name,
                geography_name,
                row["trace_status"],
                row["metric_lineage_status"],
                metric_text,
            )
        )
        inserted_count += 1
        if len(lookup_rows) >= BULK_INSERT_CHUNK_SIZE:
            _flush_metric_lookup_batch(conn, lookup_rows)
    _flush_metric_lookup_batch(conn, lookup_rows)
    return inserted_count


def _flush_metric_lookup_batch(conn: sqlite3.Connection, rows: list[tuple[Any, ...]]) -> None:
    if not rows:
        return
    conn.executemany(
        """
        INSERT OR REPLACE INTO metric_lookup(
            object_id, object_type, ticker, document_type, doc_type_key,
            period, filing_period, observation_period, observation_period_type,
            observation_start_date, observation_end_date, observation_context_key,
            fiscal_year, fiscal_quarter, metric_name, canonical_metric,
            metric_alias_text, value_text, value_numeric, unit, dimensions_json,
            is_company_total, segment_name, product_name, geography_name,
            trace_status, metric_lineage_status, text
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        rows,
    )
    rows.clear()


def _rebuild_metric_dimension_lookup(conn: sqlite3.Connection) -> int:
    """Materialize normalized dimension keys for metric series lookup."""
    conn.execute("DELETE FROM metric_dimension_lookup")
    rows = conn.execute(
        """
        SELECT
            object_id,
            ticker,
            period,
            filing_period,
            observation_period,
            fiscal_year,
            fiscal_quarter,
            canonical_metric,
            dimensions_json,
            is_company_total,
            trace_status,
            metric_lineage_status
        FROM metric_lookup
        WHERE canonical_metric IS NOT NULL
        """
    ).fetchall()
    dimension_rows: list[tuple[Any, ...]] = []
    for row in rows:
        try:
            dimensions = json.loads(row["dimensions_json"] or "{}")
        except json.JSONDecodeError:
            dimensions = {}
        if not isinstance(dimensions, Mapping):
            dimensions = {}
        if row["is_company_total"]:
            dimension_rows.append(
                (
                    row["object_id"],
                    row["ticker"],
                    row["period"],
                    row["filing_period"],
                    row["observation_period"],
                    row["fiscal_year"],
                    row["fiscal_quarter"],
                    row["canonical_metric"],
                    "company_total",
                    "__company_total__",
                    "Company Total",
                    None,
                    None,
                    1,
                    row["trace_status"],
                    row["metric_lineage_status"],
                )
            )
        else:
            seen_keys: set[str] = set()
            for axis, label in dimensions.items():
                clean_label = _metric_lookup_clean_dimension_label(label)
                if not clean_label:
                    continue
                dimension_key = _metric_dimension_key(clean_label)
                if not dimension_key or dimension_key in seen_keys:
                    continue
                seen_keys.add(dimension_key)
                dimension_rows.append(
                    (
                        row["object_id"],
                        row["ticker"],
                        row["period"],
                        row["filing_period"],
                        row["observation_period"],
                        row["fiscal_year"],
                        row["fiscal_quarter"],
                        row["canonical_metric"],
                        _metric_dimension_kind(axis, clean_label),
                        dimension_key,
                        clean_label,
                        _metric_dimension_key(axis),
                        _metric_dimension_key(clean_label),
                        0,
                        row["trace_status"],
                        row["metric_lineage_status"],
                    )
                )
        if len(dimension_rows) >= BULK_INSERT_CHUNK_SIZE:
            _flush_metric_dimension_lookup_batch(conn, dimension_rows)
    _flush_metric_dimension_lookup_batch(conn, dimension_rows)
    return conn.execute("SELECT COUNT(*) FROM metric_dimension_lookup").fetchone()[0]


def _flush_metric_dimension_lookup_batch(
    conn: sqlite3.Connection, rows: list[tuple[Any, ...]]
) -> None:
    if not rows:
        return
    conn.executemany(
        """
        INSERT OR REPLACE INTO metric_dimension_lookup(
            object_id, ticker, period, filing_period, observation_period,
            fiscal_year, fiscal_quarter,
            canonical_metric, dimension_kind, dimension_key, dimension_label,
            axis_key, member_key, is_company_total, trace_status, metric_lineage_status
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        rows,
    )
    rows.clear()


def _rebuild_company_dimension_catalog(conn: sqlite3.Connection) -> int:
    """Build ticker-level dimension catalog from normalized metric dimensions."""
    conn.execute("DELETE FROM company_dimension_catalog")
    rows = conn.execute(
        """
        SELECT ticker, dimension_key, dimension_label, dimension_kind,
               observation_period AS period, object_id
        FROM metric_dimension_lookup
        WHERE is_company_total = 0
        """
    ).fetchall()
    catalog: dict[tuple[str, str], dict[str, Any]] = {}
    for row in rows:
        key = (row["ticker"], row["dimension_key"])
        entry = catalog.setdefault(
            key,
            {
                "ticker": row["ticker"],
                "dimension_key": row["dimension_key"],
                "labels": {},
                "kinds": {},
                "periods": set(),
                "object_ids": set(),
                "aliases": set(),
            },
        )
        label = row["dimension_label"] or row["dimension_key"]
        kind = row["dimension_kind"] or "unknown"
        entry["labels"][label] = entry["labels"].get(label, 0) + 1
        entry["kinds"][kind] = entry["kinds"].get(kind, 0) + 1
        if row["period"]:
            entry["periods"].add(row["period"])
        if row["object_id"]:
            entry["object_ids"].add(row["object_id"])
        entry["aliases"].update(_metric_dimension_aliases(label, row["dimension_key"]))
    catalog_rows: list[tuple[Any, ...]] = []
    for entry in catalog.values():
        label = max(entry["labels"].items(), key=lambda item: (item[1], len(item[0])))[0]
        kind = max(entry["kinds"].items(), key=lambda item: item[1])[0]
        catalog_rows.append(
            (
                entry["ticker"],
                entry["dimension_key"],
                label,
                kind,
                len(entry["object_ids"]),
                json.dumps(sorted(entry["periods"]), ensure_ascii=False),
                " ".join(sorted(str(alias) for alias in entry["aliases"] if alias)),
            )
        )
    if catalog_rows:
        conn.executemany(
            """
            INSERT OR REPLACE INTO company_dimension_catalog(
                ticker, dimension_key, dimension_label, dimension_kind,
                source_metric_count, periods_json, aliases_text
            )
            VALUES (?, ?, ?, ?, ?, ?, ?)
            """,
            catalog_rows,
        )
    return len(catalog_rows)


def _metric_lookup_metric_name(obj: Mapping[str, Any], object_type: str) -> str:
    if object_type == "Calculation":
        for key in ("output_metric_name", "output_metric_id", "calculation_type", "name"):
            if obj.get(key):
                return str(obj[key])
    for key in ("metric_name", "canonical_metric", "name", "label"):
        if obj.get(key):
            return str(obj[key])
    return ""


def _metric_lookup_canonical(metric_name: str | None) -> str:
    return canonical_metric_name(metric_name)


def _metric_lookup_observation_context(
    obj: Mapping[str, Any],
    filing_period: Any,
) -> dict[str, Any]:
    """Derive fact-period identity without changing the canonical ontology object."""
    context = obj.get("context") if isinstance(obj.get("context"), Mapping) else {}
    start_date = _metric_lookup_first_date(
        obj,
        context,
        keys=("period_start", "start_date"),
    )
    end_date = _metric_lookup_first_date(
        obj,
        context,
        keys=("period_end", "end_date"),
    )
    instant = _metric_lookup_first_date(obj, context, keys=("instant",))
    if instant:
        start_date = start_date or instant
        end_date = end_date or instant
    raw_period_type = _metric_lookup_first_text(
        obj,
        context,
        keys=("period_type", "duration"),
    )
    period_type = _metric_lookup_period_type(
        raw_period_type,
        start_date=start_date,
        end_date=end_date,
    )
    fiscal_year = _metric_lookup_year(
        obj,
        filing_period,
        end_date=end_date,
    )
    fiscal_quarter = _metric_lookup_quarter(
        obj,
        filing_period,
    )
    if period_type == "annual":
        fiscal_quarter = None
    explicit_period = _metric_lookup_first_text(
        obj,
        context,
        keys=("observation_period", "fact_period", "fiscal_period"),
    )
    observation_period = _metric_lookup_normalize_period(explicit_period)
    explicit_coordinates = _metric_lookup_period_coordinates(observation_period)
    if (
        observation_period
        and explicit_coordinates is not None
        and (
            (fiscal_year is not None and explicit_coordinates[0] != fiscal_year)
            or (period_type == "annual" and explicit_coordinates[1] is not None)
            or (
                period_type == "annual"
                and observation_period.startswith("CY")
                and start_date is not None
                and end_date is not None
                and not _metric_lookup_is_calendar_year(start_date, end_date)
            )
            or (
                fiscal_quarter is not None
                and explicit_coordinates[1] is not None
                and explicit_coordinates[1] != fiscal_quarter
            )
        )
    ):
        # Some ingestion paths historically copied the filing label into every
        # comparative fact. Prefer the fact's fiscal/context dates when they
        # contradict that label.
        observation_period = None
    if not observation_period:
        observation_period = _metric_lookup_derived_period(
            filing_period=str(filing_period or "").strip(),
            fiscal_year=fiscal_year,
            fiscal_quarter=fiscal_quarter,
            period_type=period_type,
            start_date=start_date,
            end_date=end_date,
            has_explicit_fiscal_quarter=_metric_lookup_has_explicit_quarter(obj),
        )
    observation_period = observation_period or str(filing_period or "unknown").strip()
    identity_payload = "|".join(
        (
            observation_period,
            period_type,
            start_date or "",
            end_date or "",
        )
    )
    return {
        "filing_period": str(filing_period or "unknown").strip() or "unknown",
        "observation_period": observation_period,
        "period_type": period_type,
        "start_date": start_date,
        "end_date": end_date,
        "context_key": hashlib.sha256(identity_payload.encode("utf-8")).hexdigest()[:24],
        "fiscal_year": fiscal_year,
        "fiscal_quarter": fiscal_quarter,
    }


def _metric_lookup_first_text(
    obj: Mapping[str, Any],
    context: Mapping[str, Any],
    *,
    keys: Sequence[str],
) -> str | None:
    for source in (obj, context):
        for key in keys:
            value = source.get(key)
            if value is not None and str(value).strip():
                return str(value).strip()
    return None


def _metric_lookup_first_date(
    obj: Mapping[str, Any],
    context: Mapping[str, Any],
    *,
    keys: Sequence[str],
) -> str | None:
    value = _metric_lookup_first_text(obj, context, keys=keys)
    if not value:
        return None
    try:
        return date.fromisoformat(value[:10]).isoformat()
    except ValueError:
        return None


def _metric_lookup_period_type(
    value: str | None,
    *,
    start_date: str | None,
    end_date: str | None,
) -> str:
    normalized = re.sub(r"[^a-z0-9]+", "_", str(value or "").casefold()).strip("_")
    aliases = {
        "annual": "annual",
        "year": "annual",
        "yearly": "annual",
        "12_month": "annual",
        "12_months": "annual",
        "quarter": "quarter",
        "quarterly": "quarter",
        "3_month": "quarter",
        "3_months": "quarter",
        "three_month": "quarter",
        "three_months": "quarter",
        "year_to_date": "year_to_date",
        "ytd": "year_to_date",
        "instant": "instant",
        "point_in_time": "instant",
        "ttm": "ttm",
        "trailing_twelve_months": "ttm",
    }
    if normalized in aliases:
        return aliases[normalized]
    if start_date and end_date:
        start = date.fromisoformat(start_date)
        end = date.fromisoformat(end_date)
        days = (end - start).days + 1
        if days == 1:
            return "instant"
        if 70 <= days <= 115:
            return "quarter"
        if 160 <= days <= 300:
            return "year_to_date"
        if 330 <= days <= 400:
            return "annual"
    return normalized or "unknown"


def _metric_lookup_normalize_period(value: str | None) -> str | None:
    match = re.fullmatch(
        r"\s*(?:(CY|FY))?(19\d{2}|20\d{2})(?:Q([1-4]))?\s*",
        str(value or ""),
        flags=re.IGNORECASE,
    )
    if not match:
        return None
    prefix = (match.group(1) or "FY").upper()
    quarter = f"Q{match.group(3)}" if match.group(3) else ""
    return f"{prefix}{match.group(2)}{quarter}"


def _metric_lookup_period_coordinates(
    value: str | None,
) -> tuple[int, int | None] | None:
    match = re.fullmatch(
        r"(?:CY|FY)(19\d{2}|20\d{2})(?:Q([1-4]))?",
        str(value or ""),
        flags=re.IGNORECASE,
    )
    if not match:
        return None
    return int(match.group(1)), int(match.group(2)) if match.group(2) else None


def _metric_lookup_is_calendar_year(
    start_date: str | None,
    end_date: str | None,
) -> bool:
    if not start_date or not end_date:
        return False
    start = date.fromisoformat(start_date)
    end = date.fromisoformat(end_date)
    return (
        start.year == end.year
        and start.month == start.day == 1
        and end.month == 12
        and end.day == 31
    )


def _metric_lookup_derived_period(
    *,
    filing_period: str,
    fiscal_year: int | None,
    fiscal_quarter: int | None,
    period_type: str,
    start_date: str | None,
    end_date: str | None,
    has_explicit_fiscal_quarter: bool,
) -> str:
    normalized_filing = _metric_lookup_normalize_period(filing_period)
    if fiscal_year is None:
        return normalized_filing or filing_period
    if period_type == "annual":
        if start_date and end_date:
            start = date.fromisoformat(start_date)
            end = date.fromisoformat(end_date)
            if (
                start.year == end.year == fiscal_year
                and start.month == start.day == 1
                and end.month == 12
                and end.day == 31
            ):
                return f"CY{fiscal_year}"
        return f"FY{fiscal_year}"
    if has_explicit_fiscal_quarter and fiscal_quarter:
        return f"FY{fiscal_year}Q{fiscal_quarter}"
    if normalized_filing and "Q" not in normalized_filing and period_type == "instant":
        return normalized_filing
    if end_date and period_type in {"quarter", "year_to_date", "instant", "ttm"}:
        end = date.fromisoformat(end_date)
        return f"CY{end.year}Q{((end.month - 1) // 3) + 1}"
    if fiscal_quarter:
        filing_prefix = "CY" if (normalized_filing or "").startswith("CY") else "FY"
        return f"{filing_prefix}{fiscal_year}Q{fiscal_quarter}"
    if normalized_filing:
        filing_prefix = "CY" if normalized_filing.startswith("CY") else "FY"
        return f"{filing_prefix}{fiscal_year}"
    return f"FY{fiscal_year}"


def _metric_lookup_has_explicit_quarter(obj: Mapping[str, Any]) -> bool:
    context = obj.get("context") if isinstance(obj.get("context"), Mapping) else {}
    return any(
        source.get(key) not in (None, "")
        for source in (obj, context)
        for key in ("fiscal_quarter", "quarter")
    )


def _metric_lookup_year(
    obj: Mapping[str, Any],
    period: Any,
    *,
    end_date: str | None = None,
) -> int | None:
    for key in ("fiscal_year", "year", "calendar_year"):
        value = obj.get(key)
        if isinstance(value, int):
            return value
        if isinstance(value, str) and value.isdigit():
            return int(value)
    context = obj.get("context")
    if isinstance(context, Mapping):
        value = context.get("fiscal_year") or context.get("year") or context.get("calendar_year")
        if isinstance(value, int):
            return value
        if isinstance(value, str) and value.isdigit():
            return int(value)
    if end_date:
        return date.fromisoformat(end_date).year
    match = re.search(r"(?:CY|FY)?(20\d{2}|19\d{2})", str(period or ""), re.IGNORECASE)
    return int(match.group(1)) if match else None


def _metric_lookup_quarter(
    obj: Mapping[str, Any],
    period: Any,
) -> int | None:
    context = obj.get("context") if isinstance(obj.get("context"), Mapping) else {}
    for source in (obj, context):
        for key in ("fiscal_quarter", "quarter"):
            value = source.get(key)
            if isinstance(value, int) and 1 <= value <= 4:
                return value
            if isinstance(value, str) and value.isdigit() and 1 <= int(value) <= 4:
                return int(value)
    match = re.search(r"Q([1-4])", str(period or ""), re.IGNORECASE)
    if match:
        return int(match.group(1))
    # A period end date alone does not establish that a duration fact is a
    # quarter. Annual facts commonly end on quarter boundaries too, so keep the
    # coordinate unknown instead of silently turning an annual fact into Q4.
    return None


def _metric_lookup_xbrl_metric_name(obj: Mapping[str, Any]) -> str | None:
    for key in ("taxonomy_tag", "safe_taxonomy_tag"):
        canonical = canonical_metric_for_xbrl_tag(obj.get(key))
        if canonical:
            return canonical
    return None


def _metric_lookup_xbrl_dimensions(obj: Mapping[str, Any]) -> dict[str, str]:
    context = obj.get("context") if isinstance(obj.get("context"), Mapping) else {}
    raw_dimensions = (
        obj.get("dimensions")
        or obj.get("dimension")
        or context.get("dimensions")
        or context.get("dimension")
        or {}
    )
    return _metric_lookup_normalize_dimensions(raw_dimensions)


def _metric_lookup_xbrl_text(
    *,
    obj: Mapping[str, Any],
    fallback_text: Any,
    metric_name: str,
    period: Any,
) -> str:
    dimensions = _metric_lookup_xbrl_dimensions(obj)
    dimension_text = ", ".join(
        _metric_lookup_clean_dimension_label(value) for value in dimensions.values() if value
    )
    role_text = dimension_text or "company total"
    value_text = _metric_lookup_value_text(obj)
    unit = _metric_lookup_unit(obj)
    tag = str(obj.get("safe_taxonomy_tag") or obj.get("taxonomy_tag") or "").split(":")[-1]
    parts = [str(period or "").strip(), role_text, metric_name]
    if value_text:
        parts.append(str(value_text))
    if unit:
        parts.append(str(unit))
    if tag:
        parts.append(tag)
    text = " ".join(part for part in parts if part)
    return text or str(fallback_text or "")


def _metric_lookup_value_text(obj: Mapping[str, Any]) -> str | None:
    for key in ("value", "numeric_value", "amount", "reported_value"):
        value = obj.get(key)
        if value is not None:
            return str(value)
    return None


def _metric_lookup_numeric_value(value: str | None) -> float | None:
    normalized = str(value or "").strip().replace(",", "")
    if not normalized:
        return None
    if normalized.startswith("(") and normalized.endswith(")"):
        normalized = "-" + normalized[1:-1].strip()
    try:
        parsed = float(normalized)
    except ValueError:
        return None
    return parsed if math.isfinite(parsed) else None


def _metric_lookup_unit(obj: Mapping[str, Any]) -> str | None:
    for key in ("unit", "unit_ref", "currency", "scale"):
        value = obj.get(key)
        if value:
            return str(value)
    return None


def _metric_lookup_normalize_dimensions(raw_dimensions: Any) -> dict[str, str]:
    dimensions: dict[str, str] = {}

    def put(key: Any, value: Any) -> None:
        if key is None or value is None:
            return
        key_text = _metric_lookup_clean_dimension_label(key)
        value_text = _metric_lookup_clean_dimension_label(value)
        if key_text and value_text:
            dimensions[key_text] = value_text

    def walk(value: Any, fallback_key: Any = None) -> None:
        if isinstance(value, Mapping):
            axis = (
                value.get("axis") or value.get("dimension") or value.get("key") or value.get("name")
            )
            member = (
                value.get("member")
                or value.get("value")
                or value.get("label")
                or value.get("member_label")
                or value.get("dimension_value")
            )
            if axis and member:
                put(axis, member)
                return
            for key, child in value.items():
                if isinstance(child, (Mapping, list, tuple)):
                    walk(child, key)
                else:
                    put(key, child)
            return
        if isinstance(value, (list, tuple)):
            for item in value:
                if isinstance(item, str) and fallback_key is None:
                    put(item, item)
                else:
                    walk(item, fallback_key)
            return
        if fallback_key is not None:
            put(fallback_key, value)

    walk(raw_dimensions)
    return dimensions


def _metric_lookup_clean_dimension_label(value: Any) -> str:
    text = str(value or "").strip()
    if not text:
        return ""
    text = text.split("#")[-1].split("/")[-1].split(":")[-1]
    text = re.sub(r"(?:Member|Axis|Domain)$", "", text)
    text = text.replace("_", " ").replace("-", " ")
    text = re.sub(r"(?<=[A-Z])(?=[A-Z][a-z])", " ", text)
    text = re.sub(r"(?<=[a-z0-9])(?=[A-Z])", " ", text)
    text = re.sub(r"\s+", " ", text).strip()
    return text


def _metric_dimension_key(value: Any) -> str:
    return normalize_dimension_key(value)


def _metric_dimension_kind(axis: Any, label: Any) -> str:
    axis_key = _metric_dimension_key(axis)
    if any(term in axis_key for term in ("geograph", "region", "country", "area")):
        return "geography"
    if any(term in axis_key for term in ("product", "service", "brand")):
        return "product"
    if any(term in axis_key for term in ("segment", "business", "division")):
        return "segment"
    if any(term in axis_key for term in ("customer", "client")):
        return "customer"
    if any(term in axis_key for term in ("channel", "market")):
        return "channel"
    return "unknown"


def _metric_lookup_dimension(dimensions: Mapping[str, Any], keys: Sequence[str]) -> str | None:
    for key in keys:
        value = dimensions.get(key)
        if value:
            return _metric_lookup_clean_dimension_label(value)
    for key, value in dimensions.items():
        key_lower = str(key).lower()
        if any(marker in key_lower for marker in keys) and value:
            return _metric_lookup_clean_dimension_label(value)
    return None


def _metric_lookup_infer_dimension_from_text(
    *,
    metric_name: str,
    text: Any,
    dimensions: Mapping[str, Any],
) -> tuple[str, str] | None:
    if any(value for value in dimensions.values()):
        return None
    haystacks = [str(text or ""), metric_name]
    metric_phrases = (
        "net sales",
        "operating income",
        "operating profit",
        "gross profit",
        "gross margin",
        "operating margin",
        "membership fee revenue",
        "subscription revenue",
        "service revenue",
        "services revenue",
        "product revenue",
        "revenue",
        "revenues",
        "sales",
        "income",
        "margin",
    )
    for haystack in haystacks:
        clean = re.sub(
            r"\b(?:CY|FY)?(?:19|20)\d{2}(?:Q[1-4])?\b", " ", haystack, flags=re.IGNORECASE
        )
        clean = re.sub(
            r"[$€£¥]?\d+(?:\.\d+)?\s*(?:billion|million|thousand|bn|mm|m|b)?",
            " ",
            clean,
            flags=re.IGNORECASE,
        )
        clean = re.sub(r"\s+", " ", clean).strip()
        for phrase in metric_phrases:
            match = re.search(
                rf"\b(?P<label>[A-Za-z][A-Za-z0-9&/ .'-]{{1,80}}?)\s+{re.escape(phrase)}\b",
                clean,
                flags=re.IGNORECASE,
            )
            if not match:
                continue
            label = _metric_lookup_clean_inferred_dimension_label(match.group("label"))
            if not label:
                continue
            return (_metric_lookup_dimension_kind(label), label)
    return None


def _metric_lookup_clean_inferred_dimension_label(label: str) -> str:
    text = re.sub(
        r"\b(?:the|a|an|our|company|total|consolidated|net)\b", " ", label, flags=re.IGNORECASE
    )
    text = re.sub(
        r"\b(?:was|were|is|are|for|of|and|from|in|to|by)\b", " ", text, flags=re.IGNORECASE
    )
    text = re.sub(r"[^A-Za-z0-9&/ .'-]+", " ", text)
    text = re.sub(r"\s+", " ", text).strip(" .:-")
    if not text:
        return ""
    generic = {
        "revenue",
        "revenues",
        "sales",
        "income",
        "margin",
        "profit",
        "cost",
        "expense",
        "expenses",
        "assets",
        "liabilities",
    }
    if text.lower() in generic:
        return ""
    tokens = text.split()
    if len(tokens) > 6:
        return ""
    return text


def _metric_lookup_dimension_kind(label: str) -> str:
    return "unknown"


def _metric_lookup_is_company_total(
    *,
    metric_name: str,
    dimensions: Mapping[str, Any],
    text: Any,
) -> bool:
    if not any(value for value in dimensions.values()):
        return True
    haystack = f"{metric_name} {text}".lower()
    segment_markers = (
        "segment",
        "product",
        "geograph",
        "region",
        "country",
        "customer",
        "axis",
        "member",
    )
    if any(marker in str(key).lower() for key in dimensions for marker in segment_markers):
        return False
    for value in dimensions.values():
        value_text = str(value or "").lower()
        if (
            value_text
            and value_text not in {"total", "consolidated", "company"}
            and value_text in haystack
        ):
            return False
    if any(term in haystack for term in ("total", "consolidated", "company")):
        return True
    return False


def _metric_lookup_alias_text(
    *,
    metric_name: str,
    canonical_metric: str,
    obj: Mapping[str, Any],
    dimensions: Mapping[str, Any],
    text: Any,
) -> str:
    pieces: list[str] = [
        metric_name,
        canonical_metric,
        *metric_aliases(canonical_metric),
        str(text or ""),
    ]
    for key in (
        "label",
        "description",
        "source_label",
        "xbrl_concept",
        "concept",
        "normalization",
        "source_type",
        "calculation_type",
        "formula",
    ):
        value = obj.get(key)
        if value:
            pieces.append(str(value))
    for key, value in dimensions.items():
        if key:
            pieces.append(str(key))
            pieces.extend(_metric_dimension_aliases(key, _metric_dimension_key(key)))
        if value:
            pieces.append(str(value))
            pieces.extend(_metric_dimension_aliases(value, _metric_dimension_key(value)))
    return " ".join(piece for piece in pieces if piece)[:4000]


def _metric_dimension_aliases(label: Any, dimension_key: Any) -> set[str]:
    aliases: set[str] = set()
    label_text = _metric_lookup_clean_dimension_label(label)
    key_text = str(dimension_key or "").strip()
    key_space = key_text.replace("_", " ")
    for value in (label_text, key_text, key_space):
        value = str(value or "").strip()
        if not value:
            continue
        aliases.add(value)
        aliases.add(value.lower())
        compact = re.sub(r"[^A-Za-z0-9]+", "", value)
        if compact:
            aliases.add(compact)
            aliases.add(compact.lower())
        if value.endswith("s") and len(value) > 3:
            aliases.add(value[:-1])
            aliases.add(value[:-1].lower())
        elif re.search(r"[A-Za-z]$", value):
            aliases.add(f"{value}s")
            aliases.add(f"{value.lower()}s")
    return aliases


def _rebuild_typed_projection_lookup(conn: sqlite3.Connection, table_name: str) -> int:
    """Materialize typed serving projections for cheap candidate lookup."""
    object_types = TYPED_PROJECTION_OBJECT_TYPES[table_name]
    conn.execute(f"DELETE FROM {table_name}")
    placeholders = ",".join("?" for _ in object_types)
    rows = conn.execute(
        f"""
        SELECT
            objects.id,
            objects.type,
            objects.ticker,
            objects.document_type,
            objects.doc_type_key,
            objects.period,
            objects.text,
            objects.json,
            object_traceability.trace_status,
            object_traceability.evidence_chain_count,
            object_traceability.support_quote_count,
            object_traceability.support_claim_count
        FROM objects
        LEFT JOIN object_traceability
          ON object_traceability.object_id = objects.id
        WHERE objects.type IN ({placeholders})
          AND (objects.review_status IS NULL OR objects.review_status != 'rejected')
        """,
        object_types,
    ).fetchall()
    projection_rows: list[tuple[Any, ...]] = []
    for row in rows:
        try:
            obj = json.loads(row["json"] or "{}")
        except json.JSONDecodeError:
            obj = {}
        topic = build_company_topic_profile(dict(row), obj, str(row["text"] or "")) or {}
        common = _typed_projection_common(row, obj, topic)
        if table_name == "exposure_lookup":
            projection_rows.append(_exposure_lookup_row(row, obj, common))
        elif table_name == "agreement_lookup":
            projection_rows.append(_agreement_lookup_row(row, obj, common))
        elif table_name == "event_lookup":
            projection_rows.append(_event_lookup_row(row, obj, common))
        elif table_name == "factor_lookup":
            projection_rows.append(_factor_lookup_row(row, obj, common))
        if len(projection_rows) >= BULK_INSERT_CHUNK_SIZE:
            _flush_typed_projection_batch(conn, table_name, projection_rows)
    _flush_typed_projection_batch(conn, table_name, projection_rows)
    return len(rows)


def _typed_projection_common(
    row: sqlite3.Row,
    obj: Mapping[str, Any],
    topic: Mapping[str, Any],
) -> dict[str, Any]:
    source_text = str(row["text"] or _projection_object_values_text(obj) or "")
    lookup_text = _compact_space(
        " ".join(
            str(value or "")
            for value in (
                source_text,
                topic.get("topic_label"),
                topic.get("topic_summary"),
                topic.get("facet_text"),
                _projection_object_values_text(obj),
            )
        )
    )[:4000]
    return {
        "object_id": row["id"],
        "object_type": row["type"],
        "ticker": row["ticker"],
        "document_type": row["document_type"],
        "doc_type_key": row["doc_type_key"],
        "period": row["period"],
        "trace_status": row["trace_status"],
        "evidence_strength": topic.get("evidence_strength") or obj.get("evidence_grade"),
        "specificity_score": topic.get("specificity_score"),
        "generic_score": topic.get("generic_score"),
        "boilerplate_score": topic.get("boilerplate_score"),
        "support_quote_count": max(
            int(topic.get("support_quote_count") or 0), int(row["support_quote_count"] or 0)
        ),
        "support_claim_count": max(
            int(topic.get("support_claim_count") or 0), int(row["support_claim_count"] or 0)
        ),
        "evidence_chain_count": int(row["evidence_chain_count"] or 0),
        "lookup_text": lookup_text,
        "source_text": source_text[:2000],
        "topic_family": topic.get("topic_family"),
        "topic_label": topic.get("topic_label"),
        "topic_summary": topic.get("topic_summary"),
        "impact_channels": " ".join(topic.get("impact_channels") or []),
    }


def _exposure_lookup_row(
    row: sqlite3.Row, obj: Mapping[str, Any], common: Mapping[str, Any]
) -> tuple[Any, ...]:
    factor = _typed_projection_text(obj, ("factor", "external_factor", "factor_category", "name"))
    benchmark = _typed_projection_text(
        obj, ("benchmark", "benchmark_hint", "index", "price_benchmark")
    )
    impact_channel = _typed_projection_text(
        obj, ("impact_channel", "affected_channel", "affected_channels")
    )
    mechanism = _typed_projection_text(obj, ("mechanism", "description", "scenario_effects"))
    scenario_terms = _typed_projection_text(
        obj, ("scenario_terms", "scenario_effects", "effect_direction", "direction")
    )
    return (
        *(_common_projection_values(common)),
        factor,
        benchmark,
        impact_channel,
        mechanism,
        scenario_terms,
        _join_projection_parts(factor, benchmark, mechanism),
        _join_projection_parts(impact_channel, scenario_terms, common["lookup_text"]),
        _join_projection_parts(factor, benchmark),
        common["trace_status"],
        common["evidence_strength"],
        common["specificity_score"],
        common["generic_score"],
        common["boilerplate_score"],
        common["support_quote_count"],
        common["support_claim_count"],
        common["evidence_chain_count"],
        common["lookup_text"],
        common["source_text"],
    )


def _agreement_lookup_row(
    row: sqlite3.Row, obj: Mapping[str, Any], common: Mapping[str, Any]
) -> tuple[Any, ...]:
    return (
        *(_common_projection_values(common)),
        _typed_projection_text(obj, ("agreement_type", "contract_type", "type_name")),
        _typed_projection_text(obj, ("agreement_subtype", "economic_role", "role")),
        _typed_projection_text(
            obj, ("counterparty", "counterparties", "customer", "supplier", "lender")
        ),
        _typed_projection_text(obj, ("amount", "value", "notional_amount", "commitment_amount")),
        _typed_projection_text(
            obj, ("maturity_date", "expiration_date", "end_date", "termination_date")
        ),
        _typed_projection_text(
            obj, ("termination_terms", "termination", "default_terms", "termination_rights")
        ),
        _typed_projection_text(obj, ("covenant_terms", "covenants", "financial_covenants")),
        _typed_projection_text(obj, ("collateral_terms", "collateral", "security", "lien")),
        _typed_projection_text(obj, ("affected_channels", "impact_channel", "economic_role")),
        common["trace_status"],
        common["evidence_strength"],
        common["specificity_score"],
        common["generic_score"],
        common["boilerplate_score"],
        common["support_quote_count"],
        common["support_claim_count"],
        common["evidence_chain_count"],
        common["lookup_text"],
        common["source_text"],
    )


def _event_lookup_row(
    row: sqlite3.Row, obj: Mapping[str, Any], common: Mapping[str, Any]
) -> tuple[Any, ...]:
    event_date = _typed_projection_text(obj, ("event_date", "date", "actual_date"))
    date_expression = _typed_projection_text(
        obj, ("date_expression", "target_date", "expected_date", "timing")
    )
    return (
        *(_common_projection_values(common)),
        _typed_projection_text(obj, ("event_type", "event_subtype", "change_type")),
        _typed_projection_text(obj, ("event_status", "status", "completion_status")),
        event_date,
        date_expression,
        _typed_projection_date_sort_key(event_date or date_expression or row["period"]),
        _typed_projection_text(
            obj, ("project_or_product", "project", "product", "asset", "subject")
        ),
        _typed_projection_text(obj, ("regulatory_body", "agency", "regulator", "authority")),
        _typed_projection_text(obj, ("affected_channels", "impact_channel", "affected_objects")),
        common["trace_status"],
        common["evidence_strength"],
        common["specificity_score"],
        common["generic_score"],
        common["boilerplate_score"],
        common["support_quote_count"],
        common["support_claim_count"],
        common["evidence_chain_count"],
        common["lookup_text"],
        common["source_text"],
    )


def _factor_lookup_row(
    row: sqlite3.Row, obj: Mapping[str, Any], common: Mapping[str, Any]
) -> tuple[Any, ...]:
    return (
        *(_common_projection_values(common)),
        _typed_projection_text(obj, ("factor_type", "factor_roles", "category")),
        common["topic_family"],
        _typed_projection_text(obj, ("impact_channel", "affected_channels", "primary_channel")),
        _typed_projection_text(obj, ("business_area", "segment", "product", "activity")),
        _typed_projection_text(obj, ("risk_or_driver", "factor_roles", "category", "name")),
        common["topic_label"],
        common["topic_summary"],
        common["trace_status"],
        common["evidence_strength"],
        common["specificity_score"],
        common["generic_score"],
        common["boilerplate_score"],
        common["support_quote_count"],
        common["support_claim_count"],
        common["evidence_chain_count"],
        common["lookup_text"],
        common["source_text"],
    )


def _common_projection_values(common: Mapping[str, Any]) -> tuple[Any, ...]:
    return (
        common["object_id"],
        common["object_type"],
        common["ticker"],
        common["document_type"],
        common["doc_type_key"],
        common["period"],
    )


def _flush_typed_projection_batch(
    conn: sqlite3.Connection,
    table_name: str,
    rows: list[tuple[Any, ...]],
) -> None:
    if not rows:
        return
    insert_sql = {
        "exposure_lookup": """
            INSERT OR REPLACE INTO exposure_lookup(
                object_id, object_type, ticker, document_type, doc_type_key, period,
                factor, benchmark, impact_channel, mechanism, scenario_terms,
                direct_anchor_text, related_context_text, negative_guard_terms,
                trace_status, evidence_strength, specificity_score, generic_score,
                boilerplate_score, support_quote_count, support_claim_count,
                evidence_chain_count, lookup_text, source_text
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        "agreement_lookup": """
            INSERT OR REPLACE INTO agreement_lookup(
                object_id, object_type, ticker, document_type, doc_type_key, period,
                agreement_type, agreement_subtype, counterparty, amount, maturity_date,
                termination_terms, covenant_terms, collateral_terms, affected_channels,
                trace_status, evidence_strength, specificity_score, generic_score,
                boilerplate_score, support_quote_count, support_claim_count,
                evidence_chain_count, lookup_text, source_text
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        "event_lookup": """
            INSERT OR REPLACE INTO event_lookup(
                object_id, object_type, ticker, document_type, doc_type_key, period,
                event_type, event_status, event_date, date_expression, date_sort_key,
                project_or_product, regulatory_body, affected_channels,
                trace_status, evidence_strength, specificity_score, generic_score,
                boilerplate_score, support_quote_count, support_claim_count,
                evidence_chain_count, lookup_text, source_text
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        "factor_lookup": """
            INSERT OR REPLACE INTO factor_lookup(
                object_id, object_type, ticker, document_type, doc_type_key, period,
                factor_type, topic_family, impact_channel, business_area, risk_or_driver,
                topic_label, topic_summary, trace_status, evidence_strength,
                specificity_score, generic_score, boilerplate_score,
                support_quote_count, support_claim_count, evidence_chain_count,
                lookup_text, source_text
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
    }[table_name]
    conn.executemany(insert_sql, rows)
    rows.clear()


def _typed_projection_text(obj: Mapping[str, Any], keys: Sequence[str]) -> str | None:
    values: list[str] = []
    for key in keys:
        value = obj.get(key)
        if value is None:
            continue
        if isinstance(value, str):
            values.append(value)
        elif isinstance(value, Mapping):
            values.extend(str(item) for item in value.values() if item)
        elif isinstance(value, Sequence) and not isinstance(value, (str, bytes, bytearray)):
            values.extend(str(item) for item in value if item)
        else:
            values.append(str(value))
    text = _compact_space(" ".join(values))
    return text[:1000] if text else None


def _join_projection_parts(*values: Any) -> str:
    return _compact_space(" ".join(str(value) for value in values if value))


def _projection_object_values_text(obj: Mapping[str, Any]) -> str:
    values: list[str] = []
    for key, value in obj.items():
        if key in {"id", "type", "json"}:
            continue
        if isinstance(value, str):
            values.append(value)
        elif isinstance(value, Mapping):
            values.extend(
                str(item) for item in value.values() if isinstance(item, (str, int, float))
            )
        elif isinstance(value, Sequence) and not isinstance(value, (str, bytes, bytearray)):
            values.extend(str(item) for item in value if isinstance(item, (str, int, float)))
        elif isinstance(value, (int, float)):
            values.append(str(value))
    return _compact_space(" ".join(values))


def _typed_projection_date_sort_key(value: Any) -> str | None:
    text = str(value or "")
    match = re.search(r"(20\d{2}|19\d{2})(?:[-/ ]?(0[1-9]|1[0-2]))?(?:[-/ ]?([0-3]\d))?", text)
    if not match:
        return None
    year, month, day = match.group(1), match.group(2) or "00", match.group(3) or "00"
    return f"{year}-{month}-{day}"


def _rebuild_company_topic_index(conn: sqlite3.Connection) -> int:
    """Build serving-only company topic profiles from indexed evidence text."""
    conn.execute("DELETE FROM company_topic_index")
    conn.execute("DELETE FROM company_topic_fts")
    conn.execute("DELETE FROM company_topic_source_objects")
    placeholders = ",".join("?" for _ in COMPANY_TOPIC_OBJECT_TYPES)
    params = tuple(sorted(COMPANY_TOPIC_OBJECT_TYPES))
    candidate_count = conn.execute(
        f"""
        SELECT COUNT(*)
        FROM objects
        WHERE objects.type IN ({placeholders})
        """,
        params,
    ).fetchone()[0]
    _log_build_phase("company_topic_candidates", candidates=candidate_count)
    rows = conn.execute(
        f"""
        SELECT
            objects.id,
            objects.type,
            objects.ticker,
            objects.period,
            objects.document_type,
            objects.doc_type_key,
            objects.json,
            object_search_text.text_self,
            object_search_text.text_support,
            object_search_text.text_related,
            object_search_text.text_entities,
            object_search_text.text_aliases,
            object_search_text.compact_text,
            object_traceability.trace_status,
            object_traceability.evidence_chain_count,
            object_traceability.support_depth,
            object_traceability.support_quote_count AS trace_support_quote_count,
            object_traceability.support_claim_count AS trace_support_claim_count,
            object_traceability.support_link_count,
            object_traceability.trace_method,
            object_traceability.metric_lineage_status,
            object_traceability.answer_candidate
        FROM objects
        LEFT JOIN object_search_text ON object_search_text.object_id = objects.id
        LEFT JOIN object_traceability ON object_traceability.object_id = objects.id
        WHERE objects.type IN ({placeholders})
        ORDER BY objects.ticker, objects.id
        """,
        params,
    )
    count = 0
    topic_rows: list[tuple[Any, ...]] = []
    topic_fts_rows: list[tuple[Any, ...]] = []
    topic_source_rows: list[tuple[Any, ...]] = []
    for row in rows:
        obj = json.loads(row["json"])
        retrieval_text = _topic_retrieval_text(row)
        topic = build_company_topic_profile(dict(row), obj, retrieval_text)
        if topic is None:
            continue
        source_object_ids = list(dict.fromkeys(topic.get("source_object_ids") or []))
        trace_status = row["trace_status"] or "unknown"
        if trace_status in {"traceable", "traceable_metric_lineage"}:
            top_traceable_object_ids = source_object_ids[:5]
            untraced_object_ids: list[str] = []
        else:
            top_traceable_object_ids = []
            untraced_object_ids = source_object_ids[:5]
        support_metric_count = int(topic.get("support_metric_count") or 0)
        if topic.get("primary_object_type") in {"MetricObservation", "Calculation"}:
            support_metric_count = max(support_metric_count, 1)
        topic_rows.append(
            (
                topic["topic_id"],
                topic["ticker"],
                topic.get("period"),
                topic.get("document_type"),
                topic.get("doc_type_key"),
                topic.get("document_type"),
                topic.get("topic_label"),
                topic.get("topic_summary"),
                topic.get("topic_type"),
                topic.get("topic_family"),
                topic.get("topic_text"),
                topic.get("facet_text"),
                topic.get("primary_object_id"),
                topic.get("primary_object_type"),
                json.dumps(source_object_ids, ensure_ascii=False),
                json.dumps(top_traceable_object_ids, ensure_ascii=False),
                json.dumps(untraced_object_ids, ensure_ascii=False),
                json.dumps(topic.get("dominant_object_types") or [], ensure_ascii=False),
                json.dumps(topic.get("impact_channels") or [], ensure_ascii=False),
                json.dumps(topic.get("factor_terms") or [], ensure_ascii=False),
                json.dumps(topic.get("metric_terms") or [], ensure_ascii=False),
                json.dumps(topic.get("entity_terms") or [], ensure_ascii=False),
                json.dumps(topic.get("mechanism_terms") or [], ensure_ascii=False),
                json.dumps(topic.get("scenario_terms") or [], ensure_ascii=False),
                topic.get("evidence_strength"),
                topic.get("materiality_hint"),
                topic.get("materiality_score"),
                topic.get("specificity_score"),
                topic.get("generic_score"),
                topic.get("boilerplate_score"),
                max(
                    int(topic.get("support_quote_count") or 0),
                    int(row["trace_support_quote_count"] or 0),
                ),
                max(
                    int(topic.get("support_claim_count") or 0),
                    int(row["trace_support_claim_count"] or 0),
                ),
                support_metric_count,
                trace_status,
                int(row["evidence_chain_count"] or 0),
                row["support_depth"],
                int(row["support_link_count"] or 0),
                row["trace_method"],
                row["metric_lineage_status"],
                int(row["answer_candidate"] or 0),
                "object_search_text",
                COMPANY_TOPIC_BUILDER_VERSION,
            )
        )
        for rank, source_id in enumerate(source_object_ids):
            role = _topic_source_role(source_id, topic.get("primary_object_id"))
            topic_source_rows.append(
                (
                    topic["topic_id"],
                    source_id,
                    _topic_source_object_type(
                        source_id, topic.get("primary_object_type") if role == "primary" else None
                    ),
                    role,
                    rank,
                    trace_status if role == "primary" else None,
                    int(row["evidence_chain_count"] or 0) if role == "primary" else 0,
                )
            )
        topic_fts_rows.append(
            (
                topic["topic_id"],
                topic["ticker"],
                topic.get("period"),
                topic.get("topic_label"),
                topic.get("topic_summary"),
                _company_topic_evidence_text(topic, retrieval_text),
                _compact_space(
                    " ".join(
                        [
                            _topic_scope_token_text(topic),
                            " ".join(topic.get("entity_terms") or []),
                        ]
                    )
                ),
                " ".join(
                    list(topic.get("impact_channels") or [])
                    + list(topic.get("factor_terms") or [])
                    + list(topic.get("metric_terms") or [])
                    + list(topic.get("mechanism_terms") or [])
                    + list(topic.get("scenario_terms") or [])
                ),
                topic.get("facet_text"),
                _compact_space(
                    " ".join(
                        str(value or "")
                        for value in (
                            _topic_scope_token_text(topic),
                            topic.get("topic_label"),
                            topic.get("topic_summary"),
                            str(topic.get("topic_text") or "")[:COMPANY_TOPIC_FTS_CHAR_LIMIT],
                            topic.get("facet_text"),
                            topic.get("evidence_strength"),
                            topic.get("trace_status"),
                        )
                    )
                )[:COMPANY_TOPIC_FTS_CHAR_LIMIT],
            )
        )
        count += 1
        if len(topic_rows) >= COMPANY_TOPIC_BATCH_SIZE:
            _flush_company_topic_batch(conn, topic_rows, topic_fts_rows, topic_source_rows)
            conn.commit()
            _log_build_phase(
                "company_topic_batch_done",
                indexed=count,
                candidates=candidate_count,
            )
    _flush_company_topic_batch(conn, topic_rows, topic_fts_rows, topic_source_rows)
    conn.commit()
    return count


def _object_fts_row_with_scope_tokens(row: tuple[Any, ...]) -> tuple[Any, ...]:
    """Add indexed scope tokens to FTS text while preserving normal columns.

    The ticker/document/period/type columns on FTS tables are UNINDEXED. These
    tokens let MATCH narrow candidates at the FTS stage, while the normal SQL
    filters still enforce correctness.
    """
    (
        object_id,
        object_type,
        ticker,
        document_type,
        period,
        text_self,
        text_support,
        text_related,
        text_entities,
        text_aliases,
        compact_text,
    ) = row
    scope_text = _scope_token_text(
        ticker=ticker,
        document_type=document_type,
        period=period,
        object_type=object_type,
    )
    return (
        object_id,
        object_type,
        ticker,
        document_type,
        period,
        text_self,
        text_support,
        text_related,
        _compact_space(" ".join([scope_text, str(text_entities or "")])),
        text_aliases,
        _compact_space(" ".join([scope_text, str(compact_text or "")])),
    )


def _topic_scope_token_text(topic: Mapping[str, Any]) -> str:
    return _scope_token_text(
        ticker=topic.get("ticker"),
        document_type=topic.get("document_type"),
        period=topic.get("period"),
        object_type=topic.get("primary_object_type"),
        topic_family=topic.get("topic_family"),
    )


def _scope_token_text(
    *,
    ticker: Any = None,
    document_type: Any = None,
    period: Any = None,
    object_type: Any = None,
    topic_family: Any = None,
) -> str:
    tokens = [
        _scope_token("ticker", ticker),
        _scope_token("doctype", document_type),
        _scope_token("period", period),
        _scope_token("otype", object_type),
        _scope_token("family", topic_family),
    ]
    return " ".join(token for token in tokens if token)


def _scope_token(prefix: str, value: Any) -> str:
    normalized = re.sub(r"[^a-z0-9]+", "_", str(value or "").lower()).strip("_")
    if not normalized:
        return ""
    return f"{prefix}_{normalized}"


def _topic_retrieval_text(row: sqlite3.Row) -> str:
    parts: list[str] = []
    for key in (
        "text_self",
        "text_support",
        "text_related",
        "text_entities",
        "text_aliases",
        "compact_text",
    ):
        value = str(row[key] or "")
        if value:
            parts.append(value[:COMPANY_TOPIC_FIELD_CHAR_LIMIT])
    return "\n".join(parts)[:COMPANY_TOPIC_TEXT_CHAR_LIMIT]


def _company_topic_evidence_text(topic: dict[str, Any], retrieval_text: str) -> str:
    return _compact_space(
        " ".join(
            str(value or "")
            for value in (
                topic.get("topic_label"),
                topic.get("topic_summary"),
                topic.get("facet_text"),
                retrieval_text,
                topic.get("evidence_strength"),
                topic.get("materiality_hint"),
                " ".join(topic.get("impact_channels") or []),
                " ".join(topic.get("factor_terms") or []),
                " ".join(topic.get("metric_terms") or []),
                " ".join(topic.get("entity_terms") or []),
                " ".join(topic.get("mechanism_terms") or []),
                " ".join(topic.get("scenario_terms") or []),
            )
        )
    )[:COMPANY_TOPIC_FTS_CHAR_LIMIT]


def _topic_source_role(source_id: str, primary_object_id: Any) -> str:
    if source_id == primary_object_id:
        return "primary"
    normalized = str(source_id or "").lower()
    if "quote" in normalized:
        return "support"
    if "claim" in normalized:
        return "support"
    if "metric" in normalized or "calculation" in normalized or "xbrl" in normalized:
        return "metric"
    if "agreement" in normalized or "contract" in normalized or "covenant" in normalized:
        return "agreement"
    if "event" in normalized or "milestone" in normalized:
        return "event"
    return "related"


def _topic_source_object_type(source_id: str, fallback: Any = None) -> str:
    if fallback:
        return str(fallback)
    normalized = str(source_id or "").lower()
    prefix_map = (
        ("evidence_quote", "EvidenceQuote"),
        ("quote", "EvidenceQuote"),
        ("research_claim", "ResearchClaim"),
        ("claim", "ResearchClaim"),
        ("metric_observation", "MetricObservation"),
        ("metric", "MetricObservation"),
        ("calculation", "Calculation"),
        ("xbrl", "XBRLFact"),
        ("agreement", "AgreementTerm"),
        ("contract", "AgreementTerm"),
        ("business_event", "BusinessEvent"),
        ("event", "BusinessEvent"),
        ("external_factor_exposure", "ExternalFactorExposure"),
        ("exposure", "ExternalFactorExposure"),
        ("business_factor", "BusinessFactor"),
        ("factor", "BusinessFactor"),
        ("business_activity", "BusinessActivity"),
        ("activity", "BusinessActivity"),
    )
    for marker, object_type in prefix_map:
        if marker in normalized:
            return object_type
    return "Unknown"


def _flush_company_topic_batch(
    conn: sqlite3.Connection,
    topic_rows: list[tuple[Any, ...]],
    topic_fts_rows: list[tuple[Any, ...]],
    topic_source_rows: list[tuple[Any, ...]],
) -> None:
    if topic_rows:
        conn.executemany(
            """
            INSERT OR REPLACE INTO company_topic_index(
                topic_id, ticker, period, document_type, doc_type_key,
                filing_type, topic_label, topic_summary, topic_type, topic_family,
                topic_text, facet_text, primary_object_id, primary_object_type,
                source_object_ids, top_traceable_object_ids, untraced_object_ids,
                dominant_object_types, impact_channels, factor_terms, metric_terms,
                entity_terms, mechanism_terms, scenario_terms, evidence_strength,
                materiality_hint, materiality_score, specificity_score, generic_score,
                boilerplate_score, support_quote_count, support_claim_count,
                support_metric_count, trace_status, evidence_chain_count, support_depth,
                support_link_count, trace_method, metric_lineage_status,
                answer_candidate, created_from, builder_version
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            topic_rows,
        )
    if topic_source_rows:
        conn.executemany(
            """
            INSERT OR REPLACE INTO company_topic_source_objects(
                topic_id, object_id, object_type, role, rank,
                trace_status, evidence_chain_count
            )
            VALUES (?, ?, ?, ?, ?, ?, ?)
            """,
            topic_source_rows,
        )
    if topic_fts_rows:
        conn.executemany(
            """
            INSERT INTO company_topic_fts(
                topic_id, ticker, period, text_label, text_summary, text_evidence,
                text_entities, text_channels, text_aliases, compact_text
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            topic_fts_rows,
        )
    topic_rows.clear()
    topic_fts_rows.clear()
    topic_source_rows.clear()


def _index_edges(
    conn: sqlite3.Connection,
    path: Path | None,
    ticker: str,
    document_type: str,
    doc_type_key: str,
    period: str,
    *,
    rows: list[dict[str, Any]] | None = None,
) -> int:
    if not path and rows is None:
        return 0
    count = 0
    edge_rows_to_insert: list[tuple[Any, ...]] = []
    for edge in rows if rows is not None else read_jsonl(path):
        if not edge.get("id") or edge.get("type") != "Edge":
            continue
        edge_rows_to_insert.append(
            (
                edge["id"],
                edge.get("ticker", ticker),
                edge.get("document_type", document_type),
                doc_type_key,
                edge.get("period", period),
                edge.get("source_document_id"),
                edge.get("from_id"),
                edge.get("to_id"),
                edge.get("relation_id"),
                edge.get("relation_name"),
                edge.get("confidence"),
                edge.get("review_status"),
                json.dumps(edge, ensure_ascii=False),
                str(path or ""),
            )
        )
        count += 1
        if len(edge_rows_to_insert) >= BULK_INSERT_CHUNK_SIZE:
            _flush_edge_batch(conn, edge_rows_to_insert)
    _flush_edge_batch(conn, edge_rows_to_insert)
    return count


def _flush_edge_batch(conn: sqlite3.Connection, rows: list[tuple[Any, ...]]) -> None:
    if not rows:
        return
    conn.executemany(
        """
        INSERT OR REPLACE INTO edges(
            id, ticker, document_type, doc_type_key, period, source_document_id,
            from_id, to_id, relation_id, relation_name, confidence,
            review_status, json, artifact_path
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        rows,
    )
    rows.clear()


def _insert_quality_event(
    conn: sqlite3.Connection,
    *,
    ticker: str,
    document_type: str,
    doc_type_key: str,
    period: str,
    severity: str,
    category: str,
    object_id: str | None,
    stage: str | None,
    message: str,
    payload: dict[str, Any],
) -> None:
    event_id = ":".join(
        part
        for part in (
            "quality",
            ticker,
            period,
            doc_type_key,
            category,
            object_id or stage or message[:40],
        )
        if part
    )
    conn.execute(
        """
        INSERT OR REPLACE INTO quality_events(
            id, ticker, document_type, doc_type_key, period, severity, category,
            object_id, stage, message, json
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        (
            event_id,
            ticker,
            document_type,
            doc_type_key,
            period,
            severity,
            category,
            object_id,
            stage,
            message,
            json.dumps(payload, ensure_ascii=False),
        ),
    )


def _object_text(obj: dict[str, Any]) -> str:
    keys = TEXT_KEYS_BY_TYPE.get(obj.get("type"), ())
    parts: list[str] = []
    for key in keys:
        value = obj.get(key)
        if value is not None:
            parts.append(str(value))
    if obj.get("related_metrics"):
        parts.extend(str(metric) for metric in obj["related_metrics"])
    if obj.get("affects"):
        parts.extend(str(metric) for metric in obj["affects"])
    return _NON_WORD_RE.sub(" ", " ".join(parts)).strip()


def _taxonomy_by_id(rows: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    taxonomy: dict[str, dict[str, Any]] = {}
    for row in rows:
        for key in (
            row.get("id"),
            row.get("term_id"),
            row.get("term_key"),
            row.get("canonical_name"),
        ):
            if key:
                taxonomy[str(key)] = row
    return taxonomy


def _registry_version(conn: sqlite3.Connection) -> str | None:
    row = conn.execute(
        """
        SELECT json
        FROM objects
        WHERE type = 'OntologyRegistrySnapshot'
        ORDER BY period DESC, id
        LIMIT 1
        """
    ).fetchone()
    if not row:
        return None
    try:
        return json.loads(row["json"]).get("registry_version")
    except (TypeError, json.JSONDecodeError):
        return None


def _artifact_manifest_hash(artifact_indexes: list[Path]) -> str:
    digest = hashlib.sha256()
    for path in artifact_indexes:
        digest.update(str(path).encode("utf-8"))
        try:
            digest.update(path.read_bytes())
        except OSError:
            continue
    return f"sha256:{digest.hexdigest()}"


def _read_section_quality(ontology_dir: Path) -> dict[str, Any]:
    path = ontology_dir / "section_quality.json"
    if not path.exists():
        return {}
    try:
        return json.loads(path.read_text())
    except (json.JSONDecodeError, OSError):
        return {"status": "unknown", "fail_reasons": ["unreadable_section_quality"]}


def _section_quality_message(section_quality: dict[str, Any]) -> str:
    status = section_quality.get("status", "unknown")
    fail_reasons = section_quality.get("fail_reasons") or []
    missing_core = section_quality.get("missing_core_sections") or []
    pieces = [f"section_quality={status}"]
    if fail_reasons:
        pieces.append(f"fail_reasons={','.join(fail_reasons)}")
    if missing_core:
        pieces.append(f"missing_core_sections={','.join(missing_core)}")
    return "; ".join(pieces)


def _resolve_artifact_path(root: Path, rel_path: str | None) -> Path | None:
    if not rel_path:
        return None
    path = Path(rel_path)
    if path.is_absolute():
        return path
    return root / path
