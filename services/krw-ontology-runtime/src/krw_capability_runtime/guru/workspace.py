"""Workspace creation for the standalone guru ontology pipeline."""

from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
from datetime import datetime, timezone
from typing import Any

import yaml

from krw_capability_runtime.guru.planner import (
    build_collection_plan,
    build_ontology_manifest,
    build_source_manifest,
)

GURU_ROOT_ENV = "KRW_GURU_ROOT"
GURU_RUNNING_ROOT_ENV = "KRW_GURU_RUNNING_ROOT"
GURU_DATA_ROOT_ENV = "KRW_GURU_DATA_ROOT"
GURU_ENV_ENV = "KRW_GURU_ENV"
GURU_RELEASE_ROOT_ENV = "KRW_GURU_RELEASE_ROOT"
DEFAULT_GURU_ROOT_NAME = "krw-ontology-guru"
DEFAULT_GURU_RUNNING_ROOT_NAME = "krw-ontology-guru-running"
DEFAULT_GURU_DATA_ROOT_NAME = "krw-ontology-guru-data"
DEFAULT_GURU_ENV = "prod"
ALLOWED_GURU_ENVS = {"dev", "staging", "prod"}
GURU_RELEASE_FORMAT = "krw-guru-release/v1"
GURU_RELEASE_MANIFEST_FILENAME = "manifest.json"

_RELEASE_SKIP_NAMES = {
    ".krw_pipeline",
    "generated",
    "logs",
    "parsed",
    "raw",
    "spans",
}


def default_guru_root() -> Path:
    value = os.environ.get(GURU_ROOT_ENV)
    if value:
        return Path(value).expanduser().resolve()
    release_value = os.environ.get(GURU_RELEASE_ROOT_ENV)
    if release_value:
        return Path(release_value).expanduser().resolve()
    return guru_release_root()


def default_guru_running_root() -> Path:
    value = os.environ.get(GURU_RUNNING_ROOT_ENV)
    if value:
        return Path(value).expanduser().resolve()
    return default_guru_data_root() / "runs" / "default"


def default_guru_data_root() -> Path:
    value = os.environ.get(GURU_DATA_ROOT_ENV)
    return Path(value).expanduser().resolve() if value else Path.home() / DEFAULT_GURU_DATA_ROOT_NAME


def default_guru_env() -> str:
    return normalize_guru_env(os.environ.get(GURU_ENV_ENV))


def normalize_guru_env(env: str | None) -> str:
    value = (env or DEFAULT_GURU_ENV).strip().lower()
    if value not in ALLOWED_GURU_ENVS:
        raise ValueError(
            f"Unsupported guru env {env!r}; expected one of {sorted(ALLOWED_GURU_ENVS)}"
        )
    return value


def guru_releases_root(data_root: Path | str | None = None) -> Path:
    root_path = Path(data_root).expanduser().resolve() if data_root else default_guru_data_root()
    return root_path / "releases"


def guru_env_root(
    env: str | None = None,
    *,
    data_root: Path | str | None = None,
) -> Path:
    return guru_releases_root(data_root) / normalize_guru_env(env or default_guru_env())


def guru_release_root(
    env: str | None = None,
    *,
    release_id: str = "current",
    data_root: Path | str | None = None,
) -> Path:
    return guru_env_root(env, data_root=data_root) / release_id


def guru_runs_root(data_root: Path | str | None = None) -> Path:
    root_path = Path(data_root).expanduser().resolve() if data_root else default_guru_data_root()
    return root_path / "runs"


def guru_root(root: Path | str | None = None) -> Path:
    return Path(root).expanduser().resolve() if root is not None else default_guru_root()


def guru_running_root(root: Path | str | None = None) -> Path:
    if root is None:
        return default_guru_running_root()
    return Path(root).expanduser().resolve()


def guru_release_status(
    *,
    env: str | None = None,
    release_id: str = "current",
    data_root: Path | str | None = None,
) -> dict[str, Any]:
    """Return operational release metadata for the guru ontology."""
    env_name = normalize_guru_env(env or default_guru_env())
    env_path = guru_env_root(env_name, data_root=data_root)
    release_path = env_path / release_id
    resolved_path = release_path.resolve() if release_path.exists() else release_path
    manifest_path = resolved_path / GURU_RELEASE_MANIFEST_FILENAME
    manifest = _read_json(manifest_path)
    current_path = env_path / "current"
    current_target = os.readlink(current_path) if current_path.is_symlink() else None
    return {
        "format": GURU_RELEASE_FORMAT,
        "env": env_name,
        "data_root": str((Path(data_root).expanduser().resolve() if data_root else default_guru_data_root())),
        "env_root": str(env_path),
        "release_id": release_id,
        "release_root": str(resolved_path),
        "release_exists": resolved_path.exists(),
        "manifest_path": str(manifest_path),
        "manifest_exists": manifest_path.is_file(),
        "manifest": manifest,
        "current_symlink": current_path.is_symlink(),
        "current_symlink_path": str(current_path),
        "current_symlink_target": current_target,
        "current_release_id": Path(current_target).name.rstrip("/") if current_target else None,
        "root_is_current_symlink": release_id == "current" and release_path.is_symlink(),
    }


def promote_guru_release(
    source_root: Path | str,
    *,
    env: str | None = None,
    release_id: str | None = None,
    data_root: Path | str | None = None,
    force: bool = False,
) -> dict[str, Any]:
    """Copy a reviewed guru root into releases/<env>/<release_id> and update current."""
    source_path = Path(source_root).expanduser().resolve()
    if not source_path.exists():
        raise FileNotFoundError(f"Guru source root does not exist: {source_path}")
    env_name = normalize_guru_env(env or default_guru_env())
    release_name = release_id or _default_release_id()
    if release_name in {"current", ".", ".."} or "/" in release_name:
        raise ValueError(f"Invalid guru release_id: {release_name!r}")

    env_path = guru_env_root(env_name, data_root=data_root)
    release_path = env_path / release_name
    if release_path.exists():
        if not force:
            raise FileExistsError(f"Guru release already exists: {release_path}")
        _remove_path(release_path)
    release_path.mkdir(parents=True, exist_ok=True)

    copied_entries: list[str] = []
    for source_entry in sorted(source_path.iterdir(), key=lambda path: path.name):
        if source_entry.name in _RELEASE_SKIP_NAMES:
            continue
        destination = release_path / source_entry.name
        _copy_path(source_entry, destination)
        copied_entries.append(source_entry.name)

    manifest = _build_guru_release_manifest(
        release_path=release_path,
        source_path=source_path,
        env=env_name,
        release_id=release_name,
        copied_entries=copied_entries,
    )
    _write_json_atomic(release_path / GURU_RELEASE_MANIFEST_FILENAME, manifest)
    _point_current_symlink(env_path, release_name, force=force)
    return guru_release_status(env=env_name, release_id="current", data_root=data_root) | {
        "promoted_release_id": release_name,
        "promoted_release_root": str(release_path),
        "source_root": str(source_path),
        "copied_entries": copied_entries,
    }


def rollback_guru_release(
    *,
    env: str | None = None,
    release_id: str,
    data_root: Path | str | None = None,
    force: bool = False,
) -> dict[str, Any]:
    """Point releases/<env>/current at an existing guru release."""
    env_name = normalize_guru_env(env or default_guru_env())
    env_path = guru_env_root(env_name, data_root=data_root)
    release_path = env_path / release_id
    if not release_path.is_dir():
        raise FileNotFoundError(f"Guru release does not exist: {release_path}")
    _point_current_symlink(env_path, release_id, force=force)
    return guru_release_status(env=env_name, release_id="current", data_root=data_root) | {
        "rolled_back_to_release_id": release_id,
        "rolled_back_to_release_root": str(release_path),
    }


def initialize_guru_workspace(
    root: Path | str | None = None,
    *,
    running_root: Path | str | None = None,
    author_keys: list[str] | None = None,
    force: bool = False,
) -> dict[str, Any]:
    """Create a guru workspace and write planning manifests.

    This function does not fetch, parse, or store full source text.
    """
    root_path = guru_root(root)
    running_path = guru_running_root(running_root)
    root_path.mkdir(parents=True, exist_ok=True)
    running_path.mkdir(parents=True, exist_ok=True)
    for relative in ("reviewed", "published"):
        (root_path / relative).mkdir(parents=True, exist_ok=True)
    for relative in (
        "raw",
        "parsed",
        "spans",
        "generated",
        "generated/requests",
        "generated/responses",
        "logs",
    ):
        (running_path / relative).mkdir(parents=True, exist_ok=True)

    source_manifest = build_source_manifest(author_keys)
    ontology_manifest = build_ontology_manifest(author_keys)
    collection_plan = build_collection_plan(root_path, author_keys, running_root=running_path)

    outputs = {
        "source_manifest": root_path / "source_manifest.yaml",
        "ontology_manifest": root_path / "ontology_manifest.json",
        "collection_plan": root_path / "collection_plan.json",
    }
    for path in outputs.values():
        if path.exists() and not force:
            raise FileExistsError(f"Guru workspace file already exists: {path}")

    _write_yaml_atomic(outputs["source_manifest"], source_manifest.model_dump(mode="json"))
    _write_json_atomic(outputs["ontology_manifest"], ontology_manifest.model_dump(mode="json"))
    _write_json_atomic(outputs["collection_plan"], collection_plan.model_dump(mode="json"))

    return {
        "root": str(root_path),
        "running_root": str(running_path),
        "collection_started": False,
        "extraction_started": False,
        "author_keys": collection_plan.author_keys,
        "files": {name: str(path) for name, path in outputs.items()},
    }


def guru_workspace_status(
    root: Path | str | None = None,
    *,
    running_root: Path | str | None = None,
) -> dict[str, Any]:
    """Return a cheap status summary for a guru workspace."""
    root_path = guru_root(root)
    running_path = guru_running_root(running_root)
    files = {
        "source_manifest": root_path / "source_manifest.yaml",
        "ontology_manifest": root_path / "ontology_manifest.json",
        "collection_plan": root_path / "collection_plan.json",
        "discovery_manifest": running_path / "discovery_manifest.json",
        "raw_manifest": running_path / "raw_manifest.json",
        "parsed_manifest": running_path / "parsed_manifest.json",
        "extraction_manifest": running_path / "generated" / "extraction_manifest.json",
    }
    return {
        "root": str(root_path),
        "running_root": str(running_path),
        "release": guru_release_status() if root is None else None,
        "exists": root_path.exists(),
        "running_exists": running_path.exists(),
        "collection_started": (running_path / "raw_manifest.json").exists(),
        "extraction_started": (running_path / "generated" / "extraction_manifest.json").exists(),
        "files": {name: {"path": str(path), "exists": path.exists()} for name, path in files.items()},
    }


def _build_guru_release_manifest(
    *,
    release_path: Path,
    source_path: Path,
    env: str,
    release_id: str,
    copied_entries: list[str],
) -> dict[str, Any]:
    reviewed_dir = release_path / "reviewed"
    reviewed_files = {
        path.name: {
            "path": str(path.relative_to(release_path)),
            "rows": _count_jsonl_rows(path),
        }
        for path in sorted(reviewed_dir.glob("*.jsonl"))
    }
    return {
        "format": GURU_RELEASE_FORMAT,
        "release_id": release_id,
        "env": env,
        "created_at": _utc_now(),
        "source_root": str(source_path),
        "release_root": str(release_path),
        "copied_entries": copied_entries,
        "layout": {
            "reviewed_dir": "reviewed",
            "published_dir": "published",
            "index_dir": "indexes",
            "reports_dir": "reports",
            "shards_dir": "indexes/shards",
        },
        "files": {
            "source_manifest": {
                "path": "source_manifest.yaml",
                "exists": (release_path / "source_manifest.yaml").is_file(),
            },
            "ontology_manifest": {
                "path": "ontology_manifest.json",
                "exists": (release_path / "ontology_manifest.json").is_file(),
            },
            "collection_plan": {
                "path": "collection_plan.json",
                "exists": (release_path / "collection_plan.json").is_file(),
            },
            "guru_shard_manifest": {
                "path": "indexes/guru_shard_manifest.json",
                "exists": (release_path / "indexes" / "guru_shard_manifest.json").is_file(),
            },
        },
        "reviewed_files": reviewed_files,
        "runtime_policy": {
            "serve_root": "releases/<env>/current",
            "collection_root": "runs/<run-id>",
            "source_of_truth": "reviewed_jsonl",
        },
    }


def _copy_path(source: Path, destination: Path) -> None:
    if source.is_dir():
        shutil.copytree(source, destination, symlinks=True)
        return
    if source.is_file() or source.is_symlink():
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, destination, follow_symlinks=False)


def _remove_path(path: Path) -> None:
    if path.is_symlink() or path.is_file():
        path.unlink()
    elif path.is_dir():
        shutil.rmtree(path)


def _point_current_symlink(env_path: Path, release_id: str, *, force: bool) -> None:
    env_path.mkdir(parents=True, exist_ok=True)
    current_path = env_path / "current"
    if current_path.exists() and not current_path.is_symlink():
        if not force:
            raise FileExistsError(f"Guru current path is not a symlink: {current_path}")
        _remove_path(current_path)
    tmp_path = env_path / f".current.tmp-{os.getpid()}"
    tmp_path.unlink(missing_ok=True)
    tmp_path.symlink_to(release_id)
    os.replace(tmp_path, current_path)


def _count_jsonl_rows(path: Path) -> int:
    try:
        with path.open(encoding="utf-8") as handle:
            return sum(1 for line in handle if line.strip())
    except OSError:
        return 0


def _default_release_id() -> str:
    return datetime.now(timezone.utc).strftime("%Y%m%d%H%M%S")


def _utc_now() -> str:
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat()


def _read_json(path: Path) -> dict[str, Any]:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (FileNotFoundError, json.JSONDecodeError, OSError):
        return {}
    return payload if isinstance(payload, dict) else {}


def _write_json_atomic(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = path.with_name(f".{path.name}.tmp")
    tmp_path.write_text(
        json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    tmp_path.replace(path)


def _write_yaml_atomic(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = path.with_name(f".{path.name}.tmp")
    tmp_path.write_text(
        yaml.safe_dump(payload, allow_unicode=False, sort_keys=False),
        encoding="utf-8",
    )
    tmp_path.replace(path)
