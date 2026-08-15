"""Read-only MCP tool implementations over the v3 ontology release."""

from __future__ import annotations

import copy
from collections import OrderedDict
from dataclasses import dataclass
from enum import Enum
import hashlib
import json
import logging
import os
import re
import sqlite3
import threading
import time
from pathlib import Path
from typing import Any, Mapping, Sequence

from krw_capability_runtime.agent_index import (
    AgentRetriever,
    CHART_SERIES_RELATIVE_PATH,
    GLOBAL_SPINE_RELATIVE_PATH,
    QueryPlan,
    chart_series_runtime_enabled,
    open_ontology_store,
    query_chart_series_pack,
)
from krw_capability_runtime.agent_index.retrieval_text import format_metric_compact
from krw_capability_runtime.agent_index.research_kernel import ResearchKernel, research_request_from_plan_fields
from krw_capability_runtime.agent_index.store import (
    DEFAULT_QUERY_TYPES,
    OntologyStore,
    agent_index_cache_status,
    filing_document_roles_from_documents,
)
from krw_capability_runtime.config.paths import (
    ONTOLOGY_GLOBAL_SPINE_PATH_ENV,
    resolve_ontology_root,
)
from krw_capability_runtime.mcp_server.evidence_pack import (
    EvidencePackInputError,
    build_verified_company_evidence_pack,
)
from krw_capability_runtime.mcp_server.contracts import (
    MCP_CONTRACT_VERSION,
    PlanUncertainty,
    ResearchState,
    SearchPlan,
    compile_research_state,
    research_state_model_bytes,
    research_state_wire_bytes,
    validate_search_plan,
)
from krw_capability_runtime.release import verify_release_startup_v3


class ResponseFormat(str, Enum):
    """Supported MCP response formats."""

    JSON = "json"
    MARKDOWN = "markdown"


class ResponseDetail(str, Enum):
    """How much object/evidence detail to return from search-style tools."""

    IDS_ONLY = "ids_only"
    COMPACT = "compact"
    TICKER_SUMMARY = "ticker_summary"
    FULL = "full"


LOGGER = logging.getLogger(__name__)
SLOW_MCP_TOOL_LOG_THRESHOLD_MS = 5_000
MAX_CHAIN_RESPONSE_MODEL_BYTES = 60_000
CHAIN_RESPONSE_BUDGET_FORMAT = "krw-ontology-chain-response-budget/v1"
_SLOW_MCP_TOOL_LOG_MARKER = "[krw-ontology:mcp-slow-path]"
_MCP_TOOL_TELEMETRY_MARKER = "[krw-ontology:mcp-telemetry]"
_TRACE_TOOL_CACHE_MAX = 512
_TRACE_TOOL_CACHE_LOCK = threading.Lock()
_MCP_STORE_MODE_ENV = "KRW_MCP_STORE_MODE"
_MCP_STORE_POOL_MAX_ENV = "KRW_MCP_STORE_POOL_MAX"
_MCP_STORE_MODE_PERSISTENT = "persistent"
_DEFAULT_MCP_STORE_POOL_MAX = 8
_REPEATED_RETRIEVE_PRIOR_CALL_THRESHOLD = 2
_REPEATED_RETRIEVE_GUIDANCE_MESSAGE = (
    "You have already used krw_ontology_retrieve multiple times. "
    "Prefer query_context, targeted query, trace, or chain for the next step unless another "
    "retrieve is clearly necessary. Do not repeat broad retrieve calls."
)


@dataclass(frozen=True)
class ExecutionScope:
    """Physical filing scope derived from what the user actually requested.

    ``SearchPlan`` is model-authored and is intentionally useful for semantic
    retrieval.  It must not be treated as a source of truth for a hard filing
    period or document filter when the user only said "recent".  That used to
    make a planner-guessed FY/10-K silently exclude the current 10-Q before
    the first read even began.
    """

    document_types: list[str] | None
    periods: list[str] | None
    # Narrative objects are keyed by filing/calendar buckets, whereas a
    # MetricObservation carries the issuer's fiscal observation coordinate.
    # Keep these filters separate so a requested FY quarter is not either
    # coerced into a wrong CY quarter or dropped altogether.
    metric_periods: list[str] | None
    current_intent: bool


@dataclass(frozen=True)
class _IndexSignature:
    resolved_global_spine_path: str
    mtime_ns: int | None
    size: int | None
    release_id: str | None
    global_spine_sha256: str | None
    shard_manifest_sha256: str | None
    router_sidecar_sha256: str | None


_TRACE_TOOL_CACHE: OrderedDict[
    tuple[_IndexSignature, str, str | None],
    dict[str, Any],
] = OrderedDict()


def _read_int_env(name: str, default: int, *, min_value: int) -> int:
    raw = os.getenv(name)
    if raw is None or raw.strip() == "":
        return default
    try:
        value = int(raw)
    except ValueError:
        return default
    return value if value >= min_value else default


def _store_mode() -> str:
    return os.getenv(_MCP_STORE_MODE_ENV, "per_call").strip().lower() or "per_call"


def _persistent_store_enabled() -> bool:
    return _store_mode() == _MCP_STORE_MODE_PERSISTENT


def _index_signature(index_path: Path) -> _IndexSignature:
    resolved_path = index_path.expanduser().resolve()
    (
        release_id,
        global_spine_sha256,
        shard_manifest_sha256,
        router_sidecar_sha256,
    ) = _manifest_signature_fields(resolved_path)
    try:
        stat = resolved_path.stat()
    except OSError:
        return _IndexSignature(
            str(resolved_path),
            None,
            None,
            release_id,
            global_spine_sha256,
            shard_manifest_sha256,
            router_sidecar_sha256,
        )
    return _IndexSignature(
        str(resolved_path),
        stat.st_mtime_ns,
        stat.st_size,
        release_id,
        global_spine_sha256,
        shard_manifest_sha256,
        router_sidecar_sha256,
    )


def _manifest_signature_fields(
    resolved_index_path: Path,
) -> tuple[str | None, str | None, str | None, str | None]:
    manifest_path = resolved_index_path.parent.parent / "manifest.json"
    try:
        payload = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None, None, None, None
    if not isinstance(payload, Mapping):
        return None, None, None, None
    indexes = payload.get("indexes") if isinstance(payload.get("indexes"), Mapping) else {}
    global_spine = (
        indexes.get("global_spine") if isinstance(indexes.get("global_spine"), Mapping) else {}
    )
    shard_manifest = (
        indexes.get("shard_manifest") if isinstance(indexes.get("shard_manifest"), Mapping) else {}
    )
    router_sidecar = (
        indexes.get("router_sidecar") if isinstance(indexes.get("router_sidecar"), Mapping) else {}
    )
    release_id = payload.get("release_id")
    return (
        str(release_id) if release_id else None,
        str(global_spine.get("sha256")) if global_spine.get("sha256") else None,
        str(shard_manifest.get("sha256")) if shard_manifest.get("sha256") else None,
        str(router_sidecar.get("sha256")) if router_sidecar.get("sha256") else None,
    )


def _trace_cache_key(
    index_path: Path,
    object_id: str,
    ticker: str | None = None,
) -> tuple[_IndexSignature, str, str | None]:
    normalized_ticker = str(ticker or "").strip().upper() or None
    return (_index_signature(index_path), object_id, normalized_ticker)


class _StoreBucket:
    def __init__(
        self,
        *,
        logical_path: str,
        signature: _IndexSignature,
        generation: int,
    ):
        self.logical_path = logical_path
        self.signature = signature
        self.generation = generation
        self.idle: list[Any] = []
        self.leased = 0


class _PersistentStoreLease:
    def __init__(
        self,
        pool: "_PersistentStorePool",
        logical_path: str,
        signature: _IndexSignature,
        generation: int,
        store: Any,
    ):
        self._pool = pool
        self._logical_path = logical_path
        self._signature = signature
        self._generation = generation
        self._store = store

    def __enter__(self) -> Any:
        return self._store

    def __exit__(self, *_exc: object) -> None:
        self._pool.release(self._logical_path, self._signature, self._generation, self._store)


class _PersistentStorePool:
    """One sidecar-local pool for one already-admitted immutable release.

    Store replacement inside a process would make one MCP listener serve more
    than one data release.  A deployment must instead start a new sidecar when
    it promotes a release.  If an ostensibly immutable index changes, fail
    closed and expose a compact diagnostic rather than rotating in place.
    """

    def __init__(self) -> None:
        self._lock = threading.RLock()
        self._buckets: dict[str, _StoreBucket] = {}
        self._hits = 0
        self._misses = 0
        self._opened = 0
        self._closed = 0
        self._generation = 0
        self._identity_change: dict[str, Any] | None = None
        self._verified_signatures: set[_IndexSignature] = set()

    def acquire(self, index_path: Path) -> _PersistentStoreLease:
        logical_path = str(index_path.expanduser().absolute())
        signature = _index_signature(index_path)
        max_idle = _read_int_env(
            _MCP_STORE_POOL_MAX_ENV,
            _DEFAULT_MCP_STORE_POOL_MAX,
            min_value=1,
        )
        with self._lock:
            bucket = self._buckets.get(logical_path)
            if bucket is None:
                self._verify_signature(signature)
            if bucket is not None and bucket.signature != signature:
                self._record_identity_change(bucket.signature, signature, logical_path)
                raise RuntimeError(
                    "immutable_release_identity_changed: restart the capability sidecar "
                    "for a new release generation"
                )
            if bucket is None:
                self._generation += 1
                bucket = _StoreBucket(
                    logical_path=logical_path,
                    signature=signature,
                    generation=self._generation,
                )
                self._buckets[logical_path] = bucket
            if bucket.idle:
                store = bucket.idle.pop()
                self._hits += 1
                reused = True
            else:
                store = open_ontology_store(
                    Path(signature.resolved_global_spine_path),
                    check_same_thread=False,
                    routing="spine",
                )
                self._misses += 1
                self._opened += 1
                reused = False
            bucket.leased += 1
            while len(bucket.idle) > max_idle:
                stale = bucket.idle.pop(0)
                self._close_store(stale)
        LOGGER.debug(
            "mcp_store_acquire %s",
            json.dumps(
                {
                    "global_spine_path": str(index_path),
                    "resolved_global_spine_path": signature.resolved_global_spine_path,
                    "release_id": signature.release_id,
                    "reused": reused,
                },
                sort_keys=True,
            ),
        )
        return _PersistentStoreLease(self, logical_path, signature, bucket.generation, store)

    def release(
        self,
        logical_path: str,
        signature: _IndexSignature,
        generation: int,
        store: Any,
    ) -> None:
        max_idle = _read_int_env(
            _MCP_STORE_POOL_MAX_ENV,
            _DEFAULT_MCP_STORE_POOL_MAX,
            min_value=1,
        )
        with self._lock:
            bucket = self._buckets.get(logical_path)
            if bucket is None or bucket.signature != signature or bucket.generation != generation:
                self._close_store(store)
                return
            bucket.leased = max(0, bucket.leased - 1)
            if len(bucket.idle) < max_idle:
                bucket.idle.append(store)
                return
            self._close_store(store)

    def ensure_open(self, index_path: Path) -> None:
        with self.acquire(index_path):
            return

    def status(self) -> dict[str, Any]:
        with self._lock:
            stores = sum(bucket.leased + len(bucket.idle) for bucket in self._buckets.values())
            idle = sum(len(bucket.idle) for bucket in self._buckets.values())
            leased = sum(bucket.leased for bucket in self._buckets.values())
            global_spine_stores = [
                self._bucket_status(bucket)
                for _logical_path, bucket in sorted(self._buckets.items())
            ]
            return {
                "mode": _store_mode(),
                "pool_max": _read_int_env(
                    _MCP_STORE_POOL_MAX_ENV,
                    _DEFAULT_MCP_STORE_POOL_MAX,
                    min_value=1,
                ),
                "stores": stores,
                "active_stores": stores,
                "idle": idle,
                "leased": leased,
                "hits": self._hits,
                "misses": self._misses,
                "opened": self._opened,
                "closed": self._closed,
                "identity_changed": self._identity_change is not None,
                "identity_change": self._identity_change,
                "global_spine_stores": global_spine_stores,
            }

    def reset(self) -> None:
        with self._lock:
            for bucket in self._buckets.values():
                self._close_bucket(bucket)
            self._buckets.clear()
            self._hits = 0
            self._misses = 0
            self._opened = 0
            self._closed = 0
            self._generation = 0
            self._identity_change = None
            self._verified_signatures.clear()

    def _verify_signature(self, signature: _IndexSignature) -> None:
        if signature in self._verified_signatures:
            return
        if not signature.release_id:
            raise RuntimeError("release admission failed: release_id missing from manifest")
        release_root = Path(signature.resolved_global_spine_path).parent.parent
        verification = verify_release_startup_v3(
            release_root,
            env=os.getenv("KRW_ONTOLOGY_ENV"),
            manifest_path=release_root / "manifest.json",
            require_current_symlink=False,
        )
        if not verification.get("ok"):
            errors = ", ".join(str(value) for value in verification.get("errors") or [])
            raise RuntimeError(f"release admission failed: {errors}")
        if verification.get("release_id") != signature.release_id:
            raise RuntimeError(
                "release admission failed: manifest release_id does not match the opened index"
            )
        self._verified_signatures.add(signature)

    def _close_bucket(self, bucket: _StoreBucket) -> None:
        while bucket.idle:
            self._close_store(bucket.idle.pop())
        bucket.leased = 0


    def _record_identity_change(
        self,
        previous: _IndexSignature,
        observed: _IndexSignature,
        logical_path: str,
    ) -> None:
        self._identity_change = {
            "code": "immutable_release_identity_changed",
            "logical_path": logical_path,
            "previous_release_id": previous.release_id,
            "observed_release_id": observed.release_id,
            "previous_global_spine_sha256": previous.global_spine_sha256,
            "observed_global_spine_sha256": observed.global_spine_sha256,
        }

    def _bucket_status(self, bucket: _StoreBucket) -> dict[str, Any]:
        payload = {
            "logical_path": bucket.logical_path,
            "resolved_global_spine_path": bucket.signature.resolved_global_spine_path,
            "mtime_ns": bucket.signature.mtime_ns,
            "size": bucket.signature.size,
            "release_id": bucket.signature.release_id,
            "global_spine_sha256": bucket.signature.global_spine_sha256,
            "shard_manifest_sha256": bucket.signature.shard_manifest_sha256,
            "router_sidecar_sha256": bucket.signature.router_sidecar_sha256,
            "idle": len(bucket.idle),
            "leased": bucket.leased,
            "generation": bucket.generation,
        }
        return payload

    def _close_store(self, store: Any) -> None:
        try:
            store.close()
        finally:
            self._closed += 1


_STORE_POOL = _PersistentStorePool()


def _elapsed_ms(started_at: float) -> int:
    return int((time.perf_counter() - started_at) * 1000)


def _count_items(value: Any) -> int:
    if value is None:
        return 0
    if isinstance(value, str):
        return 1 if value else 0
    try:
        return len(value)
    except TypeError:
        return 1


def _safe_text_length(value: Any) -> int:
    return len(value) if isinstance(value, str) else 0


def _safe_payload_dict(payload: Any) -> Mapping[str, Any]:
    return payload if isinstance(payload, Mapping) else {}


def _log_mcp_tool_timing(
    tool_name: str,
    *,
    duration_ms: int,
    force: bool = False,
    **fields: Any,
) -> None:
    if not force and duration_ms < SLOW_MCP_TOOL_LOG_THRESHOLD_MS:
        return
    safe_fields = {key: value for key, value in fields.items() if value is not None}
    log_method = LOGGER.warning if duration_ms >= SLOW_MCP_TOOL_LOG_THRESHOLD_MS else LOGGER.info
    log_method(
        "%s %s",
        (
            _SLOW_MCP_TOOL_LOG_MARKER
            if duration_ms >= SLOW_MCP_TOOL_LOG_THRESHOLD_MS
            else _MCP_TOOL_TELEMETRY_MARKER
        ),
        json.dumps(
            {
                "tool_name": tool_name,
                "duration_ms": duration_ms,
                **safe_fields,
            },
            ensure_ascii=False,
            sort_keys=True,
            default=str,
        ),
    )


def _diagnostic_timing_ms(payload: Mapping[str, Any]) -> Any:
    diagnostics = _safe_payload_dict(payload.get("search_diagnostics"))
    timing_ms = diagnostics.get("timing_ms")
    return timing_ms if isinstance(timing_ms, Mapping) else None


def _diagnostic_keys(payload: Mapping[str, Any]) -> list[str]:
    diagnostics = _safe_payload_dict(payload.get("search_diagnostics"))
    return sorted(str(key) for key in diagnostics.keys()) if diagnostics else []


DEFAULT_LIMIT = 10
MAX_LIMIT = 50
MAX_DISCOVERY_LIMIT = 200
MAX_OFFSET = 10_000
MAX_LIMIT_GROUPS = 50
MAX_LIMIT_PER_GROUP = 10
MAX_COMPARE_TICKERS = 20
MAX_COMPACT_CLAIMS = 3
MAX_COMPACT_QUOTES = 3
MAX_COMPACT_SPANS = 1
MAX_COMPACT_RELATED_OBJECTS = 5
# Full bundles retain source quotes, nested observations, and calculation
# lineage. They are only necessary for a targeted gap-closing read, so keep
# that mode bounded independently of the broader compact-search limit. This
# prevents a provider from turning one exact read into an unbounded context
# expansion while still returning enough primary evidence to answer.
MAX_FULL_DETAIL_LIMIT = 20

TRACE_ONLY_OBJECT_TYPES = frozenset(
    {
        "SupportLink",
        "Edge",
        "CanonicalEntity",
        "EntityMention",
        "SourceDocument",
        "SourceLocation",
        "SourceSpan",
        "SourceTable",
        "SourceTableCell",
        "XBRLFact",
    }
)
DISCOVERY_OBJECT_TYPES = (
    "ResearchClaim",
    "EvidenceQuote",
    "BusinessFactor",
    "ExternalFactorExposure",
    "BusinessActivity",
    "BusinessEvent",
    "AgreementTerm",
    "MetricObservation",
    "Calculation",
    "CompanyBusinessProfile",
    "ChangeEvent",
    "TrendObservation",
    "TemporalLink",
    "AssumptionCandidate",
)
DISCOVERY_TYPE_SCORES = {
    "ExternalFactorExposure": 12.0,
    "EvidenceQuote": 10.0,
    "ResearchClaim": 9.0,
    "BusinessFactor": 8.0,
    "BusinessActivity": 5.0,
    "BusinessEvent": 5.0,
    "AgreementTerm": 5.0,
    "MetricObservation": 4.0,
    "ChangeEvent": 4.0,
    "TrendObservation": 4.0,
    "Calculation": 3.0,
    "CompanyBusinessProfile": 2.0,
    "TemporalLink": 2.0,
    "AssumptionCandidate": 2.0,
}

_ALLOWED_OBJECT_TYPES = set(DEFAULT_QUERY_TYPES) | {
    "XBRLFact",
    "SupportLink",
    "RunManifest",
    "OntologyRegistrySnapshot",
    "ValidationReport",
    "TaxonomyTerm",
    "SourceDocument",
    "SourceLocation",
    "SourceTable",
    "SourceTableCell",
    "CanonicalEntity",
    "EntityMention",
    "MetricObservation",
    "Calculation",
    "BusinessFactor",
    "AgreementTerm",
    "BusinessEvent",
    "BusinessActivity",
    "ExternalFactorExposure",
    "CompanyBusinessProfile",
    "TemporalLink",
    "TrendObservation",
    "ChangeEvent",
}
_OBJECT_TYPE_ALIASES = {
    # Legacy planner vocabulary is deliberately accepted at the execution
    # boundary.  A stale skill must broaden to the closest traceable serving
    # types, never turn an otherwise valid filing query into an exact SQL
    # filter that returns zero rows.
    "narrativeevidence": ("BusinessFactor", "EvidenceQuote", "ResearchClaim"),
    "narrative_evidence": ("BusinessFactor", "EvidenceQuote", "ResearchClaim"),
    "numericevidence": ("Calculation", "MetricObservation", "XBRLFact"),
    "numeric_evidence": ("Calculation", "MetricObservation", "XBRLFact"),
    "quote": ("EvidenceQuote",),
    "quotes": ("EvidenceQuote",),
    "evidencequote": ("EvidenceQuote",),
    "evidence_quote": ("EvidenceQuote",),
    "support": ("SupportLink",),
    "supportlink": ("SupportLink",),
    "support_link": ("SupportLink",),
    "support_links": ("SupportLink",),
    "entity": ("CanonicalEntity",),
    "canonical_entity": ("CanonicalEntity",),
    "entity_mention": ("EntityMention",),
    "claim": ("ResearchClaim",),
    "claims": ("ResearchClaim",),
    "researchclaim": ("ResearchClaim",),
    "research_claim": ("ResearchClaim",),
    "risk": ("BusinessFactor",),
    "risks": ("BusinessFactor",),
    "riskfactor": ("BusinessFactor",),
    "risk_factor": ("BusinessFactor",),
    "growth": ("BusinessFactor",),
    "driver": ("BusinessFactor",),
    "drivers": ("BusinessFactor",),
    "growthdriver": ("BusinessFactor",),
    "growth_driver": ("BusinessFactor",),
    "headwind": ("BusinessFactor",),
    "headwinds": ("BusinessFactor",),
    "business_factor": ("BusinessFactor",),
    "business_factors": ("BusinessFactor",),
    "agreement": ("AgreementTerm",),
    "agreement_term": ("AgreementTerm",),
    "event": ("BusinessEvent", "ChangeEvent"),
    "events": ("BusinessEvent", "ChangeEvent"),
    "business_event": ("BusinessEvent",),
    "business_events": ("BusinessEvent",),
    "metric_observation": ("MetricObservation",),
    "metric_observations": ("MetricObservation",),
    "assumption": ("AssumptionCandidate",),
    "assumptions": ("AssumptionCandidate",),
    "assumptioncandidate": ("AssumptionCandidate",),
    "assumption_candidate": ("AssumptionCandidate",),
    "activity": ("BusinessActivity",),
    "businessactivity": ("BusinessActivity",),
    "business_activity": ("BusinessActivity",),
    "business": ("BusinessActivity", "CompanyBusinessProfile"),
    "exposure": ("ExternalFactorExposure",),
    "exposures": ("ExternalFactorExposure",),
    "externalfactorexposure": ("ExternalFactorExposure",),
    "external_factor_exposure": ("ExternalFactorExposure",),
    "profile": ("CompanyBusinessProfile",),
    "companyprofile": ("CompanyBusinessProfile",),
    "company_profile": ("CompanyBusinessProfile",),
    "companybusinessprofile": ("CompanyBusinessProfile",),
    "company_business_profile": ("CompanyBusinessProfile",),
    "temporal": ("TemporalLink", "TrendObservation", "ChangeEvent"),
    "temporallink": ("TemporalLink",),
    "temporal_link": ("TemporalLink",),
    "trend": ("TrendObservation",),
    "trendobservation": ("TrendObservation",),
    "trend_observation": ("TrendObservation",),
    "change": ("ChangeEvent",),
    "changes": ("ChangeEvent",),
    "disclosure_change": ("ChangeEvent",),
    "disclosure_changes": ("ChangeEvent",),
    "changeevent": ("ChangeEvent",),
    "change_event": ("ChangeEvent",),
    "change_events": ("ChangeEvent",),
    "metric": ("MetricObservation",),
    "metrics": ("MetricObservation",),
    "financialmetric": ("MetricObservation",),
    "financial_metric": ("MetricObservation",),
    "financialmetrics": ("MetricObservation",),
    "financial_metrics": ("MetricObservation",),
    "financialmetricvalue": ("MetricObservation",),
    "financial_metric_value": ("MetricObservation",),
    "derivedmetricvalue": ("MetricObservation",),
    "derived_metric_value": ("MetricObservation",),
    "xbrl": ("XBRLFact",),
    "xbrlfact": ("XBRLFact",),
    "xbrl_fact": ("XBRLFact",),
}


def catalog_tool(
    *,
    ticker: str | None = None,
    document_types: list[str] | None = None,
    limit: int = 50,
    offset: int = 0,
    response_format: ResponseFormat = ResponseFormat.JSON,
) -> str:
    """Return available companies, documents, periods, and index metadata."""
    root_path = _runtime_root()
    index = _runtime_global_spine_path()
    limit = _bounded_limit(limit)
    offset = _bounded_offset(offset)

    with _store(index) as store:
        documents = store.list_documents(ticker=ticker, document_types=document_types)
        companies = store.list_companies()

    total = len(documents)
    page = documents[offset : offset + limit]
    payload = {
        "root": str(root_path),
        "global_spine_path": str(index),
        "companies": companies,
        "document_types": sorted({doc["document_type"] for doc in documents}),
        "periods": sorted({doc["period"] for doc in documents}),
        "documents": page,
        "pagination": _pagination(total, offset, len(page), limit),
    }
    return _format_response(payload, response_format, _markdown_catalog)


def query_tool(
    *,
    topic: str | None = None,
    ticker: str | None = None,
    tickers: list[str] | None = None,
    document_type: str | None = None,
    document_types: list[str] | None = None,
    period: str | None = None,
    periods: list[str] | None = None,
    object_type: str | None = None,
    object_types: list[str] | None = None,
    include_rejected: bool = False,
    limit: int = DEFAULT_LIMIT,
    offset: int = 0,
    group_by: str | None = None,
    limit_groups: int = DEFAULT_LIMIT,
    limit_per_group: int = 3,
    answer_candidate_only: bool = False,
    response_format: ResponseFormat = ResponseFormat.JSON,
    response_detail: ResponseDetail = ResponseDetail.COMPACT,
    **extra_args: Any,
) -> str:
    """Search accepted ontology objects and return evidence bundles."""
    started_at = time.perf_counter()
    runtime_override_error = _runtime_override_error(extra_args, response_format)
    if runtime_override_error is not None:
        return runtime_override_error
    index = _runtime_global_spine_path()
    limit = _bounded_limit(limit)
    offset = _bounded_offset(offset)
    detail = _coerce_response_detail(response_detail)
    if detail == ResponseDetail.FULL:
        limit = min(limit, MAX_FULL_DETAIL_LIMIT)
    detail_policy = _response_detail_policy(response_detail, detail)
    normalized_group_by = _coerce_group_by(group_by)
    limit_groups = _bounded_limit_groups(limit_groups)
    limit_per_group = _bounded_limit_per_group(limit_per_group)
    normalized_tickers = _merge_ticker_alias(ticker=ticker, tickers=tickers)
    normalized_document_types = _merge_scalar_list_alias(document_type, document_types)
    normalized_periods = _merge_scalar_list_alias(period, periods)
    requested_object_types = _merge_scalar_list_alias(object_type, object_types)
    input_warnings = _input_warnings(extra_args)
    summary_mode = detail == ResponseDetail.TICKER_SUMMARY or normalized_group_by == "ticker"
    fetch_limit = (
        _discovery_fetch_limit(limit, limit_groups, limit_per_group)
        if summary_mode
        else min(MAX_LIMIT + offset + 1, offset + limit + 1)
    )
    normalized_object_types, invalid_object_types = _normalize_object_types(requested_object_types)
    if invalid_object_types:
        payload = _error_payload(
            "invalid_object_type",
            f"Unsupported object_types: {', '.join(invalid_object_types)}",
            "Use canonical types or aliases. Supported canonical types: "
            + ", ".join(sorted(_ALLOWED_OBJECT_TYPES)),
        )
        payload["response_detail"] = detail.value
        payload["response_detail_policy"] = detail_policy
        return _format_response(payload, response_format, _markdown_error)
    if answer_candidate_only:
        normalized_object_types = _answer_candidate_object_types(normalized_object_types)
    search_topic, topic_normalization = _normalize_metric_query_topic_for_tool(
        topic,
        periods=normalized_periods,
        object_types=normalized_object_types,
    )

    if summary_mode and topic:
        with _store(index) as store:
            discovery = store.discover_company_topics(
                question=topic,
                tickers=normalized_tickers,
                document_types=normalized_document_types,
                periods=normalized_periods,
                limit_groups=limit_groups,
                limit_per_group=limit_per_group,
                limit=fetch_limit,
            )
        payload = {
            "query": {
                "topic": topic,
                "tickers": _upper_list(normalized_tickers),
                "ticker_alias": ticker,
                "document_type_alias": document_type,
                "period_alias": period,
                "object_type_alias": object_type,
                "document_types": normalized_document_types or [],
                "periods": _upper_list(normalized_periods),
                "object_types": normalized_object_types or [],
                "object_types_requested": requested_object_types or [],
                "include_rejected": include_rejected,
                "group_by": normalized_group_by,
                "limit_groups": limit_groups,
                "limit_per_group": limit_per_group,
                "answer_candidate_only": answer_candidate_only,
            },
            "response_detail": detail.value,
            "response_detail_policy": detail_policy,
            **discovery,
        }
        if input_warnings:
            payload["input_warnings"] = input_warnings
        if isinstance(payload.get("results_by_ticker"), Mapping):
            payload["results_by_ticker"] = {
                ticker_key: list(rows or [])[:limit_per_group]
                for ticker_key, rows in payload["results_by_ticker"].items()
            }
        payload["directness_guard"] = _directness_guard_from_summary_payload(payload, topic=topic)
        payload["pagination"] = _pagination(
            len(payload.get("ticker_candidates") or []),
            0,
            len(payload.get("ticker_candidates") or []),
            limit_groups,
        )
        payload["kernel"] = _query_kernel_envelope(
            topic=search_topic or topic,
            tickers=normalized_tickers,
            document_types=normalized_document_types,
            periods=normalized_periods,
            limit_results=limit,
            result_count=len(payload.get("ticker_candidates") or []),
            research_pack={
                "company_topic_pack": {"top_candidates": payload.get("ticker_candidates") or []}
            },
            agent_autonomy={
                "allowed_next_tools": ["krw_ontology_trace"],
                "max_additional_tool_calls": 1,
            },
            do_not_call=["broad_retrieve", "unscoped_query"],
        )
        _log_mcp_tool_timing(
            "krw_ontology_query",
            duration_ms=_elapsed_ms(started_at),
            topic_chars=_safe_text_length(topic),
            ticker_count=_count_items(normalized_tickers),
            document_type_count=_count_items(normalized_document_types),
            period_count=_count_items(normalized_periods),
            object_type_count=_count_items(normalized_object_types),
            response_detail=detail.value,
            summary_mode=True,
            group_by=normalized_group_by,
            limit=limit,
            offset=offset,
            result_count=len(payload.get("ticker_candidates") or []),
            input_warning_count=len(input_warnings),
        )
        return _format_response(payload, response_format, _markdown_bundles)

    with _store(index) as store:
        if detail == ResponseDetail.FULL:
            bundles, search_diagnostics = store.query_with_diagnostics(
                topic=search_topic,
                tickers=normalized_tickers,
                document_types=normalized_document_types,
                periods=normalized_periods,
                object_types=normalized_object_types,
                include_rejected=include_rejected,
                limit=fetch_limit,
                # A kernel-generated exact gap read marks itself as
                # answer-candidate-only. Keep its full phrase intact instead
                # of backfilling the page with partial-term results.
                strict_only=answer_candidate_only,
            )
        else:
            bundles, search_diagnostics = store.query_compact_with_diagnostics(
                topic=search_topic,
                tickers=normalized_tickers,
                document_types=normalized_document_types,
                periods=normalized_periods,
                object_types=normalized_object_types,
                include_rejected=include_rejected,
                limit=fetch_limit,
            )

    page = bundles[offset : offset + limit]
    if detail == ResponseDetail.FULL:
        results = page
    elif detail == ResponseDetail.IDS_ONLY:
        results = [_ids_only_bundle(bundle) for bundle in page]
    else:
        results = [_compact_bundle(bundle) for bundle in page]
    payload = {
        "query": {
            "topic": topic,
            "search_topic": search_topic,
            "tickers": _upper_list(normalized_tickers),
            "ticker_alias": ticker,
            "document_type_alias": document_type,
            "period_alias": period,
            "object_type_alias": object_type,
            "document_types": normalized_document_types or [],
            "periods": _upper_list(normalized_periods),
            "object_types": normalized_object_types or [],
            "object_types_requested": requested_object_types or [],
            "include_rejected": include_rejected,
            "group_by": normalized_group_by,
            "limit_groups": limit_groups,
            "limit_per_group": limit_per_group,
            "answer_candidate_only": answer_candidate_only,
        },
        "response_detail": detail.value,
        "response_detail_policy": detail_policy,
        "search_diagnostics": search_diagnostics,
        "results": results,
        "pagination": _pagination(len(bundles), offset, len(page), limit),
    }
    if topic_normalization:
        payload["query"]["topic_normalization"] = topic_normalization
        search_diagnostics.setdefault("topic_normalization", topic_normalization)
    if input_warnings:
        payload["input_warnings"] = input_warnings
        search_diagnostics.setdefault("warnings", [])
        search_diagnostics["warnings"].extend(warning["code"] for warning in input_warnings)
    payload["directness_guard"] = _directness_guard_from_summary_payload(payload, topic=topic)
    if summary_mode:
        payload.pop("results", None)
        payload.update(
            _ticker_summary_payload(
                bundles,
                limit_groups=limit_groups,
                limit_per_group=limit_per_group,
            )
        )
        payload["pagination"] = _pagination(
            len(payload.get("ticker_candidates") or []),
            0,
            len(payload.get("ticker_candidates") or []),
            limit_groups,
        )
    payload["kernel"] = _query_kernel_envelope(
        topic=search_topic or topic,
        tickers=normalized_tickers,
        document_types=normalized_document_types,
        periods=normalized_periods,
        limit_results=limit,
        result_count=len(results)
        if "results" in payload
        else len(payload.get("ticker_candidates") or []),
        research_pack={
            "query_results": results
            if "results" in payload
            else payload.get("ticker_candidates") or []
        },
        agent_autonomy={
            "allowed_next_tools": ["krw_ontology_trace"],
            "max_additional_tool_calls": 1,
        },
        do_not_call=["broad_retrieve"],
    )
    _log_mcp_tool_timing(
        "krw_ontology_query",
        duration_ms=_elapsed_ms(started_at),
        topic_chars=_safe_text_length(topic),
        ticker_count=_count_items(normalized_tickers),
        document_type_count=_count_items(normalized_document_types),
        period_count=_count_items(normalized_periods),
        object_type_count=_count_items(normalized_object_types),
        response_detail=detail.value,
        summary_mode=summary_mode,
        group_by=normalized_group_by,
        limit=limit,
        offset=offset,
        result_count=len(results)
        if "results" in payload
        else len(payload.get("ticker_candidates") or []),
        bundle_count=len(bundles),
        topic_normalized=bool(topic_normalization),
        input_warning_count=len(input_warnings),
        search_diagnostics_keys=_diagnostic_keys(payload),
        timing_ms=_diagnostic_timing_ms(payload),
    )
    return _format_response(payload, response_format, _markdown_bundles)


def topic_map_tool(
    *,
    ticker: str,
    document_types: list[str] | None = None,
    periods: list[str] | None = None,
    limit: int = DEFAULT_LIMIT,
    response_format: ResponseFormat = ResponseFormat.JSON,
) -> str:
    """Return company-specific search vocabulary for broad research questions."""
    index = _runtime_global_spine_path()
    limit = _bounded_limit(limit)
    with _store(index) as store:
        payload = store.topic_map(
            ticker=ticker,
            document_types=document_types,
            periods=periods,
            limit=limit,
        )
    return _format_response(payload, response_format, _markdown_topic_map)


def retrieve_tool(
    *,
    question: str,
    ticker: str | None = None,
    tickers: list[str] | None = None,
    document_types: list[str] | None = None,
    periods: list[str] | None = None,
    include_rejected: bool | None = None,
    limit: int = DEFAULT_LIMIT,
    group_by: str | None = None,
    limit_groups: int = DEFAULT_LIMIT,
    limit_per_group: int = 3,
    answer_candidate_only: bool = False,
    response_format: ResponseFormat = ResponseFormat.JSON,
    response_detail: ResponseDetail = ResponseDetail.COMPACT,
    agent_context: Mapping[str, Any] | None = None,
    **extra_args: Any,
) -> str:
    """Use the deterministic local planner, then retrieve evidence bundles."""
    runtime_override_error = _runtime_override_error(extra_args, response_format)
    if runtime_override_error is not None:
        return runtime_override_error
    index = _runtime_global_spine_path()
    limit = _bounded_limit(limit)
    detail = _coerce_response_detail(response_detail)
    detail_policy = _response_detail_policy(response_detail, detail)
    normalized_group_by = _coerce_group_by(group_by)
    limit_groups = _bounded_limit_groups(limit_groups)
    limit_per_group = _bounded_limit_per_group(limit_per_group)
    normalized_tickers = _merge_ticker_alias(ticker=ticker, tickers=tickers)
    input_warnings = _input_warnings(extra_args)
    agent_guidance = _retrieve_agent_guidance(agent_context)
    summary_mode = detail == ResponseDetail.TICKER_SUMMARY or normalized_group_by == "ticker"
    fetch_limit = (
        _discovery_fetch_limit(limit, limit_groups, limit_per_group) if summary_mode else limit
    )
    if summary_mode:
        with _store(index) as store:
            context = store.query_context(
                question=question,
                tickers=normalized_tickers,
                document_types=document_types,
                periods=periods,
                limit_results=limit,
                limit_tickers=limit_groups,
                include_internal_ids=True,
            )
            discovery = {key: value for key, value in context.items() if key not in {"question"}}
            if not discovery.get("ticker_candidates"):
                fallback_discovery = store.discover_company_topics(
                    question=question,
                    tickers=normalized_tickers,
                    document_types=document_types,
                    periods=periods,
                    limit_groups=limit_groups,
                    limit_per_group=limit_per_group,
                    limit=fetch_limit,
                )
                discovery.update(fallback_discovery)
        answerability = discovery.get("answerability") or {
            "direct_answerable": bool(discovery.get("ticker_candidates")),
            "related_context_available": False,
            "negative_answer_supported": False,
            "needs_user_clarification": False,
            "recommended_answer_mode": "ticker_discovery",
        }
        payload = {
            "answerability": answerability,
            "recommended_answer_mode": answerability.get("recommended_answer_mode")
            or "ticker_discovery",
            "directness_guard": _directness_guard_from_research_context(discovery),
            "question": question,
            "query": {
                "question": question,
                "tickers": _upper_list(normalized_tickers),
                "ticker_alias": ticker,
                "document_types": document_types or [],
                "periods": _upper_list(periods),
                "group_by": normalized_group_by,
                "limit_groups": limit_groups,
                "limit_per_group": limit_per_group,
                "answer_candidate_only": answer_candidate_only,
            },
            "response_detail": detail.value,
            "response_detail_policy": detail_policy,
            **discovery,
        }
        if input_warnings:
            payload["input_warnings"] = input_warnings
        _attach_agent_guidance(payload, agent_guidance)
        if isinstance(payload.get("results_by_ticker"), Mapping):
            payload["results_by_ticker"] = {
                ticker_key: list(rows or [])[:limit_per_group]
                for ticker_key, rows in payload["results_by_ticker"].items()
            }
        payload["directness_guard"] = payload.get(
            "directness_guard"
        ) or _directness_guard_from_summary_payload(
            payload,
            topic=question,
        )
        payload["pagination"] = _pagination(
            len(payload.get("ticker_candidates") or []),
            0,
            len(payload.get("ticker_candidates") or []),
            limit_groups,
        )
        return _format_response(payload, response_format, _markdown_retrieve)

    with _store(index) as store:
        research_context = store.query_context(
            question=question,
            tickers=normalized_tickers,
            document_types=document_types,
            periods=periods,
            limit_results=limit,
            limit_tickers=max(1, min(limit_groups, 20)),
            include_internal_ids=detail == ResponseDetail.FULL,
        )
        release_missing_parts = _release_missing_parts_from_context(research_context)
        if release_missing_parts:
            payload = {
                "question": question,
                "query": {
                    "question": question,
                    "tickers": _upper_list(normalized_tickers),
                    "ticker_alias": ticker,
                    "document_types": document_types or [],
                    "periods": _upper_list(periods),
                    "group_by": normalized_group_by,
                    "limit_groups": limit_groups,
                    "limit_per_group": limit_per_group,
                    "answer_candidate_only": answer_candidate_only,
                },
                "answerability": research_context.get("answerability")
                or {
                    "direct_answerable": False,
                    "related_context_available": False,
                    "negative_answer_supported": False,
                    "needs_user_clarification": False,
                    "recommended_answer_mode": "not_answerable_from_current_release",
                },
                "recommended_answer_mode": "not_answerable_from_current_release",
                "directness_guard": _directness_guard_from_research_context(research_context),
                "research_context_version": research_context.get("research_context_version"),
                "research_status": "not_answerable_from_current_release",
                "research_context": {
                    "research_context_version": research_context.get("research_context_version"),
                    "research_status": "not_answerable_from_current_release",
                    "kernel": research_context.get("kernel"),
                    "agent_autonomy": research_context.get("agent_autonomy"),
                    "missing_parts": release_missing_parts,
                    "missing_shards": research_context.get("missing_shards") or {},
                    "unknown_tickers": research_context.get("unknown_tickers") or [],
                    "do_not_call": research_context.get("do_not_call") or [],
                    "directness_guard": _directness_guard_from_research_context(research_context),
                    "research_pack": research_context.get("research_pack"),
                    "routing": research_context.get("routing"),
                },
                "research_pack": research_context.get("research_pack"),
                "kernel": research_context.get("kernel"),
                "agent_autonomy": research_context.get("agent_autonomy"),
                "missing_parts": release_missing_parts,
                "missing_shards": research_context.get("missing_shards") or {},
                "unknown_tickers": research_context.get("unknown_tickers") or [],
                "do_not_call": research_context.get("do_not_call") or [],
                "routing": research_context.get("routing"),
                "direct_evidence": [],
                "related_context": [],
                "rejected_context": [],
                "response_detail": detail.value,
                "response_detail_policy": detail_policy,
            }
            if input_warnings:
                payload["input_warnings"] = input_warnings
            _attach_agent_guidance(payload, agent_guidance)
            return _format_response(payload, response_format, _markdown_retrieve)
        if research_context.get("research_status") == "out_of_scope_for_filing_ontology":
            payload = {
                "question": question,
                "query": {
                    "question": question,
                    "tickers": _upper_list(normalized_tickers),
                    "ticker_alias": ticker,
                    "document_types": document_types or [],
                    "periods": _upper_list(periods),
                    "group_by": normalized_group_by,
                    "limit_groups": limit_groups,
                    "limit_per_group": limit_per_group,
                    "answer_candidate_only": answer_candidate_only,
                },
                "answerability": research_context.get("answerability") or {},
                "recommended_answer_mode": (research_context.get("answerability") or {}).get(
                    "recommended_answer_mode"
                ),
                "directness_guard": _directness_guard_from_research_context(research_context),
                "research_context_version": research_context.get("research_context_version"),
                "research_status": research_context.get("research_status"),
                "kernel": research_context.get("kernel"),
                "stop_guard": _stop_guard_from_research_context(research_context),
                "research_pack": research_context.get("research_pack"),
                "agent_autonomy": research_context.get("agent_autonomy"),
                "missing_parts": research_context.get("missing_parts") or [],
                "do_not_call": research_context.get("do_not_call") or [],
                "direct_evidence": [],
                "related_context": [],
                "rejected_context": [],
                "response_detail": detail.value,
                "response_detail_policy": detail_policy,
            }
            if input_warnings:
                payload["input_warnings"] = input_warnings
            _attach_agent_guidance(payload, agent_guidance)
            return _format_response(payload, response_format, _markdown_retrieve)
        if _should_skip_secondary_retrieve(research_context, detail=detail):
            payload = {
                "question": question,
                "query": {
                    "question": question,
                    "tickers": _upper_list(normalized_tickers),
                    "ticker_alias": ticker,
                    "document_types": document_types or [],
                    "periods": _upper_list(periods),
                    "group_by": normalized_group_by,
                    "limit_groups": limit_groups,
                    "limit_per_group": limit_per_group,
                    "answer_candidate_only": answer_candidate_only,
                },
                "answerability": research_context.get("answerability") or {},
                "recommended_answer_mode": (research_context.get("answerability") or {}).get(
                    "recommended_answer_mode"
                ),
                "directness_guard": _directness_guard_from_research_context(research_context),
                "research_context_version": research_context.get("research_context_version"),
                "research_status": research_context.get("research_status"),
                "research_context": {
                    "research_context_version": research_context.get("research_context_version"),
                    "research_status": research_context.get("research_status"),
                    "kernel": research_context.get("kernel"),
                    "agent_autonomy": research_context.get("agent_autonomy"),
                    "missing_parts": research_context.get("missing_parts") or [],
                    "do_not_call": research_context.get("do_not_call") or [],
                    "directness_guard": _directness_guard_from_research_context(research_context),
                    "research_pack": research_context.get("research_pack"),
                },
                "research_pack": research_context.get("research_pack"),
                "kernel": research_context.get("kernel"),
                "agent_autonomy": research_context.get("agent_autonomy"),
                "missing_parts": research_context.get("missing_parts") or [],
                "do_not_call": research_context.get("do_not_call") or [],
                "secondary_retrieve_skipped": True,
                "direct_evidence": [],
                "related_context": [],
                "rejected_context": [],
                "response_detail": detail.value,
                "response_detail_policy": detail_policy,
            }
            if input_warnings:
                payload["input_warnings"] = input_warnings
            _attach_agent_guidance(payload, agent_guidance)
            return _format_response(payload, response_format, _markdown_retrieve)

        result = AgentRetriever(store).retrieve(
            question,
            tickers=normalized_tickers,
            document_types=document_types,
            periods=periods,
            include_rejected=include_rejected,
            limit=fetch_limit,
            include_evidence_bundle=detail == ResponseDetail.FULL,
        )
    result["research_context"] = {
        "research_context_version": research_context.get("research_context_version"),
        "research_status": research_context.get("research_status"),
        "kernel": research_context.get("kernel"),
        "agent_autonomy": research_context.get("agent_autonomy"),
        "missing_parts": research_context.get("missing_parts") or [],
        "missing_shards": research_context.get("missing_shards") or {},
        "unknown_tickers": research_context.get("unknown_tickers") or [],
        "do_not_call": research_context.get("do_not_call") or [],
        "directness_guard": _directness_guard_from_research_context(research_context),
        "research_pack": research_context.get("research_pack"),
        "routing": research_context.get("routing"),
    }
    if research_context.get("routing"):
        result.setdefault("routing", research_context.get("routing"))
    result["directness_guard"] = _directness_guard_from_research_context(research_context)
    result["kernel"] = research_context.get("kernel")
    result.setdefault("answerability", research_context.get("answerability") or {})
    result.setdefault(
        "recommended_answer_mode",
        (research_context.get("answerability") or {}).get("recommended_answer_mode"),
    )
    if answer_candidate_only:
        result = _map_retrieval_context(result, _answer_candidate_bundles)
    if summary_mode:
        bundles = _retrieval_context_bundles(result)
        payload = {
            **{
                key: value
                for key, value in result.items()
                if key not in {"direct_evidence", "related_context", "rejected_context"}
            },
            **_ticker_summary_payload(
                bundles,
                limit_groups=limit_groups,
                limit_per_group=limit_per_group,
            ),
        }
    elif detail == ResponseDetail.IDS_ONLY:
        payload = {
            **{
                key: value
                for key, value in result.items()
                if key not in {"direct_evidence", "related_context", "rejected_context"}
            },
            "direct_evidence": [
                _ids_only_bundle(bundle) for bundle in result.get("direct_evidence", [])
            ],
            "related_context": [
                _ids_only_bundle(bundle) for bundle in result.get("related_context", [])
            ],
            "rejected_context": [
                _ids_only_bundle(bundle) for bundle in result.get("rejected_context", [])
            ],
        }
    else:
        payload = result if detail == ResponseDetail.FULL else _compact_retrieval(result)
    payload.setdefault("query", {})
    payload["query"].update(
        {
            "tickers": _upper_list(normalized_tickers),
            "ticker_alias": ticker,
            "group_by": normalized_group_by,
            "limit_groups": limit_groups,
            "limit_per_group": limit_per_group,
            "answer_candidate_only": answer_candidate_only,
        }
    )
    if input_warnings:
        payload["input_warnings"] = input_warnings
    _attach_agent_guidance(payload, agent_guidance)
    payload["response_detail"] = detail.value
    payload["response_detail_policy"] = detail_policy
    return _format_response(payload, response_format, _markdown_retrieve)


def trace_tool(
    *,
    object_id: str,
    ticker: str | None = None,
    response_format: ResponseFormat = ResponseFormat.JSON,
) -> str:
    """Trace one object occurrence to its source document, quotes, spans, and quality."""
    index = _runtime_global_spine_path()
    normalized_ticker = str(ticker or "").strip().upper() or None
    cache_key = _trace_cache_key(index, object_id, normalized_ticker)
    with _TRACE_TOOL_CACHE_LOCK:
        cached = _TRACE_TOOL_CACHE.get(cache_key)
        if cached is not None:
            _TRACE_TOOL_CACHE.move_to_end(cache_key)
            return _format_response(copy.deepcopy(cached), response_format, _markdown_trace)
    with _store(index) as store:
        trace = store.trace(object_id, ticker=normalized_ticker)
        resolved_from_prefix = None
        candidates = []
        if trace is None:
            candidates = store.find_object_ids(
                object_id,
                ticker=normalized_ticker,
                limit=11,
            )
            if len(candidates) == 1:
                resolved_from_prefix = object_id
                object_id = candidates[0]["id"]
                trace = store.trace(object_id, ticker=normalized_ticker)
    if trace is None:
        if candidates:
            payload = _error_payload(
                "ambiguous_object_id",
                f"Object id prefix is ambiguous: {object_id}",
                "Pass one exact candidate id from candidates.",
            )
            payload["candidates"] = candidates[:10]
        else:
            payload = _error_payload(
                "not_found",
                f"Object not found: {object_id}",
                "Call krw_ontology_query first, then pass one returned id or a unique id prefix to trace.",
            )
    else:
        payload = trace
        if resolved_from_prefix:
            payload["resolved_from_prefix"] = resolved_from_prefix
        if not resolved_from_prefix:
            with _TRACE_TOOL_CACHE_LOCK:
                _TRACE_TOOL_CACHE[cache_key] = copy.deepcopy(payload)
                _TRACE_TOOL_CACHE.move_to_end(cache_key)
                while len(_TRACE_TOOL_CACHE) > _TRACE_TOOL_CACHE_MAX:
                    _TRACE_TOOL_CACHE.popitem(last=False)
    return _format_response(payload, response_format, _markdown_trace)


def verify_evidence_tool(
    *,
    ticker: str,
    questions: Sequence[Mapping[str, Any]],
    brief_hash: str | None = None,
    response_format: ResponseFormat = ResponseFormat.JSON,
) -> str:
    """Verify exact ontology object ids and return a hash-stable evidence pack."""
    index = _runtime_global_spine_path()
    signature = _index_signature(index)
    try:
        with _store(index) as store:
            payload = build_verified_company_evidence_pack(
                store=store,
                ticker=ticker,
                questions=questions,
                release_id=signature.release_id,
                brief_hash=brief_hash,
            )
    except EvidencePackInputError as exc:
        payload = {
            "status": "input_correction_required",
            "code": exc.code,
            "message": str(exc),
            "required_change": exc.required_change,
            "invalid_fields": exc.invalid_fields,
            "allowed_next_tools": ["krw_ontology_verify_evidence"],
        }
    except ValueError as exc:
        payload = {
            "status": "input_correction_required",
            "code": "evidence_pack_validation_failed",
            "message": str(exc),
            "required_change": (
                "Correct the named evidence-pack input and call "
                "krw_ontology_verify_evidence again with the same question ids."
            ),
            "invalid_fields": ["ticker", "questions", "brief_hash"],
            "allowed_next_tools": ["krw_ontology_verify_evidence"],
        }
    return _format_response(payload, response_format, _markdown_verify_evidence)


def chain_tool(
    *,
    object_id: str,
    ticker: str | None = None,
    max_depth: int = 2,
    direction: str = "both",
    include_quote_text: bool = False,
    response_format: ResponseFormat = ResponseFormat.JSON,
) -> str:
    """Return compact evidence, semantic, and temporal chains around one object occurrence."""
    index = _runtime_global_spine_path()
    normalized_ticker = str(ticker or "").strip().upper() or None
    max_depth = max(0, min(int(max_depth), 5))
    with _store(index) as store:
        chain = store.chain(
            object_id,
            ticker=normalized_ticker,
            max_depth=max_depth,
            direction=direction,
            include_quote_text=include_quote_text,
        )
        resolved_from_prefix = None
        candidates = []
        if chain is None:
            candidates = store.find_object_ids(
                object_id,
                ticker=normalized_ticker,
                limit=11,
            )
            if len(candidates) == 1:
                resolved_from_prefix = object_id
                object_id = candidates[0]["id"]
                chain = store.chain(
                    object_id,
                    ticker=normalized_ticker,
                    max_depth=max_depth,
                    direction=direction,
                    include_quote_text=include_quote_text,
                )
    if chain is None:
        if candidates:
            payload = _error_payload(
                "ambiguous_object_id",
                f"Object id prefix is ambiguous: {object_id}",
                "Pass one exact candidate id from candidates.",
            )
            payload["candidates"] = candidates[:10]
        else:
            payload = _error_payload(
                "not_found",
                f"Object not found: {object_id}",
                "Call krw_ontology_query first, then pass one returned id or a unique id prefix to chain.",
            )
    else:
        payload = chain
        if resolved_from_prefix:
            payload["resolved_from_prefix"] = resolved_from_prefix
        payload = _apply_chain_response_budget(payload)
    return _format_response(payload, response_format, _markdown_chain)


def quality_tool(
    *,
    ticker: str | None = None,
    document_type: str | None = None,
    period: str | None = None,
    limit: int = DEFAULT_LIMIT,
    offset: int = 0,
    response_format: ResponseFormat = ResponseFormat.JSON,
) -> str:
    """Return section-quality, rejected-object, and batch-failure signals."""
    index = _runtime_global_spine_path()
    limit = _bounded_limit(limit)
    offset = _bounded_offset(offset)
    with _store(index) as store:
        quality = store.quality(ticker=ticker, document_type=document_type, period=period)

    events = quality["events"]
    page = events[offset : offset + limit]
    payload = {
        "documents": quality["documents"],
        "events": page,
        "summary": quality["summary"],
        "pagination": _pagination(len(events), offset, len(page), limit),
    }
    if isinstance(quality.get("routing"), Mapping):
        payload["routing"] = dict(quality["routing"])
    if isinstance(quality.get("topology"), Mapping):
        payload["topology"] = dict(quality["topology"])
    return _format_response(payload, response_format, _markdown_quality)


def compare_tool(
    *,
    tickers: list[str] | None = None,
    ticker: str | None = None,
    ticker_a: str | None = None,
    ticker_b: str | None = None,
    topic: str | None = None,
    metric: str | None = None,
    document_types: list[str] | None = None,
    periods: list[str] | None = None,
    limit_per_ticker: int = 5,
    response_format: ResponseFormat = ResponseFormat.JSON,
    response_detail: ResponseDetail = ResponseDetail.COMPACT,
    **extra_args: Any,
) -> str:
    """Compare companies by a topic search or canonical metric."""
    runtime_override_error = _runtime_override_error(extra_args, response_format)
    if runtime_override_error is not None:
        return runtime_override_error
    normalized_tickers = _merge_compare_ticker_aliases(
        tickers=tickers,
        ticker=ticker,
        ticker_a=ticker_a,
        ticker_b=ticker_b,
    )
    input_warnings = _input_warnings(extra_args)
    detail = _coerce_response_detail(response_detail)
    detail_policy = _response_detail_policy(response_detail, detail)
    period_compare = len(normalized_tickers) == 1 and len(periods or []) >= 2
    if len(normalized_tickers) < 2 and not period_compare:
        payload = _error_payload(
            "invalid_request",
            "Compare requires at least two tickers or one ticker with at least two periods.",
            "Pass tickers like ['VG', 'XOM'], or tickers=['VG'] with periods like ['FY2024', 'FY2025'].",
        )
        payload["query"] = {
            "tickers": normalized_tickers,
            "ticker_alias": ticker,
            "ticker_a_alias": ticker_a,
            "ticker_b_alias": ticker_b,
        }
        payload["response_detail"] = detail.value
        payload["response_detail_policy"] = detail_policy
        if input_warnings:
            payload["input_warnings"] = input_warnings
        return _format_response(payload, response_format, _markdown_error)
    if len(normalized_tickers) > MAX_COMPARE_TICKERS:
        payload = _error_payload(
            "invalid_request",
            f"Compare supports at most {MAX_COMPARE_TICKERS} tickers per call.",
            "Split the comparison into smaller batches.",
        )
        payload["query"] = {"tickers": normalized_tickers}
        payload["response_detail"] = detail.value
        payload["response_detail_policy"] = detail_policy
        if input_warnings:
            payload["input_warnings"] = input_warnings
        return _format_response(payload, response_format, _markdown_error)

    index = _runtime_global_spine_path()
    limit_per_ticker = max(1, min(int(limit_per_ticker), 10))
    with _store(index) as store:
        if period_compare:
            result = _compare_periods(
                store,
                ticker=normalized_tickers[0],
                topic=topic,
                metric=metric,
                document_types=document_types,
                periods=periods or [],
                limit_per_period=limit_per_ticker,
                compact=detail != ResponseDetail.FULL,
            )
        elif detail == ResponseDetail.FULL:
            result = store.compare(
                tickers=normalized_tickers,
                topic=topic,
                metric=metric,
                document_types=document_types,
                periods=periods,
                limit_per_ticker=limit_per_ticker,
            )
        else:
            result = store.compare_compact(
                tickers=normalized_tickers,
                topic=topic,
                metric=metric,
                document_types=document_types,
                periods=periods,
                limit_per_ticker=limit_per_ticker,
            )
    if "kernel" not in result:
        request = research_request_from_plan_fields(
            question=f"compare {topic or metric or 'comparison'}",
            tickers=normalized_tickers,
            document_types=document_types or [],
            periods=periods or [],
            universe=None,
            limit_results=limit_per_ticker,
            limit_tickers=len(normalized_tickers),
            mode="compare",
        )
        result["kernel"] = ResearchKernel().build_envelope(
            request,
            research_status="sufficient_for_default_answer"
            if result.get("results")
            else "needs_targeted_followup",
            answer_mode="comparison_research_state",
            research_pack={"comparison_contexts": result.get("comparison_contexts") or {}},
            answerability={"related_context_available": bool(result.get("results"))},
            missing_parts=[] if result.get("results") else ["comparison_candidates_not_found"],
            recommended_tools=[],
            agent_autonomy={
                "allowed_next_tools": ["krw_ontology_trace"],
                "max_additional_tool_calls": 1,
            },
            do_not_call=["raw_fts_winner_by_hit_count", "broad_retrieve"],
        )
    result["comparison_rows"] = _comparison_rows(result)
    payload = result if detail == ResponseDetail.FULL else _compact_compare(result)
    payload["query"] = {
        "tickers": normalized_tickers,
        "ticker_alias": ticker,
        "ticker_a_alias": ticker_a,
        "ticker_b_alias": ticker_b,
        "topic": topic,
        "metric": metric,
        "document_types": document_types or [],
        "periods": _upper_list(periods),
        "limit_per_ticker": limit_per_ticker,
    }
    if input_warnings:
        payload["input_warnings"] = input_warnings
    payload["response_detail"] = detail.value
    payload["response_detail_policy"] = detail_policy
    return _format_response(payload, response_format, _markdown_compare)


def plan_query_tool(
    *,
    search_plan: SearchPlan | Mapping[str, Any],
    response_format: ResponseFormat = ResponseFormat.JSON,
) -> str:
    """Validate and normalize an agent-authored SearchPlan v2 without retrieval."""
    plan = validate_search_plan(search_plan)
    payload = {
        "contract_version": MCP_CONTRACT_VERSION,
        "valid": True,
        "plan": plan.model_dump(mode="json"),
        "execution_preview": {
            "clause_count": len(plan.clauses),
            "required_clause_count": sum(1 for clause in plan.clauses if clause.required),
            "routing_clauses": [
                {
                    "clause_id": clause.clause_id,
                    "query": clause_routing_query(clause),
                    "required": clause.required,
                }
                for clause in plan.clauses
            ],
            "allow_relaxed": plan.uncertainty != PlanUncertainty.LOW,
            "limit_tickers": plan.limit_tickers,
            "limit_results": plan.limit_results,
        },
    }
    return _format_response(payload, response_format, _markdown_plan)


def index_context_tool(
    *,
    include_counts: bool = False,
    include_capabilities: bool = True,
    include_quality_summary: bool = False,
    allow_expensive: bool = False,
    response_format: ResponseFormat = ResponseFormat.JSON,
) -> str:
    """Return a compact AI capability card for the current v3 ontology release."""
    started_at = time.perf_counter()
    index = _runtime_global_spine_path()
    counts_requested = bool(include_counts)
    quality_requested = bool(include_quality_summary)
    effective_include_counts = counts_requested and allow_expensive
    effective_include_quality_summary = quality_requested and allow_expensive
    with _store(index) as store:
        payload = store.index_context(
            include_counts=effective_include_counts,
            include_capabilities=include_capabilities,
            include_quality_summary=effective_include_quality_summary,
        )
    payload["index_context_guard"] = {
        "mode": "lightweight_by_default",
        "diagnostic_only": True,
        "expensive_counts_requested": counts_requested,
        "expensive_quality_summary_requested": quality_requested,
        "allow_expensive": bool(allow_expensive),
        "counts_returned": effective_include_counts,
        "quality_summary_returned": effective_include_quality_summary,
        "reason": (
            "index_context is an operational/debug capability card for the current v3 ontology release. "
            "Expensive table counts and quality summary scans are disabled by default; "
            "use query_context for normal research questions."
        ),
        "how_to_enable_expensive": (
            "Pass allow_expensive=true with include_counts and/or include_quality_summary "
            "only for explicit audit/debug operations."
        ),
        "recommended_normal_research_tool": "krw_ontology_query_context",
    }
    guard = payload["index_context_guard"]
    _log_mcp_tool_timing(
        "krw_ontology_index_context",
        duration_ms=_elapsed_ms(started_at),
        force=counts_requested or quality_requested or bool(allow_expensive),
        include_counts_requested=counts_requested,
        include_quality_summary_requested=quality_requested,
        allow_expensive=bool(allow_expensive),
        counts_returned=guard.get("counts_returned"),
        quality_summary_returned=guard.get("quality_summary_returned"),
        include_capabilities=bool(include_capabilities),
        guard_mode=guard.get("mode"),
    )
    return _format_response(payload, response_format, _markdown_index_context)


def company_context_tool(
    *,
    ticker: str,
    document_types: list[str] | None = None,
    periods: list[str] | None = None,
    limit_topics: int = 12,
    include_internal_ids: bool = True,
    response_format: ResponseFormat = ResponseFormat.JSON,
) -> str:
    """Return compressed evidence-derived topic context for one company."""
    index = _runtime_global_spine_path()
    with _store(index) as store:
        payload = store.company_context(
            ticker=ticker,
            document_types=document_types,
            periods=periods,
            limit_topics=limit_topics,
            include_internal_ids=include_internal_ids,
        )
    return _format_response(payload, response_format, _markdown_company_context)


def raw_query_context_tool(
    *,
    question: str,
    ticker: str | None = None,
    tickers: list[str] | None = None,
    document_types: list[str] | None = None,
    periods: list[str] | None = None,
    universe: str | None = None,
    limit_results: int = 10,
    limit_tickers: int = 20,
    include_internal_ids: bool = True,
    response_format: ResponseFormat = ResponseFormat.JSON,
) -> str:
    """Return the store's internal research pack for backend regression tests.

    This function is intentionally not registered as an MCP tool.  The public
    query_context surface is query_context_tool and emits ResearchState v2 only.
    """
    started_at = time.perf_counter()
    index = _runtime_global_spine_path()
    with _store(index) as store:
        payload = store.query_context(
            question=question,
            ticker=ticker,
            tickers=tickers,
            document_types=document_types,
            periods=periods,
            universe=universe,
            limit_results=limit_results,
            limit_tickers=limit_tickers,
            include_internal_ids=include_internal_ids,
        )
    payload_dict = _safe_payload_dict(payload)
    answerability = _safe_payload_dict(payload_dict.get("answerability"))
    research_pack = _safe_payload_dict(payload_dict.get("research_pack"))
    kernel = _safe_payload_dict(payload_dict.get("kernel"))
    _log_mcp_tool_timing(
        "krw_ontology_query_context",
        duration_ms=_elapsed_ms(started_at),
        question_chars=_safe_text_length(question),
        ticker_count=_count_items(tickers) + (1 if ticker else 0),
        document_type_count=_count_items(document_types),
        period_count=_count_items(periods),
        has_universe=bool(universe),
        limit_results=limit_results,
        limit_tickers=limit_tickers,
        include_internal_ids=bool(include_internal_ids),
        research_status=payload_dict.get("research_status"),
        recommended_answer_mode=answerability.get("recommended_answer_mode"),
        kernel_intent=kernel.get("intent"),
        kernel_primary_context=kernel.get("primary_context"),
        kernel_status=kernel.get("status"),
        ticker_candidate_count=_count_items(payload_dict.get("ticker_candidates")),
        missing_part_count=_count_items(payload_dict.get("missing_parts")),
        research_pack_keys=sorted(str(key) for key in research_pack.keys())
        if research_pack
        else [],
        search_diagnostics_keys=_diagnostic_keys(payload_dict),
        timing_ms=_diagnostic_timing_ms(payload_dict),
    )
    return _format_response(payload, response_format, _markdown_query_context)


def query_context_tool(
    *,
    search_plan: SearchPlan | Mapping[str, Any],
) -> "QueryContextResult":
    """Execute an explicit agent plan and return a compact ResearchState v2.

    The plan is required and validated with extra fields forbidden.  Each
    clause's retrieval_query is sent directly to compact retrieval; neither
    the original question nor a keyword intent router is allowed to replace it.
    Chart-ready series, when the verified sidecar has them, ride the private
    presentation channel instead of the model-visible state.
    """
    started_at = time.perf_counter()
    # The echoed state must retain the caller's already validated plan: the
    # Rust kernel pins it byte-for-byte across the MCP boundary.  Execute a
    # safe normalized copy instead, so legacy type labels cannot create a
    # false empty retrieval while the public plan contract remains stable.
    plan = validate_search_plan(search_plan)
    execution_plan, _execution_warnings = normalize_execution_search_plan(plan)
    index = _runtime_global_spine_path()
    signature = _index_signature(index)
    retrieval_started_at = time.perf_counter()
    with _store(index) as store:
        raw_payload = _execute_search_plan(store=store, search_plan=execution_plan)
    _attach_chart_series_sidecar_to_search_plan_payload(
        raw_payload=raw_payload,
        requested_tickers=plan.tickers,
        chart_clauses=_plan_chart_clauses(plan),
        release_id=signature.release_id,
        source_manifest_hash=signature.shard_manifest_sha256,
    )
    presentation_pack = raw_payload.pop("presentation_series_pack", None)
    retrieval_elapsed_ms = _elapsed_ms(retrieval_started_at)
    compile_started_at = time.perf_counter()
    state = compile_research_state(
        search_plan=plan,
        raw_payload=raw_payload,
        release_id=signature.release_id,
    )
    compile_elapsed_ms = _elapsed_ms(compile_started_at)
    execution_telemetry = _safe_payload_dict(
        _safe_payload_dict(raw_payload.get("search_diagnostics")).get("telemetry")
    )
    _log_mcp_tool_timing(
        "krw_ontology_query_context",
        duration_ms=_elapsed_ms(started_at),
        force=True,
        question_sha256=hashlib.sha256(plan.question.encode("utf-8")).hexdigest()[:16],
        question_chars=_safe_text_length(plan.question),
        ticker_count=len(plan.tickers),
        document_type_count=len(plan.document_types),
        period_count=len(plan.periods),
        has_universe=bool(plan.universe),
        limit_results=plan.limit_results,
        limit_tickers=plan.limit_tickers,
        clause_count=len(plan.clauses),
        intent=plan.intent,
        evidence_unit_count=len(state.evidence_units),
        missing_part_count=len(state.missing_parts),
        answerability_status=state.answerability.status,
        strong_claim_allowed=state.answerability.strong_claim_allowed,
        covered_clause_count=sum(
            1 for coverage in state.clause_coverage if coverage.status == "covered"
        ),
        partial_clause_count=sum(
            1 for coverage in state.clause_coverage if coverage.status == "partial"
        ),
        missing_clause_count=sum(
            1 for coverage in state.clause_coverage if coverage.status == "missing"
        ),
        evidence_directness_counts={
            directness: sum(1 for unit in state.evidence_units if unit.directness == directness)
            for directness in ("direct", "metric_lineage", "related", "unverified")
        },
        evidence_grade_counts={
            grade: sum(1 for unit in state.evidence_units if unit.evidence_grade == grade)
            for grade in ("strong", "medium", "weak", "unverified")
        },
        calculation_coverage_statuses={
            status: sum(1 for coverage in state.calculation_coverage if coverage.status == status)
            for status in ("covered", "partial", "missing")
        },
        retrieval_ms=retrieval_elapsed_ms,
        compile_ms=compile_elapsed_ms,
        model_text_bytes=research_state_model_bytes(state),
        mcp_wire_bytes=research_state_wire_bytes(state),
        execution=execution_telemetry,
    )
    return QueryContextResult(state=state, presentation_pack=presentation_pack)


@dataclass(frozen=True)
class QueryContextResult:
    """Canonical query_context return: model state plus private presentation data.

    ``presentation_pack`` is carried in the MCP result ``_meta`` under
    ``com.krwontology/presentationSeries`` and never serialized into the
    model-visible ResearchState.
    """

    state: ResearchState
    presentation_pack: dict[str, Any] | None = None


def _plan_chart_clauses(plan: SearchPlan) -> list[dict[str, Any]]:
    """Project validated clause intent into the private chart channel."""
    return [
        {
            "clause_id": clause.clause_id,
            "required": clause.required,
            "tickers": list(clause.tickers),
            "metrics": list(clause.metrics),
            "metric_scope": clause.metric_scope,
            "metric_dimensions": list(clause.metric_dimensions),
            "calculation_window": clause.calculation_window,
        }
        for clause in plan.clauses
    ]


def query_context_from_search_plan(search_plan: SearchPlan) -> QueryContextResult:
    """Canonical in-process entrypoint for an already validated SearchPlan.

    This is not an MCP tool and intentionally has no object envelope.  The
    low-level descriptor validates the MCP root value first, then invokes this
    function with the same typed value.  Keeping the imported implementation
    behind this small typed seam lets its previous keyword-oriented helper
    remain an implementation detail while the canonical external ABI stays a
    direct `SearchPlan`.
    """

    return query_context_tool(search_plan=search_plan)


def _attach_chart_series_sidecar_to_search_plan_payload(
    *,
    raw_payload: dict[str, Any],
    requested_tickers: Sequence[str],
    chart_clauses: Sequence[Mapping[str, Any]] | None = None,
    release_id: str | None = None,
    source_manifest_hash: str | None = None,
) -> None:
    """Attach a bounded presentation pack for the private MCP ``_meta`` channel.

    SearchPlan v2 deliberately executes directly against the shard store, rather
    than through ``OntologySpineRouter.query_context``.  The verified sidecar
    pack is presentation data, not research evidence: it is stored on the raw
    payload under ``presentation_series_pack`` and never enters the
    model-visible ResearchState.  Attachment is not gated by question keywords;
    chart-worthiness is decided later by the deterministic presentation
    compiler in the Rust harness.
    """
    if not chart_series_runtime_enabled():
        return
    routing = _safe_payload_dict(raw_payload.get("routing"))
    resolved_tickers = _dedupe_preserving_order(
        [
            *[str(ticker).strip().upper() for ticker in requested_tickers if str(ticker).strip()],
            *[
                str(ticker).strip().upper()
                for ticker in routing.get("resolved_tickers") or []
                if str(ticker).strip()
            ],
        ]
    )
    if not resolved_tickers:
        return

    chart_series_path = _runtime_root() / CHART_SERIES_RELATIVE_PATH
    diagnostics = raw_payload.setdefault("search_diagnostics", {})
    if not isinstance(diagnostics, dict):
        diagnostics = {}
        raw_payload["search_diagnostics"] = diagnostics
    chart_diagnostics = {
        "available": chart_series_path.is_file(),
        "source": "chart_series_sidecar",
    }
    diagnostics["chart_series"] = chart_diagnostics
    if not chart_diagnostics["available"]:
        chart_diagnostics["disabled_reason"] = "missing"
        return

    pack = query_chart_series_pack(
        chart_series_path,
        tickers=resolved_tickers,
        chart_clauses=chart_clauses,
        limit_series=8,
        limit_points=12,
        release_id=release_id,
        source_manifest_hash=source_manifest_hash,
    )
    if not pack:
        chart_diagnostics["matched"] = False
        return

    chart_diagnostics["matched"] = True
    chart_diagnostics["series_count"] = len(pack.get("series") or [])
    raw_payload["presentation_series_pack"] = pack


def _execute_search_plan(*, store: Any, search_plan: SearchPlan) -> dict[str, Any]:
    """Execute only plan-authored compact queries and assemble compiler input."""
    started_at = time.perf_counter()
    scope = execution_scope_for_plan(search_plan)
    results_by_ticker: dict[str, list[dict[str, Any]]] = {}
    evidence_by_occurrence: dict[tuple[str, str], dict[str, Any]] = {}
    diagnostics: list[Mapping[str, Any]] = []
    unknown_tickers: list[str] = []
    failed_tickers: list[str] = []
    shard_errors: dict[str, str] = {}
    filing_document_roles: dict[str, Any] = {}
    warnings: list[str] = []
    execution_routing: dict[str, Any] = {}
    omitted_evidence_count = 0
    truncation_possible = False
    route_planned_tickers = getattr(store, "route_planned_tickers", None)
    if callable(route_planned_tickers):
        resolved_tickers, routing_diagnostics = route_planned_tickers(
            clauses=[
                {
                    "clause_id": clause.clause_id,
                    "query": clause_routing_query(clause),
                    "required": clause.required,
                }
                for clause in search_plan.clauses
            ],
            explicit_tickers=search_plan.tickers or None,
            limit=search_plan.limit_tickers,
        )
    else:
        resolved_tickers = list(search_plan.tickers)
        routing_diagnostics = {
            "mode": "explicit_plan_scope",
            "resolved_tickers": resolved_tickers,
            "fallback_used": False,
        }
    unknown_tickers.extend(_diagnostic_unknown_tickers(routing_diagnostics))
    warnings.extend(_diagnostic_warnings(routing_diagnostics))
    route_error = str(routing_diagnostics.get("error") or "").strip()
    if route_error:
        warnings.append(route_error)
    if not resolved_tickers and not route_error:
        warnings.append("planned_ticker_candidates_not_found")
    list_documents = getattr(store, "list_documents", None)
    if resolved_tickers and callable(list_documents):
        try:
            filing_document_roles.update(
                filing_document_roles_from_documents(
                    list_documents(
                        tickers=resolved_tickers,
                        document_types=scope.document_types,
                    ),
                    tickers=resolved_tickers,
                )
            )
        except (OSError, RuntimeError, ValueError, sqlite3.Error):
            warnings.append("filing_document_roles_unavailable")

    def record_clause_rows(
        clause: Any,
        rows: Sequence[Mapping[str, Any]],
        clause_diagnostics: Mapping[str, Any],
    ) -> None:
        diagnostics.append(clause_diagnostics)
        unknown_tickers.extend(_diagnostic_unknown_tickers(clause_diagnostics))
        failed_tickers.extend(_diagnostic_failed_tickers(clause_diagnostics))
        shard_errors.update(_diagnostic_shard_errors(clause_diagnostics))
        warnings.extend(_diagnostic_warnings(clause_diagnostics))
        filing_document_roles.update(_diagnostic_filing_roles(clause_diagnostics))
        for raw_row in rows:
            row = dict(raw_row)
            object_id = str(row.get("id") or row.get("object_id") or "").strip()
            if not object_id:
                continue
            occurrence_ticker = str(row.get("ticker") or "").strip().upper()
            occurrence_key = (occurrence_ticker, object_id)
            existing = evidence_by_occurrence.get(occurrence_key)
            if existing is None:
                existing = row
                existing["_plan_clause_ids"] = []
                existing["_plan_clause_matches"] = []
                evidence_by_occurrence[occurrence_key] = existing
            clause_ids = existing.setdefault("_plan_clause_ids", [])
            if clause.clause_id not in clause_ids:
                clause_ids.append(clause.clause_id)
            clause_matches = existing.setdefault("_plan_clause_matches", [])
            predicate_terms = list(clause.required_predicates)
            relation_verified = _planned_relation_verified(row, predicate_terms)
            clause_matches.append(
                {
                    "clause_id": clause.clause_id,
                    "planned_match_mode": row.get("planned_match_mode") or "strict",
                    "planned_evidence_terms": list(
                        row.get("planned_evidence_terms") or _clause_evidence_terms(clause)
                    ),
                    "planned_predicate_terms": predicate_terms,
                    "planned_metric_terms": _clause_metric_terms(clause),
                    "planned_metric_scope": clause.metric_scope,
                    "planned_relation_verified": relation_verified,
                }
            )

    batch_query = getattr(store, "query_planned_batch_with_diagnostics", None)
    if callable(batch_query):
        clauses = []
        for clause in search_plan.clauses:
            clause_scope = execution_scope_for_clause(search_plan, clause.retrieval_query)
            clauses.append(
                {
                    "clause_id": clause.clause_id,
                    "retrieval_query": clause.retrieval_query,
                # ``retrieval_query`` already contains the compiler-selected
                # filing-language alternative plus the required semantics.
                # Passing a second non-empty hint list made the store discard
                # that selected alternative and search only generic concepts.
                "retrieval_terms": None,
                "predicate_terms": list(clause.required_predicates),
                "metrics": list(clause.metrics),
                "metric_dimensions": list(clause.metric_dimensions),
                "metric_scope": clause.metric_scope,
                "calculation_window": clause.calculation_window,
                "comparison_axes": list(search_plan.comparison_axes),
                "tickers": list(clause.tickers),
                "object_types": clause.object_types or None,
                "allow_relaxed": search_plan.uncertainty != PlanUncertainty.LOW,
                    # Internal execution metadata, not a SearchPlan schema
                    # extension.  The shard router consumes it per clause.
                    "document_types": clause_scope.document_types,
                    "periods": clause_scope.periods,
                    "metric_periods": clause_scope.metric_periods,
                    "query_intent_text": (
                        search_plan.question if clause_scope.current_intent else None
                    ),
                }
            )
        batch_payload, batch_diagnostics = batch_query(
            clauses=clauses,
            tickers=resolved_tickers,
            document_types=scope.document_types,
            periods=scope.periods,
            query_intent_text=search_plan.question if scope.current_intent else None,
            include_rejected=False,
            limit=search_plan.limit_results,
        )
        diagnostics.append(batch_diagnostics)
        unknown_tickers.extend(_diagnostic_unknown_tickers(batch_diagnostics))
        failed_tickers.extend(_diagnostic_failed_tickers(batch_diagnostics))
        shard_errors.update(_diagnostic_shard_errors(batch_diagnostics))
        warnings.extend(_diagnostic_warnings(batch_diagnostics))
        execution_routing = _safe_payload_dict(batch_diagnostics.get("routing"))
        omitted_evidence_count = int(batch_diagnostics.get("omitted_count") or 0)
        truncation_possible = bool(batch_diagnostics.get("truncation_possible"))
        for clause in search_plan.clauses:
            clause_payload = batch_payload.get(clause.clause_id) or {}
            raw_rows = clause_payload.get("rows") or []
            rows = [row for row in raw_rows if isinstance(row, Mapping)]
            clause_diagnostics = clause_payload.get("diagnostics") or {}
            if not isinstance(clause_diagnostics, Mapping):
                clause_diagnostics = {}
            record_clause_rows(clause, rows, clause_diagnostics)
    else:
        for clause in search_plan.clauses:
            clause_scope = execution_scope_for_clause(search_plan, clause.retrieval_query)
            rows, clause_diagnostics = _query_planned_compact(
                store=store,
                retrieval_query=clause.retrieval_query,
                retrieval_terms=[],
                predicate_terms=clause.required_predicates,
                metrics=clause.metrics,
                metric_dimensions=clause.metric_dimensions,
                metric_scope=clause.metric_scope,
                calculation_window=clause.calculation_window,
                comparison_axes=search_plan.comparison_axes,
                tickers=clause.tickers or resolved_tickers,
                document_types=clause_scope.document_types,
                periods=clause_scope.periods,
                metric_periods=clause_scope.metric_periods,
                query_intent_text=(
                    search_plan.question if clause_scope.current_intent else None
                ),
                object_types=clause.object_types or None,
                include_rejected=False,
                allow_relaxed=search_plan.uncertainty != PlanUncertainty.LOW,
                limit=search_plan.limit_results,
            )
            omitted_evidence_count += int(clause_diagnostics.get("omitted_count") or 0)
            truncation_possible = truncation_possible or bool(
                clause_diagnostics.get("truncation_possible")
            )
            record_clause_rows(clause, rows, clause_diagnostics)

    for row in evidence_by_occurrence.values():
        ticker = str(row.get("ticker") or "").strip().upper() or "UNKNOWN"
        results_by_ticker.setdefault(ticker, []).append(row)

    failed_tickers = _dedupe_preserving_order(failed_tickers)
    if failed_tickers:
        warnings.append("ticker_shard_query_failed")
    if omitted_evidence_count or truncation_possible:
        warnings.append("planned_evidence_truncated")
    final_routing = dict(routing_diagnostics)
    for key in (
        "fanout_parallel",
        "fanout_workers",
        "failed_tickers",
        "shard_errors",
    ):
        if key in execution_routing:
            final_routing[key] = execution_routing[key]
    final_routing["resolved_tickers"] = [
        ticker for ticker in resolved_tickers if ticker not in set(failed_tickers)
    ]
    if failed_tickers:
        final_routing["failed_tickers"] = failed_tickers
        final_routing["shard_errors"] = shard_errors

    relation_tools = [
        {
            "tool": "krw_ontology_chain",
            "object_id": object_id,
            **(
                {"ticker": str(row.get("ticker") or occurrence_ticker)}
                if row.get("ticker") or occurrence_ticker
                else {}
            ),
            "clause_id": clause.clause_id,
            "purpose": (
                "verify the planned directed relation with a structured ontology path; "
                "cross-company associations and similar-topic links are not causal proof"
            ),
        }
        for clause in search_plan.clauses
        if clause.required_predicates
        for (occurrence_ticker, object_id), row in evidence_by_occurrence.items()
        if clause.clause_id in list(row.get("_plan_clause_ids") or [])
    ]
    trace_tools = [
        {
            "tool": "krw_ontology_trace",
            "object_id": object_id,
            **(
                {"ticker": str(row.get("ticker") or occurrence_ticker)}
                if row.get("ticker") or occurrence_ticker
                else {}
            ),
            "purpose": "verify selected evidence lineage before a strong claim",
        }
        for (occurrence_ticker, object_id), row in evidence_by_occurrence.items()
        if str(row.get("trace_status") or "") in {"traceable", "traceable_metric_lineage"}
    ]
    recommended_tools = _dedupe_recommended_tools([*relation_tools, *trace_tools])[:8]
    execution_telemetry = _planned_execution_telemetry(
        diagnostics=diagnostics,
        routing=final_routing,
        omitted_evidence_count=omitted_evidence_count,
        truncation_possible=truncation_possible,
        total_ms=_elapsed_ms(started_at),
    )
    return {
        "results_by_ticker": results_by_ticker,
        "unknown_tickers": _dedupe_preserving_order(unknown_tickers),
        "filing_document_roles": filing_document_roles,
        "omitted_evidence_count": omitted_evidence_count,
        "truncation_possible": truncation_possible,
        "warnings": _dedupe_preserving_order(warnings),
        "recommended_tools": recommended_tools,
        "search_diagnostics": {
            "mode": "explicit_search_plan_v2",
            "clause_count": len(search_plan.clauses),
            "diagnostic_count": len(diagnostics),
            "routing": final_routing,
            "telemetry": execution_telemetry,
        },
        "routing": final_routing,
    }


def _clause_evidence_terms(clause: Any) -> list[str]:
    return _dedupe_preserving_order(
        [
            *list(clause.required_concepts),
            *list(clause.metrics),
            *list(clause.metric_dimensions),
        ]
    )


def _planned_relation_verified(
    row: Mapping[str, Any], predicate_terms: Sequence[str]
) -> bool:
    """Check whether a filing row contains one planned relation wording.

    Planner predicates are lexical alternatives (for example ``due to`` or
    ``driven by``), not a demand that a single filing sentence repeat every
    synonym.  Requiring every wording created false negative direct evidence
    even when the exact causal sentence was present.  This remains a narrow
    text-presence signal; it never invents an ontology edge or changes the
    evidence record itself.
    """

    normalized = [str(term).strip().casefold() for term in predicate_terms if str(term).strip()]
    if not normalized:
        return False
    # The planned-query fast path returns compact bundles: the atomic filing
    # text lives under `text` and, for exact provenance, under `object`.
    # Looking only for the old expanded-envelope fields meant a quote with the
    # requested causal wording was always marked related after compaction.
    pieces = [
        str(row.get(field) or "")
        for field in (
            "text",
            "title",
            "summary",
            "quote_text",
            "claim_text",
            "retrieval_text",
        )
    ]
    object_payload = row.get("object")
    if isinstance(object_payload, Mapping):
        pieces.extend(
            str(object_payload.get(field) or "")
            for field in (
                "quote_text",
                "claim_text",
                "normalized_claim_text",
                "description",
                "mechanism",
                "text",
                "raw_text",
            )
        )
    text = " ".join(pieces).casefold()
    return any(term in text for term in normalized)


def _clause_metric_terms(clause: Any) -> list[str]:
    return _dedupe_preserving_order([*list(clause.metrics), *list(clause.metric_dimensions)])


def clause_routing_query(clause: Any) -> str:
    """Return the production router text for one validated SearchPlan clause."""
    if (
        clause.required_concepts
        or clause.required_predicates
        or clause.metrics
        or clause.metric_dimensions
    ):
        return " ".join(
            _dedupe_preserving_order(
                [*_clause_evidence_terms(clause), *list(clause.required_predicates)]
            )
        )
    return clause.retrieval_query


def _query_planned_compact(
    *,
    store: Any,
    retrieval_query: str,
    retrieval_terms: Sequence[str],
    predicate_terms: Sequence[str],
    metrics: Sequence[str],
    metric_dimensions: Sequence[str],
    metric_scope: str,
    calculation_window: str | None,
    comparison_axes: Sequence[str],
    tickers: Sequence[str] | None,
    document_types: Sequence[str] | None,
    periods: Sequence[str] | None,
    query_intent_text: str | None,
    object_types: Sequence[str] | None,
    include_rejected: bool,
    allow_relaxed: bool,
    limit: int,
    metric_periods: Sequence[str] | None = None,
) -> tuple[list[dict[str, Any]], dict[str, Any]]:
    """Call the v2 planned primitive without falling back to keyword routing."""
    planned_query = getattr(store, "query_planned_compact_with_diagnostics", None)
    if callable(planned_query):
        return planned_query(
            retrieval_query=retrieval_query,
            retrieval_terms=retrieval_terms,
            predicate_terms=predicate_terms,
            metrics=metrics,
            metric_dimensions=metric_dimensions,
            metric_scope=metric_scope,
            calculation_window=calculation_window,
            comparison_axes=comparison_axes,
            tickers=tickers,
            document_types=document_types,
            periods=periods,
            metric_periods=metric_periods,
            query_intent_text=query_intent_text,
            object_types=object_types,
            include_rejected=include_rejected,
            allow_relaxed=allow_relaxed,
            limit=limit,
        )

    store_for_ticker = getattr(store, "_store_for_ticker", None)
    if tickers and callable(store_for_ticker):
        rows: list[dict[str, Any]] = []
        unavailable_tickers: list[str] = []
        strict_count = 0
        relaxed_count = 0
        for ticker in tickers:
            try:
                ticker_store = store_for_ticker(ticker)
            except (KeyError, FileNotFoundError):
                unavailable_tickers.append(str(ticker).upper())
                continue
            ticker_rows, ticker_diagnostics = ticker_store.query_planned_compact_with_diagnostics(
                retrieval_query=retrieval_query,
                retrieval_terms=retrieval_terms,
                predicate_terms=predicate_terms,
                metrics=metrics,
                metric_dimensions=metric_dimensions,
                metric_scope=metric_scope,
                calculation_window=calculation_window,
                comparison_axes=comparison_axes,
                tickers=[ticker],
                document_types=document_types,
                periods=periods,
                metric_periods=metric_periods,
                query_intent_text=query_intent_text,
                object_types=object_types,
                include_rejected=include_rejected,
                allow_relaxed=allow_relaxed,
                limit=limit,
            )
            rows.extend(ticker_rows)
            strict_count += int(ticker_diagnostics.get("strict_result_count") or 0)
            relaxed_count += int(ticker_diagnostics.get("relaxed_result_count") or 0)
        return rows[:limit], {
            "execution_mode": "planned_fts_company_fanout",
            "retrieval_query": retrieval_query,
            "strict_result_count": strict_count,
            "relaxed_result_count": relaxed_count,
            "relaxed_enabled": allow_relaxed,
            "result_count": min(len(rows), limit),
            "unavailable_tickers": unavailable_tickers,
            "warnings": ["ticker_not_available"] if unavailable_tickers else [],
            "keyword_expansion_used": False,
            "intent_reclassification_used": False,
        }

    return [], {
        "execution_mode": "planned_fts_unavailable",
        "retrieval_query": retrieval_query,
        "strict_result_count": 0,
        "relaxed_result_count": 0,
        "relaxed_enabled": allow_relaxed,
        "result_count": 0,
        "warnings": ["planned_ticker_discovery_unavailable"],
        "keyword_expansion_used": False,
        "intent_reclassification_used": False,
    }


def _diagnostic_unknown_tickers(diagnostics: Mapping[str, Any]) -> list[str]:
    values: list[str] = []
    for container in (diagnostics, _safe_payload_dict(diagnostics.get("routing"))):
        for key in ("unknown_tickers", "unavailable_tickers"):
            raw = container.get(key)
            if isinstance(raw, Sequence) and not isinstance(raw, (str, bytes)):
                values.extend(str(value).strip().upper() for value in raw if str(value).strip())
    return _dedupe_preserving_order(values)


def _diagnostic_failed_tickers(diagnostics: Mapping[str, Any]) -> list[str]:
    values: list[str] = []
    for container in (diagnostics, _safe_payload_dict(diagnostics.get("routing"))):
        raw = container.get("failed_tickers")
        if isinstance(raw, Sequence) and not isinstance(raw, (str, bytes)):
            values.extend(str(value).strip().upper() for value in raw if str(value).strip())
        errors = container.get("shard_errors")
        if isinstance(errors, Mapping):
            values.extend(str(value).strip().upper() for value in errors)
    return _dedupe_preserving_order(values)


def _diagnostic_shard_errors(diagnostics: Mapping[str, Any]) -> dict[str, str]:
    values: dict[str, str] = {}
    for container in (diagnostics, _safe_payload_dict(diagnostics.get("routing"))):
        errors = container.get("shard_errors")
        if not isinstance(errors, Mapping):
            continue
        values.update(
            {
                str(ticker).strip().upper(): str(error).strip()
                for ticker, error in errors.items()
                if str(ticker).strip()
            }
        )
    return values


def _planned_execution_telemetry(
    *,
    diagnostics: Sequence[Mapping[str, Any]],
    routing: Mapping[str, Any],
    omitted_evidence_count: int,
    truncation_possible: bool,
    total_ms: int,
) -> dict[str, Any]:
    leaves: list[Mapping[str, Any]] = []
    for diagnostic in diagnostics:
        shard_diagnostics = diagnostic.get("shard_diagnostics")
        if isinstance(shard_diagnostics, Mapping):
            leaves.extend(
                value for value in shard_diagnostics.values() if isinstance(value, Mapping)
            )
        else:
            leaves.append(diagnostic)
    route_timing = _safe_payload_dict(routing.get("timing_ms"))
    batch_diagnostic = next(
        (
            diagnostic
            for diagnostic in diagnostics
            if diagnostic.get("execution_mode") == "planned_shard_batch"
            and "clause_count" in diagnostic
        ),
        {},
    )
    batch_timing = _safe_payload_dict(batch_diagnostic.get("timing_ms"))
    route_queries = routing.get("queries")
    score_summaries = []
    if isinstance(route_queries, Sequence) and not isinstance(
        route_queries,
        (str, bytes),
    ):
        score_summaries = [
            dict(summary)
            for item in route_queries
            if isinstance(item, Mapping)
            and isinstance((summary := item.get("score_summary")), Mapping)
        ]
    return {
        "total_ms": total_ms,
        "route_ms": int(route_timing.get("total") or 0),
        "batch_ms": int(batch_timing.get("total") or 0),
        "shard_open_count": int(batch_diagnostic.get("shard_open_count") or 0),
        "fanout_workers": int(batch_diagnostic.get("fanout_workers") or 0),
        "strict_rows": sum(int(item.get("strict_result_count") or 0) for item in leaves),
        "relaxed_rows": sum(int(item.get("relaxed_result_count") or 0) for item in leaves),
        "metric_rows": sum(int(item.get("metric_result_count") or 0) for item in leaves),
        "clause_query_ms": sum(
            int(_safe_payload_dict(item.get("timing_ms")).get("total") or 0) for item in leaves
        ),
        "pre_truncation_count": int(batch_diagnostic.get("pre_truncation_count") or 0),
        "omitted_evidence_count": omitted_evidence_count,
        "truncation_possible": truncation_possible,
        "route_score_summaries": score_summaries,
        "failed_ticker_count": len(_diagnostic_failed_tickers(routing)),
    }


def _diagnostic_warnings(diagnostics: Mapping[str, Any]) -> list[str]:
    values: list[str] = []
    containers: list[Mapping[str, Any]] = [diagnostics]
    routing = _safe_payload_dict(diagnostics.get("routing"))
    if routing:
        containers.append(routing)
    shard_diagnostics = diagnostics.get("shard_diagnostics")
    if isinstance(shard_diagnostics, Mapping):
        containers.extend(
            value for value in shard_diagnostics.values() if isinstance(value, Mapping)
        )
    for container in containers:
        raw = container.get("warnings")
        if isinstance(raw, Sequence) and not isinstance(raw, (str, bytes)):
            values.extend(str(value).strip() for value in raw if str(value).strip())
        if container.get("clause_query_error"):
            values.append("planned_clause_query_failed")
    return _dedupe_preserving_order(values)


def _diagnostic_filing_roles(diagnostics: Mapping[str, Any]) -> dict[str, Any]:
    current_prior = _safe_payload_dict(diagnostics.get("current_document_prior"))
    roles = current_prior.get("filing_document_roles")
    return dict(roles) if isinstance(roles, Mapping) else {}


def _dedupe_preserving_order(values: Sequence[str]) -> list[str]:
    result: list[str] = []
    seen: set[str] = set()
    for value in values:
        normalized = str(value).strip()
        key = normalized.casefold()
        if not normalized or key in seen:
            continue
        seen.add(key)
        result.append(normalized)
    return result


def _dedupe_recommended_tools(
    values: Sequence[Mapping[str, Any]],
) -> list[dict[str, Any]]:
    result: list[dict[str, Any]] = []
    seen: set[tuple[str, str, str, str]] = set()
    for value in values:
        payload = dict(value)
        key = (
            str(payload.get("tool") or ""),
            str(payload.get("object_id") or ""),
            str(payload.get("ticker") or ""),
            str(payload.get("clause_id") or ""),
        )
        if not key[0] or key in seen:
            continue
        seen.add(key)
        result.append(payload)
    return result


def _markdown_index_context(payload: Mapping[str, Any]) -> str:
    guard = payload.get("index_context_guard") or {}
    return "\n".join(
        [
            "# KRW Ontology Index Context",
            f"- status: {payload.get('index_status')}",
            f"- schema: {payload.get('schema_version')}",
            f"- tickers: {', '.join(payload.get('available_tickers') or [])}",
            f"- guard_mode: {guard.get('mode')}",
            f"- counts_returned: {guard.get('counts_returned')}",
            f"- quality_summary_returned: {guard.get('quality_summary_returned')}",
            f"- recommended_normal_research_tool: {guard.get('recommended_normal_research_tool')}",
        ]
    )


def _markdown_company_context(payload: Mapping[str, Any]) -> str:
    lines = [f"# {payload.get('ticker')} Company Context"]
    for topic in payload.get("company_topics") or []:
        lines.append(f"- {topic.get('topic_label')}: {topic.get('topic_summary')}")
    return "\n".join(lines)


def _markdown_query_context(payload: Mapping[str, Any]) -> str:
    answerability = payload.get("answerability") or {}
    autonomy = payload.get("agent_autonomy") or {}
    guard = _directness_guard_from_research_context(payload)
    stop_guard = _stop_guard_from_research_context(payload)
    lines = [
        "# KRW Ontology Query Context",
        f"- research_status: {payload.get('research_status')}",
        f"- direct_answerable: {answerability.get('direct_answerable')}",
        f"- related_context_available: {answerability.get('related_context_available')}",
        f"- recommended_answer_mode: {answerability.get('recommended_answer_mode')}",
        f"- strong_claim_allowed: {guard.get('strong_claim_allowed')}",
        f"- strong_claim_requires: {', '.join(guard.get('strong_claim_requires') or [])}",
        f"- allowed_next_tools: {', '.join(autonomy.get('allowed_next_tools') or [])}",
        f"- max_additional_tool_calls: {autonomy.get('max_additional_tool_calls')}",
    ]
    if stop_guard:
        lines.append(f"- cannot_answer_reason: {stop_guard.get('cannot_answer_reason')}")
    research_pack = (
        payload.get("research_pack") if isinstance(payload.get("research_pack"), Mapping) else {}
    )
    current_anchors = payload.get("current_document_anchors")
    if not isinstance(current_anchors, Mapping) and isinstance(research_pack, Mapping):
        current_anchors = research_pack.get("current_document_anchors")
    if isinstance(current_anchors, Mapping) and current_anchors:
        anchor_text = ", ".join(
            str(
                anchor.get("source_label")
                or f"{ticker} {anchor.get('period')} {anchor.get('document_type')}"
            )
            for ticker, anchor in current_anchors.items()
            if isinstance(anchor, Mapping)
        )
        if anchor_text:
            lines.append(f"- current_document_anchors: {anchor_text}")
    filing_roles = payload.get("filing_document_roles")
    if not isinstance(filing_roles, Mapping) and isinstance(research_pack, Mapping):
        filing_roles = research_pack.get("filing_document_roles")
    if isinstance(filing_roles, Mapping) and filing_roles:
        role_parts: list[str] = []
        for ticker, role_payload in filing_roles.items():
            if not isinstance(role_payload, Mapping):
                continue
            current_driver = role_payload.get("current_driver")
            annual_baseline = role_payload.get("annual_baseline")
            current_label = (
                current_driver.get("source_label") if isinstance(current_driver, Mapping) else None
            )
            annual_label = (
                annual_baseline.get("source_label")
                if isinstance(annual_baseline, Mapping)
                else None
            )
            role_parts.append(
                f"{ticker} current_driver={current_label or 'n/a'} "
                f"annual_baseline={annual_label or 'n/a'}"
            )
        if role_parts:
            lines.append(f"- filing_document_roles: {'; '.join(role_parts)}")
    cross_company_pack = (
        research_pack.get("cross_company_signal_pack")
        if isinstance(research_pack, Mapping)
        else None
    )
    if isinstance(cross_company_pack, Mapping):
        lines.append("- cross_company_signal_pack: available")
        if cross_company_pack.get("latest_period_anchor"):
            lines.append(
                f"- latest_period_anchor: {cross_company_pack.get('latest_period_anchor')}"
            )
        for signal in (cross_company_pack.get("signals") or [])[:4]:
            if not isinstance(signal, Mapping):
                continue
            companies = ", ".join(str(value) for value in signal.get("companies_supporting") or [])
            lines.append(
                f"- signal: {signal.get('signal')} | companies: {companies} | strength: {signal.get('strength')}"
            )
        for row in (cross_company_pack.get("company_evidence_rows") or [])[:6]:
            if not isinstance(row, Mapping):
                continue
            summary = str(row.get("commentary_summary") or "").replace("\n", " ").strip()
            if len(summary) > 180:
                summary = summary[:177] + "..."
            lines.append(
                f"- evidence: {row.get('ticker')} {row.get('period')} {row.get('document_type')} "
                f"{row.get('signal')} ({row.get('evidence_strength')}): {summary}"
            )
    for candidate in payload.get("ticker_candidates") or []:
        lines.append(
            f"- {candidate.get('ticker')}: {candidate.get('tier') or candidate.get('top_tier')}"
        )
    return "\n".join(lines)


def _compare_periods(
    store: OntologyStore,
    *,
    ticker: str,
    topic: str | None,
    metric: str | None,
    document_types: list[str] | None,
    periods: list[str],
    limit_per_period: int,
    compact: bool,
) -> dict[str, Any]:
    ticker = ticker.upper()
    period_values = _upper_list(periods)

    def run_period(period: str, period_store: OntologyStore) -> tuple[str, list[dict[str, Any]]]:
        if metric:
            compare_fn = period_store.compare_compact if compact else period_store.compare
            period_result = compare_fn(
                tickers=[ticker],
                metric=metric,
                document_types=document_types,
                periods=[period],
                limit_per_ticker=limit_per_period,
            )
            return period, period_result["results"].get(ticker, [])
        if compact:
            rows, _diagnostics = period_store.query_compact_with_diagnostics(
                topic=topic,
                tickers=[ticker],
                document_types=document_types,
                periods=[period],
                limit=limit_per_period,
            )
            return period, rows
        return period, period_store.query(
            topic=topic,
            tickers=[ticker],
            document_types=document_types,
            periods=[period],
            limit=limit_per_period,
        )

    results: dict[str, list[dict[str, Any]]] = {}
    if compact and len(period_values) > 1:
        from concurrent.futures import ThreadPoolExecutor, as_completed

        worker_count = min(len(period_values), 4)
        period_results: dict[str, list[dict[str, Any]]] = {}
        with ThreadPoolExecutor(max_workers=worker_count) as executor:

            def run_period_with_own_store(period: str) -> tuple[str, list[dict[str, Any]]]:
                with _store(store.index_path) as period_store:
                    return run_period(period, period_store)

            futures = {}
            for period in period_values:
                futures[executor.submit(run_period_with_own_store, period)] = period
            for future in as_completed(futures):
                period, rows = future.result()
                period_results[period] = rows
        results = {period: period_results.get(period, []) for period in period_values}
    else:
        for period in period_values:
            period, rows = run_period(period, store)
            results[period] = rows
    return {
        "mode": "period_metric" if metric else "period_topic",
        "ticker": ticker,
        "topic": topic,
        "metric": metric,
        "periods": period_values,
        "results": results,
    }


_RUNTIME_PATH_OVERRIDE_ARGS = {
    "root",
    "index_path",
    "global_spine_path",
    "release_root",
}


def _runtime_root() -> Path:
    return resolve_ontology_root(None, fallback_to_cwd=False)


def _runtime_global_spine_path() -> Path:
    raw_index_path = os.environ.get(ONTOLOGY_GLOBAL_SPINE_PATH_ENV)
    if raw_index_path:
        return Path(raw_index_path).expanduser().absolute()
    return (_runtime_root() / GLOBAL_SPINE_RELATIVE_PATH).absolute()


def _runtime_override_error(
    extra_args: Mapping[str, Any] | None,
    response_format: ResponseFormat,
) -> str | None:
    if not extra_args:
        return None
    forbidden = sorted(str(key) for key in extra_args if str(key) in _RUNTIME_PATH_OVERRIDE_ARGS)
    if not forbidden:
        return None
    payload = _error_payload(
        "runtime_path_override_not_allowed",
        "MCP tools read only the configured v3 release runtime.",
        "Set KRW_ONTOLOGY_RELEASE_ROOT before starting the MCP server; do not pass root or index paths to tools.",
    )
    payload["forbidden_args"] = forbidden
    return _format_response(payload, response_format, _markdown_error)


def _store(index_path: Path) -> Any:
    if not index_path.exists():
        raise FileNotFoundError(
            "KRW Ontology v3 global spine not found at "
            f"{index_path}. Build and promote a release with: "
            "uv run krw-ontology release force"
        )
    if _persistent_store_enabled():
        return _STORE_POOL.acquire(index_path)
    return open_ontology_store(index_path, routing="spine")


def trusted_guru_light_company_context(*, ticker: str) -> dict[str, Any]:
    """Return trusted neutral company orientation for the Guru runtime.

    The caller supplies only the authenticated ticker. The ontology release
    derives the company name, business description, activities, revenue
    vocabulary, and filing availability; no user or model text is copied into
    this projection.
    """
    normalized = str(ticker or "").strip().upper()
    index = _runtime_global_spine_path()
    with _store(index) as store:
        return store.guru_light_company_context(ticker=normalized)


def reset_mcp_runtime_caches() -> None:
    """Reset MCP process-local caches. Intended for tests and release restarts."""
    _STORE_POOL.reset()
    with _TRACE_TOOL_CACHE_LOCK:
        _TRACE_TOOL_CACHE.clear()


def mcp_runtime_cache_status() -> dict[str, Any]:
    """Return MCP process-local cache and persistent SQLite store status."""
    from krw_capability_runtime.agent_index.retriever import retriever_cache_status

    with _TRACE_TOOL_CACHE_LOCK:
        trace_cache = {
            "size": len(_TRACE_TOOL_CACHE),
            "max": _TRACE_TOOL_CACHE_MAX,
        }
    return {
        "store": _STORE_POOL.status(),
        "trace": trace_cache,
        "query_cache": agent_index_cache_status(),
        "retriever": retriever_cache_status(),
    }


def _coerce_response_detail(response_detail: ResponseDetail | str) -> ResponseDetail:
    try:
        detail = ResponseDetail(response_detail)
    except ValueError:
        return ResponseDetail.COMPACT
    return detail


def _response_detail_value(response_detail: ResponseDetail | str) -> str:
    return (
        response_detail.value
        if isinstance(response_detail, ResponseDetail)
        else str(response_detail)
    )


def _response_detail_policy(
    requested_response_detail: ResponseDetail | str,
    effective_response_detail: ResponseDetail,
) -> dict[str, str]:
    requested = _response_detail_value(requested_response_detail)
    if (
        requested == ResponseDetail.FULL.value
        and effective_response_detail == ResponseDetail.COMPACT
    ):
        action = "downgraded_full_disabled"
    elif requested == effective_response_detail.value:
        action = "as_requested"
    else:
        action = "defaulted_to_compact"
    return {
        "requested": requested,
        "effective": effective_response_detail.value,
        "action": action,
    }


def _normalize_metric_query_topic_for_tool(
    topic: str | None,
    *,
    periods: Sequence[str] | None,
    object_types: Sequence[str] | None,
) -> tuple[str | None, dict[str, Any]]:
    if not topic or not periods:
        return topic, {}
    if not set(object_types or ()).intersection({"MetricObservation", "Calculation"}):
        return topic, {}
    original = " ".join(str(topic).split())
    removed_tokens: list[str] = []

    def replace_period_token(match: re.Match[str]) -> str:
        removed_tokens.append(match.group(0))
        return " "

    normalized = re.sub(
        r"\b(?:CY|FY)?(?:19|20)\d{2}(?:Q[1-4])?\b",
        replace_period_token,
        original,
        flags=re.IGNORECASE,
    )
    normalized = " ".join(normalized.split())
    if normalized == original:
        return topic, {}
    period_years = sorted(
        {
            int(year)
            for period in periods or []
            for year in re.findall(r"(?:19|20)\d{2}", str(period or ""))
        }
    )
    topic_years = sorted({int(year) for year in re.findall(r"\b((?:19|20)\d{2})\b", original)})
    diagnostics: dict[str, Any] = {
        "original_topic": original,
        "normalized_topic": normalized,
        "removed_period_tokens": removed_tokens,
        "reason": "metric_query_period_tokens_are_filters_not_fts_terms",
    }
    if period_years:
        diagnostics["period_years"] = period_years
        topic_only_years = [year for year in topic_years if year not in period_years]
        if topic_only_years:
            diagnostics["warning"] = "topic_years_differ_from_period_filters"
            diagnostics["topic_only_years"] = topic_only_years
    return normalized or topic, diagnostics


def _coerce_group_by(group_by: str | None) -> str | None:
    if not group_by:
        return None
    normalized = str(group_by).strip().lower()
    if normalized in {"ticker", "tickers"}:
        return "ticker"
    return None


def _normalize_object_types(
    object_types: list[str] | None,
) -> tuple[list[str] | None, list[str]]:
    if not object_types:
        return None, []
    normalized: list[str] = []
    invalid: list[str] = []
    for raw_value in object_types:
        value = str(raw_value).strip()
        if not value:
            continue
        if value in _ALLOWED_OBJECT_TYPES:
            normalized.append(value)
            continue
        alias_key = value.lower().replace("-", "_").replace(" ", "_")
        mapped = _OBJECT_TYPE_ALIASES.get(alias_key)
        if mapped:
            normalized.extend(mapped)
            continue
        invalid.append(value)
    return _unique(normalized), invalid


def normalize_execution_search_plan(search_plan: SearchPlan) -> tuple[SearchPlan, list[str]]:
    """Return the safe physical plan for read-only execution.

    ``SearchPlan`` intentionally keeps object types as an open vocabulary so
    an older agent image can be decoded without an MCP contract failure.
    Execution is stricter in a different direction: known legacy aliases are
    converted to serving types and unknown filters are dropped.  Dropping an
    unrecognised *narrowing* filter is safer than silently claiming the filing
    has no evidence because a model used an obsolete ontology label.

    The returned plan remains a normal canonical ``SearchPlan``; no extra
    model turn, repair protocol, or tool capability is introduced.
    """

    clauses = []
    for clause in search_plan.clauses:
        normalized, _unknown = _normalize_object_types(list(clause.object_types))
        clauses.append(clause.model_copy(update={"object_types": normalized or []}))
    return search_plan.model_copy(update={"clauses": clauses}), []


_EXPLICIT_DOCUMENT_TYPE_RE = re.compile(
    r"\b(?:form\s+)?(10\s*[- ]?\s*[kq])\b|\b(annual|quarterly)\s+report\b|"
    r"(?:사업|분기)\s*보고서",
    flags=re.IGNORECASE,
)
_EXPLICIT_PERIOD_RE = re.compile(
    r"\b(?:FY|CY)\s*(?:19|20)\d{2}(?:\s*Q[1-4])?\b|"
    r"\b(?:19|20)\d{2}\s*Q[1-4]\b|"
    r"\b(?:19|20)\d{2}\b|"
    r"(?:19|20)\d{2}\s*년(?:\s*[1-4]\s*분기)?|[1-4]\s*분기",
    flags=re.IGNORECASE,
)
_CURRENT_INTENT_RE = re.compile(
    r"\b(?:latest|recent|current|newest|last\s+quarter|this\s+quarter)\b|"
    r"최근|최신|현재|직전\s*분기|이번\s*분기|요즘",
    flags=re.IGNORECASE,
)
_AMBIGUOUS_FISCAL_QUARTER_RE = re.compile(
    r"\bFY\s*(?:19|20)\d{2}\s*Q[1-4]\b|"
    r"\b(?:19|20)\d{2}\s*(?:fiscal\s+year|fiscal)\s*Q?[1-4]\b|"
    r"(?:19|20)\d{2}\s*회계\s*연도\s*[1-4]\s*분기|"
    r"FY\s*(?:19|20)\d{2}\s*[1-4]\s*분기",
    flags=re.IGNORECASE,
)
_EXPLICIT_FISCAL_QUARTER_PATTERNS = (
    re.compile(
        r"\bFY\s*(?P<year>(?:19|20)\d{2})\s*Q?\s*(?P<quarter>[1-4])\b",
        flags=re.IGNORECASE,
    ),
    re.compile(
        r"\b(?P<year>(?:19|20)\d{2})\s*(?:fiscal\s+year|fiscal)\s*Q?\s*(?P<quarter>[1-4])\b",
        flags=re.IGNORECASE,
    ),
    re.compile(
        r"(?P<year>(?:19|20)\d{2})\s*회계\s*연도\s*(?P<quarter>[1-4])\s*분기",
        flags=re.IGNORECASE,
    ),
)


def _explicit_document_types_from_question(question: str) -> list[str]:
    text = str(question or "")
    document_types: list[str] = []
    for match in _EXPLICIT_DOCUMENT_TYPE_RE.finditer(text):
        normalized = "".join(part for part in match.groups() if part).casefold()
        if "10" in normalized and "k" in normalized:
            document_types.append("10-K")
        elif "10" in normalized and "q" in normalized:
            document_types.append("10-Q")
        elif "annual" in normalized or "사업" in match.group(0):
            document_types.append("10-K")
        elif "quarterly" in normalized or "분기" in match.group(0):
            document_types.append("10-Q")
    return _dedupe_preserving_order(document_types)


def _question_period_years(question: str) -> set[str]:
    return {
        match.group(0)[:4]
        for match in re.finditer(r"(?:19|20)\d{2}", str(question or ""))
    }


def _explicit_period_scope(search_plan: SearchPlan, text: str) -> list[str] | None:
    """Return a physical period filter only when one textual scope is unambiguous."""

    if _AMBIGUOUS_FISCAL_QUARTER_RE.search(text):
        # An issuer FY quarter and an ontology CY quarter can have different
        # dates. Do not convert FY2024Q3 -> CY2024Q3 or use the same
        # coordinate as an exact filter; the broad read retains the confirmed
        # source label.
        return None
    if not _EXPLICIT_PERIOD_RE.search(text):
        return None
    requested_years = _question_period_years(text)
    candidate_periods = [str(period).strip() for period in search_plan.periods if str(period).strip()]
    compatible_periods = [
        period
        for period in candidate_periods
        if not requested_years or any(year in period for year in requested_years)
    ]
    # If the planner could not retain a compatible canonical identifier, do
    # not invent an FY<->CY mapping. Broad retrieval is truthful and lets
    # returned source metadata establish the basis later.
    return _dedupe_preserving_order(compatible_periods) or None


def _explicit_metric_period_scope(search_plan: SearchPlan, text: str) -> list[str] | None:
    """Keep an explicit issuer-fiscal coordinate for metric lookup only.

    Filing narrative rows are indexed by the document's calendar routing
    bucket, so using ``FY2024Q3`` as their physical filter can create a false
    empty result. Metric observations have their own issuer fiscal year and
    quarter, however. Losing the period entirely makes an explicit fiscal
    question select the newest metric instead. This helper preserves the
    user-requested label without ever converting it to a calendar quarter.
    """

    fiscal_quarters = _explicit_fiscal_quarter_labels(text)
    if fiscal_quarters:
        return fiscal_quarters
    if not _EXPLICIT_PERIOD_RE.search(text):
        return None
    requested_years = _question_period_years(text)
    compatible_periods = [
        str(period).strip()
        for period in search_plan.periods
        if str(period).strip()
        and (
            not requested_years
            or any(year in str(period) for year in requested_years)
        )
    ]
    return _dedupe_preserving_order(compatible_periods) or None


def _explicit_fiscal_quarter_labels(text: str) -> list[str]:
    """Extract a user-authored fiscal coordinate without planner conversion."""

    labels: list[str] = []
    for pattern in _EXPLICIT_FISCAL_QUARTER_PATTERNS:
        for match in pattern.finditer(str(text or "")):
            labels.append(f"FY{match.group('year')}Q{match.group('quarter')}")
    return _dedupe_preserving_order(labels)


def execution_scope_for_plan(search_plan: SearchPlan) -> ExecutionScope:
    """Choose hard filters from explicit user wording, never planner guesses.

    The planner may reasonably include an annual baseline in a "recent"
    proposal.  Passing that baseline back as an exact physical filter is a
    category error: it prevents the retrieval layer from selecting the latest
    compatible document.  Explicit user scope remains exact; ambiguous scope
    deliberately stays broad and is ranked by the store's deterministic
    current-document policy.
    """

    question = str(search_plan.question or "")
    explicit_document_types = _explicit_document_types_from_question(question)
    explicit_period = bool(_EXPLICIT_PERIOD_RE.search(question))

    if explicit_document_types:
        document_types: list[str] | None = explicit_document_types
    else:
        document_types = None

    periods = _explicit_period_scope(search_plan, question) if explicit_period else None
    metric_periods = (
        _explicit_metric_period_scope(search_plan, question) if explicit_period else None
    )

    current_intent = bool(_CURRENT_INTENT_RE.search(question)) and not explicit_period
    return ExecutionScope(
        document_types=document_types,
        periods=periods,
        metric_periods=metric_periods,
        current_intent=current_intent,
    )


def execution_scope_for_clause(search_plan: SearchPlan, retrieval_query: str) -> ExecutionScope:
    """Derive a clause-local physical time scope for a mixed user question.

    SearchPlan keeps the original question at plan level, but its clauses are
    independent evidence obligations.  In a question such as "FY2022 revenue
    and recent risks", applying FY2022 to every clause silently hides the
    current risk filing.  The compiler-preserved retrieval query is the only
    clause-level literal available without extending the ontology wire schema.
    If it does not retain a historical literal, prefer the question's current
    intent over incorrectly forcing that clause into another clause's history.
    """

    question = str(search_plan.question or "")
    query = str(retrieval_query or "")
    has_explicit_period = bool(_EXPLICIT_PERIOD_RE.search(question))
    has_current_intent = bool(_CURRENT_INTENT_RE.search(question))
    if not (has_explicit_period and has_current_intent):
        return execution_scope_for_plan(search_plan)

    document_types = _explicit_document_types_from_question(question) or None
    if _EXPLICIT_PERIOD_RE.search(query):
        return ExecutionScope(
            document_types=document_types,
            periods=_explicit_period_scope(search_plan, query),
            metric_periods=_explicit_metric_period_scope(search_plan, query),
            current_intent=False,
        )
    # The current wording may live in a different user clause and therefore
    # not be repeated by every safe retrieval alternative.  Never let an
    # unrelated historical filter erase the current evidence frontier.
    return ExecutionScope(
        document_types=document_types,
        periods=None,
        metric_periods=None,
        current_intent=True,
    )


def _answer_candidate_object_types(object_types: list[str] | None) -> list[str]:
    if object_types is None:
        return list(DISCOVERY_OBJECT_TYPES)
    filtered = [
        object_type for object_type in object_types if object_type not in TRACE_ONLY_OBJECT_TYPES
    ]
    return filtered or list(DISCOVERY_OBJECT_TYPES)


def _is_answer_candidate_bundle(bundle: Mapping[str, Any]) -> bool:
    return str(bundle.get("type") or "") not in TRACE_ONLY_OBJECT_TYPES


def _answer_candidate_bundles(bundles: Sequence[Mapping[str, Any]]) -> list[Mapping[str, Any]]:
    return [bundle for bundle in bundles if _is_answer_candidate_bundle(bundle)]


def _ids_only_bundle(bundle: Mapping[str, Any]) -> dict[str, Any]:
    obj = bundle.get("object")
    return {
        "id": bundle.get("id"),
        "type": bundle.get("type"),
        "ticker": _object_value(obj, "ticker") or bundle.get("ticker"),
        "document_id": bundle.get("document_id"),
    }


def _ticker_summary_payload(
    bundles: Sequence[Mapping[str, Any]],
    *,
    limit_groups: int,
    limit_per_group: int,
) -> dict[str, Any]:
    grouped: dict[str, list[Mapping[str, Any]]] = {}
    for bundle in bundles:
        if not _is_answer_candidate_bundle(bundle):
            continue
        grouped.setdefault(_ticker_from_bundle(bundle), []).append(bundle)

    candidates: list[dict[str, Any]] = []
    results_by_ticker: dict[str, list[dict[str, Any]]] = {}
    for ticker, ticker_bundles in grouped.items():
        ranked = sorted(ticker_bundles, key=_bundle_score, reverse=True)
        selected = ranked[:limit_per_group]
        compact_objects = [_compact_bundle(bundle) for bundle in selected]
        results_by_ticker[ticker] = compact_objects
        candidates.append(
            {
                "ticker": ticker,
                "score": round(sum(max(_bundle_score(bundle), 0.0) for bundle in selected), 3),
                "tier": _ticker_tier(ticker_bundles),
                "matched_object_counts": _object_type_counts(ticker_bundles),
                "evidence_counts": _evidence_counts(ticker_bundles),
                "top_reasons": [_bundle_reason(bundle) for bundle in selected],
                "top_object_ids": [
                    str(bundle.get("id")) for bundle in selected if bundle.get("id")
                ],
                "top_objects": compact_objects,
            }
        )

    candidates.sort(key=lambda item: float(item.get("score") or 0.0), reverse=True)
    candidates = candidates[:limit_groups]
    allowed = {candidate["ticker"] for candidate in candidates}
    return {
        "ticker_candidates": candidates,
        "results_by_ticker": {
            ticker: results_by_ticker[ticker]
            for ticker in sorted(allowed)
            if ticker in results_by_ticker
        },
    }


def _ticker_from_bundle(bundle: Mapping[str, Any]) -> str:
    obj = bundle.get("object")
    ticker = _object_value(obj, "ticker") or bundle.get("ticker")
    return str(ticker or "UNKNOWN")


def _bundle_score(bundle: Mapping[str, Any]) -> float:
    object_type = str(bundle.get("type") or "")
    if object_type in TRACE_ONLY_OBJECT_TYPES:
        return -100.0
    score = DISCOVERY_TYPE_SCORES.get(object_type, 1.0)
    evidence = bundle.get("evidence") or {}
    score += min(len(evidence.get("quotes") or []), 3) * 2.0
    score += min(len(evidence.get("claims") or []), 3) * 1.5
    score += min(len(evidence.get("metrics") or []), 2) * 0.75
    obj = bundle.get("object")
    grade = str(_object_value(obj, "evidence_grade") or "").lower() if obj is not None else ""
    if grade == "direct":
        score += 3.0
    elif grade == "indirect":
        score += 1.0
    return score


def _ticker_tier(bundles: Sequence[Mapping[str, Any]]) -> str:
    object_types = {str(bundle.get("type") or "") for bundle in bundles}
    if object_types & {"ExternalFactorExposure", "EvidenceQuote", "ResearchClaim"}:
        return "direct"
    if object_types & {
        "BusinessFactor",
        "BusinessActivity",
        "BusinessEvent",
        "AgreementTerm",
        "MetricObservation",
    }:
        return "related"
    if object_types:
        return "inferred"
    return "insufficient"


def _object_type_counts(bundles: Sequence[Mapping[str, Any]]) -> dict[str, int]:
    counts: dict[str, int] = {}
    for bundle in bundles:
        object_type = str(bundle.get("type") or "Unknown")
        counts[object_type] = counts.get(object_type, 0) + 1
    return dict(sorted(counts.items()))


def _evidence_counts(bundles: Sequence[Mapping[str, Any]]) -> dict[str, int]:
    counts = {"quotes": 0, "claims": 0, "metrics": 0}
    for bundle in bundles:
        evidence = bundle.get("evidence") or {}
        counts["quotes"] += len(evidence.get("quotes") or [])
        counts["claims"] += len(evidence.get("claims") or [])
        counts["metrics"] += len(evidence.get("metrics") or [])
    return counts


def _bundle_reason(bundle: Mapping[str, Any]) -> str:
    object_type = str(bundle.get("type") or "")
    obj = bundle.get("object")
    title = None
    for attr in (
        "label",
        "title",
        "factor_name",
        "claim_text",
        "quote_text",
        "activity_name",
        "event_name",
    ):
        value = _object_value(obj, attr) if obj is not None else None
        if value:
            title = str(value)
            break
    if not title:
        title = str(bundle.get("text") or bundle.get("id") or "")
    title = title.replace("\n", " ").strip()
    if len(title) > 140:
        title = title[:137].rstrip() + "..."
    return f"{object_type}: {title}" if object_type else title


def _object_value(obj: Any, key: str) -> Any:
    if obj is None:
        return None
    if isinstance(obj, Mapping):
        return obj.get(key)
    return getattr(obj, key, None)


def _compact_retrieval(result: dict[str, Any]) -> dict[str, Any]:
    payload = {
        key: value
        for key, value in result.items()
        if key
        not in {
            "direct_evidence",
            "related_context",
            "rejected_context",
            "results",
            "compare",
            "quality",
        }
    }
    for field_name in ("direct_evidence", "related_context", "rejected_context"):
        values = result.get(field_name)
        payload[field_name] = (
            [_compact_bundle(item) for item in values] if isinstance(values, list) else []
        )
    if "compare" in result:
        payload["compare"] = _compact_compare(result["compare"])
    if "quality" in result:
        payload["quality"] = _compact_quality_payload(result["quality"])
    return payload


def _compact_compare(result: dict[str, Any]) -> dict[str, Any]:
    return {
        **{
            key: value
            for key, value in result.items()
            if key not in {"results", "comparison_contexts"}
        },
        "results": {
            ticker: [_compact_bundle(item) for item in items]
            for ticker, items in result.get("results", {}).items()
        },
        "comparison_contexts": {
            ticker: _compact_research_context(context)
            for ticker, context in (result.get("comparison_contexts") or {}).items()
        },
    }


def _compact_research_context(context: Mapping[str, Any]) -> dict[str, Any]:
    return {
        "answerability": context.get("answerability") or {},
        "query_frame": context.get("query_frame") or {},
        "top_candidate": context.get("top_candidate"),
        "recommended_tools": list(context.get("recommended_tools") or [])[:3],
        "research_status": context.get("research_status"),
        "kernel": context.get("kernel") or {},
        "agent_autonomy": context.get("agent_autonomy") or {},
        "missing_parts": context.get("missing_parts") or [],
        "missing_shards": context.get("missing_shards") or {},
        "unknown_tickers": context.get("unknown_tickers") or [],
        "routing": context.get("routing") or {},
        "do_not_call": context.get("do_not_call") or [],
        "directness_guard": context.get("directness_guard") or {},
        "research_pack_summary": context.get("research_pack_summary") or {},
    }


def _query_kernel_envelope(
    *,
    topic: str | None,
    tickers: Sequence[str] | None,
    document_types: Sequence[str] | None,
    periods: Sequence[str] | None,
    limit_results: int,
    result_count: int,
    research_pack: Mapping[str, Any],
    agent_autonomy: Mapping[str, Any],
    do_not_call: Sequence[str],
) -> dict[str, Any]:
    request = research_request_from_plan_fields(
        question=topic or "structured query",
        tickers=tickers or [],
        document_types=document_types or [],
        periods=periods or [],
        universe=None,
        limit_results=limit_results,
        limit_tickers=len(tickers or []),
        mode="query",
    )
    return ResearchKernel().build_envelope(
        request,
        research_status="sufficient_for_default_answer"
        if result_count
        else "needs_targeted_followup",
        answer_mode="targeted_search_results",
        research_pack=research_pack,
        answerability={
            "direct_answerable": False,
            "related_context_available": bool(result_count),
            "negative_answer_supported": False,
            "needs_user_clarification": False,
            "recommended_answer_mode": "targeted_search_results"
            if result_count
            else "needs_targeted_followup",
        },
        missing_parts=[] if result_count else ["query_results_not_found"],
        recommended_tools=[],
        agent_autonomy=agent_autonomy,
        do_not_call=do_not_call,
    )


def _directness_guard_from_research_context(context: Mapping[str, Any]) -> dict[str, Any]:
    research_pack = context.get("research_pack") if isinstance(context, Mapping) else None
    if isinstance(research_pack, Mapping) and isinstance(
        research_pack.get("directness_guard"), Mapping
    ):
        return dict(research_pack["directness_guard"])
    if isinstance(context.get("directness_guard"), Mapping):
        return dict(context["directness_guard"])
    return {}


def _stop_guard_from_research_context(context: Mapping[str, Any]) -> dict[str, Any]:
    if isinstance(context.get("stop_guard"), Mapping):
        return dict(context["stop_guard"])
    research_pack = context.get("research_pack") if isinstance(context, Mapping) else None
    if isinstance(research_pack, Mapping):
        for key in ("stop_guard", "valuation_guard"):
            if isinstance(research_pack.get(key), Mapping):
                return dict(research_pack[key])
    return {}


def _should_skip_secondary_retrieve(context: Mapping[str, Any], *, detail: ResponseDetail) -> bool:
    if detail == ResponseDetail.FULL:
        return False
    status = str(context.get("research_status") or "")
    if status not in {"sufficient_for_default_answer", "sufficient_but_trace_recommended"}:
        return False
    do_not_call = {str(value) for value in context.get("do_not_call") or []}
    return "krw_ontology_retrieve" in do_not_call


def _release_missing_parts_from_context(context: Mapping[str, Any]) -> list[str]:
    release_missing_markers = {"ticker_shard_missing", "ticker_shard_not_found"}
    missing_parts = [
        str(value)
        for value in context.get("missing_parts") or []
        if str(value) in release_missing_markers
    ]
    if context.get("missing_shards") and "ticker_shard_missing" not in missing_parts:
        missing_parts.append("ticker_shard_missing")
    if context.get("unknown_tickers") and "ticker_shard_not_found" not in missing_parts:
        missing_parts.append("ticker_shard_not_found")
    return missing_parts


def _directness_guard_from_summary_payload(
    payload: Mapping[str, Any], *, topic: str | None
) -> dict[str, Any]:
    existing = _directness_guard_from_research_context(payload)
    if existing:
        return existing
    candidates = list(payload.get("ticker_candidates") or [])
    tiers = [
        str(candidate.get("tier") or "")
        for candidate in candidates
        if isinstance(candidate, Mapping)
    ]
    result_rows = list(payload.get("results") or [])
    if not tiers and result_rows:
        tiers = [str(row.get("tier") or "") for row in result_rows if isinstance(row, Mapping)]
    has_direct = any(tier in {"traceable_direct", "traceable_metric_lineage"} for tier in tiers)
    has_related = any(
        tier
        in {
            "traceable_related",
            "untraced_related",
            "broad_related_candidate",
            "untraced_direct_candidate",
            "related",
        }
        for tier in tiers
    )
    if not has_direct and not has_related and result_rows:
        has_related = True
    query_frame = (
        payload.get("query_frame") if isinstance(payload.get("query_frame"), Mapping) else {}
    )
    requires_direct = bool(
        query_frame.get("question_requires_direct_match")
        or query_frame.get("requires_direct_match")
        or _topic_text_requires_direct_match(topic)
    )
    recommended_answer_mode = (
        "direct_answer"
        if has_direct
        else "no_direct_evidence_with_related_context"
        if requires_direct and has_related
        else "related_context_only"
        if has_related
        else "not_answerable"
    )
    return {
        "requires_direct_match": requires_direct,
        "direct_answerable": has_direct,
        "related_context_available": has_related,
        "negative_answer_supported": bool(requires_direct and not has_direct and has_related),
        "recommended_answer_mode": recommended_answer_mode,
        "strong_claim_allowed": has_direct,
        "strong_claim_requires": ["traceable_direct", "traceable_metric_lineage"],
    }


def _topic_text_requires_direct_match(topic: str | None) -> bool:
    text = str(topic or "").lower()
    return any(
        term in text
        for term in ("direct", "directly", "직접", "direct exposure", "directly exposed")
    )


def _comparison_rows(result: dict[str, Any]) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    results = result.get("results") or {}
    mode = result.get("mode")
    if mode in {"period_topic", "period_metric"}:
        ticker = result.get("ticker")
        for period, items in results.items():
            rows.append(
                _comparison_row(
                    comparison_key=str(period),
                    ticker=ticker,
                    period=str(period),
                    items=items,
                    topic=result.get("topic"),
                    metric=result.get("metric"),
                )
            )
        return rows

    for ticker, items in results.items():
        evaluation = (result.get("comparison_evaluations") or {}).get(ticker) or {}
        rows.append(
            _comparison_row(
                comparison_key=str(ticker),
                ticker=str(ticker),
                period=None,
                items=items,
                topic=result.get("topic"),
                metric=result.get("metric"),
                evaluation=evaluation,
            )
        )
    return rows


def _comparison_row(
    *,
    comparison_key: str,
    ticker: str | None,
    period: str | None,
    items: list[dict[str, Any]],
    topic: str | None,
    metric: str | None,
    evaluation: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    evaluation = evaluation or {}
    if not items:
        missing_reason = (
            "ticker_shard_missing"
            if evaluation.get("reason") == "ticker_shard_missing"
            else "no_matching_ontology_objects"
        )
        return {
            "comparison_key": comparison_key,
            "ticker": ticker,
            "period": period,
            "document_type": None,
            "topic": topic,
            "metric": metric,
            "source_label": None,
            "summary": "",
            "object_id": None,
            "object_type": None,
            "confidence": "unsupported",
            "evidence_grade": None,
            "evidence_counts": {"claims": 0, "quotes": 0, "spans": 0},
            "direct_answerable": bool(evaluation.get("direct_answerable")),
            "related_context_available": bool(evaluation.get("related_context_available")),
            "negative_answer_supported": bool(evaluation.get("negative_answer_supported")),
            "recommended_answer_mode": evaluation.get("recommended_answer_mode")
            or "not_answerable",
            "semantic_relevance": evaluation.get("semantic_relevance"),
            "trace_status": evaluation.get("trace_status"),
            "tier": evaluation.get("tier") or "not_answerable",
            "evidence_chain_count": evaluation.get("evidence_chain_count") or 0,
            "support_depth": evaluation.get("support_depth"),
            "support_quote_count": evaluation.get("support_quote_count") or 0,
            "support_claim_count": evaluation.get("support_claim_count") or 0,
            "matched_required_facets": evaluation.get("matched_required_facets") or [],
            "missing_required_facets": evaluation.get("missing_required_facets") or [],
            "why_tier": evaluation.get("why_tier")
            or (
                "The company shard declared for this comparison key is missing from the active release."
                if missing_reason == "ticker_shard_missing"
                else "No matching ontology objects were returned for this comparison key."
            ),
            "caveats": [
                "The company shard declared for this comparison key is missing from the active release."
                if missing_reason == "ticker_shard_missing"
                else "No matching ontology objects were returned for this comparison key."
            ],
            "missing": True,
            "missing_reason": missing_reason,
        }

    item = _best_comparison_item(items, topic=topic)
    answerability_fields = _comparison_answerability_fields(item, evaluation, topic=topic)
    obj = item.get("object") or {}
    evidence = item.get("evidence") or {}
    quality = item.get("quality") or {}
    row_period = period or item.get("period")
    document_type = item.get("document_type")
    row_ticker = ticker or item.get("ticker")
    return {
        "comparison_key": comparison_key,
        "ticker": row_ticker,
        "period": row_period,
        "document_type": document_type,
        "topic": topic,
        "metric": metric,
        "source_label": _source_label(row_ticker, row_period, document_type),
        "summary": _short_text(item.get("text"), 500),
        "object_id": item.get("id"),
        "object_type": item.get("type"),
        "confidence": obj.get("confidence") or "unknown",
        "evidence_grade": obj.get("evidence_grade") or quality.get("evidence_grade"),
        "evidence_counts": {
            "claims": len(evidence.get("claims") or []),
            "quotes": len(evidence.get("quotes") or []),
            "spans": len(evidence.get("spans") or []),
        },
        **answerability_fields,
        "caveats": _comparison_caveats(item),
        "missing": False,
        "missing_reason": None,
    }


def _best_comparison_item(
    items: list[dict[str, Any]], *, topic: str | None = None
) -> dict[str, Any]:
    return max(items, key=lambda item: _comparison_item_score(item, topic=topic))


def _comparison_item_score(
    item: dict[str, Any], *, topic: str | None = None
) -> tuple[int, int, int, int, str]:
    obj = item.get("object") or {}
    evidence = item.get("evidence") or {}
    grade_score = {
        "direct": 4,
        "indirect": 3,
        "derived": 2,
        "unknown": 1,
        "unsupported": 0,
    }.get(str(obj.get("evidence_grade") or "").lower(), 1)
    type_score = {
        "EvidenceQuote": 5,
        "ResearchClaim": 4,
        "ExternalFactorExposure": 3,
        "BusinessActivity": 3,
        "MetricObservation": 3,
        "BusinessFactor": 2,
        "BusinessEvent": 2,
        "AgreementTerm": 2,
    }.get(str(item.get("type") or ""), 1)
    tier_score = {
        "traceable_direct": 5,
        "traceable_metric_lineage": 5,
        "traceable_related": 3,
        "untraced_direct_candidate": 2,
        "broad_related_candidate": 1,
        "not_answerable": 0,
    }.get(str(item.get("tier") or ""), 1)
    support_count = len(evidence.get("claims") or []) + len(evidence.get("quotes") or [])
    if not support_count:
        support_count = int(item.get("support_claim_count") or 0) + int(
            item.get("support_quote_count") or 0
        )
    direct_topic_score = 1 if topic and _comparison_item_directly_matches_topic(item, topic) else 0
    return (
        direct_topic_score,
        tier_score,
        grade_score + type_score,
        support_count,
        str(item.get("id") or ""),
    )


def _comparison_answerability_fields(
    item: dict[str, Any],
    evaluation: Mapping[str, Any],
    *,
    topic: str | None,
) -> dict[str, Any]:
    evidence = item.get("evidence") or {}
    inferred_tier = _infer_comparison_item_tier(item)
    tier = item.get("tier") or evaluation.get("tier") or inferred_tier
    direct_topic_match = bool(topic and _comparison_item_directly_matches_topic(item, topic))
    candidate_trace_status = item.get("trace_status") or evaluation.get("trace_status")
    if (
        topic
        and direct_topic_match
        and candidate_trace_status in {"traceable", "traceable_metric_lineage"}
        and str(tier)
        in {"traceable_related", "untraced_direct_candidate", "broad_related_candidate"}
    ):
        tier = "traceable_direct"
    if topic and str(tier) == "traceable_related" and direct_topic_match:
        tier = "traceable_direct"
    elif (
        topic
        and str(tier) == "traceable_direct"
        and not direct_topic_match
        and inferred_tier != "traceable_metric_lineage"
    ):
        tier = "traceable_related"
    trace_status = (
        item.get("trace_status")
        or evaluation.get("trace_status")
        or _infer_trace_status_from_tier(tier)
    )
    semantic_relevance = item.get("semantic_relevance") or evaluation.get("semantic_relevance")
    support_quote_count = item.get("support_quote_count")
    if support_quote_count is None:
        support_quote_count = evaluation.get("support_quote_count")
    if support_quote_count is None:
        support_quote_count = len(evidence.get("quotes") or [])
    support_claim_count = item.get("support_claim_count")
    if support_claim_count is None:
        support_claim_count = evaluation.get("support_claim_count")
    if support_claim_count is None:
        support_claim_count = len(evidence.get("claims") or [])
    direct_answerable = tier in {"traceable_direct", "traceable_metric_lineage"}
    return {
        "direct_answerable": bool(direct_answerable),
        "related_context_available": bool(
            evaluation.get("related_context_available") or tier == "traceable_related"
        ),
        "negative_answer_supported": bool(evaluation.get("negative_answer_supported")),
        "recommended_answer_mode": evaluation.get("recommended_answer_mode"),
        "semantic_relevance": semantic_relevance,
        "trace_status": trace_status,
        "tier": tier,
        "evidence_chain_count": item.get("evidence_chain_count")
        or evaluation.get("evidence_chain_count"),
        "support_depth": item.get("support_depth") or evaluation.get("support_depth"),
        "support_quote_count": support_quote_count,
        "support_claim_count": support_claim_count,
        "matched_required_facets": item.get("matched_required_facets")
        or evaluation.get("matched_required_facets")
        or [],
        "missing_required_facets": item.get("missing_required_facets")
        or evaluation.get("missing_required_facets")
        or [],
        "why_tier": item.get("why_tier")
        or evaluation.get("why_tier")
        or _default_compare_why_tier(tier),
    }


def _infer_comparison_item_tier(item: Mapping[str, Any]) -> str:
    evidence = item.get("evidence") or {}
    if evidence.get("metric_lineage") or item.get("type") in {"MetricObservation", "Calculation"}:
        return "traceable_metric_lineage"
    if item.get("type") in {"EvidenceQuote", "ResearchClaim"}:
        return "traceable_direct"
    if evidence.get("claims") or evidence.get("quotes"):
        return "traceable_related"
    return "untraced_direct_candidate"


def _comparison_item_directly_matches_topic(item: Mapping[str, Any], topic: str) -> bool:
    terms = _meaningful_topic_terms(topic)
    if not terms:
        return False
    evidence = item.get("evidence") or {}
    text_parts = [str(item.get("text") or "")]
    for key in ("claims", "quotes"):
        for obj in evidence.get(key) or []:
            if isinstance(obj, Mapping):
                text_parts.append(str(obj.get("text") or ""))
    haystack = " ".join(text_parts).lower()
    matched = [term for term in terms if _topic_term_in_text(term, haystack)]
    if len(terms) <= 4:
        return len(matched) == len(terms)
    return len(matched) >= len(terms) - 1


def _meaningful_topic_terms(topic: str) -> list[str]:
    stopwords = {
        "and",
        "or",
        "the",
        "a",
        "an",
        "of",
        "to",
        "by",
        "for",
        "with",
        "risk",
        "impact",
        "effect",
        "effects",
        "exposure",
        "compare",
        "comparison",
        "payment",
        "payments",
        "growth",
        "revenue",
        "cost",
        "costs",
    }
    raw_terms = [
        token.strip().lower()
        for token in re.split(r"[^A-Za-z0-9]+", str(topic or ""))
        if token.strip()
    ]
    terms: list[str] = []
    for term in raw_terms:
        if len(term) < 2 or term in stopwords:
            continue
        if term not in terms:
            terms.append(term)
    return terms[:8]


def _topic_term_in_text(term: str, text: str) -> bool:
    if term == "ai":
        return bool(re.search(r"\bai\b", text)) or "artificial intelligence" in text
    return bool(re.search(rf"\b{re.escape(term)}\b", text))


def _infer_trace_status_from_tier(tier: Any) -> str:
    return (
        "traceable"
        if str(tier) in {"traceable_direct", "traceable_metric_lineage", "traceable_related"}
        else "untraced"
    )


def _default_compare_why_tier(tier: Any) -> str:
    if tier == "traceable_direct":
        return (
            "Selected comparison evidence is directly traceable to filing claim or quote support."
        )
    if tier == "traceable_metric_lineage":
        return "Selected comparison evidence is supported by metric lineage."
    if tier == "traceable_related":
        return "Selected comparison evidence is traceable but should be treated as related context unless it matches the exact comparison premise."
    return "Selected comparison evidence needs follow-up trace or narrower search before being used as a strong conclusion."


def _comparison_caveats(item: dict[str, Any]) -> list[str]:
    caveats: list[str] = []
    obj = item.get("object") or {}
    quality = item.get("quality") or {}
    evidence_grade = obj.get("evidence_grade") or quality.get("evidence_grade")
    if evidence_grade in {"derived", "unsupported"}:
        caveats.append(f"Evidence grade is {evidence_grade}.")
    if quality.get("section_quality") in {"warn", "fail"}:
        caveats.append(f"Section quality is {quality.get('section_quality')}.")
    if quality.get("events"):
        caveats.append("Quality events are present for the selected source.")
    return caveats


def _source_label(ticker: Any, period: Any, document_type: Any) -> str | None:
    parts = [str(value) for value in (ticker, period, document_type) if value]
    return " ".join(parts) if parts else None


def _compact_quality_payload(quality: dict[str, Any]) -> dict[str, Any]:
    documents = quality.get("documents", [])
    return {
        "documents": [_compact_document(doc) for doc in documents],
        "events": quality.get("events", [])[:10],
        "summary": quality.get("summary", {}),
    }


def _compact_bundle(item: dict[str, Any]) -> dict[str, Any]:
    evidence = item.get("evidence") or {}
    document = item.get("document") or {}
    quality = item.get("quality") or {}
    evidence_summary = _compact_evidence_summary(item, evidence)
    return {
        "id": item.get("id"),
        "type": item.get("type"),
        "ticker": item.get("ticker"),
        "document_type": item.get("document_type"),
        "period": item.get("period"),
        "section": item.get("section"),
        "text": _short_text(item.get("text"), 700),
        "trace_id": item.get("id"),
        "semantic_relevance": item.get("semantic_relevance"),
        "trace_status": item.get("trace_status"),
        "tier": item.get("tier"),
        "evidence_chain_count": item.get("evidence_chain_count"),
        "support_depth": item.get("support_depth"),
        "support_quote_count": item.get("support_quote_count"),
        "support_claim_count": item.get("support_claim_count"),
        "matched_required_facets": item.get("matched_required_facets"),
        "missing_required_facets": item.get("missing_required_facets"),
        "why_tier": item.get("why_tier"),
        # The compact path is allowed to omit deep source expansion, but it
        # must still carry the answer-bearing observation itself.  Without
        # this small projection a numeric row became only a display string
        # and the analyst could not tell value, unit, or period apart.
        "object": _compact_object_summary(item.get("object")),
        "evidence_summary": evidence_summary,
        "evidence": {
            "claims": [
                _compact_evidence_object(claim, max_chars=320)
                for claim in evidence.get("claims", [])[:MAX_COMPACT_CLAIMS]
            ],
            "quotes": [
                _compact_evidence_object(quote, max_chars=420)
                for quote in evidence.get("quotes", [])[:MAX_COMPACT_QUOTES]
            ],
            "spans": [
                _compact_evidence_object(span, max_chars=260)
                for span in evidence.get("spans", [])[:MAX_COMPACT_SPANS]
            ],
            "related_objects": [
                _compact_evidence_object(related, max_chars=320)
                for related in evidence.get("related_objects", [])[:MAX_COMPACT_RELATED_OBJECTS]
            ],
            "metric_lineage": _compact_metric_lineage(evidence.get("metric_lineage")),
        },
        "quality": {
            "object_status": quality.get("object_status"),
            "section_quality": quality.get("section_quality"),
            "evidence_grade": (item.get("object") or {}).get("evidence_grade"),
            "quality_event_count": len(quality.get("events") or []),
            "events": [
                _compact_quality_event(event) for event in (quality.get("events") or [])[:3]
            ],
            "batch_failures": (document.get("counts") or {}).get("batch_failures", 0),
            "rejected_objects": (document.get("counts") or {}).get("rejected_objects", 0),
        },
    }


_COMPACT_OBJECT_FIELDS = (
    "id",
    "type",
    "metric_name",
    "canonical_metric",
    "value",
    "unit",
    "currency",
    "formatted_value",
    "period_type",
    "start_date",
    "end_date",
    "metric_scope",
    "dimensions",
)


def _compact_object_summary(value: Any) -> dict[str, Any]:
    if not isinstance(value, Mapping):
        return {}
    summary: dict[str, Any] = {
        key: value[key]
        for key in _COMPACT_OBJECT_FIELDS
        if key != "dimensions" and value.get(key) is not None
    }
    dimensions = value.get("dimensions")
    if isinstance(dimensions, Mapping):
        compact_dimensions: dict[str, Any] = {}
        for key, dimension_value in list(dimensions.items())[:8]:
            if isinstance(dimension_value, (str, int, float, bool)):
                compact_dimensions[_short_text(str(key), 64)] = (
                    _short_text(str(dimension_value), 128)
                    if isinstance(dimension_value, str)
                    else dimension_value
                )
        if compact_dimensions:
            summary["dimensions"] = compact_dimensions
    return summary


def _compact_evidence_summary(
    item: Mapping[str, Any], evidence: Mapping[str, Any]
) -> dict[str, Any]:
    # Fast compact retrieval intentionally skips evidence expansion.  The
    # traceability counts are therefore the best available indication that
    # more source material exists behind the row.
    counts = {
        "claim_count": max(
            len(evidence.get("claims") or []), int(item.get("support_claim_count") or 0)
        ),
        "quote_count": max(
            len(evidence.get("quotes") or []), int(item.get("support_quote_count") or 0)
        ),
        "span_count": len(evidence.get("spans") or []),
        "related_object_count": len(evidence.get("related_objects") or []),
    }
    counts["truncated"] = (
        counts["claim_count"] > MAX_COMPACT_CLAIMS
        or counts["quote_count"] > MAX_COMPACT_QUOTES
        or counts["span_count"] > MAX_COMPACT_SPANS
        or counts["related_object_count"] > MAX_COMPACT_RELATED_OBJECTS
        or bool(item.get("compact_only") and (counts["claim_count"] or counts["quote_count"]))
    )
    return counts


def _compact_evidence_object(obj: dict[str, Any], *, max_chars: int) -> dict[str, Any]:
    return {
        "id": obj.get("id"),
        "type": obj.get("type"),
        "ticker": obj.get("ticker"),
        "document_type": obj.get("document_type"),
        "period": obj.get("period"),
        "section": obj.get("section"),
        "text": _short_text(obj.get("text"), max_chars),
    }


def _compact_document(doc: dict[str, Any]) -> dict[str, Any]:
    counts = doc.get("counts") or {}
    return {
        "ticker": doc.get("ticker"),
        "document_type": doc.get("document_type"),
        "period": doc.get("period"),
        "section_quality_status": doc.get("section_quality_status"),
        "batch_failures": counts.get("batch_failures", 0),
        "rejected_objects": counts.get("rejected_objects", 0),
    }


def _compact_metric_lineage(lineage: dict[str, Any] | None) -> dict[str, Any] | None:
    if not lineage:
        return None
    return {
        "trace_type": lineage.get("trace_type"),
        "formatted_value": lineage.get("formatted_value"),
        "calculation": _compact_evidence_object(lineage["calculation"], max_chars=240)
        if lineage.get("calculation")
        else None,
        "input_metrics": [
            _compact_evidence_object(metric, max_chars=240)
            for metric in (lineage.get("input_metrics") or [])[:5]
        ],
        "xbrl_facts": [
            _compact_evidence_object(fact, max_chars=240)
            for fact in (lineage.get("xbrl_facts") or [])[:5]
        ],
        "source_document_ids": lineage.get("source_document_ids") or [],
    }


def _compact_quality_event(event: dict[str, Any]) -> dict[str, Any]:
    return {
        "id": event.get("id"),
        "severity": event.get("severity"),
        "category": event.get("category"),
        "stage": event.get("stage"),
        "message": _short_text(event.get("message"), 260),
    }


def _bounded_limit(limit: int) -> int:
    return max(1, min(int(limit), MAX_LIMIT))


def _bounded_limit_groups(limit: int) -> int:
    return max(1, min(int(limit), MAX_LIMIT_GROUPS))


def _bounded_limit_per_group(limit: int) -> int:
    return max(1, min(int(limit), MAX_LIMIT_PER_GROUP))


def _discovery_fetch_limit(limit: int, limit_groups: int, limit_per_group: int) -> int:
    minimum = max(40, int(limit_groups) * int(limit_per_group) * 2)
    return max(1, min(MAX_DISCOVERY_LIMIT, max(int(limit), minimum)))


def _bounded_offset(offset: int) -> int:
    return max(0, min(int(offset), MAX_OFFSET))


def _upper_list(values: list[str] | None) -> list[str]:
    return [value.upper() for value in values or []]


def _merge_ticker_alias(*, ticker: str | None, tickers: list[str] | None) -> list[str] | None:
    return _merge_upper_scalar_list_alias(ticker, tickers)


def _merge_compare_ticker_aliases(
    *,
    tickers: list[str] | None,
    ticker: str | None,
    ticker_a: str | None,
    ticker_b: str | None,
) -> list[str]:
    return _merge_upper_scalar_list_alias(ticker_a, tickers, ticker_b, ticker) or []


def _merge_scalar_list_alias(
    scalar: str | None,
    values: list[str] | None,
) -> list[str] | None:
    merged: list[str] = []
    for value in [*(values or []), scalar]:
        if not value:
            continue
        normalized = str(value).strip()
        if normalized and normalized not in merged:
            merged.append(normalized)
    return merged or None


def _merge_upper_scalar_list_alias(
    scalar: str | None,
    values: list[str] | None,
    *extra_scalars: str | None,
) -> list[str] | None:
    merged: list[str] = []
    for value in [*(values or []), scalar, *extra_scalars]:
        if not value:
            continue
        normalized = str(value).strip().upper()
        if normalized and normalized not in merged:
            merged.append(normalized)
    return merged or None


def _input_warnings(extra_args: Mapping[str, Any] | None) -> list[dict[str, Any]]:
    if not extra_args:
        return []
    return [
        {
            "code": "ignored_extra_args",
            "message": "Unsupported extra MCP arguments were ignored.",
            "args": sorted(str(key) for key in extra_args),
        }
    ]


def _retrieve_agent_guidance(agent_context: Mapping[str, Any] | None) -> dict[str, Any] | None:
    tool_usage = _agent_context_tool_usage(agent_context)
    retrieve_count = tool_usage.get("krw_ontology_retrieve")
    if retrieve_count is None or retrieve_count < _REPEATED_RETRIEVE_PRIOR_CALL_THRESHOLD:
        return None
    return {
        "severity": "soft",
        "reason": "repeated_retrieve",
        "message": _REPEATED_RETRIEVE_GUIDANCE_MESSAGE,
        "tool_usage": tool_usage,
    }


def _agent_context_tool_usage(agent_context: Mapping[str, Any] | None) -> dict[str, int]:
    if not isinstance(agent_context, Mapping):
        return {}
    raw_tool_usage = agent_context.get("tool_usage")
    if not isinstance(raw_tool_usage, Mapping):
        return {}

    tool_usage: dict[str, int] = {}
    for key in ("total", "krw_ontology_retrieve"):
        value = _non_negative_int(raw_tool_usage.get(key))
        if value is not None:
            tool_usage[key] = value
    return tool_usage


def _non_negative_int(value: Any) -> int | None:
    if isinstance(value, bool):
        return None
    if isinstance(value, int):
        return value if value >= 0 else None
    if isinstance(value, str) and re.fullmatch(r"\d+", value.strip()):
        return int(value)
    return None


def _attach_agent_guidance(
    payload: dict[str, Any],
    agent_guidance: Mapping[str, Any] | None,
) -> None:
    if not agent_guidance:
        return
    payload["agent_guidance"] = dict(agent_guidance)
    research_context = payload.get("research_context")
    if isinstance(research_context, dict):
        research_context.setdefault("agent_guidance", dict(agent_guidance))


def _pagination(total_count: int, offset: int, count: int, limit: int) -> dict[str, Any]:
    next_offset = offset + count if total_count > offset + count else None
    return {
        "total_count": total_count,
        "count": count,
        "offset": offset,
        "limit": limit,
        "has_more": next_offset is not None,
        "next_offset": next_offset,
    }


def _error_payload(code: str, message: str, suggestion: str) -> dict[str, Any]:
    return {
        "error": {
            "code": code,
            "message": message,
            "suggestion": suggestion,
        }
    }


def _compact_json_bytes(payload: Any) -> int:
    return len(
        json.dumps(
            payload,
            ensure_ascii=False,
            separators=(",", ":"),
            default=str,
        ).encode("utf-8")
    )


def _apply_chain_response_budget(
    payload: dict[str, Any],
    *,
    max_model_bytes: int = MAX_CHAIN_RESPONSE_MODEL_BYTES,
) -> dict[str, Any]:
    """Bound repeated chain context without dropping direct evidence lineage."""
    if payload.get("error"):
        return payload

    resolved_budget = max(1, int(max_model_bytes))
    bounded = copy.deepcopy(payload)
    original_model_bytes = _compact_json_bytes(bounded)
    chain = bounded.get("chain")
    if not isinstance(chain, dict):
        chain = {}
    global_chain = bounded.get("global_chain")
    if not isinstance(global_chain, dict):
        nested_global_chain = chain.get("global_chain")
        global_chain = nested_global_chain if isinstance(nested_global_chain, dict) else {}
    object_locator = bounded.get("object_locator")
    if not isinstance(object_locator, dict):
        object_locator = {}

    collection_specs: list[tuple[str, dict[str, Any], str, tuple[int, int, int]]] = [
        ("global_spine_neighbors", bounded, "global_spine_neighbors", (4, 2, 1)),
        ("global_paths", global_chain, "paths", (6, 2, 1)),
        ("edge_paths", chain, "edge_paths", (8, 4, 1)),
        ("semantic_neighbors", chain, "semantic_neighbors", (5, 3, 1)),
        ("temporal_context", chain, "temporal_context", (4, 2, 1)),
        ("replica_locations", object_locator, "replica_locations", (5, 3, 1)),
    ]
    collections: dict[str, list[Any]] = {}
    original_counts: dict[str, int] = {}
    for name, container, field, _minimums in collection_specs:
        value = container.get(field)
        collection = value if isinstance(value, list) else []
        collections[name] = collection
        original_counts[name] = len(collection)

    original_global_cross_company_count = sum(
        int((path or {}).get("cross_company_hops") or 0) > 0
        for path in collections["global_paths"]
        if isinstance(path, Mapping)
    )
    original_global_truncated = bool(global_chain.get("truncated"))
    original_replica_truncated = bool(object_locator.get("replica_locations_truncated"))
    budget_metadata: dict[str, Any] = {
        "format": CHAIN_RESPONSE_BUDGET_FORMAT,
        "max_model_bytes": resolved_budget,
        "original_model_bytes": original_model_bytes,
        "final_model_bytes": 0,
        "within_budget": False,
        "truncated": False,
        "omitted_counts": {},
        "preserved": {
            "object_and_document": "complete",
            "evidence_chain": "complete",
            "highest_ranked_context": True,
            "cross_company_path_if_available": True,
        },
    }
    bounded["response_budget"] = budget_metadata

    def sync_metadata() -> None:
        omitted_counts = {
            name: original_counts[name] - len(collection)
            for name, collection in collections.items()
            if original_counts[name] > len(collection)
        }
        response_truncated = bool(omitted_counts)
        budget_metadata["omitted_counts"] = omitted_counts
        budget_metadata["truncated"] = response_truncated

        returned_global_cross_company_count = sum(
            int((path or {}).get("cross_company_hops") or 0) > 0
            for path in collections["global_paths"]
            if isinstance(path, Mapping)
        )
        if global_chain:
            global_chain["total_path_count"] = original_counts["global_paths"]
            global_chain["path_count"] = len(collections["global_paths"])
            global_chain["total_cross_company_path_count"] = original_global_cross_company_count
            global_chain["cross_company_path_count"] = returned_global_cross_company_count
            global_chain["response_truncated"] = bool(omitted_counts.get("global_paths"))
            global_chain["truncated"] = original_global_truncated or bool(
                omitted_counts.get("global_paths")
            )

        if chain:
            chain["total_edge_path_count"] = original_counts["edge_paths"]
            chain["edge_path_count"] = len(collections["edge_paths"])
            chain["edge_paths_truncated"] = bool(omitted_counts.get("edge_paths"))
            chain["total_semantic_neighbor_count"] = original_counts["semantic_neighbors"]
            chain["semantic_neighbor_count"] = len(collections["semantic_neighbors"])
            chain["semantic_neighbors_truncated"] = bool(omitted_counts.get("semantic_neighbors"))
            chain["total_temporal_context_count"] = original_counts["temporal_context"]
            chain["temporal_context_count"] = len(collections["temporal_context"])
            chain["temporal_context_truncated"] = bool(omitted_counts.get("temporal_context"))

        bounded["total_global_spine_neighbor_count"] = original_counts["global_spine_neighbors"]
        bounded["global_spine_neighbor_count"] = len(collections["global_spine_neighbors"])
        bounded["global_spine_neighbors_truncated"] = bool(
            omitted_counts.get("global_spine_neighbors")
        )
        if object_locator:
            object_locator["response_replica_location_count"] = len(
                collections["replica_locations"]
            )
            object_locator["response_replica_locations_omitted"] = int(
                omitted_counts.get("replica_locations") or 0
            )
            object_locator["replica_locations_truncated"] = original_replica_truncated or bool(
                omitted_counts.get("replica_locations")
            )

    def removable_index(name: str, collection: list[Any]) -> int | None:
        if name != "global_paths" or original_global_cross_company_count <= 0:
            return len(collection) - 1 if collection else None
        returned_cross_company_count = sum(
            int((path or {}).get("cross_company_hops") or 0) > 0
            for path in collection
            if isinstance(path, Mapping)
        )
        for index in range(len(collection) - 1, -1, -1):
            path = collection[index]
            is_cross_company = bool(
                isinstance(path, Mapping) and int(path.get("cross_company_hops") or 0) > 0
            )
            if not is_cross_company or returned_cross_company_count > 1:
                return index
        return None

    sync_metadata()
    for minimum_index in range(3):
        for name, _container, _field, minimums in collection_specs:
            collection = collections[name]
            minimum = min(original_counts[name], minimums[minimum_index])
            while _compact_json_bytes(bounded) > resolved_budget and len(collection) > minimum:
                index = removable_index(name, collection)
                if index is None:
                    break
                collection.pop(index)
                sync_metadata()
            if _compact_json_bytes(bounded) <= resolved_budget:
                break
        if _compact_json_bytes(bounded) <= resolved_budget:
            break

    sync_metadata()
    final_model_bytes = _compact_json_bytes(bounded)
    budget_metadata["final_model_bytes"] = final_model_bytes
    final_model_bytes = _compact_json_bytes(bounded)
    budget_metadata["final_model_bytes"] = final_model_bytes
    budget_metadata["within_budget"] = final_model_bytes <= resolved_budget
    if not budget_metadata["within_budget"]:
        budget_metadata["unbounded_reason"] = "preserved_accuracy_floor_exceeds_budget"
    final_model_bytes = _compact_json_bytes(bounded)
    budget_metadata["final_model_bytes"] = final_model_bytes
    return bounded


def _format_response(
    payload: dict[str, Any],
    response_format: ResponseFormat,
    markdown_formatter: Any,
) -> str:
    if response_format == ResponseFormat.MARKDOWN:
        return markdown_formatter(payload)
    # MCP tool text is model input, not a human-facing log file.  Compact JSON
    # preserves the exact data and ordering while avoiding whitespace tokens
    # on every non-ResearchState tool call.
    return json.dumps(
        payload,
        ensure_ascii=False,
        separators=(",", ":"),
        default=str,
    )


def _markdown_catalog(payload: dict[str, Any]) -> str:
    lines = [
        "# Ontology Catalog",
        f"- Root: `{payload['root']}`",
        f"- Global spine: `{payload['global_spine_path']}`",
        f"- Companies: {', '.join(payload['companies']) or '(none)'}",
        f"- Documents returned: {payload['pagination']['count']}",
    ]
    for doc in payload["documents"]:
        lines.append(
            f"- `{doc['ticker']}` `{doc['document_type']}` `{doc['period']}` "
            f"section_quality={doc.get('section_quality_status')}"
        )
    return "\n".join(lines)


def _markdown_bundles(payload: dict[str, Any]) -> str:
    if payload.get("response_detail") == ResponseDetail.TICKER_SUMMARY.value:
        return _markdown_ticker_summary(payload)
    guard = _directness_guard_from_research_context(payload)
    lines = [
        "# Ontology Query Results",
        f"- Results: {payload['pagination']['count']}",
        f"- Strong claim allowed: {guard.get('strong_claim_allowed')}",
        f"- Strong claim requires: {', '.join(guard.get('strong_claim_requires') or [])}",
    ]
    diagnostics = payload.get("search_diagnostics") or {}
    warnings = diagnostics.get("warnings") or []
    if warnings:
        lines.append(f"- Search warnings: {', '.join(warnings)}")
    if diagnostics.get("fts_query"):
        lines.append(f"- FTS query: `{diagnostics['fts_query']}`")
    for item in payload["results"]:
        lines.extend(_bundle_lines(item))
    return "\n".join(lines)


def _markdown_topic_map(payload: dict[str, Any]) -> str:
    lines = [
        "# Ontology Topic Map",
        f"- Ticker: `{payload.get('ticker')}`",
        f"- Profile IDs: {', '.join(payload.get('source', {}).get('company_business_profile_ids') or []) or '(none)'}",
        f"- Fallback used: {payload.get('source', {}).get('fallback_used')}",
    ]
    topics = payload.get("topics") or {}
    for section in ("external_factors", "business_activities", "metrics", "projects_assets"):
        entries = topics.get(section) or []
        lines.append(f"## {section}")
        if not entries:
            lines.append("- No entries")
            continue
        for entry in entries[:10]:
            search_terms = ", ".join(entry.get("search_terms") or [])
            lines.append(f"- `{entry.get('term')}` search_terms={search_terms}")
    suggestions = payload.get("suggested_first_queries") or []
    if suggestions:
        lines.append("## Suggested First Queries")
        for suggestion in suggestions[:10]:
            lines.append(
                f"- topic=`{suggestion.get('topic')}` object_types={suggestion.get('object_types')}"
            )
    return "\n".join(lines)


def _retrieval_context_bundles(result: Mapping[str, Any]) -> list[dict[str, Any]]:
    bundles: list[dict[str, Any]] = []
    for field_name in ("direct_evidence", "related_context", "rejected_context"):
        values = result.get(field_name)
        if isinstance(values, list):
            bundles.extend(item for item in values if isinstance(item, dict))
    return bundles


def _map_retrieval_context(result: dict[str, Any], mapper: Any) -> dict[str, Any]:
    mapped = dict(result)
    for field_name in ("direct_evidence", "related_context", "rejected_context"):
        values = result.get(field_name)
        mapped[field_name] = mapper(values) if isinstance(values, list) else []
    return mapped


def _markdown_retrieve(payload: dict[str, Any]) -> str:
    if payload.get("response_detail") == ResponseDetail.TICKER_SUMMARY.value:
        return _markdown_ticker_summary(payload)
    answerability = payload.get("answerability") or {}
    guard = _directness_guard_from_research_context(payload)
    stop_guard = _stop_guard_from_research_context(payload)
    lines = [
        "# Ontology Retrieval",
        f"- Direct answerable: {answerability.get('direct_answerable')}",
        f"- Related context available: {answerability.get('related_context_available')}",
        f"- Recommended answer mode: {payload.get('recommended_answer_mode') or answerability.get('recommended_answer_mode')}",
        f"- Strong claim allowed: {guard.get('strong_claim_allowed')}",
        f"- Strong claim requires: {', '.join(guard.get('strong_claim_requires') or [])}",
        f"- Plan: `{json.dumps(payload.get('plan', {}), ensure_ascii=False)}`",
    ]
    if stop_guard:
        lines.append(f"- Cannot answer reason: {stop_guard.get('cannot_answer_reason')}")
    agent_guidance = payload.get("agent_guidance")
    if isinstance(agent_guidance, Mapping) and agent_guidance.get("message"):
        lines.append(f"- Agent guidance: {agent_guidance.get('message')}")
    for title, field_name in (
        ("Direct Evidence", "direct_evidence"),
        ("Related Context", "related_context"),
        ("Rejected Context", "rejected_context"),
    ):
        results = payload.get(field_name)
        if isinstance(results, list) and results:
            lines.append(f"## {title}")
            for item in results:
                lines.extend(_bundle_lines(item, prefix="### "))
    return "\n".join(lines)


def _markdown_ticker_summary(payload: dict[str, Any]) -> str:
    candidates = payload.get("ticker_candidates") or []
    if not candidates:
        return "No ticker candidates found."
    answerability = payload.get("answerability") or {}
    guard = _directness_guard_from_research_context(payload)
    lines = [
        "# Ticker Discovery Summary",
        "",
        f"- Recommended answer mode: {payload.get('recommended_answer_mode') or answerability.get('recommended_answer_mode')}",
        f"- Strong claim allowed: {guard.get('strong_claim_allowed')}",
        f"- Strong claim requires: {', '.join(guard.get('strong_claim_requires') or [])}",
        "",
    ]
    agent_guidance = payload.get("agent_guidance")
    if isinstance(agent_guidance, Mapping) and agent_guidance.get("message"):
        lines.extend([f"- Agent guidance: {agent_guidance.get('message')}", ""])
    for index, candidate in enumerate(candidates, 1):
        lines.append(
            f"{index}. `{candidate.get('ticker')}` "
            f"tier={candidate.get('tier')} score={candidate.get('score')}"
        )
        counts = candidate.get("matched_object_counts") or {}
        if counts:
            lines.append(f"- Object counts: {counts}")
        for reason in (candidate.get("top_reasons") or [])[:3]:
            if isinstance(reason, Mapping):
                why = (
                    reason.get("why_direct")
                    or reason.get("why_not_direct")
                    or reason.get("topic_label")
                    or reason.get("object_id")
                )
                lines.append(f"- {why}")
                core = reason.get("matched_core_terms") or []
                mechanisms = reason.get("matched_mechanisms") or []
                channels = reason.get("matched_impact_channels") or []
                if core or mechanisms or channels:
                    lines.append(
                        f"- Matched facets: core={core} mechanisms={mechanisms} channels={channels}"
                    )
                missing = reason.get("missing_required_facets") or []
                if missing:
                    lines.append(f"- Missing for direct: {missing}")
            else:
                lines.append(f"- {reason}")
    return "\n".join(lines)


def _markdown_trace(payload: dict[str, Any]) -> str:
    if payload.get("error"):
        return _markdown_error(payload)
    obj = payload.get("object", {})
    lines = [
        "# Ontology Trace",
        f"- Object: `{obj.get('id')}` ({obj.get('type')})",
        f"- Scope: `{obj.get('ticker')}` `{obj.get('document_type')}` `{obj.get('period')}`",
        f"- Text: {_short_text(_display_text(obj))}",
    ]
    for quote in payload.get("evidence", {}).get("quotes", [])[:5]:
        lines.append(f"- Quote `{quote.get('id')}`: {_short_text(quote.get('text'))}")
    for span in payload.get("evidence", {}).get("spans", [])[:3]:
        lines.append(f"- Span `{span.get('id')}`: {_short_text(span.get('text'))}")
    metric_lineage = payload.get("evidence", {}).get("metric_lineage")
    if metric_lineage:
        lines.append(f"- Metric lineage: {metric_lineage.get('formatted_value')}")
        if metric_lineage.get("calculation"):
            lines.append(f"- Calculation: `{metric_lineage['calculation'].get('id')}`")
        for fact in (metric_lineage.get("xbrl_facts") or [])[:3]:
            lines.append(f"- XBRLFact `{fact.get('id')}`: {_short_text(fact.get('text'))}")
    return "\n".join(lines)


def _markdown_verify_evidence(payload: dict[str, Any]) -> str:
    summary = payload.get("verification_summary") or {}
    lines = [
        "# Verified Company Evidence",
        f"- Ticker: `{payload.get('ticker')}`",
        f"- Release: `{payload.get('release_id') or 'unknown'}`",
        f"- Pack hash: `{payload.get('pack_hash')}`",
        f"- Verified objects: {summary.get('verified_object_count', 0)}",
        f"- Strong-claim evidence: {summary.get('strong_claim_evidence_count', 0)}",
    ]
    if payload.get("brief_hash"):
        lines.insert(3, f"- Brief hash: `{payload.get('brief_hash')}`")
    for question in payload.get("evidence_by_question") or []:
        lines.append(f"## {question.get('question_id')} ({question.get('answerability')})")
        for item in question.get("evidence") or []:
            document = item.get("document") or {}
            lines.append(
                f"- `{item.get('source_object_id')}` "
                f"{document.get('period')} {document.get('document_type')} "
                f"[{item.get('evidence_grade')}]: {_short_text(item.get('verified_excerpt'))}"
            )
    for rejected in payload.get("rejected_refs") or []:
        lines.append(f"- Rejected `{rejected.get('object_id')}`: {rejected.get('reason')}")
    return "\n".join(lines)


def _markdown_chain(payload: dict[str, Any]) -> str:
    if payload.get("error"):
        return _markdown_error(payload)
    obj = payload.get("object", {})
    chain = payload.get("chain") or {}
    evidence_chain = chain.get("evidence_chain") or {}
    global_chain = payload.get("global_chain") or chain.get("global_chain") or {}
    response_budget = payload.get("response_budget") or {}
    lines = [
        "# Ontology Chain",
        f"- Object: `{obj.get('id')}` ({obj.get('type')})",
        f"- Scope: `{obj.get('ticker')}` `{obj.get('document_type')}` `{obj.get('period')}`",
        f"- Text: {_short_text(obj.get('text'))}",
        f"- Direction: {chain.get('direction')} max_depth={chain.get('max_depth')}",
        f"- Evidence: claims={len(evidence_chain.get('claims') or [])}, "
        f"quotes={len(evidence_chain.get('quotes') or [])}, "
        f"spans={len(evidence_chain.get('spans') or [])}",
        f"- Semantic neighbors: {len(chain.get('semantic_neighbors') or [])}",
        f"- Temporal context: {len(chain.get('temporal_context') or [])}",
        f"- Global paths: {global_chain.get('path_count', 0)} "
        f"(cross-company={global_chain.get('cross_company_path_count', 0)}, "
        f"truncated={bool(global_chain.get('truncated'))})",
    ]
    if response_budget:
        lines.append(
            f"- Response budget: {response_budget.get('final_model_bytes')} / "
            f"{response_budget.get('max_model_bytes')} bytes; "
            f"omitted={response_budget.get('omitted_counts') or {}}"
        )
    warnings = (payload.get("quality") or {}).get("warnings") or []
    if warnings:
        lines.append(f"- Warnings: {', '.join(warnings)}")
    for neighbor in (chain.get("semantic_neighbors") or [])[:5]:
        neighbor_obj = neighbor.get("object") or {}
        lines.append(
            f"- Neighbor `{neighbor_obj.get('id')}` ({neighbor_obj.get('type')}): "
            f"{_short_text(neighbor_obj.get('text'))}"
        )
    for temporal in (chain.get("temporal_context") or [])[:5]:
        lines.append(
            f"- Temporal `{temporal.get('id')}` ({temporal.get('type')}): "
            f"{_short_text(temporal.get('text'))}"
        )
    for path in (global_chain.get("paths") or [])[:5]:
        terminal = path.get("terminal_object") or {}
        lines.append(
            f"- Global path `{path.get('path_id')}` depth={path.get('depth')} "
            f"score={path.get('score')} to `{terminal.get('object_id')}` "
            f"({terminal.get('ticker')}; association={path.get('contains_discovery_association')})"
        )
    return "\n".join(lines)


def _markdown_quality(payload: dict[str, Any]) -> str:
    summary = payload.get("summary", {})
    lines = [
        "# Ontology Quality",
        f"- Documents: {summary.get('documents', 0)}",
        f"- Events: {summary.get('events', 0)}",
        f"- Rejected objects: {summary.get('rejected_objects', 0)}",
        f"- Batch failures: {summary.get('batch_failures', 0)}",
        f"- Section warnings: {summary.get('section_warnings', 0)}",
    ]
    for event in payload.get("events", [])[:10]:
        lines.append(
            f"- `{event.get('category')}` `{event.get('severity')}` "
            f"{event.get('ticker')} {event.get('document_type')} {event.get('period')}: "
            f"{_short_text(event.get('message'))}"
        )
    return "\n".join(lines)


def _markdown_compare(payload: dict[str, Any]) -> str:
    lines = [
        "# Ontology Compare",
        f"- Mode: {payload.get('mode')}",
        f"- Topic: {payload.get('topic')}",
        f"- Metric: {payload.get('metric')}",
    ]
    comparison_rows = payload.get("comparison_rows") or []
    if comparison_rows:
        lines.append("## Comparison Rows")
        for row in comparison_rows:
            status = "missing" if row.get("missing") else "matched"
            lines.append(
                f"- `{row.get('comparison_key')}` {status}: "
                f"{_short_text(row.get('summary') or row.get('missing_reason'))}"
            )
    comparison_contexts = payload.get("comparison_contexts") or {}
    if comparison_contexts:
        lines.append("## Directness Guards")
        for ticker, context in comparison_contexts.items():
            guard = context.get("directness_guard") or {}
            answerability = context.get("answerability") or {}
            lines.append(
                f"- `{ticker}` mode={answerability.get('recommended_answer_mode')} "
                f"strong_claim_allowed={guard.get('strong_claim_allowed')} "
                f"requires={','.join(guard.get('strong_claim_requires') or [])}"
            )
    for ticker, items in payload.get("results", {}).items():
        lines.append(f"## {ticker}")
        if not items:
            lines.append("- No results")
            continue
        for item in items:
            lines.extend(_bundle_lines(item, prefix="- "))
    return "\n".join(lines)


def _markdown_plan(payload: dict[str, Any]) -> str:
    plan = payload.get("plan") or {}
    return "# Ontology Query Plan\n" + json.dumps(plan, ensure_ascii=False, indent=2, default=str)


def _markdown_error(payload: dict[str, Any]) -> str:
    error = payload.get("error", {})
    return (
        f"# Error\n- Code: `{error.get('code')}`\n"
        f"- Message: {error.get('message')}\n"
        f"- Suggestion: {error.get('suggestion')}"
    )


def _bundle_lines(item: dict[str, Any], *, prefix: str = "## ") -> list[str]:
    lines = [
        f"{prefix}`{item.get('id')}` ({item.get('type')})",
        f"- Scope: `{item.get('ticker')}` `{item.get('document_type')}` `{item.get('period')}`",
        f"- Text: {_short_text(item.get('text'))}",
    ]
    quotes = item.get("evidence", {}).get("quotes", [])
    if quotes:
        lines.append(f"- Quote `{quotes[0].get('id')}`: {_short_text(quotes[0].get('text'))}")
    return lines


def _display_text(obj: dict[str, Any]) -> str:
    if obj.get("type") == "MetricObservation" or obj.get("metric_name"):
        return format_metric_compact(obj)
    for key in (
        "claim_text",
        "quote_text",
        "description",
        "mechanism",
        "business_model_summary",
        "interpretation",
        "assumption_text",
        "text",
        "raw_text",
    ):
        if obj.get(key):
            return str(obj[key])
    if obj.get("metric_name"):
        return f"{obj['metric_name']}: {obj.get('value')} {obj.get('unit')}"
    return str(obj.get("name") or obj.get("id") or "")


def _short_text(value: Any, max_chars: int = 320) -> str:
    text = " ".join(str(value or "").split())
    if len(text) <= max_chars:
        return text
    return text[: max_chars - 3].rstrip() + "..."


def _unique(values: list[str]) -> list[str]:
    seen = set()
    output = []
    for value in values:
        if value in seen:
            continue
        seen.add(value)
        output.append(value)
    return output


def query_plan_from_payload(payload: dict[str, Any]) -> QueryPlan:
    """Helper for tests and future adapters."""
    return QueryPlan(**payload)
