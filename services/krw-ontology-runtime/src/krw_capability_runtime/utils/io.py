"""JSONL read/write, atomic file write, and SHA-256 utilities."""

from __future__ import annotations

import hashlib
import json
import os
import tempfile
from pathlib import Path
from typing import Any


def read_jsonl(path: Path) -> list[dict[str, Any]]:
    """Read a JSONL file into a list of dicts."""
    if not path.exists():
        return []
    objects: list[dict[str, Any]] = []
    with open(path) as f:
        for line in f:
            line = line.strip()
            if line:
                objects.append(json.loads(line))
    return objects


def write_jsonl(path: Path, objects: list[dict[str, Any]]) -> None:
    """Write a list of dicts as JSONL (one JSON object per line)."""
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w") as f:
        for obj in objects:
            f.write(json.dumps(obj, ensure_ascii=False) + "\n")


def atomic_write(path: Path, content: str) -> None:
    """Write content to a file atomically using temp file + rename."""
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_path = tempfile.mkstemp(dir=path.parent, prefix=".tmp_")
    try:
        with os.fdopen(fd, "w") as f:
            f.write(content)
        os.replace(tmp_path, path)
    except BaseException:
        # Clean up temp file on failure
        try:
            os.unlink(tmp_path)
        except OSError:
            pass
        raise


def atomic_write_json(path: Path, data: Any) -> None:
    """JSON pretty-print + atomic write."""
    content = json.dumps(data, indent=2, ensure_ascii=False)
    atomic_write(path, content)


def compute_sha256(content: bytes) -> str:
    """Compute SHA-256 hex digest of bytes."""
    return hashlib.sha256(content).hexdigest()


def find_project_root(start: Path | None = None) -> Path:
    """Find project root by walking up to find pyproject.toml."""
    current = start or Path.cwd()
    while current != current.parent:
        if (current / "pyproject.toml").exists():
            return current
        current = current.parent
    return Path.cwd()
