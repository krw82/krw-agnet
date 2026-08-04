"""Release manifest and local release-directory helpers."""

from __future__ import annotations

import json
import os
import shutil
import hashlib
from collections.abc import Mapping, Sequence
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from krw_capability_runtime.agent_index.chart_series import (
    CHART_SERIES_RELATIVE_PATH,
    CHART_SERIES_SCHEMA_VERSION,
    verify_chart_series_index,
)
from krw_capability_runtime.agent_index.cache_seal import (
    read_immutable_sqlite_cache_seal,
    read_immutable_sqlite_cache_sha256,
    record_immutable_sqlite_cache_sha256,
)
from krw_capability_runtime.agent_index.router_sidecar import (
    ROUTER_SIDECAR_BUILDER_VERSION,
    ROUTER_SIDECAR_RELATIVE_PATH,
    ROUTER_SIDECAR_SCHEMA_VERSION,
    immutable_file_sha256,
    verify_router_sidecar,
)
from krw_capability_runtime.agent_index.router_coherence import (
    ROUTER_COHERENCE_BUILDER_VERSION,
    ROUTER_COHERENCE_RELATIVE_PATH,
    ROUTER_COHERENCE_SCHEMA_VERSION,
    verify_router_coherence,
)
from krw_capability_runtime.agent_index.spine_schema import (
    GLOBAL_SPINE_BUILDER_VERSION,
    GLOBAL_SPINE_LAYOUT,
    GLOBAL_SPINE_RELATIVE_PATH,
    GLOBAL_SPINE_SCHEMA_VERSION,
    GLOBAL_SPINE_TABLES,
    read_spine_verification_sha256,
    verify_global_spine_schema,
)
from krw_capability_runtime.agent_index.spine_builder import (
    COMPANY_SHARD_SCHEMA_VERSION,
    SPINE_PROJECTION_VERSION,
)
from krw_capability_runtime.agent_index.source_artifact_sqlite import (
    SOURCE_ARTIFACT_SQLITE_BUILDER_VERSION,
    SOURCE_ARTIFACT_SQLITE_SCHEMA_VERSION,
)
from krw_capability_runtime.agent_index.spine_verify import verify_spine_shard_release
from krw_capability_runtime.agent_index.metric_dictionary import (
    metric_dictionary_binding_errors,
)
from krw_capability_runtime.config.paths import (
    ONTOLOGY_ENV_ENV,
    ONTOLOGY_MANIFEST_PATH_ENV,
)

RELEASE_FORMAT_V3 = "krw-ontology-release/v3"
RELEASE_FORMAT = RELEASE_FORMAT_V3
RELEASE_MANIFEST_FILENAME = "manifest.json"
RELEASE_VERIFY_REPORT_FORMAT = "krw-ontology-release-verify/v1"
RELEASE_EVENT_FORMAT = "krw-ontology-release-event/v1"
RELEASE_VERIFY_DIRNAME = "verify"
RELEASE_VERIFY_REPORT_FILENAME = "release_verify.json"
RELEASE_EVENTS_FILENAME = "release_events.jsonl"
RELEASE_EVENTS_DIRNAME = "events"
FAILED_RELEASE_DIRNAME = "failed"
FAILED_RELEASE_METADATA_FILENAME = "failure.json"
FAILED_RELEASE_FORMAT = "krw-ontology-failed-release/v1"
RELEASE_ENV_RESERVED_DIRNAMES = frozenset(
    {
        "current",
        "candidates",
        FAILED_RELEASE_DIRNAME,
        "incoming",
        "locks",
        RELEASE_EVENTS_DIRNAME,
    }
)
ALLOWED_ONTOLOGY_ENVS = {"dev", "staging", "prod"}


def normalize_ontology_env(env: str | None = None) -> str:
    """Return a normalized ontology env name."""
    raw = (env or os.environ.get(ONTOLOGY_ENV_ENV) or "dev").strip().lower()
    if raw not in ALLOWED_ONTOLOGY_ENVS:
        allowed = ", ".join(sorted(ALLOWED_ONTOLOGY_ENVS))
        raise ValueError(f"KRW ontology env must be one of {allowed}; got {raw!r}")
    return raw


def find_release_manifest_path(root: Path, manifest_path: Path | str | None = None) -> Path | None:
    """Find the canonical release manifest for a root."""
    if manifest_path is not None:
        explicit = Path(manifest_path).expanduser()
        return explicit if explicit.exists() else None
    raw_manifest_path = os.environ.get(ONTOLOGY_MANIFEST_PATH_ENV)
    if raw_manifest_path:
        configured = Path(raw_manifest_path).expanduser()
        if configured.exists():
            return configured
    canonical = root / RELEASE_MANIFEST_FILENAME
    if canonical.exists():
        return canonical
    return None


def load_release_manifest(
    root: Path | str,
    *,
    manifest_path: Path | str | None = None,
) -> tuple[dict[str, Any], Path | None]:
    """Load a release manifest if present."""
    root_path = Path(root).expanduser()
    found_path = find_release_manifest_path(root_path, manifest_path)
    if found_path is None:
        return {}, None
    try:
        return json.loads(found_path.read_text(encoding="utf-8")), found_path
    except json.JSONDecodeError as exc:
        raise ValueError(f"Invalid release manifest JSON: {found_path}") from exc


def _relative_or_absolute(path: Path, root: Path) -> str:
    try:
        return str(path.relative_to(root))
    except ValueError:
        return str(path)


def build_release_manifest_v3(
    root: Path | str,
    *,
    release_id: str,
    env: str | None = None,
    source_root: Path | str | None = None,
    global_spine_path: Path | str | None = None,
    router_sidecar_path: Path | str | None = None,
    router_coherence_path: Path | str | None = None,
    shard_manifest_path: Path | str | None = None,
) -> dict[str, Any]:
    """Build a v3 release manifest for global spine + company shards."""
    root_path = Path(root).expanduser().resolve()
    resolved_env = normalize_ontology_env(env)
    resolved_spine_path = (
        Path(global_spine_path).expanduser().resolve()
        if global_spine_path is not None
        else root_path / GLOBAL_SPINE_RELATIVE_PATH
    )
    resolved_shard_manifest_path = (
        Path(shard_manifest_path).expanduser().resolve()
        if shard_manifest_path is not None
        else root_path / "indexes" / "shard_manifest.json"
    )
    resolved_router_sidecar_path = (
        Path(router_sidecar_path).expanduser().resolve()
        if router_sidecar_path is not None
        else root_path / ROUTER_SIDECAR_RELATIVE_PATH
    )
    resolved_router_coherence_path = (
        Path(router_coherence_path).expanduser().resolve()
        if router_coherence_path is not None
        else root_path / ROUTER_COHERENCE_RELATIVE_PATH
    )
    index_outputs = _build_release_index_outputs_v3(
        root_path,
        release_id=release_id,
        global_spine_path=resolved_spine_path,
        router_sidecar_path=resolved_router_sidecar_path,
        router_coherence_path=resolved_router_coherence_path,
        shard_manifest_path=resolved_shard_manifest_path,
    )
    spine_counts = index_outputs["global_spine"].get("counts") or {}
    dictionary_binding = index_outputs["company_shards"].get("metric_dictionary")
    binding_errors = metric_dictionary_binding_errors(dictionary_binding)
    if binding_errors:
        raise ValueError(
            "Cannot build a ready v3 manifest with an unbound metric dictionary: "
            + ", ".join(binding_errors)
        )
    return {
        "format": RELEASE_FORMAT_V3,
        "release_id": release_id,
        "env": resolved_env,
        "status": "ready",
        "created_at": datetime.now(timezone.utc).isoformat(),
        "root": str(root_path),
        "source_root": str(Path(source_root).expanduser().resolve())
        if source_root
        else str(root_path),
        "index_layout": GLOBAL_SPINE_LAYOUT,
        "monolith_required": False,
        "metric_dictionary": dictionary_binding,
        "global_spine_path": _relative_or_absolute(resolved_spine_path, root_path),
        "indexes": index_outputs,
        "verification": {
            "status": "required",
            "path": f"{RELEASE_VERIFY_DIRNAME}/{RELEASE_VERIFY_REPORT_FILENAME}",
            "consistency_report": f"{RELEASE_VERIFY_DIRNAME}/consistency_report.json",
            "chain_smoke": f"{RELEASE_VERIFY_DIRNAME}/chain_smoke.json",
            "quality_report": f"{RELEASE_VERIFY_DIRNAME}/quality_report.json",
            "routing_report": f"{RELEASE_VERIFY_DIRNAME}/routing_report.json",
        },
        "builder": {
            "release_builder_version": "release-builder/v3",
            "spine_schema_version": GLOBAL_SPINE_SCHEMA_VERSION,
            "spine_builder_version": GLOBAL_SPINE_BUILDER_VERSION,
            "spine_projection_version": SPINE_PROJECTION_VERSION,
            "source_artifact_sqlite_schema_version": SOURCE_ARTIFACT_SQLITE_SCHEMA_VERSION,
            "source_artifact_sqlite_builder_version": SOURCE_ARTIFACT_SQLITE_BUILDER_VERSION,
            "company_shard_schema_version": COMPANY_SHARD_SCHEMA_VERSION,
            "router_sidecar_schema_version": ROUTER_SIDECAR_SCHEMA_VERSION,
            "router_sidecar_builder_version": ROUTER_SIDECAR_BUILDER_VERSION,
            "router_coherence_schema_version": ROUTER_COHERENCE_SCHEMA_VERSION,
            "router_coherence_builder_version": ROUTER_COHERENCE_BUILDER_VERSION,
        },
        "global_object_count": spine_counts.get("global_object_locator", 0),
        "global_edge_count": spine_counts.get("global_edge_spine", 0),
        "global_chain_link_count": spine_counts.get("global_chain_index", 0),
        "company_shard_count": index_outputs["company_shards"].get("count", 0),
    }


def write_release_manifest_v3(
    root: Path | str,
    *,
    release_id: str,
    env: str | None = None,
    source_root: Path | str | None = None,
    global_spine_path: Path | str | None = None,
    router_sidecar_path: Path | str | None = None,
    router_coherence_path: Path | str | None = None,
    shard_manifest_path: Path | str | None = None,
) -> dict[str, Any]:
    """Write a v3 manifest.json for a global spine + company shards release."""
    root_path = Path(root).expanduser().resolve()
    _assert_not_active_release_path(root_path, "root")
    root_path.mkdir(parents=True, exist_ok=True)
    resolved_spine_path = (
        Path(global_spine_path).expanduser().resolve()
        if global_spine_path is not None
        else root_path / GLOBAL_SPINE_RELATIVE_PATH
    )
    resolved_router_sidecar_path = (
        Path(router_sidecar_path).expanduser().resolve()
        if router_sidecar_path is not None
        else root_path / ROUTER_SIDECAR_RELATIVE_PATH
    )
    resolved_router_coherence_path = (
        Path(router_coherence_path).expanduser().resolve()
        if router_coherence_path is not None
        else root_path / ROUTER_COHERENCE_RELATIVE_PATH
    )
    if not resolved_spine_path.is_file():
        raise FileNotFoundError(
            f"Cannot write a ready v3 manifest without the global spine: {resolved_spine_path}"
        )
    if not resolved_router_sidecar_path.is_file():
        raise FileNotFoundError(
            "Cannot write a ready v3 manifest without the Serving Index V2 sidecar: "
            f"{resolved_router_sidecar_path}. Run build_spine_shard_release_outputs first."
        )
    if not resolved_router_coherence_path.is_file():
        raise FileNotFoundError(
            "Cannot write a ready v3 manifest without the Router coherence index: "
            f"{resolved_router_coherence_path}. Run build_spine_shard_release_outputs first."
        )
    manifest = build_release_manifest_v3(
        root_path,
        release_id=release_id,
        env=env,
        source_root=source_root,
        global_spine_path=resolved_spine_path,
        router_sidecar_path=resolved_router_sidecar_path,
        router_coherence_path=resolved_router_coherence_path,
        shard_manifest_path=shard_manifest_path,
    )
    router_output = manifest["indexes"]["router_sidecar"]
    if not router_output.get("verification_ok"):
        raise ValueError(
            "Cannot write a ready v3 manifest with an invalid router sidecar: "
            + ", ".join(router_output.get("verification_errors") or [])
        )
    coherence_output = manifest["indexes"]["router_coherence"]
    if not coherence_output.get("verification_ok"):
        raise ValueError(
            "Cannot write a ready v3 manifest with an invalid router coherence index: "
            + ", ".join(coherence_output.get("verification_errors") or [])
        )
    _write_json_atomic(root_path / RELEASE_MANIFEST_FILENAME, manifest)
    return manifest


def _build_release_index_outputs_v3(
    root_path: Path,
    *,
    release_id: str,
    global_spine_path: Path,
    router_sidecar_path: Path,
    router_coherence_path: Path,
    shard_manifest_path: Path,
) -> dict[str, Any]:
    global_schema_verification = verify_global_spine_schema(
        global_spine_path,
        deep=False,
        trust_seal=True,
        require_trusted_seal=True,
    )
    if not global_schema_verification.get("ok"):
        raise ValueError(
            "Cannot build a ready v3 manifest with an invalid global spine schema: "
            + ", ".join(global_schema_verification.get("errors") or [])
        )
    global_spine_metadata = global_schema_verification.get("metadata") or {}
    spine_count_tables = tuple(table for table in GLOBAL_SPINE_TABLES if table != "metadata")
    spine_counts = _required_global_spine_metadata_counts(
        global_spine_metadata,
        spine_count_tables,
    )
    global_spine_sha256 = (
        immutable_file_sha256(global_spine_path) if global_spine_path.is_file() else None
    )
    shard_manifest = _read_v3_shard_manifest(shard_manifest_path)
    shard_entries = _read_v3_shard_entries(
        root_path,
        shard_manifest_path,
        payload=shard_manifest,
    )
    shard_binding_errors = _shard_manifest_serving_binding_errors(
        shard_manifest,
        shard_entries,
    )
    if shard_binding_errors:
        raise ValueError(
            "Cannot build a ready v3 manifest with stale company shard bindings: "
            + ", ".join(shard_binding_errors)
        )
    global_binding_errors = _global_spine_serving_binding_errors(global_spine_metadata)
    if global_binding_errors:
        raise ValueError(
            "Cannot build a ready v3 manifest with stale global spine bindings: "
            + ", ".join(global_binding_errors)
        )
    dictionary_binding = shard_manifest.get("metric_dictionary")
    router_verification = (
        verify_router_sidecar(
            router_sidecar_path,
            expected_global_spine_sha256=global_spine_sha256,
            expected_release_id=release_id,
            deep=False,
        )
        if router_sidecar_path.is_file()
        else {
            "ok": False,
            "errors": ["router_sidecar_missing"],
            "counts": {},
            "metadata": {},
        }
    )
    router_metadata = router_verification.get("metadata") or {}
    router_sidecar_sha256: str | None = None
    if router_sidecar_path.is_file():
        _router_seal, router_seal_status = read_immutable_sqlite_cache_seal(
            router_sidecar_path,
            kind="router_sidecar",
            cache_key=(
                str(router_metadata.get("build_fingerprint_sha256"))
                if router_metadata.get("build_fingerprint_sha256")
                else None
            ),
        )
        if router_seal_status != "valid":
            raise ValueError(
                "Cannot build a ready v3 manifest without a matching deep router sidecar seal: "
                f"{router_seal_status}"
            )
        router_sidecar_sha256 = immutable_file_sha256(router_sidecar_path)
        record_immutable_sqlite_cache_sha256(router_sidecar_path, router_sidecar_sha256)
    coherence_verification = (
        verify_router_coherence(
            router_coherence_path,
            expected_global_spine_sha256=global_spine_sha256,
            expected_release_id=release_id,
            deep=True,
        )
        if router_coherence_path.is_file()
        else {
            "ok": False,
            "errors": ["router_coherence_missing"],
            "counts": {},
            "metadata": {},
        }
    )
    coherence_metadata = coherence_verification.get("metadata") or {}
    router_coherence_sha256: str | None = None
    if router_coherence_path.is_file():
        _coherence_seal, coherence_seal_status = read_immutable_sqlite_cache_seal(
            router_coherence_path,
            kind="router_coherence",
            cache_key=(
                str(coherence_metadata.get("semantic_cache_key"))
                if coherence_metadata.get("semantic_cache_key")
                else None
            ),
        )
        if coherence_seal_status != "valid":
            raise ValueError(
                "Cannot build a ready v3 manifest without a matching deep router coherence seal: "
                f"{coherence_seal_status}"
            )
        router_coherence_sha256 = immutable_file_sha256(router_coherence_path)
        record_immutable_sqlite_cache_sha256(
            router_coherence_path,
            router_coherence_sha256,
        )
    outputs: dict[str, Any] = {
        "global_spine": {
            "path": _relative_or_absolute(global_spine_path, root_path),
            "sha256": global_spine_sha256,
            "schema_version": GLOBAL_SPINE_SCHEMA_VERSION,
            "builder_version": GLOBAL_SPINE_BUILDER_VERSION,
            "spine_projection_version": SPINE_PROJECTION_VERSION,
            "required": True,
            "counts": spine_counts,
        },
        "router_sidecar": {
            "path": _relative_or_absolute(router_sidecar_path, root_path),
            "sha256": router_sidecar_sha256,
            "schema_version": ROUTER_SIDECAR_SCHEMA_VERSION,
            "builder_version": ROUTER_SIDECAR_BUILDER_VERSION,
            "required": True,
            "counts": router_verification.get("counts") or {},
            "ranking_profile_id": router_metadata.get("ranking_profile_id"),
            "ranking_profile_sha256": router_metadata.get("ranking_profile_sha256"),
            "source_global_spine_sha256": router_metadata.get("source_global_spine_sha256"),
            "content_sha256": router_metadata.get("content_sha256"),
            "build_fingerprint_sha256": router_metadata.get("build_fingerprint_sha256"),
            "verification_ok": bool(router_verification.get("ok")),
            "verification_errors": list(router_verification.get("errors") or []),
        },
        "router_coherence": {
            "path": _relative_or_absolute(router_coherence_path, root_path),
            "sha256": router_coherence_sha256,
            "schema_version": ROUTER_COHERENCE_SCHEMA_VERSION,
            "builder_version": ROUTER_COHERENCE_BUILDER_VERSION,
            "required": True,
            "counts": coherence_verification.get("counts") or {},
            "profile_id": coherence_metadata.get("profile_id"),
            "profile_sha256": coherence_metadata.get("profile_sha256"),
            "source_global_spine_sha256": coherence_metadata.get("source_global_spine_sha256"),
            "semantic_cache_key": coherence_metadata.get("semantic_cache_key"),
            "build_fingerprint_sha256": coherence_metadata.get("build_fingerprint_sha256"),
            "verification_ok": bool(coherence_verification.get("ok")),
            "verification_errors": list(coherence_verification.get("errors") or []),
        },
        "company_shards": {
            "dir": _relative_or_absolute(root_path / "indexes" / "companies", root_path),
            "required": True,
            "schema_version": COMPANY_SHARD_SCHEMA_VERSION,
            "source_artifact_sqlite_schema_version": SOURCE_ARTIFACT_SQLITE_SCHEMA_VERSION,
            "source_artifact_sqlite_builder_version": SOURCE_ARTIFACT_SQLITE_BUILDER_VERSION,
            "count": len(shard_entries),
            "tickers": shard_entries,
            "metric_dictionary": dictionary_binding,
        },
        "shard_manifest": {
            "path": _relative_or_absolute(shard_manifest_path, root_path),
            "sha256": _file_sha256(shard_manifest_path) if shard_manifest_path.is_file() else None,
            "required": True,
        },
    }
    chart_series_path = root_path / CHART_SERIES_RELATIVE_PATH
    if chart_series_path.is_file():
        chart_verification = verify_chart_series_index(chart_series_path)
        outputs["chart_series"] = {
            "path": _relative_or_absolute(chart_series_path, root_path),
            "sha256": _file_sha256(chart_series_path),
            "schema_version": CHART_SERIES_SCHEMA_VERSION,
            "required": False,
            "counts": chart_verification.get("counts") or {},
            "verification_ok": bool(chart_verification.get("ok")),
        }
    return outputs


def _read_v3_shard_manifest(shard_manifest_path: Path) -> dict[str, Any]:
    if not shard_manifest_path.is_file():
        return {}
    try:
        payload = json.loads(shard_manifest_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        return {}
    return payload if isinstance(payload, dict) else {}


def _read_v3_shard_entries(
    root_path: Path,
    shard_manifest_path: Path,
    *,
    payload: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    payload = dict(payload or _read_v3_shard_manifest(shard_manifest_path))
    raw_shards = payload.get("shards")
    if not isinstance(raw_shards, Mapping):
        return {}
    entries: dict[str, Any] = {}
    for ticker, raw_entry in sorted(raw_shards.items()):
        if not isinstance(raw_entry, Mapping):
            continue
        raw_path = raw_entry.get("path") or raw_entry.get("shard_path")
        if not isinstance(raw_path, str) or not raw_path:
            continue
        raw_candidate = Path(raw_path)
        if raw_candidate.is_absolute():
            shard_path = raw_candidate.expanduser().resolve()
        elif raw_candidate.parts and raw_candidate.parts[0] == "indexes":
            shard_path = (root_path / raw_candidate).resolve()
        else:
            shard_path = (root_path / "indexes" / raw_candidate).resolve()
        entries[str(ticker)] = {
            "path": _relative_or_absolute(shard_path, root_path),
            "sha256": raw_entry.get("sha256")
            or (_file_sha256(shard_path) if shard_path.is_file() else None),
            "schema_version": raw_entry.get("schema_version"),
            "source_artifact_sqlite_schema_version": raw_entry.get(
                "source_artifact_sqlite_schema_version"
            ),
            "source_artifact_sqlite_builder_version": raw_entry.get(
                "source_artifact_sqlite_builder_version"
            ),
            "document_count": raw_entry.get("document_count"),
            "object_count": raw_entry.get("object_count"),
            "edge_count": raw_entry.get("edge_count"),
            "quality_event_count": raw_entry.get("quality_event_count"),
            "metric_dictionary": raw_entry.get("metric_dictionary"),
        }
    return entries


def _shard_manifest_serving_binding_errors(
    shard_manifest: Mapping[str, Any],
    shard_entries: Mapping[str, Any],
) -> list[str]:
    expected_top_level = {
        "company_shard_schema_version": COMPANY_SHARD_SCHEMA_VERSION,
        "source_artifact_sqlite_schema_version": SOURCE_ARTIFACT_SQLITE_SCHEMA_VERSION,
        "source_artifact_sqlite_builder_version": SOURCE_ARTIFACT_SQLITE_BUILDER_VERSION,
    }
    errors = [
        f"shard_manifest_binding_mismatch:{key}"
        for key, value in expected_top_level.items()
        if shard_manifest.get(key) != value
    ]
    for ticker, raw_entry in shard_entries.items():
        entry = raw_entry if isinstance(raw_entry, Mapping) else {}
        expected_entry = {
            "schema_version": COMPANY_SHARD_SCHEMA_VERSION,
            "source_artifact_sqlite_schema_version": SOURCE_ARTIFACT_SQLITE_SCHEMA_VERSION,
            "source_artifact_sqlite_builder_version": SOURCE_ARTIFACT_SQLITE_BUILDER_VERSION,
        }
        for key, value in expected_entry.items():
            if entry.get(key) != value:
                errors.append(f"shard_entry_binding_mismatch:{ticker}:{key}")
    return errors


def _global_spine_serving_binding_errors(
    metadata: Mapping[str, Any],
) -> list[str]:
    expected = {
        "schema_version": GLOBAL_SPINE_SCHEMA_VERSION,
        "builder_version": GLOBAL_SPINE_BUILDER_VERSION,
        "spine_projection_version": SPINE_PROJECTION_VERSION,
        "source_artifact_sqlite_schema_version": SOURCE_ARTIFACT_SQLITE_SCHEMA_VERSION,
        "source_artifact_sqlite_builder_version": SOURCE_ARTIFACT_SQLITE_BUILDER_VERSION,
        "company_shard_schema_version": COMPANY_SHARD_SCHEMA_VERSION,
    }
    return [
        f"global_spine_binding_mismatch:{key}"
        for key, value in expected.items()
        if metadata.get(key) != value
    ]


def _required_global_spine_metadata_counts(
    metadata: Mapping[str, Any],
    tables: Sequence[str],
) -> dict[str, int]:
    raw_counts = metadata.get("counts")
    counts = raw_counts if isinstance(raw_counts, Mapping) else {}
    errors: list[str] = []
    result: dict[str, int] = {}
    for table in tables:
        value = counts.get(table)
        if isinstance(value, bool) or not isinstance(value, int) or value < 0:
            errors.append(f"global_spine_metadata_count_invalid:{table}")
            continue
        result[table] = value
    if errors:
        raise ValueError(
            "Cannot build a ready v3 manifest without immutable builder counts: "
            + ", ".join(errors)
        )
    return result


def verify_release_root(
    root: Path | str,
    *,
    env: str | None = None,
    manifest_path: Path | str | None = None,
    require_current_symlink: bool = False,
    deep: bool = False,
) -> dict[str, Any]:
    """Verify a v3 global spine + company shard release for activation."""
    supplied_root = Path(root).expanduser()
    root_path = supplied_root.resolve()
    manifest, found_manifest_path = load_release_manifest(root_path, manifest_path=manifest_path)
    if manifest.get("format") != RELEASE_FORMAT_V3:
        errors = (
            _release_filesystem_errors(root_path)
            if deep
            else _release_filesystem_light_errors(root_path)
        )
        if require_current_symlink and not _is_current_symlink_path(supplied_root):
            errors.insert(0, "current_symlink_required")
        errors.append("manifest_missing" if not manifest else "manifest_format_unsupported")
        return {
            "ok": False,
            "errors": errors,
            "root": str(root_path),
            "supplied_root": str(supplied_root),
            "manifest_path": str(found_manifest_path) if found_manifest_path else None,
            "manifest": manifest,
            "env": manifest.get("env"),
            "release_id": manifest.get("release_id"),
            "index_layout": manifest.get("index_layout"),
            "global_spine_path": None,
            "global_spine_present": False,
            "global_spine_verification": None,
            "spine_shard_verification": None,
            "smoke_verification": None,
            "current_symlink": _is_current_symlink_path(supplied_root),
            "verification_mode": "release-root-v3",
        }
    return _verify_release_root_v3(
        supplied_root=supplied_root,
        root_path=root_path,
        manifest=manifest,
        found_manifest_path=found_manifest_path,
        env=env,
        require_current_symlink=require_current_symlink,
        deep=deep,
    )


def _verify_release_root_v3(
    *,
    supplied_root: Path,
    root_path: Path,
    manifest: dict[str, Any],
    found_manifest_path: Path | None,
    env: str | None,
    require_current_symlink: bool,
    deep: bool,
) -> dict[str, Any]:
    errors: list[str] = []
    if require_current_symlink and not _is_current_symlink_path(supplied_root):
        errors.append("current_symlink_required")
    errors.extend(
        _release_filesystem_errors(root_path)
        if deep
        else _release_filesystem_light_errors(root_path)
    )
    errors.extend(_release_manifest_startup_errors_v3(root_path, manifest))
    expected_env = normalize_ontology_env(env) if env is not None else None
    manifest_env = manifest.get("env")
    if expected_env is not None and manifest_env != expected_env:
        errors.append("manifest_env_mismatch")
    spine_shard_verification = verify_spine_shard_release(
        root_path,
        manifest_path=found_manifest_path,
        deep=deep,
    )
    errors.extend(f"spine_shard:{error}" for error in spine_shard_verification["errors"])
    global_spine_path = _resolve_v3_global_spine_path(root_path, manifest)
    return {
        "ok": not errors,
        "errors": errors,
        "root": str(root_path),
        "supplied_root": str(supplied_root),
        "manifest_path": str(found_manifest_path) if found_manifest_path else None,
        "manifest": manifest,
        "env": manifest_env,
        "release_id": manifest.get("release_id"),
        "index_layout": manifest.get("index_layout"),
        "global_spine_path": str(global_spine_path),
        "global_spine_present": global_spine_path.exists(),
        "global_spine_verification": spine_shard_verification.get("global_spine_verification"),
        "router_sidecar_path": spine_shard_verification.get("router_sidecar_path"),
        "router_sidecar_present": bool(
            spine_shard_verification.get("router_sidecar_path")
            and Path(str(spine_shard_verification["router_sidecar_path"])).exists()
        ),
        "router_sidecar_verification": spine_shard_verification.get("router_sidecar_verification"),
        "router_coherence_path": spine_shard_verification.get("router_coherence_path"),
        "router_coherence_present": bool(
            spine_shard_verification.get("router_coherence_path")
            and Path(str(spine_shard_verification["router_coherence_path"])).exists()
        ),
        "router_coherence_verification": spine_shard_verification.get(
            "router_coherence_verification"
        ),
        "chart_series_verification": spine_shard_verification.get("chart_series_verification"),
        "spine_shard_verification": spine_shard_verification,
        "smoke_verification": None,
        "current_symlink": _is_current_symlink_path(supplied_root),
        "verification_mode": "release-root-v3-deep" if deep else "release-root-v3-light",
        "deep": deep,
    }


def verify_release_startup_v3(
    root: Path | str,
    *,
    env: str | None = None,
    manifest_path: Path | str | None = None,
    require_current_symlink: bool = False,
    check_sqlite: bool = True,
) -> dict[str, Any]:
    """Verify the cheap v3 runtime contract before MCP startup."""
    supplied_root = Path(root).expanduser()
    root_path = supplied_root.resolve()
    errors: list[str] = []
    if require_current_symlink and not _is_current_symlink_path(supplied_root):
        errors.append("current_symlink_required")
    if not root_path.exists():
        errors.append("release_root_missing")
    elif not root_path.is_dir():
        errors.append("release_root_not_directory")

    manifest, found_manifest_path = load_release_manifest(root_path, manifest_path=manifest_path)
    if not manifest:
        errors.append("manifest_missing")
    else:
        errors.extend(_release_manifest_startup_errors_v3(root_path, manifest))

    expected_env = normalize_ontology_env(env) if env is not None else None
    manifest_env = manifest.get("env")
    if expected_env is not None and manifest_env != expected_env:
        errors.append("manifest_env_mismatch")

    global_spine_path = _resolve_v3_global_spine_path(root_path, manifest)
    router_sidecar_path = _resolve_v3_router_sidecar_path(root_path, manifest)
    router_coherence_path = _resolve_optional_router_coherence_path(root_path, manifest)
    chart_series_path = _resolve_optional_chart_series_path(root_path, manifest)
    index_outputs = manifest.get("indexes") if isinstance(manifest.get("indexes"), Mapping) else {}
    global_spine_output = (
        index_outputs.get("global_spine")
        if isinstance(index_outputs.get("global_spine"), Mapping)
        else {}
    )
    router_sidecar_output = (
        index_outputs.get("router_sidecar")
        if isinstance(index_outputs.get("router_sidecar"), Mapping)
        else {}
    )
    router_coherence_output = (
        index_outputs.get("router_coherence")
        if isinstance(index_outputs.get("router_coherence"), Mapping)
        else {}
    )
    spine_verification: dict[str, Any] | None = None
    if not global_spine_path.exists():
        errors.append("global_spine_missing")
    elif not global_spine_path.is_file():
        errors.append("global_spine_not_file")
    elif check_sqlite:
        spine_verification = _verify_global_spine_startup(
            global_spine_path,
            expected_sha256=(
                str(global_spine_output.get("sha256"))
                if global_spine_output.get("sha256")
                else None
            ),
        )
        errors.extend(spine_verification["errors"])
    else:
        spine_verification = {
            "ok": True,
            "errors": [],
            "path": str(global_spine_path),
            "sqlite_checked": False,
            "verification_mode": "startup",
        }

    router_sidecar_verification: dict[str, Any] | None = None
    if not router_sidecar_path.exists():
        errors.append("router_sidecar_missing")
    elif not router_sidecar_path.is_file():
        errors.append("router_sidecar_not_file")
    else:
        expected_global_sha = global_spine_output.get("sha256")
        expected_sidecar_sha = router_sidecar_output.get("sha256")
        expected_build_fingerprint = router_sidecar_output.get("build_fingerprint_sha256")
        if not isinstance(expected_sidecar_sha, str) or not expected_sidecar_sha:
            errors.append("router_sidecar:router_sidecar_manifest_sha256_missing")
        else:
            _sidecar_seal, sidecar_seal_status = read_immutable_sqlite_cache_seal(
                router_sidecar_path,
                kind="router_sidecar",
                cache_key=(str(expected_build_fingerprint) if expected_build_fingerprint else None),
            )
            if sidecar_seal_status != "valid":
                errors.append(
                    "router_sidecar:router_sidecar_verification_seal_required:"
                    f"{sidecar_seal_status}"
                )
            else:
                sealed_sidecar_sha = read_immutable_sqlite_cache_sha256(router_sidecar_path)
                if sealed_sidecar_sha is not None and sealed_sidecar_sha != expected_sidecar_sha:
                    errors.append("router_sidecar:router_sidecar_manifest_sha256_mismatch")
        router_sidecar_verification = verify_router_sidecar(
            router_sidecar_path,
            expected_global_spine_sha256=(
                str(expected_global_sha) if expected_global_sha else None
            ),
            expected_release_id=(
                str(manifest.get("release_id")) if manifest.get("release_id") else None
            ),
            expected_ranking_profile_sha256=(
                str(router_sidecar_output.get("ranking_profile_sha256"))
                if router_sidecar_output.get("ranking_profile_sha256")
                else None
            ),
            expected_build_fingerprint_sha256=(
                str(router_sidecar_output.get("build_fingerprint_sha256"))
                if router_sidecar_output.get("build_fingerprint_sha256")
                else None
            ),
            deep=False,
        )
        router_sidecar_verification["sqlite_checked"] = bool(check_sqlite)
        router_sidecar_verification["binding_checked"] = True
        router_sidecar_verification["artifact_seal_status"] = (
            sidecar_seal_status
            if isinstance(expected_sidecar_sha, str) and expected_sidecar_sha
            else "manifest_sha256_missing"
        )
        errors.extend(
            f"router_sidecar:{error}" for error in router_sidecar_verification.get("errors") or []
        )

    router_coherence_verification: dict[str, Any] | None = None
    if router_coherence_output:
        if router_coherence_path is None or not router_coherence_path.exists():
            errors.append("router_coherence_missing")
        elif not router_coherence_path.is_file():
            errors.append("router_coherence_not_file")
        else:
            expected_coherence_sha = router_coherence_output.get("sha256")
            expected_semantic_key = router_coherence_output.get("semantic_cache_key")
            if not isinstance(expected_coherence_sha, str) or not expected_coherence_sha:
                errors.append("router_coherence:router_coherence_manifest_sha256_missing")
                coherence_seal_status = "manifest_sha256_missing"
            else:
                _coherence_seal, coherence_seal_status = read_immutable_sqlite_cache_seal(
                    router_coherence_path,
                    kind="router_coherence",
                    cache_key=(str(expected_semantic_key) if expected_semantic_key else None),
                )
                if coherence_seal_status != "valid":
                    errors.append(
                        "router_coherence:router_coherence_verification_seal_required:"
                        f"{coherence_seal_status}"
                    )
                else:
                    sealed_coherence_sha = read_immutable_sqlite_cache_sha256(router_coherence_path)
                    if (
                        sealed_coherence_sha is not None
                        and sealed_coherence_sha != expected_coherence_sha
                    ):
                        errors.append("router_coherence:router_coherence_manifest_sha256_mismatch")
            router_coherence_verification = verify_router_coherence(
                router_coherence_path,
                expected_global_spine_sha256=(
                    str(global_spine_output.get("sha256"))
                    if global_spine_output.get("sha256")
                    else None
                ),
                expected_release_id=(
                    str(manifest.get("release_id")) if manifest.get("release_id") else None
                ),
                expected_profile_sha256=(
                    str(router_coherence_output.get("profile_sha256"))
                    if router_coherence_output.get("profile_sha256")
                    else None
                ),
                deep=False,
                require_trusted_seal=True,
            )
            router_coherence_verification["sqlite_checked"] = bool(check_sqlite)
            router_coherence_verification["binding_checked"] = True
            router_coherence_verification["artifact_seal_status"] = coherence_seal_status
            errors.extend(
                f"router_coherence:{error}"
                for error in router_coherence_verification.get("errors") or []
            )

    chart_series_verification: dict[str, Any] | None = None
    chart_series_output = (
        ((manifest.get("indexes") or {}).get("chart_series") or {}) if manifest else {}
    )
    chart_series_required = bool(
        isinstance(chart_series_output, Mapping) and chart_series_output.get("required") is True
    )
    if chart_series_path is not None:
        if chart_series_path.exists() and chart_series_path.is_file():
            if check_sqlite:
                chart_series_verification = verify_chart_series_index(chart_series_path)
                if chart_series_required and not chart_series_verification.get("ok"):
                    errors.extend(
                        f"chart_series:{error}"
                        for error in chart_series_verification.get("errors") or []
                    )
            else:
                chart_series_verification = {
                    "ok": True,
                    "errors": [],
                    "path": str(chart_series_path),
                    "sqlite_checked": False,
                    "verification_mode": "startup",
                }
        elif chart_series_required:
            errors.append("chart_series_missing")

    return {
        "ok": not errors,
        "errors": errors,
        "root": str(root_path),
        "supplied_root": str(supplied_root),
        "manifest_path": str(found_manifest_path) if found_manifest_path else None,
        "manifest": manifest,
        "env": manifest_env,
        "release_id": manifest.get("release_id"),
        "index_layout": manifest.get("index_layout"),
        "global_spine_path": str(global_spine_path),
        "global_spine_present": global_spine_path.exists(),
        "global_spine_verification": spine_verification,
        "router_sidecar_path": str(router_sidecar_path),
        "router_sidecar_present": router_sidecar_path.exists(),
        "router_sidecar_verification": router_sidecar_verification,
        "router_coherence_path": (
            str(router_coherence_path) if router_coherence_path is not None else None
        ),
        "router_coherence_present": bool(
            router_coherence_path is not None and router_coherence_path.exists()
        ),
        "router_coherence_verification": router_coherence_verification,
        "chart_series_path": str(chart_series_path) if chart_series_path is not None else None,
        "chart_series_present": bool(chart_series_path is not None and chart_series_path.exists()),
        "chart_series_verification": chart_series_verification,
        "current_symlink": _is_current_symlink_path(supplied_root),
        "verification_mode": "startup-v3",
    }


def _release_manifest_startup_errors_v3(root_path: Path, manifest: Mapping[str, Any]) -> list[str]:
    errors: list[str] = []
    if manifest.get("format") != RELEASE_FORMAT_V3:
        errors.append("manifest_format_unsupported")
    if manifest.get("status") != "ready":
        errors.append("manifest_status_not_ready")
    if manifest.get("index_layout") != GLOBAL_SPINE_LAYOUT:
        errors.append("manifest_index_layout_unsupported")
    if manifest.get("monolith_required") is not False:
        errors.append("manifest_monolith_required_not_false")
    errors.extend(_release_serving_binding_errors(manifest))
    errors.extend(metric_dictionary_binding_errors(manifest.get("metric_dictionary")))
    release_id = manifest.get("release_id")
    if not release_id:
        errors.append("manifest_release_id_missing")
    elif (
        isinstance(release_id, str)
        and root_path.parent.name in ALLOWED_ONTOLOGY_ENVS
        and root_path.name != release_id
    ):
        errors.append("manifest_release_id_directory_mismatch")
    outputs = manifest.get("indexes")
    if not isinstance(outputs, Mapping):
        errors.append("manifest_indexes_missing")
        return errors
    for role in ("global_spine", "router_sidecar", "company_shards", "shard_manifest"):
        output = outputs.get(role)
        if not isinstance(output, Mapping):
            errors.append(f"manifest_indexes_{role}_missing")
            continue
        if output.get("required") is not True:
            errors.append(f"manifest_indexes_{role}_not_required")
    global_spine = outputs.get("global_spine")
    if isinstance(global_spine, Mapping):
        raw_path = global_spine.get("path")
        if not isinstance(raw_path, str) or not raw_path:
            errors.append("manifest_indexes_global_spine_path_missing")
        else:
            errors.extend(
                _release_manifest_relative_file_startup_errors(root_path, "global_spine", raw_path)
            )
    router_sidecar = outputs.get("router_sidecar")
    if isinstance(router_sidecar, Mapping):
        raw_path = router_sidecar.get("path")
        if not isinstance(raw_path, str) or not raw_path:
            errors.append("manifest_indexes_router_sidecar_path_missing")
        else:
            errors.extend(
                _release_manifest_relative_file_startup_errors(
                    root_path,
                    "router_sidecar",
                    raw_path,
                )
            )
        for field in (
            "sha256",
            "ranking_profile_sha256",
            "source_global_spine_sha256",
            "build_fingerprint_sha256",
        ):
            if not isinstance(router_sidecar.get(field), str) or not router_sidecar.get(field):
                errors.append(f"manifest_indexes_router_sidecar_{field}_missing")
        if router_sidecar.get("verification_ok") is not True:
            errors.append("manifest_indexes_router_sidecar_verification_not_ok")
    router_coherence = outputs.get("router_coherence")
    if isinstance(router_coherence, Mapping):
        if router_coherence.get("required") is not True:
            errors.append("manifest_indexes_router_coherence_not_required")
        raw_path = router_coherence.get("path")
        if not isinstance(raw_path, str) or not raw_path:
            errors.append("manifest_indexes_router_coherence_path_missing")
        else:
            errors.extend(
                _release_manifest_relative_file_startup_errors(
                    root_path,
                    "router_coherence",
                    raw_path,
                )
            )
        for field in (
            "sha256",
            "profile_sha256",
            "source_global_spine_sha256",
            "semantic_cache_key",
            "build_fingerprint_sha256",
        ):
            if not isinstance(router_coherence.get(field), str) or not router_coherence.get(field):
                errors.append(f"manifest_indexes_router_coherence_{field}_missing")
        if router_coherence.get("verification_ok") is not True:
            errors.append("manifest_indexes_router_coherence_verification_not_ok")
    company_shards = outputs.get("company_shards")
    if isinstance(company_shards, Mapping):
        errors.extend(
            "manifest_indexes_company_shards_" + error
            for error in metric_dictionary_binding_errors(company_shards.get("metric_dictionary"))
        )
        raw_dir = company_shards.get("dir")
        if not isinstance(raw_dir, str) or not raw_dir:
            errors.append("manifest_indexes_company_shards_dir_missing")
        else:
            errors.extend(
                _release_manifest_relative_directory_errors(root_path, "company_shards", raw_dir)
            )
    shard_manifest = outputs.get("shard_manifest")
    if isinstance(shard_manifest, Mapping):
        raw_path = shard_manifest.get("path")
        if not isinstance(raw_path, str) or not raw_path:
            errors.append("manifest_indexes_shard_manifest_path_missing")
        else:
            errors.extend(
                _release_manifest_relative_file_startup_errors(
                    root_path, "shard_manifest", raw_path
                )
            )
    chart_series = outputs.get("chart_series")
    if isinstance(chart_series, Mapping):
        raw_path = chart_series.get("path")
        if chart_series.get("required") is True:
            if not isinstance(raw_path, str) or not raw_path:
                errors.append("manifest_indexes_chart_series_path_missing")
            else:
                errors.extend(
                    _release_manifest_relative_file_startup_errors(
                        root_path, "chart_series", raw_path
                    )
                )
        elif isinstance(raw_path, str) and raw_path:
            errors.extend(
                _release_manifest_relative_optional_file_errors(root_path, "chart_series", raw_path)
            )
    debug_monolith = outputs.get("debug_monolith")
    if isinstance(debug_monolith, Mapping) and debug_monolith.get("required") is True:
        errors.append("manifest_debug_monolith_required")
    return errors


def _release_serving_binding_errors(manifest: Mapping[str, Any]) -> list[str]:
    errors: list[str] = []
    builder = manifest.get("builder")
    builder = builder if isinstance(builder, Mapping) else {}
    expected_builder = {
        "spine_schema_version": GLOBAL_SPINE_SCHEMA_VERSION,
        "spine_builder_version": GLOBAL_SPINE_BUILDER_VERSION,
        "spine_projection_version": SPINE_PROJECTION_VERSION,
        "source_artifact_sqlite_schema_version": SOURCE_ARTIFACT_SQLITE_SCHEMA_VERSION,
        "source_artifact_sqlite_builder_version": SOURCE_ARTIFACT_SQLITE_BUILDER_VERSION,
        "company_shard_schema_version": COMPANY_SHARD_SCHEMA_VERSION,
        "router_sidecar_schema_version": ROUTER_SIDECAR_SCHEMA_VERSION,
        "router_sidecar_builder_version": ROUTER_SIDECAR_BUILDER_VERSION,
    }
    indexes = manifest.get("indexes")
    indexes = indexes if isinstance(indexes, Mapping) else {}
    if isinstance(indexes.get("router_coherence"), Mapping):
        expected_builder.update(
            {
                "router_coherence_schema_version": ROUTER_COHERENCE_SCHEMA_VERSION,
                "router_coherence_builder_version": ROUTER_COHERENCE_BUILDER_VERSION,
            }
        )
    for key, value in expected_builder.items():
        if builder.get(key) != value:
            errors.append(f"manifest_builder_binding_mismatch:{key}")
    global_spine = indexes.get("global_spine")
    global_spine = global_spine if isinstance(global_spine, Mapping) else {}
    if global_spine.get("schema_version") != GLOBAL_SPINE_SCHEMA_VERSION:
        errors.append("manifest_global_spine_schema_version_mismatch")
    if global_spine.get("builder_version") != GLOBAL_SPINE_BUILDER_VERSION:
        errors.append("manifest_global_spine_builder_version_mismatch")
    if global_spine.get("spine_projection_version") != SPINE_PROJECTION_VERSION:
        errors.append("manifest_spine_projection_version_mismatch")
    router_sidecar = indexes.get("router_sidecar")
    router_sidecar = router_sidecar if isinstance(router_sidecar, Mapping) else {}
    if router_sidecar.get("schema_version") != ROUTER_SIDECAR_SCHEMA_VERSION:
        errors.append("manifest_router_sidecar_schema_version_mismatch")
    if router_sidecar.get("builder_version") != ROUTER_SIDECAR_BUILDER_VERSION:
        errors.append("manifest_router_sidecar_builder_version_mismatch")
    router_coherence = indexes.get("router_coherence")
    if isinstance(router_coherence, Mapping):
        if router_coherence.get("schema_version") != ROUTER_COHERENCE_SCHEMA_VERSION:
            errors.append("manifest_router_coherence_schema_version_mismatch")
        if router_coherence.get("builder_version") != ROUTER_COHERENCE_BUILDER_VERSION:
            errors.append("manifest_router_coherence_builder_version_mismatch")
    company_shards = indexes.get("company_shards")
    company_shards = company_shards if isinstance(company_shards, Mapping) else {}
    if company_shards.get("schema_version") != COMPANY_SHARD_SCHEMA_VERSION:
        errors.append("manifest_company_shard_schema_version_mismatch")
    if (
        company_shards.get("source_artifact_sqlite_schema_version")
        != SOURCE_ARTIFACT_SQLITE_SCHEMA_VERSION
    ):
        errors.append("manifest_source_artifact_schema_version_mismatch")
    if (
        company_shards.get("source_artifact_sqlite_builder_version")
        != SOURCE_ARTIFACT_SQLITE_BUILDER_VERSION
    ):
        errors.append("manifest_source_artifact_builder_version_mismatch")
    return errors


def _release_manifest_relative_optional_file_errors(
    root_path: Path, role: str, raw_path: str
) -> list[str]:
    errors: list[str] = []
    candidate = Path(raw_path)
    if candidate.is_absolute():
        errors.append(f"manifest_indexes_{role}_path_not_relative")
        resolved = candidate.expanduser().resolve()
    else:
        resolved = (root_path / candidate).resolve()
    try:
        resolved.relative_to(root_path)
    except ValueError:
        errors.append(f"manifest_indexes_{role}_path_outside_root")
    if resolved.exists() and not resolved.is_file():
        errors.append(f"manifest_indexes_{role}_not_file")
    return errors


def _release_manifest_relative_file_startup_errors(
    root_path: Path, role: str, raw_path: str
) -> list[str]:
    errors: list[str] = []
    candidate = Path(raw_path)
    if candidate.is_absolute():
        errors.append(f"manifest_indexes_{role}_path_not_relative")
        resolved = candidate.expanduser().resolve()
    else:
        resolved = (root_path / candidate).resolve()
    try:
        resolved.relative_to(root_path)
    except ValueError:
        errors.append(f"manifest_indexes_{role}_path_outside_root")
    if not resolved.exists():
        errors.append(f"manifest_indexes_{role}_missing_file")
    elif not resolved.is_file():
        errors.append(f"manifest_indexes_{role}_not_file")
    return errors


def _resolve_v3_global_spine_path(root_path: Path, manifest: Mapping[str, Any]) -> Path:
    outputs = manifest.get("indexes")
    if isinstance(outputs, Mapping):
        global_spine = outputs.get("global_spine")
        if isinstance(global_spine, Mapping):
            raw_path = global_spine.get("path")
            if isinstance(raw_path, str) and raw_path:
                candidate = Path(raw_path)
                return (
                    candidate.expanduser().resolve()
                    if candidate.is_absolute()
                    else (root_path / candidate).resolve()
                )
    raw_path = manifest.get("global_spine_path")
    if isinstance(raw_path, str) and raw_path:
        candidate = Path(raw_path)
        return (
            candidate.expanduser().resolve()
            if candidate.is_absolute()
            else (root_path / candidate).resolve()
        )
    return (root_path / GLOBAL_SPINE_RELATIVE_PATH).resolve()


def _resolve_v3_router_sidecar_path(root_path: Path, manifest: Mapping[str, Any]) -> Path:
    outputs = manifest.get("indexes")
    if isinstance(outputs, Mapping):
        router_sidecar = outputs.get("router_sidecar")
        if isinstance(router_sidecar, Mapping):
            raw_path = router_sidecar.get("path")
            if isinstance(raw_path, str) and raw_path:
                candidate = Path(raw_path)
                return (
                    candidate.expanduser().resolve()
                    if candidate.is_absolute()
                    else (root_path / candidate).resolve()
                )
    return (root_path / ROUTER_SIDECAR_RELATIVE_PATH).resolve()


def _resolve_optional_router_coherence_path(
    root_path: Path,
    manifest: Mapping[str, Any],
) -> Path | None:
    outputs = manifest.get("indexes")
    if not isinstance(outputs, Mapping):
        return None
    router_coherence = outputs.get("router_coherence")
    if not isinstance(router_coherence, Mapping):
        return None
    raw_path = router_coherence.get("path")
    if not isinstance(raw_path, str) or not raw_path:
        return None
    candidate = Path(raw_path)
    return (
        candidate.expanduser().resolve()
        if candidate.is_absolute()
        else (root_path / candidate).resolve()
    )


def _resolve_optional_chart_series_path(
    root_path: Path, manifest: Mapping[str, Any]
) -> Path | None:
    outputs = manifest.get("indexes")
    if not isinstance(outputs, Mapping):
        default_path = root_path / CHART_SERIES_RELATIVE_PATH
        return default_path.resolve() if default_path.exists() else None
    chart_series = outputs.get("chart_series")
    if not isinstance(chart_series, Mapping):
        default_path = root_path / CHART_SERIES_RELATIVE_PATH
        return default_path.resolve() if default_path.exists() else None
    raw_path = chart_series.get("path")
    if not isinstance(raw_path, str) or not raw_path:
        return None
    candidate = Path(raw_path)
    return (
        candidate.expanduser().resolve()
        if candidate.is_absolute()
        else (root_path / candidate).resolve()
    )


def _verify_global_spine_startup(
    path: Path,
    *,
    expected_sha256: str | None,
) -> dict[str, Any]:
    schema_verification = verify_global_spine_schema(
        path,
        deep=False,
        trust_seal=True,
        require_trusted_seal=True,
    )
    errors = list(schema_verification.get("errors") or [])
    metadata = dict(schema_verification.get("metadata") or {})
    sealed_sha256 = read_spine_verification_sha256(path)
    if not expected_sha256:
        errors.append("global_spine_manifest_sha256_missing")
    elif sealed_sha256 is None:
        errors.append("global_spine_verification_seal_sha256_missing")
    elif sealed_sha256 != expected_sha256:
        errors.append("global_spine_manifest_sha256_mismatch")
    if metadata.get("schema_version") != GLOBAL_SPINE_SCHEMA_VERSION:
        errors.append("global_spine_schema_version_mismatch")
    if metadata.get("builder_version") != GLOBAL_SPINE_BUILDER_VERSION:
        errors.append("global_spine_builder_version_mismatch")
    if metadata.get("spine_projection_version") != SPINE_PROJECTION_VERSION:
        errors.append("global_spine_projection_version_mismatch")
    if (
        metadata.get("source_artifact_sqlite_schema_version")
        != SOURCE_ARTIFACT_SQLITE_SCHEMA_VERSION
    ):
        errors.append("global_spine_source_artifact_schema_version_mismatch")
    if (
        metadata.get("source_artifact_sqlite_builder_version")
        != SOURCE_ARTIFACT_SQLITE_BUILDER_VERSION
    ):
        errors.append("global_spine_source_artifact_builder_version_mismatch")
    if metadata.get("company_shard_schema_version") != COMPANY_SHARD_SCHEMA_VERSION:
        errors.append("global_spine_company_shard_schema_version_mismatch")
    if metadata.get("index_layout") != GLOBAL_SPINE_LAYOUT:
        errors.append("global_spine_index_layout_mismatch")
    return {
        "ok": not errors,
        "errors": errors,
        "path": str(path),
        "metadata": metadata,
        "schema_verification": schema_verification,
        "sqlite_checked": True,
        "verification_mode": "startup-quick-schema",
    }


def _is_current_symlink_path(path: Path) -> bool:
    return path.name == "current" and path.is_symlink()


def _release_filesystem_errors(root_path: Path) -> list[str]:
    errors: list[str] = []
    if not root_path.exists():
        return ["release_root_missing"]
    if not root_path.is_dir():
        return ["release_root_not_directory"]
    if not (root_path / "companies").is_dir():
        errors.append("companies_dir_missing")
    if not (root_path / "indexes").is_dir():
        errors.append("indexes_dir_missing")
    for path in sorted(
        root_path.rglob("*"), key=lambda item: item.relative_to(root_path).as_posix()
    ):
        relative = path.relative_to(root_path).as_posix()
        if path.is_symlink() and not path.exists():
            errors.append(f"broken_symlink:{relative}")
        if _is_release_temp_path(path):
            errors.append(f"release_temp_artifact:{relative}")
    return errors


def _release_filesystem_light_errors(root_path: Path) -> list[str]:
    errors: list[str] = []
    if not root_path.exists():
        return ["release_root_missing"]
    if not root_path.is_dir():
        return ["release_root_not_directory"]
    if not (root_path / "companies").is_dir():
        errors.append("companies_dir_missing")
    if not (root_path / "indexes").is_dir():
        errors.append("indexes_dir_missing")
    return errors


def _is_release_temp_path(path: Path) -> bool:
    name = path.name
    if name == ".build" or name.endswith(".tmp") or name.endswith(".building"):
        return True
    return name.startswith(".") and ".tmp" in name


def _release_manifest_relative_directory_errors(
    root_path: Path,
    role: str,
    raw_path: str,
) -> list[str]:
    candidate = Path(raw_path)
    errors: list[str] = []
    if candidate.is_absolute():
        errors.append(f"manifest_index_output_path_not_relative:{role}")
        resolved = candidate.expanduser().resolve()
    else:
        resolved = (root_path / candidate).resolve()
    try:
        resolved.relative_to(root_path)
    except ValueError:
        errors.append(f"manifest_index_output_path_outside_root:{role}")
    if not resolved.is_dir():
        errors.append(f"manifest_index_output_missing:{role}")
    return errors


def write_release_verification_report(
    root: Path | str,
    *,
    env: str | None = None,
    manifest_path: Path | str | None = None,
    require_current_symlink: bool = False,
    deep: bool = False,
    verification: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Write a deterministic verification report for an immutable release root."""
    supplied_root = Path(root).expanduser()
    root_path = supplied_root.resolve()
    _assert_not_active_release_path(root_path, "root")
    if not root_path.is_dir():
        raise FileNotFoundError(f"Release root not found: {root_path}")
    if verification is None:
        verification_payload = verify_release_root(
            supplied_root,
            env=env,
            manifest_path=manifest_path,
            require_current_symlink=require_current_symlink,
            deep=deep,
        )
    else:
        verification_payload = verification
    verification_payload = {
        **verification_payload,
        "errors": list(verification_payload.get("errors") or []),
    }
    report_deep = bool(verification_payload.get("deep"))
    report_manifest = verification_payload.get("manifest")
    file_trace = _release_file_trace(
        root_path,
        deep=report_deep,
        manifest=report_manifest if isinstance(report_manifest, Mapping) else None,
    )
    reproducibility_hash = _release_reproducibility_hash(
        release_id=verification_payload.get("release_id"),
        env=verification_payload.get("env"),
        files=file_trace,
    )
    report_path = root_path / RELEASE_VERIFY_DIRNAME / RELEASE_VERIFY_REPORT_FILENAME
    report = {
        "format": RELEASE_VERIFY_REPORT_FORMAT,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "root": str(root_path),
        "supplied_root": str(supplied_root),
        "ok": bool(verification_payload.get("ok")),
        "errors": list(verification_payload.get("errors") or []),
        "env": verification_payload.get("env"),
        "release_id": verification_payload.get("release_id"),
        "manifest_path": _relative_or_absolute(
            Path(str(verification_payload["manifest_path"])).expanduser().resolve(),
            root_path,
        )
        if verification_payload.get("manifest_path")
        else None,
        "global_spine_path": _relative_or_absolute(
            Path(str(verification_payload["global_spine_path"])).expanduser().resolve(),
            root_path,
        )
        if verification_payload.get("global_spine_path")
        else None,
        "reproducibility_hash": reproducibility_hash,
        "file_count": len(file_trace),
        "files": file_trace,
        "verification": verification_payload,
    }
    _write_json_atomic(report_path, report)
    return {**report, "path": str(report_path)}


def _write_json_atomic(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    tmp_path.write_text(
        json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    os.replace(tmp_path, path)


def _read_json_object(path: Path) -> dict[str, Any] | None:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (FileNotFoundError, json.JSONDecodeError):
        return None
    return payload if isinstance(payload, dict) else None


def _release_file_trace(
    root: Path,
    *,
    deep: bool = False,
    manifest: Mapping[str, Any] | None = None,
) -> list[dict[str, Any]]:
    if not deep:
        return _release_file_trace_light(root, manifest=manifest)
    entries: list[dict[str, Any]] = []
    if not root.exists():
        return entries
    for path in sorted(root.rglob("*"), key=lambda item: item.relative_to(root).as_posix()):
        relative_path = path.relative_to(root)
        if relative_path.parts and relative_path.parts[0] == RELEASE_VERIFY_DIRNAME:
            continue
        if path.is_symlink():
            entries.append(
                {
                    "path": relative_path.as_posix(),
                    "role": _release_file_role(relative_path),
                    "kind": "symlink",
                    "target": os.readlink(path),
                }
            )
            continue
        if not path.is_file():
            continue
        stat = path.stat()
        entries.append(
            {
                "path": relative_path.as_posix(),
                "role": _release_file_role(relative_path),
                "kind": "file",
                "size_bytes": stat.st_size,
                "sha256": _file_sha256(path),
                "mtime": datetime.fromtimestamp(stat.st_mtime, tz=timezone.utc).isoformat(),
            }
        )
    return entries


def _release_file_trace_light(
    root: Path, *, manifest: Mapping[str, Any] | None = None
) -> list[dict[str, Any]]:
    entries: list[dict[str, Any]] = []
    seen: set[str] = set()

    def add(path: Path) -> None:
        try:
            relative_path = path.resolve().relative_to(root)
        except ValueError:
            return
        relative = relative_path.as_posix()
        if relative in seen or relative.startswith(f"{RELEASE_VERIFY_DIRNAME}/"):
            return
        seen.add(relative)
        if path.is_symlink():
            entries.append(
                {
                    "path": relative,
                    "role": _release_file_role(relative_path),
                    "kind": "symlink",
                    "target": os.readlink(path),
                }
            )
            return
        if not path.is_file():
            return
        stat = path.stat()
        entries.append(
            {
                "path": relative,
                "role": _release_file_role(relative_path),
                "kind": "file",
                "size_bytes": stat.st_size,
                "mtime": datetime.fromtimestamp(stat.st_mtime, tz=timezone.utc).isoformat(),
            }
        )

    add(root / RELEASE_MANIFEST_FILENAME)
    global_spine_path = _resolve_v3_global_spine_path(root, manifest or {})
    add(global_spine_path)
    shard_manifest_path = _manifest_index_file_path(
        root, manifest, "shard_manifest", default="indexes/shard_manifest.json"
    )
    add(shard_manifest_path)
    chart_series_path = _manifest_index_file_path(
        root,
        manifest,
        "chart_series",
        default=CHART_SERIES_RELATIVE_PATH.as_posix(),
    )
    add(chart_series_path)
    add(root / "indexes" / "build_summary.json")
    shard_manifest = _read_json_object(shard_manifest_path)
    shards = shard_manifest.get("shards") if isinstance(shard_manifest, Mapping) else None
    if isinstance(shards, Mapping):
        company_shards_dir = _manifest_index_dir_path(
            root, manifest, "company_shards", default="indexes/companies"
        )
        for entry in shards.values():
            if not isinstance(entry, Mapping):
                continue
            raw_path = entry.get("path") or entry.get("shard_path")
            if not isinstance(raw_path, str) or not raw_path:
                continue
            candidate = Path(raw_path)
            if candidate.is_absolute():
                shard_path = candidate.expanduser().resolve()
            elif candidate.parts and candidate.parts[0] == "indexes":
                shard_path = (root / candidate).resolve()
            else:
                shard_path = (root / "indexes" / candidate).resolve()
                if not shard_path.exists():
                    shard_path = (company_shards_dir / candidate.name).resolve()
            add(shard_path)
    return sorted(entries, key=lambda item: item["path"])


def _manifest_index_file_path(
    root: Path,
    manifest: Mapping[str, Any] | None,
    role: str,
    *,
    default: str,
) -> Path:
    output = ((manifest or {}).get("indexes") or {}).get(role)
    raw_path = output.get("path") if isinstance(output, Mapping) else None
    candidate = Path(raw_path) if isinstance(raw_path, str) and raw_path else Path(default)
    return (
        candidate.expanduser().resolve()
        if candidate.is_absolute()
        else (root / candidate).resolve()
    )


def _manifest_index_dir_path(
    root: Path,
    manifest: Mapping[str, Any] | None,
    role: str,
    *,
    default: str,
) -> Path:
    output = ((manifest or {}).get("indexes") or {}).get(role)
    raw_path = output.get("dir") if isinstance(output, Mapping) else None
    candidate = Path(raw_path) if isinstance(raw_path, str) and raw_path else Path(default)
    return (
        candidate.expanduser().resolve()
        if candidate.is_absolute()
        else (root / candidate).resolve()
    )


def _release_file_role(relative_path: Path) -> str:
    parts = relative_path.parts
    path_text = relative_path.as_posix()
    if path_text == RELEASE_MANIFEST_FILENAME:
        return "release_manifest"
    if path_text == GLOBAL_SPINE_RELATIVE_PATH.as_posix():
        return "global_spine"
    if path_text == "indexes/shard_manifest.json":
        return "shard_manifest"
    if path_text == CHART_SERIES_RELATIVE_PATH.as_posix():
        return "chart_series"
    if (
        len(parts) >= 3
        and parts[0] == "indexes"
        and parts[1] == "companies"
        and relative_path.suffix == ".sqlite"
    ):
        return "company_shard"
    if parts and parts[0] == "indexes":
        return "index_artifact"
    if parts and parts[0] == "companies":
        return "ontology_artifact"
    return "release_artifact"


def _release_reproducibility_hash(
    *,
    release_id: Any,
    env: Any,
    files: list[dict[str, Any]],
) -> str:
    deterministic_files: list[dict[str, Any]] = []
    for entry in files:
        deterministic_entry = {
            key: entry[key]
            for key in ("path", "role", "kind", "size_bytes", "sha256", "target")
            if key in entry
        }
        deterministic_files.append(deterministic_entry)
    payload = {
        "format": RELEASE_FORMAT,
        "release_id": str(release_id or ""),
        "env": str(env or ""),
        "files": deterministic_files,
    }
    encoded = json.dumps(payload, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode(
        "utf-8"
    )
    return hashlib.sha256(encoded).hexdigest()


def _file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _cached_file_sha256(path: Path) -> str:
    """Hash an immutable release artifact once per stat identity."""
    return immutable_file_sha256(path)


def release_env_root(releases_root: Path | str, env: str | None = None) -> Path:
    """Return the env-specific release directory under a releases root."""
    resolved_env = normalize_ontology_env(env)
    root = Path(releases_root).expanduser()
    return root if root.name == resolved_env else root / resolved_env


def _assert_not_active_release_path(path: Path, label: str) -> None:
    if _path_points_at_active_release(path):
        raise ValueError(
            f"Refusing {label}={path}: current is an immutable release pointer. "
            "Build a candidate release and promote it instead."
        )


def _path_points_at_active_release(path: Path) -> bool:
    expanded = path.expanduser().absolute()
    for candidate in (expanded, *expanded.parents):
        if candidate.name == "current" and candidate.parent.name in ALLOWED_ONTOLOGY_ENVS:
            return True
        env_root = candidate.parent
        if env_root.name not in ALLOWED_ONTOLOGY_ENVS:
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


def current_release_id(env_root: Path) -> str | None:
    """Return the release id pointed to by current, if any."""
    current = env_root / "current"
    if not current.is_symlink():
        return None
    target = os.readlink(current)
    return Path(target).name.rstrip("/")


def list_release_ids(env_root: Path) -> list[str]:
    """List release directories newest first."""
    if not env_root.exists():
        return []
    dirs = [
        path
        for path in env_root.iterdir()
        if path.is_dir()
        and path.name not in RELEASE_ENV_RESERVED_DIRNAMES
        and not path.name.startswith(".")
    ]
    return [path.name for path in sorted(dirs, key=lambda path: path.stat().st_mtime, reverse=True)]


def quarantine_local_release(
    releases_root: Path | str,
    *,
    env: str | None,
    release_path: Path | str,
    action: str,
    error: str,
) -> dict[str, Any] | None:
    """Move a failed local candidate out of the promotable release namespace."""
    source = Path(release_path).expanduser()
    if not source.exists() and not source.is_symlink():
        return None
    env_root = release_env_root(releases_root, env).expanduser()
    failed_root = env_root / FAILED_RELEASE_DIRNAME
    failed_root.mkdir(parents=True, exist_ok=True)
    target = failed_root / source.name
    if target.exists() or target.is_symlink():
        suffix = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
        target = failed_root / f"{source.name}-{suffix}"
    shutil.move(str(source), str(target))
    payload = {
        "format": FAILED_RELEASE_FORMAT,
        "env": normalize_ontology_env(env),
        "release_id": source.name,
        "action": action,
        "failed_at": datetime.now(timezone.utc).isoformat(),
        "error": error,
        "original_path": str(source),
        "quarantine_path": str(target),
    }
    _write_json_atomic(target / FAILED_RELEASE_METADATA_FILENAME, payload)
    return {**payload, "path": str(target)}


def promote_local_release(
    releases_root: Path | str,
    *,
    env: str | None,
    release_id: str,
    action: str = "promote",
    preverified: Mapping[str, Any] | None = None,
    preverified_report: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    """Atomically point an env current symlink to a release id."""
    env_root = release_env_root(releases_root, env)
    previous_release_id = current_release_id(env_root)
    if previous_release_id == release_id:
        raise ValueError(f"Release {release_id} is already current")
    release_dir = env_root / release_id
    if not release_dir.is_dir():
        raise FileNotFoundError(f"Release directory not found: {release_dir}")
    event_log_paths = _release_event_log_paths(env_root=env_root, release_dir=release_dir)
    activated_before = event_log_paths["release_event_log"].exists()
    # A release worker verifies the candidate immediately before promotion and
    # passes that exact result here.  Reuse it so activation does not repeat a
    # full, sequential scan of every immutable shard.  Standalone promotion
    # calls still perform their own deep verification because they do not share
    # the worker's in-process trust boundary.
    if preverified is None:
        verification = verify_release_root(release_dir, env=env, deep=True)
    else:
        verification = _validated_preverified_release(
            preverified,
            release_dir=release_dir,
            env=env,
            release_id=release_id,
        )
    if not verification["ok"]:
        if not activated_before:
            write_release_verification_report(
                release_dir,
                env=env,
                verification=verification,
            )
        raise ValueError(f"Release verification failed: {', '.join(verification['errors'])}")
    if preverified_report is not None:
        verify_report = dict(preverified_report)
        if verify_report.get("ok") is not True:
            raise ValueError("Preverified release report invalid: verification_not_ok")
        verify_report.setdefault(
            "path",
            str(release_dir / RELEASE_VERIFY_DIRNAME / RELEASE_VERIFY_REPORT_FILENAME),
        )
    else:
        verify_report = (
            _load_existing_release_verification_report(release_dir)
            if activated_before
            else write_release_verification_report(
                release_dir,
                env=env,
                verification=verification,
            )
        )
    if not verify_report["ok"]:
        raise ValueError(
            f"Release verification report failed: {', '.join(verify_report['errors'])}"
        )
    env_root.mkdir(parents=True, exist_ok=True)
    current = env_root / "current"
    _prepare_release_event_log_paths(event_log_paths)
    next_link = env_root / "current.next"
    if next_link.exists() or next_link.is_symlink():
        next_link.unlink()
    next_link.symlink_to(release_id)
    os.replace(next_link, current)
    event_logs = _write_release_changed_events(
        env_root=env_root,
        release_dir=release_dir,
        current=current,
        env=normalize_ontology_env(env),
        release_id=release_id,
        previous_release_id=previous_release_id,
        action=action,
        verification=verification,
        event_log_paths=event_log_paths,
    )
    return {
        "env_root": str(env_root.resolve()),
        "env": normalize_ontology_env(env),
        "release_id": release_id,
        "previous_release_id": previous_release_id,
        "current": str(current),
        "event_log": event_logs["env_event_log"],
        "release_event_log": event_logs["release_event_log"],
        "event_log_errors": event_logs["event_log_errors"],
        "verify_report": verify_report["path"],
    }


def _validated_preverified_release(
    verification: Mapping[str, Any],
    *,
    release_dir: Path,
    env: str | None,
    release_id: str,
) -> dict[str, Any]:
    """Bind an in-process verification result to the candidate being promoted."""
    payload = dict(verification)
    errors = list(payload.get("errors") or [])
    if payload.get("ok") is not True or errors:
        raise ValueError(
            "Preverified release invalid: " + (", ".join(errors) or "verification_not_ok")
        )
    if payload.get("release_id") != release_id:
        raise ValueError("Preverified release invalid: release_id_mismatch")
    if payload.get("env") != normalize_ontology_env(env):
        raise ValueError("Preverified release invalid: env_mismatch")
    try:
        verified_root = Path(str(payload["root"])).expanduser().resolve()
    except (KeyError, OSError, RuntimeError, TypeError, ValueError) as exc:
        raise ValueError("Preverified release invalid: root_missing") from exc
    if verified_root != release_dir.resolve():
        raise ValueError("Preverified release invalid: root_mismatch")
    payload["errors"] = errors
    return payload


def _load_existing_release_verification_report(release_dir: Path) -> dict[str, Any]:
    verify_report_path = release_dir / RELEASE_VERIFY_DIRNAME / RELEASE_VERIFY_REPORT_FILENAME
    try:
        release_verify = json.loads(verify_report_path.read_text(encoding="utf-8"))
    except FileNotFoundError as exc:
        raise ValueError(
            "Previously activated release verification artifacts invalid: release_verify_missing"
        ) from exc
    except (json.JSONDecodeError, OSError) as exc:
        raise ValueError(
            f"Previously activated release verification artifacts invalid: release_verify_invalid:{exc}"
        ) from exc
    if not isinstance(release_verify, dict):
        raise ValueError(
            "Previously activated release verification artifacts invalid: release_verify_not_object"
        )
    if release_verify.get("ok") is not True:
        raise ValueError(
            "Previously activated release verification artifacts invalid: release_verify_not_ok"
        )
    verification_payload = release_verify.get("verification")
    manifest_payload = release_verify.get("manifest")
    release_format = None
    if isinstance(verification_payload, Mapping):
        verification_manifest = verification_payload.get("manifest")
        if isinstance(verification_manifest, Mapping):
            release_format = verification_manifest.get("format")
    if release_format is None and isinstance(manifest_payload, Mapping):
        release_format = manifest_payload.get("format")
    if release_format == RELEASE_FORMAT_V3:
        return {
            **release_verify,
            "path": str(verify_report_path),
            "errors": [],
            "ok": True,
        }
    raise ValueError(
        "Previously activated release verification artifacts invalid: release_format_not_v3"
    )


def _release_event_log_paths(*, env_root: Path, release_dir: Path) -> dict[str, Path]:
    return {
        "env_event_log": env_root / RELEASE_EVENTS_FILENAME,
        "release_event_log": env_root / RELEASE_EVENTS_DIRNAME / f"{release_dir.name}.jsonl",
    }


def _prepare_release_event_log_paths(paths: Mapping[str, Path]) -> None:
    for path in paths.values():
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("a", encoding="utf-8"):
            pass


def _write_release_changed_events(
    *,
    env_root: Path,
    release_dir: Path,
    current: Path,
    env: str,
    release_id: str,
    previous_release_id: str | None,
    action: str,
    verification: Mapping[str, Any],
    event_log_paths: Mapping[str, Path] | None = None,
) -> dict[str, Any]:
    payload = {
        "format": RELEASE_EVENT_FORMAT,
        "event": "release_changed",
        "action": action,
        "env": env,
        "release_id": release_id,
        "previous_release_id": previous_release_id,
        "current": str(current),
        "changed_at": datetime.now(timezone.utc).isoformat(),
        "verification_ok": bool(verification.get("ok")),
        "verification_errors": list(verification.get("errors") or []),
        "manifest_path": verification.get("manifest_path"),
        "global_spine_path": verification.get("global_spine_path"),
    }
    paths = event_log_paths or _release_event_log_paths(env_root=env_root, release_dir=release_dir)
    env_event_log = paths["env_event_log"]
    release_event_log = paths["release_event_log"]
    event_log_errors: list[str] = []
    for label, path in paths.items():
        try:
            _append_release_event(path, payload)
        except OSError as exc:
            event_log_errors.append(f"{label}:{path}:{exc}")
    return {
        "env_event_log": str(env_event_log),
        "release_event_log": str(release_event_log),
        "event_log_errors": event_log_errors,
    }


def _append_release_event(path: Path, payload: Mapping[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(dict(payload), sort_keys=True) + "\n")


def rollback_local_release(
    releases_root: Path | str,
    *,
    env: str | None,
    release_id: str | None = None,
) -> dict[str, Any]:
    """Rollback current to a requested or previous release id."""
    env_root = release_env_root(releases_root, env)
    current_id = current_release_id(env_root)
    target_id = release_id
    if target_id is None:
        for candidate in list_release_ids(env_root):
            if candidate != current_id:
                target_id = candidate
                break
    if target_id is None:
        raise FileNotFoundError("No rollback target found")
    return promote_local_release(env_root, env=env, release_id=target_id, action="rollback")
