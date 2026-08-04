"""Stat-bound verification seals for immutable content-addressed SQLite caches."""

from __future__ import annotations

import hashlib
import json
import os
import time
from collections.abc import Mapping
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


IMMUTABLE_SQLITE_CACHE_SEAL_FORMAT = "krw-ontology-immutable-sqlite-cache-seal/v1"
IMMUTABLE_SQLITE_CACHE_SEAL_SUFFIX = ".cache-seal.json"
# ``st_dev`` can change after an APFS volume remount or reboot without the
# immutable file changing. Keep it in a seal for diagnostics, but do not make
# a healthy cached verification fail solely because the mount identity moved.
IMMUTABLE_FILE_IDENTITY_FIELDS = ("inode", "size_bytes", "mtime_ns")


def immutable_sqlite_cache_seal_path(path: Path | str) -> Path:
    resolved = Path(path).expanduser().resolve()
    return resolved.with_name(resolved.name + IMMUTABLE_SQLITE_CACHE_SEAL_SUFFIX)


def read_immutable_sqlite_cache_seal(
    path: Path | str,
    *,
    kind: str,
    cache_key: str | None = None,
) -> tuple[dict[str, Any], str]:
    """Return a matching seal and status without scanning SQLite contents."""
    resolved = Path(path).expanduser().resolve()
    seal_path = immutable_sqlite_cache_seal_path(resolved)
    if not resolved.is_file():
        return {}, "database_missing"
    try:
        payload = json.loads(seal_path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return {}, "missing"
    except (OSError, json.JSONDecodeError):
        return {}, "invalid"
    if not isinstance(payload, Mapping):
        return {}, "invalid"
    result = dict(payload)
    if result.get("format") != IMMUTABLE_SQLITE_CACHE_SEAL_FORMAT:
        return result, "format_mismatch"
    if result.get("kind") != kind:
        return result, "kind_mismatch"
    if cache_key is not None and result.get("cache_key") != cache_key:
        return result, "cache_key_mismatch"
    if not immutable_file_identity_matches(
        result.get("database_identity"),
        immutable_file_identity(resolved),
    ):
        return result, "identity_mismatch"
    if result.get("deep_verified") is not True:
        return result, "not_deep_verified"
    return result, "valid"


def write_immutable_sqlite_cache_seal(
    path: Path | str,
    *,
    kind: str,
    cache_key: str,
    verification: Mapping[str, Any],
    metadata: Mapping[str, Any] | None = None,
    counts: Mapping[str, Any] | None = None,
    source_path: Path | str | None = None,
    details: Mapping[str, Any] | None = None,
) -> Path:
    """Bind a successful deep verification to the current immutable file identity."""
    resolved = Path(path).expanduser().resolve()
    if not resolved.is_file():
        raise FileNotFoundError(f"immutable cache database missing while sealing: {resolved}")
    if verification.get("ok") is not True:
        raise ValueError("cannot seal a failed immutable cache verification")
    integrity_check = verification.get("integrity_check")
    integrity_source = str(verification.get("integrity_source") or "")
    if integrity_check != "ok" and integrity_source not in {
        "sqlite_integrity_check",
        "immutable_cache_seal",
        "inherited_immutable_cache_seal",
    }:
        raise ValueError("immutable cache seal requires a successful deep integrity check")
    payload = {
        "format": IMMUTABLE_SQLITE_CACHE_SEAL_FORMAT,
        "kind": str(kind),
        "cache_key": str(cache_key),
        "deep_verified": True,
        "source_integrity": integrity_source or "sqlite_integrity_check",
        "database_identity": immutable_file_identity(resolved),
        "metadata": dict(metadata or verification.get("metadata") or {}),
        "counts": {
            str(key): int(value)
            for key, value in dict(counts or verification.get("counts") or {}).items()
            if isinstance(value, int) and not isinstance(value, bool)
        },
        "source_path": (
            str(Path(source_path).expanduser().resolve()) if source_path is not None else None
        ),
        "sha256": None,
        "sha256_database_identity": None,
        "details": dict(details or {}),
        "verified_at": datetime.now(timezone.utc).isoformat(),
    }
    seal_path = immutable_sqlite_cache_seal_path(resolved)
    seal_path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = seal_path.with_name(f".{seal_path.name}.{os.getpid()}.{time.time_ns()}.tmp")
    try:
        tmp_path.write_text(
            json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        os.replace(tmp_path, seal_path)
    finally:
        tmp_path.unlink(missing_ok=True)
    return seal_path


def read_immutable_sqlite_cache_sha256(path: Path | str) -> str | None:
    resolved = Path(path).expanduser().resolve()
    seal_path = immutable_sqlite_cache_seal_path(resolved)
    try:
        payload = json.loads(seal_path.read_text(encoding="utf-8"))
    except (FileNotFoundError, OSError, json.JSONDecodeError):
        return None
    if not isinstance(payload, Mapping):
        return None
    identity = immutable_file_identity(resolved) if resolved.is_file() else None
    digest = payload.get("sha256")
    if (
        payload.get("format") != IMMUTABLE_SQLITE_CACHE_SEAL_FORMAT
        or not immutable_file_identity_matches(payload.get("database_identity"), identity)
        or not immutable_file_identity_matches(payload.get("sha256_database_identity"), identity)
        or not isinstance(digest, str)
        or len(digest) != 64
        or any(char not in "0123456789abcdef" for char in digest)
    ):
        return None
    return digest


def record_immutable_sqlite_cache_sha256(path: Path | str, sha256: str) -> Path:
    if len(sha256) != 64 or any(char not in "0123456789abcdef" for char in sha256):
        raise ValueError("immutable cache sha256 must be lowercase 64-hex")
    resolved = Path(path).expanduser().resolve()
    seal_path = immutable_sqlite_cache_seal_path(resolved)
    payload = json.loads(seal_path.read_text(encoding="utf-8"))
    if not isinstance(payload, Mapping):
        raise ValueError("immutable cache seal payload invalid")
    identity = immutable_file_identity(resolved)
    if payload.get(
        "format"
    ) != IMMUTABLE_SQLITE_CACHE_SEAL_FORMAT or not immutable_file_identity_matches(
        payload.get("database_identity"), identity
    ):
        raise RuntimeError("cannot record sha256 for a changed immutable cache file")
    updated = {
        **dict(payload),
        "sha256": sha256,
        "sha256_database_identity": identity,
        "sha256_recorded_at": datetime.now(timezone.utc).isoformat(),
    }
    tmp_path = seal_path.with_name(f".{seal_path.name}.{os.getpid()}.{time.time_ns()}.tmp")
    try:
        tmp_path.write_text(
            json.dumps(updated, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        os.replace(tmp_path, seal_path)
    finally:
        tmp_path.unlink(missing_ok=True)
    return seal_path


def remove_immutable_sqlite_cache_seal(path: Path | str) -> None:
    immutable_sqlite_cache_seal_path(path).unlink(missing_ok=True)


def immutable_file_identity(path: Path | str) -> dict[str, int]:
    """Return the content-relevant stat identity for an immutable artifact.

    ``ctime`` is intentionally excluded: chmod, xattr, backup, and release
    bookkeeping can change it without changing a single content byte.  The
    inode, size, and nanosecond mtime still invalidate the seal for normal
    replacement, truncation, and in-place content mutations.
    """
    stat = Path(path).expanduser().resolve().stat()
    return {
        "device": int(stat.st_dev),
        "inode": int(stat.st_ino),
        "size_bytes": int(stat.st_size),
        "mtime_ns": int(stat.st_mtime_ns),
    }


def immutable_file_identity_matches(sealed: Any, current: Mapping[str, int]) -> bool:
    """Match immutable files across remounts while rejecting byte-relevant drift.

    Existing v1 seals may contain ``device`` and ``ctime``. Both are ignored:
    device is mount-scoped and ctime changes for metadata-only operations.
    """
    return bool(
        isinstance(sealed, Mapping)
        and all(sealed.get(field) == current.get(field) for field in IMMUTABLE_FILE_IDENTITY_FIELDS)
    )


def assert_trusted_immutable_copy(
    source_path: Path | str,
    target_path: Path | str,
    *,
    role: str,
    cached_source_sha256: str | None = None,
) -> None:
    """Prove a release copy before inheriting a source artifact's deep seal."""
    source = Path(source_path).expanduser().resolve()
    target = Path(target_path).expanduser().resolve()
    if not source.is_file():
        raise FileNotFoundError(f"trusted {role} source missing: {source}")
    if not target.is_file():
        raise FileNotFoundError(f"copied {role} missing: {target}")
    source_stat = source.stat()
    target_stat = target.stat()
    if (
        source_stat.st_size == target_stat.st_size
        and source_stat.st_mtime_ns == target_stat.st_mtime_ns
    ):
        return
    expected_sha256 = cached_source_sha256 or _file_sha256(source)
    if _file_sha256(target) != expected_sha256:
        raise ValueError(f"copied {role} SHA-256 mismatch")


def inherit_immutable_sqlite_cache_seal(
    source_path: Path | str,
    target_path: Path | str,
    *,
    kind: str,
    role: str,
    details: Mapping[str, Any] | None = None,
) -> Path:
    """Bind a copied immutable SQLite artifact to its new file identity."""
    source = Path(source_path).expanduser().resolve()
    target = Path(target_path).expanduser().resolve()
    seal, seal_status = read_immutable_sqlite_cache_seal(source, kind=kind)
    if seal_status != "valid":
        raise ValueError(f"cannot inherit {role} seal from source: {seal_status}")
    source_sha256 = read_immutable_sqlite_cache_sha256(source)
    assert_trusted_immutable_copy(
        source,
        target,
        role=role,
        cached_source_sha256=source_sha256,
    )
    verification = {
        "ok": True,
        "integrity_check": "ok",
        "integrity_source": "inherited_immutable_cache_seal",
        "verification_mode": "deep-sealed-inherited-copy",
        "metadata": dict(seal.get("metadata") or {}),
        "counts": dict(seal.get("counts") or {}),
    }
    seal_path = write_immutable_sqlite_cache_seal(
        target,
        kind=kind,
        cache_key=str(seal.get("cache_key") or ""),
        verification=verification,
        metadata=verification["metadata"],
        counts=verification["counts"],
        source_path=source,
        details={"inheritance": "byte-identical-release-copy", **dict(details or {})},
    )
    if source_sha256 is not None:
        record_immutable_sqlite_cache_sha256(target, source_sha256)
    return seal_path


def _file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(8 * 1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()
