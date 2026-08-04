"""Shared fail-fast runtime admission for HTTP and stdio MCP transports."""

from __future__ import annotations

import hashlib
import os
from pathlib import Path
from typing import Any

from krw_capability_runtime.config.paths import (
    DEFAULT_ONTOLOGY_ROOT,
    ONTOLOGY_ENV_ENV,
    ONTOLOGY_GLOBAL_SPINE_PATH_ENV,
    ONTOLOGY_MANIFEST_PATH_ENV,
    ONTOLOGY_RELEASE_ROOT_ENV,
    ONTOLOGY_ROOT_ENV,
    ONTOLOGY_SHARD_MANIFEST_PATH_ENV,
)
from krw_capability_runtime.release import normalize_ontology_env, verify_release_startup_v3


def prepare_mcp_runtime(
    *,
    root: Path,
    env: str | None,
    expected_release_id: str | None = None,
    require_current_symlink: bool = False,
    store_mode: str = "persistent",
) -> dict[str, Any]:
    """Admit one physical immutable v3 release before exposing MCP.

    A production deployment may nominate ``.../prod/current`` as the
    *admission pointer*, but a running sidecar must never keep that symlink in
    its data path.  Resolve it once, verify the resulting release directory,
    and publish only the physical directory to the rest of the process.  A
    later promotion therefore starts a new sidecar/pool generation; it cannot
    silently switch an active run to another release.
    """
    resolved_env = normalize_ontology_env(env)
    normalized_store_mode = store_mode.strip().lower()
    if normalized_store_mode not in {"persistent", "per_call"}:
        raise RuntimeError(f"store_mode must be persistent or per_call, got {store_mode!r}")
    supplied_root = _absolute_without_resolving(root)
    require_symlink = require_current_symlink or resolved_env == "prod"
    if require_symlink and not _is_current_release_pointer(supplied_root):
        raise RuntimeError("production release admission requires an env/current symlink")
    physical_root = supplied_root.resolve()
    verification = verify_release_startup_v3(
        physical_root,
        env=resolved_env,
        manifest_path=physical_root / "manifest.json",
        require_current_symlink=False,
    )
    if not verification["ok"]:
        errors = ", ".join(str(error) for error in verification["errors"])
        raise RuntimeError(f"release verification failed: {errors}")
    if require_symlink and supplied_root.resolve() != physical_root:
        raise RuntimeError("release pointer changed during startup admission")
    release_id = verification.get("release_id")
    if expected_release_id and release_id != expected_release_id:
        raise RuntimeError(f"release_id mismatch: expected {expected_release_id}, got {release_id}")
    manifest_path = verification.get("manifest_path")
    verification["release_manifest_sha256"] = _verified_release_manifest_sha256(
        verification=verification,
        manifest_path=manifest_path,
    )
    runtime_root = physical_root
    runtime_global_spine_path = _runtime_global_spine_path(
        runtime_root=runtime_root,
        verification=verification,
    )
    shard_manifest_path = runtime_root / "indexes" / "shard_manifest.json"

    os.environ[ONTOLOGY_ENV_ENV] = resolved_env
    os.environ[ONTOLOGY_RELEASE_ROOT_ENV] = str(runtime_root)
    os.environ[ONTOLOGY_ROOT_ENV] = str(runtime_root)
    if manifest_path:
        os.environ[ONTOLOGY_MANIFEST_PATH_ENV] = str(manifest_path)
    os.environ[ONTOLOGY_GLOBAL_SPINE_PATH_ENV] = str(runtime_global_spine_path)
    os.environ[ONTOLOGY_SHARD_MANIFEST_PATH_ENV] = str(shard_manifest_path)
    os.environ["KRW_ONTOLOGY_INDEX_LAYOUT"] = str(verification["index_layout"])
    os.environ["KRW_MCP_STORE_MODE"] = normalized_store_mode
    verification["runtime_root"] = str(runtime_root)
    verification["admission_pointer"] = str(supplied_root)
    verification["admitted_release_root"] = str(physical_root)
    verification["runtime_global_spine_path"] = str(runtime_global_spine_path)
    verification["runtime_shard_manifest_path"] = str(shard_manifest_path)
    verification["runtime_store_opened"] = False
    return verification


def _verified_release_manifest_sha256(
    *,
    verification: dict[str, Any],
    manifest_path: Any,
) -> str:
    """Hash the exact manifest bytes that the admitted release exposes.

    The data-release fingerprint deliberately covers bytes, not a reconstructed
    JSON object.  This lets deployment binding pin one immutable release file
    and prevents readiness from inventing a semantic-but-different manifest.
    """

    if not isinstance(manifest_path, str) or not manifest_path:
        raise RuntimeError("verified release is missing manifest_path")
    path = Path(manifest_path)
    if path.is_symlink() or not path.is_file():
        raise RuntimeError("verified release manifest must be a regular file")
    try:
        raw = path.read_bytes()
    except OSError as error:
        raise RuntimeError("verified release manifest cannot be read for identity") from error
    if not raw:
        raise RuntimeError("verified release manifest is empty")
    # The parsed shape was already validated by ``verify_release_startup_v3``.
    # Check it is still the same logical manifest before using its byte pin.
    try:
        import json

        parsed = json.loads(raw)
    except json.JSONDecodeError as error:
        raise RuntimeError("verified release manifest changed to invalid JSON") from error
    if parsed != verification.get("manifest"):
        raise RuntimeError("verified release manifest changed during startup admission")
    return "sha256:" + hashlib.sha256(raw).hexdigest()


def configured_mcp_root() -> Path:
    """Return the configured release path without resolving its trust-boundary symlink."""
    raw = os.getenv(ONTOLOGY_RELEASE_ROOT_ENV) or os.getenv(ONTOLOGY_ROOT_ENV)
    if raw:
        return Path(raw).expanduser().absolute()
    return DEFAULT_ONTOLOGY_ROOT.expanduser().absolute()


def _absolute_without_resolving(path: Path) -> Path:
    return path.expanduser().absolute()


def _is_current_release_pointer(path: Path) -> bool:
    """Return whether ``path`` is the explicit env-scoped active pointer."""

    return path.name == "current" and path.parent.name in {"dev", "staging", "prod"} and path.is_symlink()


def _runtime_global_spine_path(
    *,
    runtime_root: Path,
    verification: dict[str, Any],
) -> Path:
    manifest = (
        verification.get("manifest") if isinstance(verification.get("manifest"), dict) else {}
    )
    raw_spine_path = manifest.get("global_spine_path")
    if not isinstance(raw_spine_path, str) or not raw_spine_path:
        raw_spine_path = ((manifest.get("indexes") or {}).get("global_spine") or {}).get("path")
    if isinstance(raw_spine_path, str) and raw_spine_path:
        candidate = Path(raw_spine_path).expanduser()
        return candidate.absolute() if candidate.is_absolute() else runtime_root / candidate
    raise RuntimeError("verified v3 release manifest is missing global spine path")
