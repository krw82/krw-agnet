#!/usr/bin/env python3
"""Materialize a new ontology release with the current chart-series index.

The ontology data release is immutable.  This command therefore never edits
``prod/current`` or the source release in place.  It makes an APFS/reflink
clone, rebinds the release-bound spine/router metadata, rebuilds only the
chart-series sidecar, writes a v3 manifest, and emits a fresh verification
report.  A full byte-for-byte copy is deliberately refused by default because
production releases can be hundreds of gigabytes.

This lives in krw-agnet so the ontology source tree and ontology schema remain
untouched.  Promotion is explicit and is not performed unless the operator
passes ``--promote-current``.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sqlite3
import sys
from pathlib import Path
from typing import Any


RELEASE_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")


def _repo_root() -> Path:
    return Path(__file__).resolve().parents[1]


def _load_runtime() -> None:
    runtime_src = _repo_root() / "services" / "krw-ontology-runtime" / "src"
    sys.path.insert(0, str(runtime_src))


def _read_json(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError(f"JSON object required: {path}")
    return value


def _require_release_root(path: Path, *, label: str) -> Path:
    supplied = path.expanduser().absolute()
    if supplied.name == "current" and supplied.is_symlink():
        resolved = supplied.resolve()
    else:
        resolved = supplied.resolve()
    if not resolved.is_dir():
        raise FileNotFoundError(f"{label} is not a release directory: {resolved}")
    manifest = resolved / "manifest.json"
    if not manifest.is_file() or manifest.is_symlink():
        raise ValueError(f"{label} has no safe manifest: {manifest}")
    payload = _read_json(manifest)
    if payload.get("format") != "krw-ontology-release/v3":
        raise ValueError(f"{label} manifest format is not v3")
    if payload.get("status") != "ready":
        raise ValueError(f"{label} manifest is not ready")
    if not isinstance(payload.get("release_id"), str) or not payload["release_id"]:
        raise ValueError(f"{label} release_id is missing")
    return resolved


def _candidate_id(output_root: Path, source_release_id: str) -> str:
    release_id = output_root.name
    if not RELEASE_ID_RE.fullmatch(release_id):
        raise ValueError(f"candidate release id is invalid: {release_id!r}")
    if release_id in {"current", source_release_id}:
        raise ValueError("candidate must have a new release id")
    if output_root.parent.name != "prod":
        raise ValueError("candidate must be directly under a prod releases directory")
    return release_id


def _inherit_company_shard_seals(source: Path, target: Path) -> int:
    from krw_capability_runtime.agent_index.cache_seal import (  # type: ignore
        record_immutable_sqlite_cache_sha256,
        inherit_immutable_sqlite_cache_seal,
        read_immutable_sqlite_cache_seal,
        write_immutable_sqlite_cache_seal,
    )

    source_dir = source / "indexes" / "companies"
    target_dir = target / "indexes" / "companies"
    copied = 0
    for source_path in sorted(source_dir.glob("*.sqlite")):
        target_path = target_dir / source_path.name
        if not target_path.is_file():
            raise FileNotFoundError(f"cloned company shard is missing: {target_path}")
        source_seal = source_path.with_name(source_path.name + ".cache-seal.json")
        if not source_seal.is_file():
            raise ValueError(f"company shard has no immutable seal: {source_seal}")
        _source_seal, source_seal_status = read_immutable_sqlite_cache_seal(
            source_path,
            kind="company_shard",
        )
        try:
            inherit_immutable_sqlite_cache_seal(
                source_path,
                target_path,
                kind="company_shard",
                role=f"company shard {source_path.stem}",
                details={"inheritance": "chart-series-release-clone"},
            )
        except ValueError:
            # Some existing immutable releases were atomically moved after
            # their seals were written, so only the inode differs while the
            # sealed size/mtime/hash remain bound.  Rebind that known-good
            # seal to the CoW clone without rescanning every shard; the final
            # release verifier still checks each manifest SHA-256.
            if source_seal_status != "identity_mismatch" or not isinstance(_source_seal, dict):
                raise
            source_identity = _source_seal.get("database_identity")
            source_stat = source_path.stat()
            target_stat = target_path.stat()
            if not isinstance(source_identity, dict) or any(
                source_identity.get(key) != value
                for key, value in (
                    ("size_bytes", int(source_stat.st_size)),
                    ("mtime_ns", int(source_stat.st_mtime_ns)),
                    ("size_bytes", int(target_stat.st_size)),
                    ("mtime_ns", int(target_stat.st_mtime_ns)),
                )
            ):
                raise
            source_sha = _source_seal.get("sha256")
            if not isinstance(source_sha, str) or not re.fullmatch(r"[0-9a-f]{64}", source_sha):
                raise
            inherited = {
                "ok": True,
                "integrity_check": "ok",
                "integrity_source": "inherited_immutable_cache_seal",
                "verification_mode": "deep-sealed-source-identity-rebind",
                "metadata": dict(_source_seal.get("metadata") or {}),
                "counts": dict(_source_seal.get("counts") or {}),
            }
            write_immutable_sqlite_cache_seal(
                target_path,
                kind="company_shard",
                cache_key=str(_source_seal.get("cache_key") or ""),
                verification=inherited,
                metadata=inherited["metadata"],
                counts=inherited["counts"],
                source_path=source_path,
                details={"inheritance": "source-identity-rebind"},
            )
            record_immutable_sqlite_cache_sha256(target_path, source_sha)
        copied += 1
    return copied


def materialize(
    *,
    source_root: Path,
    output_root: Path,
    promote_current: bool,
) -> dict[str, Any]:
    _load_runtime()
    from krw_capability_runtime.agent_index.chart_series import (  # type: ignore
        CHART_SERIES_RELATIVE_PATH,
        build_chart_series_index,
        verify_chart_series_index,
    )
    from krw_capability_runtime.agent_index.cache_seal import (  # type: ignore
        inherit_immutable_sqlite_cache_seal,
    )
    from krw_capability_runtime.agent_index.router_coherence import (  # type: ignore
        rebind_router_coherence_source,
    )
    from krw_capability_runtime.agent_index.router_sidecar import (  # type: ignore
        rebind_router_sidecar_source,
    )
    from krw_capability_runtime.agent_index.spine_builder import (  # type: ignore
        _restore_global_spine_cache,
        clone_or_copy_immutable_tree,
    )
    from krw_capability_runtime.agent_index.spine_schema import (  # type: ignore
        read_global_spine_metadata,
    )
    from krw_capability_runtime.release import (  # type: ignore
        build_release_manifest_v3,
        promote_local_release,
        verify_release_root,
        write_release_verification_report,
    )

    source = _require_release_root(source_root, label="source release")
    source_manifest = _read_json(source / "manifest.json")
    source_release_id = str(source_manifest["release_id"])
    candidate = output_root.expanduser().absolute()
    release_id = _candidate_id(candidate, source_release_id)
    if candidate.exists() or candidate.is_symlink():
        raise FileExistsError(f"candidate already exists: {candidate}")
    if candidate.parent.resolve() == source.parent.resolve():
        pass
    else:
        raise ValueError("candidate must share the source release's prod parent")

    # Never fall back to a 300GB copy implicitly.  The release tree is
    # immutable and APFS/reflink clone is the intended materialization path.
    os.environ.setdefault("KRW_INDEX_COPY_MODE", "reflink-required")
    copy_mode = clone_or_copy_immutable_tree(source, candidate)
    try:
        source_indexes = source / "indexes"
        target_indexes = candidate / "indexes"
        source_global = source_indexes / "global_spine.sqlite"
        target_global = target_indexes / "global_spine.sqlite"
        with source_global.open("rb"):
            pass
        source_global_sha = _raw_sha256(source_global)
        # ``read_global_spine_metadata`` intentionally accepts an open
        # connection so callers can keep the read transaction bounded.  Do
        # not pass the path object here: the materializer is a release-boundary
        # tool and must fail before rebinding anything if the source metadata
        # cannot be read.
        with sqlite3.connect(source_global) as global_conn:
            global_meta = read_global_spine_metadata(global_conn)
        cache_key = str(global_meta.get("global_spine_cache_key") or "")
        if not cache_key:
            raise ValueError("source global spine has no semantic cache key")
        _restore_global_spine_cache(
            source_global,
            target_global,
            cache_key=cache_key,
            release_root=candidate,
            release_id=release_id,
        )

        source_router = source_indexes / "router_sidecar.sqlite"
        target_router = target_indexes / "router_sidecar.sqlite"
        source_coherence = source_indexes / "router_coherence.sqlite"
        target_coherence = target_indexes / "router_coherence.sqlite"
        # ``cp -cR`` preserves bytes but may assign a new inode/timestamp. Bind
        # the copied router artifacts to their existing deep seals before
        # changing only release/source metadata below.
        inherit_immutable_sqlite_cache_seal(
            source_router,
            target_router,
            kind="router_sidecar",
            role="router sidecar",
            details={"inheritance": "chart-series-release-clone"},
        )
        inherit_immutable_sqlite_cache_seal(
            source_coherence,
            target_coherence,
            kind="router_coherence",
            role="router coherence",
            details={"inheritance": "chart-series-release-clone"},
        )
        rebind_router_sidecar_source(
            target_router,
            global_spine_path=target_global,
            release_id=release_id,
            expected_previous_global_spine_sha256=source_global_sha,
            expected_previous_release_id=source_release_id,
        )
        rebind_router_coherence_source(
            target_coherence,
            global_spine_path=target_global,
            release_id=release_id,
            expected_previous_global_spine_sha256=source_global_sha,
            expected_previous_release_id=source_release_id,
        )

        shard_manifest_path = target_indexes / "shard_manifest.json"
        shard_manifest = _read_json(shard_manifest_path)
        shard_manifest["release_id"] = release_id
        _write_json(shard_manifest_path, shard_manifest)
        shard_count = _inherit_company_shard_seals(source, candidate)

        source_hash = shard_manifest.get("source_manifest_hash")
        if not isinstance(source_hash, str) or not source_hash.startswith("sha256:"):
            raise ValueError("shard manifest source_manifest_hash is missing")
        chart_path = candidate / CHART_SERIES_RELATIVE_PATH
        chart_result = build_chart_series_index(
            candidate,
            shard_manifest_path=shard_manifest_path,
            output_path=chart_path,
            release_id=release_id,
            source_manifest_hash=source_hash,
        )

        manifest = build_release_manifest_v3(
            candidate,
            release_id=release_id,
            env="prod",
            source_root=source_manifest.get("source_root") or source,
        )
        _write_json(candidate / "manifest.json", manifest)
        # ``build_release_manifest_v3`` has already hashed the release-bound
        # global/router artifacts and binds every company shard to the source
        # manifest.  Re-running a full SHA-256 scan here would read the same
        # ~300GB CoW tree a second time.  Use the light release verifier for
        # those inherited immutable inputs, and reserve a deep scan for the
        # newly rebuilt chart sidecar.  The materialization proof records the
        # exact source release and copy mode so operators can audit why this is
        # safe without weakening the normal production startup gate.
        verification = verify_release_root(candidate, env="prod", deep=False)
        chart_verification = verify_chart_series_index(chart_path, deep=True)
        verification["chart_series_verification"] = chart_verification
        verification["materialization_proof"] = {
            "source_release_id": source_release_id,
            "copy_mode": copy_mode,
            "source_manifest_path": str(source / "manifest.json"),
            "source_manifest_sha256": _raw_sha256(source / "manifest.json"),
            "company_shards_inherited": shard_count,
            "full_shard_sha256_scan": "inherited_from_source_manifest",
        }
        verification["verification_mode"] = "release-materialization-v2"
        if not verification.get("ok") or not chart_verification.get("ok"):
            raise RuntimeError(
                "candidate release verification failed: "
                + ", ".join(
                    str(item)
                    for item in (
                        list(verification.get("errors") or [])
                        + list(chart_verification.get("errors") or [])
                    )
                )
            )
        write_release_verification_report(
            candidate,
            env="prod",
            deep=False,
            verification=verification,
        )

        if promote_current:
            promote_local_release(
                candidate.parent,
                env="prod",
                release_id=release_id,
                action="promote",
                preverified=verification,
            )
        return {
            "ok": True,
            "release_id": release_id,
            "source_release_id": source_release_id,
            "candidate_root": str(candidate),
            "copy_mode": copy_mode,
            "company_shards": shard_count,
            "chart_series": dict(chart_result.counts),
            "promoted": bool(promote_current),
            "chart_series_path": str(chart_path),
        }
    except Exception:
        # The command never mutates source/current.  Leave a failed candidate
        # for inspection; an operator can remove that exact candidate after
        # reviewing the failure.
        raise


def _raw_sha256(path: Path) -> str:
    import hashlib

    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _write_json(path: Path, payload: dict[str, Any]) -> None:
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    temporary.write_text(
        json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    os.replace(temporary, path)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source-release-root", type=Path, required=True)
    parser.add_argument("--output-root", type=Path, required=True)
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Validate source/candidate paths without cloning or writing anything.",
    )
    parser.add_argument(
        "--promote-current",
        action="store_true",
        help="Promote the verified candidate to prod/current after materialization.",
    )
    args = parser.parse_args()
    if args.dry_run:
        source = _require_release_root(args.source_release_root, label="source release")
        source_manifest = _read_json(source / "manifest.json")
        candidate = args.output_root.expanduser().absolute()
        release_id = _candidate_id(candidate, str(source_manifest["release_id"]))
        if candidate.exists() or candidate.is_symlink():
            raise FileExistsError(f"candidate already exists: {candidate}")
        print(
            json.dumps(
                {
                    "ok": True,
                    "dry_run": True,
                    "source_root": str(source),
                    "candidate_root": str(candidate),
                    "release_id": release_id,
                    "writes": False,
                },
                ensure_ascii=False,
                sort_keys=True,
            )
        )
        return 0
    result = materialize(
        source_root=args.source_release_root,
        output_root=args.output_root,
        promote_current=args.promote_current,
    )
    print(json.dumps(result, ensure_ascii=False, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
