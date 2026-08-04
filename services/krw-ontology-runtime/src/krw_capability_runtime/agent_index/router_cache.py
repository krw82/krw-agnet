"""Content-addressed cache for immutable Router Sidecar artifacts."""

from __future__ import annotations

import hashlib
import json
import os
import time
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from krw_capability_runtime.agent_index.cache_seal import (
    immutable_file_identity_matches,
    immutable_sqlite_cache_seal_path,
    write_immutable_sqlite_cache_seal,
)
from krw_capability_runtime.agent_index.router_sidecar import (
    ROUTER_SIDECAR_BUILDER_VERSION,
    ROUTER_SIDECAR_SCHEMA_VERSION,
    immutable_file_sha256,
    load_router_ranking_profile,
    rebind_router_sidecar_source,
    verify_router_sidecar,
)


ROUTER_SIDECAR_CACHE_FORMAT_VERSION = "krw-ontology-router-sidecar-cache/v1"


@dataclass(frozen=True)
class RouterSidecarCacheRestore:
    path: Path
    cache_path: Path
    cache_key: str
    copy_mode: str
    verification: Mapping[str, Any]


def router_sidecar_semantic_cache_key(
    *,
    fragment_cache_keys: Sequence[tuple[str, str]],
    source_manifest_hash: str | None,
    generate_links: bool,
    global_spine_schema_version: str,
    global_spine_builder_version: str,
    spine_projection_version: str,
    cross_company_link_builder_version: str,
    metric_dictionary: Mapping[str, Any],
    ranking_profile_path: Path | str | None = None,
) -> str:
    """Hash every semantic input while excluding release ID and timestamps."""
    profile = load_router_ranking_profile(ranking_profile_path)
    ordered_fragments = [
        [str(ticker).upper(), str(cache_key)]
        for ticker, cache_key in sorted(fragment_cache_keys, key=lambda item: item[0])
    ]
    if not ordered_fragments or any(not cache_key for _ticker, cache_key in ordered_fragments):
        raise ValueError("router sidecar cache requires every ordered fragment cache key")
    payload = {
        "cache_format": ROUTER_SIDECAR_CACHE_FORMAT_VERSION,
        "cross_company_link_builder_version": cross_company_link_builder_version,
        "fragment_cache_keys": ordered_fragments,
        "generate_links": bool(generate_links),
        "global_spine_builder_version": global_spine_builder_version,
        "global_spine_schema_version": global_spine_schema_version,
        "metric_dictionary": dict(metric_dictionary),
        "ranking_profile": profile,
        "router_sidecar_builder_version": ROUTER_SIDECAR_BUILDER_VERSION,
        "router_sidecar_schema_version": ROUTER_SIDECAR_SCHEMA_VERSION,
        "source_manifest_hash": source_manifest_hash,
        "spine_projection_version": spine_projection_version,
    }
    encoded = json.dumps(
        payload,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")
    return f"{ROUTER_SIDECAR_CACHE_FORMAT_VERSION}:{hashlib.sha256(encoded).hexdigest()}"


def router_sidecar_cache_path(cache_root: Path | str, cache_key: str) -> Path:
    digest = str(cache_key).rsplit(":", 1)[-1]
    if len(digest) != 64 or any(char not in "0123456789abcdef" for char in digest):
        raise ValueError("invalid router sidecar cache key digest")
    root = Path(cache_root).expanduser().resolve()
    return root / "v3" / "router_sidecars" / digest[:2] / f"{digest}.sqlite"


def verify_router_sidecar_cache(path: Path | str, *, cache_key: str) -> tuple[str, ...]:
    resolved = Path(path).expanduser().resolve()
    seal_path = _cache_seal_path(resolved)
    errors: list[str] = []
    if not resolved.is_file():
        return ("router_sidecar_cache_missing",)
    if not seal_path.is_file():
        return ("router_sidecar_cache_seal_missing",)
    try:
        seal = json.loads(seal_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return ("router_sidecar_cache_seal_invalid",)
    if not isinstance(seal, Mapping):
        return ("router_sidecar_cache_seal_invalid",)
    if seal.get("format") != ROUTER_SIDECAR_CACHE_FORMAT_VERSION:
        errors.append("router_sidecar_cache_format_mismatch")
    if seal.get("cache_key") != cache_key:
        errors.append("router_sidecar_cache_key_mismatch")
    stat = resolved.stat()
    if int(seal.get("size_bytes") or -1) != stat.st_size:
        errors.append("router_sidecar_cache_size_mismatch")
    sealed_stat = seal.get("file_stat")
    stat_unchanged = immutable_file_identity_matches(sealed_stat, _file_stat(resolved))
    if not errors and not stat_unchanged and seal.get("sha256") != immutable_file_sha256(resolved):
        errors.append("router_sidecar_cache_sha256_mismatch")
    verification = verify_router_sidecar(resolved, deep=False)
    if not verification.get("ok"):
        errors.extend(f"router_sidecar_cache:{error}" for error in verification.get("errors") or [])
    metadata = verification.get("metadata") or {}
    if metadata.get("schema_version") != ROUTER_SIDECAR_SCHEMA_VERSION:
        errors.append("router_sidecar_cache_schema_version_mismatch")
    if metadata.get("builder_version") != ROUTER_SIDECAR_BUILDER_VERSION:
        errors.append("router_sidecar_cache_builder_version_mismatch")
    for key in ("content_sha256", "ranking_profile_sha256"):
        if seal.get(key) != metadata.get(key):
            errors.append(f"router_sidecar_cache_{key}_mismatch")
    return tuple(dict.fromkeys(errors))


def publish_router_sidecar_cache(
    source_path: Path | str,
    cache_path: Path | str,
    *,
    cache_key: str,
    copy_file: Callable[[Path, Path], str],
) -> str:
    """Publish one verified immutable sidecar and an integrity seal atomically."""
    source = Path(source_path).expanduser().resolve()
    target = Path(cache_path).expanduser().resolve()
    existing_errors = verify_router_sidecar_cache(target, cache_key=cache_key)
    if not existing_errors:
        return "existing"
    verification = verify_router_sidecar(source, deep=False)
    if not verification.get("ok"):
        raise ValueError(
            "cannot cache invalid router sidecar: " + ", ".join(verification.get("errors") or [])
        )
    target.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = target.parent / f".{target.name}.{os.getpid()}.{time.time_ns()}.tmp"
    tmp_seal = _cache_seal_path(tmp_path)
    _cleanup_cache_files(tmp_path)
    try:
        source_deep_verification = verify_router_sidecar(source, deep=True)
        copy_mode = copy_file(source, tmp_path)
        copied_verification = verify_router_sidecar(tmp_path, deep=False)
        if not copied_verification.get("ok"):
            raise RuntimeError(
                "copied router sidecar cache failed verification: "
                + ", ".join(copied_verification.get("errors") or [])
            )
        metadata = copied_verification.get("metadata") or {}
        seal = {
            "format": ROUTER_SIDECAR_CACHE_FORMAT_VERSION,
            "cache_key": cache_key,
            "size_bytes": tmp_path.stat().st_size,
            "sha256": immutable_file_sha256(tmp_path),
            "schema_version": metadata.get("schema_version"),
            "builder_version": metadata.get("builder_version"),
            "content_sha256": metadata.get("content_sha256"),
            "ranking_profile_sha256": metadata.get("ranking_profile_sha256"),
        }
        os.replace(tmp_path, target)
        if source_deep_verification.get("ok"):
            write_immutable_sqlite_cache_seal(
                target,
                kind="router_sidecar",
                cache_key=str(
                    (source_deep_verification.get("metadata") or {}).get("build_fingerprint_sha256")
                    or ""
                ),
                verification=source_deep_verification,
                metadata=source_deep_verification.get("metadata") or {},
                counts=source_deep_verification.get("counts") or {},
                source_path=source,
            )
        seal["file_stat"] = _file_stat(target)
        tmp_seal.write_text(
            json.dumps(seal, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        os.replace(tmp_seal, _cache_seal_path(target))
        errors = verify_router_sidecar_cache(target, cache_key=cache_key)
        if errors:
            raise RuntimeError("published router sidecar cache invalid: " + ", ".join(errors))
        return copy_mode
    finally:
        _cleanup_cache_files(tmp_path)


def restore_router_sidecar_cache(
    cache_path: Path | str,
    target_path: Path | str,
    *,
    cache_key: str,
    global_spine_path: Path | str,
    release_id: str,
    copy_file: Callable[[Path, Path], str],
) -> RouterSidecarCacheRestore:
    """Clone a cache hit, rebind its source, verify it, then atomically publish it."""
    source = Path(cache_path).expanduser().resolve()
    target = Path(target_path).expanduser().resolve()
    errors = verify_router_sidecar_cache(source, cache_key=cache_key)
    if errors:
        raise ValueError("invalid router sidecar cache: " + ", ".join(errors))
    cached_verification = verify_router_sidecar(source, deep=True)
    cached_metadata = cached_verification.get("metadata") or {}
    target.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = target.parent / f".{target.name}.{os.getpid()}.{time.time_ns()}.tmp"
    _cleanup_cache_files(tmp_path)
    try:
        copy_mode = copy_file(source, tmp_path)
        rebound = rebind_router_sidecar_source(
            tmp_path,
            global_spine_path=global_spine_path,
            release_id=release_id,
            expected_previous_global_spine_sha256=cached_metadata.get("source_global_spine_sha256"),
            expected_previous_release_id=cached_metadata.get("release_id"),
        )
        if not rebound.get("ok"):
            raise RuntimeError(
                "rebound router sidecar cache failed verification: "
                + ", ".join(rebound.get("errors") or [])
            )
        os.replace(tmp_path, target)
        final_verification = verify_router_sidecar(
            target,
            expected_global_spine_sha256=(rebound.get("metadata") or {}).get(
                "source_global_spine_sha256"
            ),
            expected_release_id=release_id,
            deep=False,
        )
        if not final_verification.get("ok"):
            raise RuntimeError(
                "published router sidecar cache restore failed verification: "
                + ", ".join(final_verification.get("errors") or [])
            )
        inherited_verification = {
            **cached_verification,
            "path": str(target),
            "metadata": final_verification.get("metadata") or {},
            "counts": final_verification.get("counts") or {},
            "integrity_check": "ok",
            "integrity_source": "immutable_cache_seal",
            "verification_mode": "router-sidecar-deep-sealed-inherited",
        }
        write_immutable_sqlite_cache_seal(
            target,
            kind="router_sidecar",
            cache_key=str(
                (final_verification.get("metadata") or {}).get("build_fingerprint_sha256") or ""
            ),
            verification=inherited_verification,
            metadata=final_verification.get("metadata") or {},
            counts=final_verification.get("counts") or {},
            source_path=source,
            details={"restore_mode": "metadata-rebind-from-immutable-cache"},
        )
        return RouterSidecarCacheRestore(
            path=target,
            cache_path=source,
            cache_key=cache_key,
            copy_mode=copy_mode,
            verification=final_verification,
        )
    finally:
        _cleanup_cache_files(tmp_path)


def _cache_seal_path(path: Path) -> Path:
    return path.with_name(path.name + ".seal.json")


def _file_stat(path: Path) -> dict[str, int]:
    stat = path.stat()
    return {
        "device": int(stat.st_dev),
        "inode": int(stat.st_ino),
        "size_bytes": int(stat.st_size),
        "mtime_ns": int(stat.st_mtime_ns),
    }


def _cleanup_cache_files(path: Path) -> None:
    for candidate in (
        path,
        Path(str(path) + "-wal"),
        Path(str(path) + "-shm"),
        _cache_seal_path(path),
        immutable_sqlite_cache_seal_path(path),
    ):
        candidate.unlink(missing_ok=True)
