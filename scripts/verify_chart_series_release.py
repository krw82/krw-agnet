#!/usr/bin/env python3
"""Verify the chart-series sidecar is part of an ontology data release.

This is an operator/build gate, not a per-question research check.  A missing
or stale sidecar must stop a sealed release before it is published; once a
release is admitted, a malformed individual presentation pack remains
failure-inert in the Rust runtime.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import sys
from typing import Any, NoReturn


def _fail(message: str) -> "NoReturn":
    raise SystemExit(f"chart_series_release_invalid:{message}")


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return f"sha256:{digest.hexdigest()}"


def _hash_matches(expected: Any, actual: str) -> bool:
    """Accept the two manifest spellings used by existing v3 releases."""
    if not isinstance(expected, str):
        return False
    normalized = expected.removeprefix("sha256:")
    return normalized == actual.removeprefix("sha256:")


def _safe_release_path(root: Path, raw: Any, label: str) -> Path:
    if not isinstance(raw, str) or not raw or Path(raw).is_absolute():
        _fail(f"{label}_path_invalid")
    unresolved = root / raw
    if unresolved.is_symlink():
        _fail(f"{label}_symlink_forbidden")
    candidate = unresolved.resolve()
    try:
        candidate.relative_to(root)
    except ValueError:
        _fail(f"{label}_path_escapes_release")
    if not candidate.is_file():
        _fail(f"{label}_missing_or_unsafe")
    return candidate


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--release-root", type=Path, required=True)
    args = parser.parse_args()

    # `releases/*/current` is intentionally a symlink in the local operator
    # layout. Resolve it once, then apply all containment checks to the real
    # immutable release directory.
    root = args.release_root.expanduser().resolve()
    if not root.is_dir():
        _fail("release_root_missing_or_unsafe")

    manifest_path = root / "manifest.json"
    if manifest_path.is_symlink() or not manifest_path.is_file():
        _fail("manifest_missing_or_unsafe")
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        _fail(f"manifest_invalid:{type(exc).__name__}")
    if not isinstance(manifest, dict):
        _fail("manifest_not_object")

    release_id = manifest.get("release_id")
    manifest_hash = manifest.get("manifest_hash")
    if not isinstance(release_id, str) or not release_id.strip():
        _fail("release_id_missing")

    indexes = manifest.get("indexes")
    chart_output = indexes.get("chart_series") if isinstance(indexes, dict) else None
    if not isinstance(chart_output, dict) or chart_output.get("required") is not True:
        _fail("manifest_chart_series_not_required")
    chart_path = _safe_release_path(root, chart_output.get("path"), "chart_series")

    shard_output = indexes.get("shard_manifest") if isinstance(indexes, dict) else None
    shard_path = _safe_release_path(
        root,
        shard_output.get("path") if isinstance(shard_output, dict) else None,
        "shard_manifest",
    )
    expected_shard_hash = shard_output.get("sha256") if isinstance(shard_output, dict) else None
    if expected_shard_hash and not _hash_matches(expected_shard_hash, _sha256(shard_path)):
        _fail("shard_manifest_hash_mismatch")
    try:
        shard_manifest_payload = json.loads(shard_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        _fail(f"shard_manifest_invalid:{type(exc).__name__}")
    if not isinstance(shard_manifest_payload, dict):
        _fail("shard_manifest_not_object")
    source_manifest_hash = (
        shard_manifest_payload.get("source_manifest_hash")
        or manifest.get("source_manifest_hash")
        or manifest_hash
    )
    if not isinstance(source_manifest_hash, str) or not source_manifest_hash.startswith("sha256:"):
        _fail("source_manifest_hash_missing")

    source_root = Path(__file__).resolve().parents[1]
    runtime_src = source_root / "services" / "krw-ontology-runtime" / "src"
    if not runtime_src.is_dir():
        _fail("runtime_source_missing")
    sys.path.insert(0, str(runtime_src))
    try:
        from krw_capability_runtime.agent_index.chart_series import verify_chart_series_index
    except ImportError as exc:
        _fail(f"runtime_import_failed:{exc.__class__.__name__}")

    verification = verify_chart_series_index(chart_path, deep=True)
    if not verification.get("ok"):
        _fail(";".join(str(error) for error in verification.get("errors") or []) or "sidecar_invalid")
    metadata = verification.get("metadata")
    if not isinstance(metadata, dict):
        _fail("sidecar_metadata_missing")
    if metadata.get("release_id") != release_id:
        _fail("sidecar_release_id_mismatch")
    if metadata.get("source_manifest_hash") != source_manifest_hash:
        _fail("sidecar_source_manifest_hash_mismatch")
    if chart_output.get("sha256") and not _hash_matches(
        chart_output.get("sha256"), _sha256(chart_path)
    ):
        _fail("chart_series_hash_mismatch")

    print(
        json.dumps(
            {
                "ok": True,
                "release_id": release_id,
                "chart_series": str(chart_path.relative_to(root)),
                "chart_series_sha256": _sha256(chart_path),
                "source_manifest_hash": source_manifest_hash,
            },
            ensure_ascii=False,
            sort_keys=True,
        )
    )
    return 0


if __name__ == "__main__":
    main()
